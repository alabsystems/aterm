// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stats deck in motion, as the presents show it (`docs/DESIGN-stats-deck-gpu-motion-2026-10-26.md`):
//! which presents showed a deck card the splice changed, and what each such frame cost the main
//! thread from what asked for it to the present's return (`metrics percentiles`' `deck_frame_*`),
//! beside the renderer's tray card uploads and compares (`ctl metrics`' `tray_*`); the deck
//! card's generations (card serials, [`aterm_gpu::TrayQuad::serial`]), which tell the renderer
//! what changed without comparing the card (§3.6);
//! and each window's deck clock (§3.4), which draws the needle's frames on the display's vsyncs.
//!
//! **The deck clock.** A window's clock runs while its needle has a frame due
//! ([`crate::stats_live::LiveFacts::needle_due`]: the needle's own whole-pixel law, unchanged).
//! Adjacent panel frames come from the window's display link where it has one
//! (`aterm_window::Window::set_frame_clock`, `WindowEvent::FrameTick`, macOS 14 and later),
//! asked through an injected port whose test arm is inert. Isolated pixel steps and windows
//! without a running link use a synthetic vsync grid driven by the loop's deadline
//! (`DeadlineOwner::DeckClock`); a headless window's panel is 60 Hz or the dev seam
//! `ATERM_DEV_PANEL_HZ`. A tick
//! samples the needle at the vsync it targets and asks a frame only where that draws the needle
//! elsewhere; every frame samples it at the vsync it will reach ([`DeckClock::sample_at`]). The
//! link is paused across a gap longer than [`LINK_LEAD`] periods and from the vsync the needle
//! rests on, so a still deck costs no tick.
//!
//! **A link that never ticks.** A link asked to run that sends no tick for [`LINK_SILENCE`]
//! (a hang detector: its silence counts only while it is asked to run) falls silent: the window's
//! clock then keeps the loop's timer, as a window with no link does (the synthetic grid at the
//! panel's period, and the effect lane's interval for the needle, today's cadence), and the
//! metrics count it (`deck_clock_links_silent`, `deck_clock_link_fallbacks`). The link is still
//! run and paused as the needle wants, so its first tick after that puts the clock back on it
//! (`deck_clock_link_resumes`). `examples/objc_frame_clock_drive.rs` (`verify appkit`) drives
//! both on a real window.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use aterm_window::window::Window;

use crate::{App, WindowId, WindowState, metrics};

/// How far ahead of a needle frame due a window's display link runs, in its periods: across a
/// longer gap the link is paused and the loop's deadline resumes it this far ahead, so a
/// sweep's slow tail costs a few wakes a frame, not one a vsync. A resumed link's first tick
/// targets the next vsync; one period covers that startup without waking for earlier frames
/// on which the needle cannot move a pixel.
pub(crate) const LINK_LEAD: u32 = 1;

/// The largest gap between adjacent frames in base panel intervals (including the minimum
/// tick floor). This admits the doubled interval used for a parked drawable or Low Power Mode.
/// Longer gaps are isolated steps on the timer grid.
const LINK_DENSE_PERIODS: u32 = 2;

/// How long past a frame due a running link may stay silent before the loop draws the frame
/// itself, in its periods: a link whose window left every screen never strands the needle.
pub(crate) const LINK_GRACE: u32 = 4;

/// How long a window's display link may be asked to run without one tick before its deck clock
/// falls back to the loop's timer ([`DeckClock::watch_link`]): a hang detector, not a latency
/// budget. A running link ticks on its screen's next vsync, within one period (16.7 ms at the
/// 60 Hz floor `frame_clock.rs` holds it to), so this is 15 periods at 60 Hz and 30 at 120: a main
/// thread busy for several frames (a burst of output, a resize) does not trip it, and a false trip
/// costs one frame on the timer before the next tick resumes the link; while a link that never
/// ticks (a window no screen shows, a link the OS never fires) costs at most this long of
/// [`LINK_GRACE`]'s stand-in frames before the deck is back on its cadence without a link.
pub(crate) const LINK_SILENCE: Duration = Duration::from_millis(250);

/// The synthetic panel of a window with no display (headless), frames per second, where
/// `ATERM_DEV_PANEL_HZ` does not set it.
const HEADLESS_HZ: u32 = 60;

/// The port a window's display link is asked through: run (`true`) or pause it; whether the
/// window has one. The app's arm asks the window ([`window_port`]); a test's answers alone,
/// so no test reaches WindowServer.
pub(crate) type FrameClockPort = fn(Option<&Window>, bool) -> bool;

/// The app's [`FrameClockPort`]: the window's own frame clock (none without a window).
pub(crate) fn window_port(window: Option<&Window>, want: bool) -> bool {
    window.is_some_and(|w| w.set_frame_clock(want))
}

/// The synthetic panel's period for a window with no display: `ATERM_DEV_PANEL_HZ` (1..=1000,
/// builds with dev seams), else [`HEADLESS_HZ`].
fn headless_period() -> Duration {
    static PERIOD: OnceLock<Duration> = OnceLock::new();
    *PERIOD.get_or_init(|| {
        let hz = aterm_types::dev_seam!("ATERM_DEV_PANEL_HZ")
            .and_then(|v| v.to_str().and_then(|v| v.trim().parse::<u32>().ok()))
            .filter(|hz| (1..=1000).contains(hz))
            .unwrap_or(HEADLESS_HZ);
        Duration::from_nanos(1_000_000_000 / u64::from(hz))
    })
}

/// The synthetic vsync grid's origin: one per process, as a display's vsyncs run free of what
/// any window draws.
fn grid_origin() -> Instant {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

/// The grid's `k` with `origin + k · period` at or after `t` (`up`), or at or before it.
fn grid_point(t: Instant, period: Duration, up: bool) -> Instant {
    let origin = grid_origin();
    let p = period.as_nanos().max(1);
    let d = t.saturating_duration_since(origin).as_nanos();
    let k = if up { d.div_ceil(p) } else { d / p };
    origin + Duration::from_nanos(u64::try_from(k * p).unwrap_or(u64::MAX))
}

/// The synthetic grid's first vsync at or after `t`.
pub(crate) fn grid_ceil(t: Instant, period: Duration) -> Instant {
    grid_point(t, period, true)
}

/// The synthetic grid's last vsync at or before `t`.
pub(crate) fn grid_floor(t: Instant, period: Duration) -> Instant {
    grid_point(t, period, false)
}

/// The deck's frame interval (SPECS `deck-frame-interval`): one period of the window's clock,
/// no shorter than the effect tick's floor, or two while a deck frame this run waited
/// [`crate::EFFECT_LANE_ACQUIRE_PARK_NS`] or more for a drawable (the effect lane's park law)
/// or the Mac is in Low Power Mode. The cursor trail's lane keeps its own
/// (`effect-present-interval`).
pub(crate) fn deck_frame_interval(period: Duration, parked: bool, low_power: bool) -> Duration {
    period.max(crate::MIN_EFFECT_TICK_INTERVAL) * if parked || low_power { 2 } else { 1 }
}

/// Whether a window's deck frames the needle at its clock's rate ([`deck_frame_interval`];
/// SPECS `deck-frame-interval`): a window with no OS window (headless, whose synthetic panel
/// stands in for a display link, so a headless take runs the link's path), or one whose clock
/// is its display link (`link`) and which presents through the GPU. Every other window keeps the
/// effect lane's interval, today's cadence (design §3.7): a Mac before 14, whose clock is the
/// loop's timer, where 120 Hz timer-paced presents stall on their drawables (§8); Linux and
/// Windows, whose FIFO and per-arch rules the effect lane holds; and a CPU-presented window,
/// whose present reports no acquire wait for the park rule to read.
pub(crate) const fn deck_clock_at_panel_rate(
    windowed: bool,
    link: Option<bool>,
    cpu_present: bool,
) -> bool {
    !windowed || (matches!(link, Some(true)) && !cpu_present)
}

/// One window's deck clock.
#[derive(Debug)]
pub(crate) struct DeckClock {
    /// How the window's display link is asked ([`FrameClockPort`]).
    pub(crate) port: FrameClockPort,
    /// Whether the window has a display link (`Some(true)`), has none and runs on the synthetic
    /// grid (`Some(false)`), or is not known yet (`None`: not asked, or asked with no OS window
    /// to answer for, so asked again on the next sync); it runs on the grid meanwhile.
    link: Option<bool>,
    /// The link was asked to run.
    running: bool,
    /// When this run began: the first moment a needle frame was due.
    since: Option<Instant>,
    /// The last tick: the vsync it targeted and the period it ran at.
    last: Option<(Instant, Duration)>,
    /// When the last tick came, or the link was last asked to run, whichever is later: a
    /// running link silent [`LINK_GRACE`] periods past it is ticked by the loop.
    heard: Option<Instant>,
    /// The tick whose frame no frame has served yet: the vsync it targeted (the newest tick's,
    /// while one rides on it), and when the frame was first asked.
    asked: Option<(Instant, Instant)>,
    /// A frame asked went unserved for [`LINK_GRACE`] periods: the window draws nothing now (a
    /// headless window outside a capture, a present that keeps failing), so the clock owes it
    /// no frame until the window next lays its deck out ([`DeckClock::served`]).
    stalled: bool,
    /// A deck frame this run waited [`crate::EFFECT_LANE_ACQUIRE_PARK_NS`] or more for a
    /// drawable ([`deck_frame_interval`]).
    parked: bool,
    /// How long the link was asked to run over its runs since its last tick, the current run
    /// left out ([`LINK_SILENCE`]).
    unheard: Duration,
    /// When the link's current run began, or its last tick in it came, whichever is later;
    /// `None` while the link is paused.
    run_at: Option<Instant>,
    /// The link ran [`LINK_SILENCE`] without a tick: the clock keeps the loop's timer, as a
    /// window with no link does, until the link ticks again; the window's lease on the metrics'
    /// count of such windows.
    silent: metrics::DeckLinkSilence,
}

impl Default for DeckClock {
    fn default() -> Self {
        Self {
            port: window_port,
            link: None,
            running: false,
            since: None,
            last: None,
            heard: None,
            asked: None,
            stalled: false,
            parked: false,
            unheard: Duration::ZERO,
            run_at: None,
            silent: metrics::DeckLinkSilence::default(),
        }
    }
}

impl DeckClock {
    /// Use the display link for adjacent panel frames. Isolated pixel steps stay on the
    /// timer grid: repeatedly starting a native link costs wakes beyond the ticks it sends.
    /// A timer-requested frame must be served before it can start a link for the next step.
    fn link_for_due(&self, due: Instant, period: Duration) -> bool {
        (self.running || self.asked.is_none())
            && self.last.is_none_or(|(last, _)| {
                due.saturating_duration_since(last)
                    <= deck_frame_interval(period, false, false) * LINK_DENSE_PERIODS
            })
    }

    /// Asks the port whether the window has a display link, while that is not known: the answer
    /// is kept only from a window that can make a link or from an OS window, so a window not yet
    /// given its OS window (which answers for none) is asked again on its next sync.
    pub(crate) fn ask_link(&mut self, window: Option<&Window>) {
        if self.link.is_some() {
            return;
        }
        let has = (self.port)(window, false);
        if has || window.is_some() {
            self.link = Some(has);
        }
    }

    /// Whether the window has a display link, as [`Self::ask_link`] last kept it.
    #[cfg(any(all(test, unix), all(target_os = "macos", debug_assertions)))]
    pub(crate) const fn has_link(&self) -> Option<bool> {
        self.link
    }

    /// Whether the clock keeps its display link's time: the window has a link, and it has not
    /// fallen silent ([`LINK_SILENCE`]).
    pub(crate) fn on_link(&self) -> bool {
        self.link == Some(true) && !self.silent.get()
    }

    /// The link answer the clock's timing reads ([`Self::sample_at`],
    /// [`deck_clock_at_panel_rate`]): a silent link's window is timed as one with none.
    pub(crate) fn timing_link(&self) -> Option<bool> {
        if self.silent.get() {
            Some(false)
        } else {
            self.link
        }
    }

    /// The link fell silent ([`LINK_SILENCE`]) and has not ticked since.
    #[cfg(any(all(test, unix), all(target_os = "macos", debug_assertions)))]
    pub(crate) fn silent(&self) -> bool {
        self.silent.get()
    }

    /// Runs (`want`) or pauses the window's display link at `now`, through the port, where the
    /// window has one and the link is not already so; a run opens the hang detector's span and a
    /// pause closes it. A port that answers no link leaves the clock on the synthetic grid.
    pub(crate) fn steer(&mut self, window: Option<&Window>, want: bool, now: Instant) {
        if self.link != Some(true) || want == self.running {
            return;
        }
        self.running = want;
        if want {
            self.heard = Some(now);
            self.run_at = Some(now);
        } else if let Some(at) = self.run_at.take() {
            self.unheard = self
                .unheard
                .saturating_add(now.saturating_duration_since(at));
        }
        if !(self.port)(window, want) {
            self.link = Some(false);
            self.running = false;
            self.run_at = None;
            self.unheard = Duration::ZERO;
            self.silent.set(false);
        }
    }

    /// When the hang detector decides, while the link runs unheard and has not fallen silent:
    /// [`LINK_SILENCE`] into its silence over every run since its last tick.
    pub(crate) fn silence_deadline(&self) -> Option<Instant> {
        if self.link != Some(true) || self.silent.get() {
            return None;
        }
        let at = self.run_at?;
        Some(at + LINK_SILENCE.saturating_sub(self.unheard))
    }

    /// The hang detector at `now`: a link asked to run that has sent no tick for
    /// [`LINK_SILENCE`] of its runs falls silent, and the clock keeps the loop's timer until it
    /// ticks; whether it fell silent now.
    pub(crate) fn watch_link(&mut self, now: Instant) -> bool {
        if !self.silence_deadline().is_some_and(|due| now >= due) {
            return false;
        }
        self.silent.set(true);
        metrics::note_deck_link_silent();
        true
    }

    /// The window's display link ticked at `now`: the window has one, the hang detector's
    /// silence starts again, and a silent clock goes back on the link; whether it went back now.
    pub(crate) fn link_ticked(&mut self, now: Instant) -> bool {
        if self.link.is_none() {
            self.link = Some(true);
        }
        self.unheard = Duration::ZERO;
        if self.running {
            self.run_at = Some(now);
        }
        metrics::note_deck_link_tick();
        if !self.silent.get() {
            return false;
        }
        self.silent.set(false);
        metrics::note_deck_link_resumed();
        true
    }

    /// The tick whose frame is asked and unserved, while within [`LINK_GRACE`] periods of when
    /// it was first asked: a frame that never draws (a present that failed, a window that
    /// stopped drawing) holds the needle no longer, however many ticks rode on it.
    fn asked_at(&self, now: Instant, period: Duration) -> Option<(Instant, Instant)> {
        self.asked
            .filter(|&(_, asked)| now < asked + period * LINK_GRACE)
    }

    /// A frame of the window laid its deck out, sampling the needle where the clock asked: it
    /// presented, or it proved the card unchanged (a step no pixel shows), so the tick that
    /// asked it is served, and a stalled clock runs again.
    pub(crate) fn served(&mut self) {
        self.asked = None;
        self.stalled = false;
    }

    /// Where a frame of this window drawn at `now` samples the needle: the vsync of the tick
    /// that asked it, while unshown; on a running display link, the next vsync it predicts from
    /// its last tick (`now` before its first); on the synthetic grid (`period`, also while the
    /// link is paused, unknown or silent), the grid's vsync at or
    /// before `now`, so its frames change only on its ticks.
    pub(crate) fn sample_at(&self, now: Instant, period: Duration) -> Instant {
        if let Some((target, _)) = self.asked_at(now, period) {
            return target;
        }
        match (self.timing_link(), self.last, self.running) {
            (Some(true), Some((target, period)), true) => {
                if now <= target || period.is_zero() {
                    return target.max(now);
                }
                let p = period.as_nanos();
                let k = now.saturating_duration_since(target).as_nanos().div_ceil(p);
                target + Duration::from_nanos(u64::try_from(k * p).unwrap_or(u64::MAX))
            }
            (Some(true), None, true) => now,
            _ => grid_floor(now, period),
        }
    }

    /// The vsync of the tick whose frame is asked and unshown.
    pub(crate) fn asked_target(&self) -> Option<Instant> {
        self.asked.map(|(target, _)| target)
    }

    /// Ask a frame for the vsync at `target`, as a tick that saw the needle move does.
    #[cfg(test)]
    pub(crate) const fn ask_for_test(&mut self, target: Instant) {
        self.asked = Some((target, target));
    }

    /// The window's display link is running.
    #[cfg(all(test, unix))]
    pub(crate) const fn link_running(&self) -> bool {
        self.running
    }

    /// A deck frame of this run waited for a drawable ([`deck_frame_interval`]).
    #[cfg(all(test, unix))]
    pub(crate) const fn parked(&self) -> bool {
        self.parked
    }
}

impl App {
    /// The period `ws`'s deck clock runs at: its display link's last tick's (while it keeps the
    /// link's time, [`DeckClock::on_link`]), else its panel's refresh, else the synthetic
    /// panel's ([`headless_period`]).
    pub(crate) fn deck_clock_period(&self, ws: &WindowState) -> Duration {
        let clock = &ws.deck_clock;
        clock
            .last
            .filter(|_| clock.on_link())
            .map(|(_, period)| period)
            .filter(|p| !p.is_zero())
            .or(ws.frame_interval)
            .unwrap_or_else(headless_period)
    }

    /// `ws`'s deck frame interval, the needle's [`View::interval`]: [`deck_frame_interval`] of
    /// its clock where [`deck_clock_at_panel_rate`] holds, else the effect lane's
    /// (`WindowState::effect_present_interval`).
    ///
    /// [`View::interval`]: crate::stats_needle::View::interval
    pub(crate) fn deck_frame_interval(&self, ws: &WindowState) -> Duration {
        let cpu = matches!(ws.present, Some(crate::PresentTarget::Cpu { .. }));
        if !deck_clock_at_panel_rate(ws.os_window.is_some(), ws.deck_clock.timing_link(), cpu) {
            return ws.effect_present_interval();
        }
        let low_power = self
            .vitals
            .snapshot
            .as_ref()
            .and_then(|snap| snap.thermal)
            .and_then(|t| t.low_power)
            == Some(true);
        deck_frame_interval(self.deck_clock_period(ws), ws.deck_clock.parked, low_power)
    }

    /// The shortest deck frame interval `ws` draws the needle at ([`Self::deck_frame_interval`]
    /// with no frame of the run parked and out of Low Power Mode; on the effect lane, its
    /// fastest, `WindowState::effect_present_interval_fastest`): [`Self::deck_frame_interval`]
    /// never comes under it while the clock keeps its period, so an episode's frames walked on it
    /// ([`crate::stats_needle::Needle::frames_owed`]) are as many as the window can draw.
    pub(crate) fn deck_frame_interval_fastest(&self, ws: &WindowState) -> Duration {
        let cpu = matches!(ws.present, Some(crate::PresentTarget::Cpu { .. }));
        if !deck_clock_at_panel_rate(ws.os_window.is_some(), ws.deck_clock.timing_link(), cpu) {
            return ws.effect_present_interval_fastest();
        }
        deck_frame_interval(self.deck_clock_period(ws), false, false)
    }

    /// When window `wid`'s deck clock owes it a needle frame, on its clock's next sample; never
    /// while the clock is stalled.
    fn needle_due(&self, wid: WindowId, now: Instant) -> Option<Instant> {
        let ws = self.windows.get(&wid)?;
        if ws.deck_clock.stalled {
            return None;
        }
        let at = ws.deck_clock.sample_at(now, self.deck_clock_period(ws));
        self.live_facts(wid)?.needle_due(at, now)
    }

    /// Runs or pauses each window's display link for its needle, and books each run's length:
    /// the link runs from [`LINK_LEAD`] periods before a frame due, and pauses across a longer
    /// gap and once the needle rests. Isolated steps use the timer grid without starting the
    /// link. A window whose port says it has no link keeps the
    /// synthetic grid, as does one whose link fell silent ([`DeckClock::watch_link`]), whose
    /// link is still run and paused so that its next tick resumes it.
    pub(crate) fn sync_deck_clocks(&mut self, now: Instant) {
        use std::ops::Bound::{Excluded, Unbounded};
        // Window by window without a list: this runs on every turn of the loop.
        let mut next = self.windows.keys().next().copied();
        while let Some(wid) = next {
            next = self
                .windows
                .range((Excluded(wid), Unbounded))
                .next()
                .map(|(&k, _)| k);
            let due = self.needle_due(wid, now);
            let Some(ws) = self.windows.get(&wid) else {
                continue;
            };
            let period = self.deck_clock_period(ws);
            let Some(ws) = self.windows.get_mut(&wid) else {
                continue;
            };
            let window = ws.os_window.as_deref();
            let clock = &mut ws.deck_clock;
            match (due, clock.since) {
                (Some(_), None) => clock.since = Some(now),
                (None, Some(since)) => {
                    let ran = now.saturating_duration_since(since).as_nanos();
                    metrics::note_deck_clock_run(u64::try_from(ran).unwrap_or(u64::MAX));
                    clock.since = None;
                    clock.parked = false;
                }
                _ => {}
            }
            clock.ask_link(window);
            let want =
                due.is_some_and(|d| clock.link_for_due(d, period) && d <= now + period * LINK_LEAD);
            clock.steer(window, want, now);
            let _ = clock.watch_link(now);
        }
    }

    /// When window `wid`'s deck clock next needs the loop, and the period it would tick at
    /// there: on the synthetic grid, its first vsync at or after the frame due, after its last
    /// tick and not before the needle's episode began, so it ticks at most once a period whatever
    /// a frame draws, and a reading never ticks it for a vsync that passed before the reading
    /// landed (its frames sample the grid's last vsync, at which the new episode's start would
    /// read as a frame due). Isolated steps take this path even with a native link. For a
    /// paused link whose frames are adjacent,
    /// its resume, [`LINK_LEAD`] periods before the frame due (no tick: the sync runs it); for a
    /// running link, [`LINK_GRACE`] periods after it was last heard from, or its hang detector's
    /// deadline ([`DeckClock::silence_deadline`]) if sooner. A silent link's window is on the
    /// grid.
    fn deck_clock_next(&self, wid: WindowId, now: Instant) -> Option<(Instant, Option<Duration>)> {
        let due = self.needle_due(wid, now)?;
        let ws = self.windows.get(&wid)?;
        let period = self.deck_clock_period(ws);
        let clock = &ws.deck_clock;
        Some(
            match (
                clock.on_link() && clock.link_for_due(due, period),
                clock.running,
            ) {
                (true, true) => {
                    let grace = clock.heard.unwrap_or(now) + period * LINK_GRACE;
                    (
                        clock
                            .silence_deadline()
                            .map_or(grace, |silence| silence.min(grace)),
                        Some(period),
                    )
                }
                (true, false) => (
                    due.checked_sub(period * LINK_LEAD)
                        .unwrap_or(due)
                        .max(now + Duration::from_millis(1)),
                    None,
                ),
                _ => {
                    let began = self.vitals.needle.get().map_or(due, |n| n.t0);
                    let after = clock
                        .last
                        .map_or(due, |(target, _)| due.max(target + Duration::from_nanos(1)))
                        .max(began);
                    (grid_ceil(after, period), Some(period))
                }
            },
        )
    }

    /// The loop's deadline for the deck clocks (`DeadlineOwner::DeckClock`,
    /// [`Self::deck_clock_next`]).
    pub(crate) fn deck_clock_deadline(&self, now: Instant) -> Option<Instant> {
        self.windows
            .keys()
            .filter_map(|&wid| self.deck_clock_next(wid, now).map(|(at, _)| at))
            .min()
    }

    /// Ticks each window whose deck clock the loop drives and whose tick has come: the
    /// synthetic grid's, at that vsync; a running link silent past its grace, at `now`.
    pub(crate) fn service_deck_clocks(&mut self, now: Instant) {
        let ticks: Vec<(WindowId, Instant, Duration)> = self
            .windows
            .iter()
            .filter_map(|(&wid, ws)| {
                let (at, period) = self.deck_clock_next(wid, now)?;
                let period = period?;
                if now < at {
                    return None;
                }
                let target = if ws.deck_clock.on_link() && ws.deck_clock.running {
                    now
                } else {
                    grid_floor(now, period)
                };
                Some((wid, target, period))
            })
            .collect();
        for (wid, target, period) in ticks {
            self.deck_clock_tick(wid, target, period, now);
        }
    }

    /// Window `wid`'s display link ticked at `now` for the vsync at `target`, `period` apart
    /// (`WindowEvent::FrameTick`): a silent clock goes back on the link
    /// ([`DeckClock::link_ticked`]).
    pub(crate) fn on_deck_frame_tick(
        &mut self,
        wid: WindowId,
        target: Instant,
        period: Duration,
        now: Instant,
    ) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            let _ = ws.deck_clock.link_ticked(now);
        }
        self.deck_clock_tick(wid, target, period, now);
    }

    /// A tick of window `wid`'s deck clock for the vsync at `target`: where the needle stands
    /// there other than where the window last drew it, the tick asks one frame, which samples
    /// it at `target`. A frame already asked and not yet served takes the newer vsync instead of
    /// a second frame; one unserved for [`LINK_GRACE`] periods stalls the clock until a frame of
    /// the window lays its deck out ([`DeckClock::served`]). A headless window has no window to
    /// ask: the clock draws it.
    pub(crate) fn deck_clock_tick(
        &mut self,
        wid: WindowId,
        target: Instant,
        period: Duration,
        now: Instant,
    ) {
        let moved = self.live_facts(wid).is_some_and(|facts| {
            let drawn = self
                .windows
                .get(&wid)
                .and_then(|ws| ws.deck_needle_drawn.get());
            crate::stats_live::shows(facts.needle_at(target)) != crate::stats_live::shows(drawn)
        });
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let clock = &mut ws.deck_clock;
        clock.last = Some((target, period));
        clock.heard = Some(now);
        if clock.asked.is_some() && clock.asked_at(now, period).is_none() {
            // The frame asked went unserved its grace: the window draws nothing now.
            clock.asked = None;
            clock.stalled = true;
            metrics::note_deck_clock_tick(false);
            return;
        }
        metrics::note_deck_clock_tick(moved);
        if !moved {
            return;
        }
        let pending = clock.asked_at(now, period);
        clock.asked = Some((target, pending.map_or(now, |(_, at)| at)));
        if pending.is_some() {
            return;
        }
        if matches!(ws.present, Some(crate::PresentTarget::Virtual { .. })) {
            self.redraw_window(wid);
        } else {
            ws.request_redraw();
        }
    }
}

/// The next deck card generation: a card serial (`app_native_motion::next_card_serial`), the
/// one counter every window's cards take theirs from, so no deck card shares a serial with
/// another card, the deck's or any other a window's tray holds.
fn next_generation() -> u64 {
    crate::app_native_motion::next_card_serial()
}

/// A window's deck card changes and the presents that showed them.
#[derive(Clone, Debug, Default)]
pub(crate) struct DeckMotion {
    /// The deck card's generation ([`aterm_gpu::TrayQuad::serial`]): new each time the splice
    /// changes its pixels (a live edge patch or a whole raster); 0 before the first card.
    generation: u64,
    /// The generation the last present's quad carried ([`Self::stamp`]); 0 before any.
    stamped: u64,
    /// The rows changed since the generation [`Self::stamped`] names.
    since_stamped: Option<(u32, u32)>,
    /// The generation the last booked deck frame showed.
    booked: u64,
}

impl DeckMotion {
    /// The splice changed the deck card's rows `rows` (`y0..y1`, card pixels); an empty band
    /// changed nothing.
    pub(crate) fn card_changed(&mut self, rows: (u32, u32)) {
        if rows.0 >= rows.1 {
            return;
        }
        self.generation = next_generation();
        self.since_stamped = Some(
            self.since_stamped
                .map_or(rows, |(y0, y1)| (y0.min(rows.0), y1.max(rows.1))),
        );
    }

    /// What a present's quad of the deck card carries: its generation, and the rows changed
    /// since the generation the last present's quad carried (`None` before any). A renderer
    /// that holds that generation uploads the band alone; one that holds another compares.
    pub(crate) fn stamp(&mut self) -> (u64, Option<aterm_gpu::TrayDelta>) {
        let delta = (self.stamped != 0).then(|| aterm_gpu::TrayDelta {
            since: self.stamped,
            rows: self.since_stamped.unwrap_or((0, 0)),
        });
        self.stamped = self.generation;
        self.since_stamped = None;
        (self.generation, delta)
    }

    /// Whether a present now shows a card change no booked frame showed, marking it booked.
    fn take_unbooked(&mut self) -> bool {
        let fresh = self.booked != self.generation;
        self.booked = self.generation;
        fresh
    }
}

impl App {
    /// Book a successful present of `wid`: the renderer's running tray card totals, and, where
    /// it shows a deck card the splice changed since the last frame booked, the deck frame's
    /// main-thread time from what asked for it (its deck clock's tick, else `asked`, the
    /// redraw's entry) to `presented_at`, and its wait for a drawable (`acquire_wait_ns`,
    /// [`deck_frame_interval`]'s park). The tick's frame is shown.
    pub(crate) fn note_deck_present(
        &mut self,
        wid: WindowId,
        asked: Instant,
        presented_at: Instant,
        acquire_wait_ns: u64,
    ) {
        if let Some(gpu) = self.backend.gpu_ref() {
            metrics::note_tray_totals(
                gpu.tray_uploads(),
                gpu.tray_upload_bytes(),
                gpu.tray_compare_bytes(),
            );
        }
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let ticked = ws.deck_clock.asked;
        ws.deck_clock.served();
        if ws.deck_card.is_some() && ws.deck_motion.take_unbooked() {
            let from = ticked.map_or(asked, |(_, at)| at.min(asked));
            let ns = presented_at.saturating_duration_since(from).as_nanos();
            metrics::note_deck_frame(u64::try_from(ns).unwrap_or(u64::MAX));
            if ws.deck_clock.since.is_some()
                && acquire_wait_ns >= crate::EFFECT_LANE_ACQUIRE_PARK_NS
            {
                ws.deck_clock.parked = true;
            }
        }
    }
}

#[cfg(test)]
#[path = "stats_clock_tests.rs"]
mod tests;
