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
mod tests {
    use super::DeckMotion;

    /// A frame the clock asked that the redraw proves unchanged (its early-out: nothing it would
    /// draw differs from what the window shows) serves the tick, as a presented one does, so the
    /// clock neither holds the needle on that vsync nor stalls. The redraw is the real one, on
    /// the scripted CPU surface; without the early-out's `served` (the mutant) the ask stays.
    #[test]
    fn a_frame_proved_unchanged_serves_the_tick_that_asked_it() {
        let wid = crate::WindowId(0);
        let mut app = crate::scripted_redraw_fixture::scripted_app();
        for _ in 0..4 {
            app.redraw_window(wid);
        }
        let proved = app.windows[&wid].redraws_proved_unchanged;
        app.windows
            .get_mut(&wid)
            .expect("the window")
            .deck_clock
            .ask_for_test(std::time::Instant::now());
        app.redraw_window(wid);
        let ws = &app.windows[&wid];
        assert!(
            ws.redraws_proved_unchanged > proved,
            "CONTROL: the asked frame reached the early-out"
        );
        assert_eq!(
            ws.deck_clock.asked_target(),
            None,
            "the frame proved unchanged served the tick"
        );
    }
    #[cfg(unix)]
    use super::{
        DeckClock, LINK_GRACE, LINK_LEAD, LINK_SILENCE, deck_clock_at_panel_rate, grid_ceil,
        grid_floor,
    };
    use crate::stats_live::{DeckLive, FlowAt};
    use crate::stats_plan::DeckForm;
    #[cfg(unix)]
    use std::cell::RefCell;
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    #[cfg(unix)]
    thread_local! {
        /// What the inert port was asked, in turn.
        static ASKED: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
    }

    /// The inert arm of the clock's port: a window that has a display link, which the test
    /// ticks by hand; it reaches no window.
    #[cfg(unix)]
    fn inert_link(_: Option<&aterm_window::window::Window>, want: bool) -> bool {
        ASKED.with(|a| a.borrow_mut().push(want));
        true
    }

    #[cfg(unix)]
    fn asked() -> Vec<bool> {
        ASKED.with(|a| a.borrow().clone())
    }

    /// A focused deck whose dial's needle rests on a calm reading at `t0` and sweeps to a busy
    /// one landed a second later; the second's moment.
    #[cfg(unix)]
    fn sweeping() -> (crate::App, crate::WindowId, Instant) {
        use crate::stats_live::tests::{busier, fit_deck, live_app};
        let t0 = Instant::now();
        let (mut app, wid) = live_app(t0);
        app.vitals.landed_period = Some(Duration::from_secs(3600));
        let ws = app.windows.get_mut(&wid).expect("the window");
        ws.cols = 160;
        ws.scale = 2.0;
        fit_deck(&mut app, wid);
        let _ = app.stats_deck_fp(wid);
        assert!(
            app.windows[&wid].deck_needle_px.get() > 0.0,
            "CONTROL: the deck draws a dial"
        );
        app.retarget_needle(t0);
        let _ = app.stats_deck_fp(wid);
        let t1 = t0 + Duration::from_secs(1);
        let n = busier(&mut app, 0.95, t1);
        assert!(
            (n.tip.x0 - n.target().0).abs() > 10.0,
            "CONTROL: the needle sweeps"
        );
        (app, wid, t1)
    }

    /// A present of `wid` at `now`, as the splice and the present path see it: the deck draws
    /// the needle where the window's clock samples it, and the present shows the tick's frame.
    #[cfg(unix)]
    fn present(app: &mut crate::App, wid: crate::WindowId, now: Instant) -> Option<Instant> {
        let facts = app.live_facts(wid).expect("the deck draws");
        let ws = &app.windows[&wid];
        let at = ws.deck_clock.sample_at(now, app.deck_clock_period(ws));
        ws.deck_needle_drawn.set(facts.needle_at(at));
        app.note_deck_present(wid, now, now, 0);
        Some(at)
    }

    #[cfg(unix)]
    fn redraws(app: &crate::App, wid: crate::WindowId) -> u32 {
        app.windows[&wid].redraw_requests_for_test.get()
    }

    #[cfg(unix)]
    #[test]
    fn isolated_steps_draw_without_restarting_the_native_link() {
        let _attached = crate::stats_deck::tests::attach_stats();
        let (mut app, wid, t1) = sweeping();
        let period = Duration::from_micros(8_333);
        let clock = &mut app.windows.get_mut(&wid).unwrap().deck_clock;
        clock.port = inert_link;
        clock.last = Some((t1 - Duration::from_secs(1), period));
        app.sync_deck_clocks(t1);
        assert!(!app.windows[&wid].deck_clock.link_running());
        let at = app.deck_clock_deadline(t1).expect("the step is still owed");
        let before = redraws(&app, wid);
        app.service_deck_clocks(at);
        assert!(redraws(&app, wid) > before, "the timer draws the step");
        app.sync_deck_clocks(at);
        assert!(
            !app.windows[&wid].deck_clock.link_running(),
            "an unserved timer frame cannot start the native link"
        );
        present(&mut app, wid, at);

        let (mut dense, wid, t1) = sweeping();
        let clock = &mut dense.windows.get_mut(&wid).unwrap().deck_clock;
        clock.port = inert_link;
        clock.last = Some((t1, period));
        dense.sync_deck_clocks(t1);
        assert!(
            dense.windows[&wid].deck_clock.link_running(),
            "CONTROL: adjacent panel frames still run the native link"
        );
    }

    /// A sweep driven as the loop drives it: native ticks for adjacent panel frames and the
    /// timer grid for isolated steps. The link costs a few ticks per native frame, and when
    /// the needle comes to rest the
    /// clock pauses, asks no deadline, and a tick then asks nothing. A clock that never paused
    /// (the CONTROL's mutant) would leave its last ask `true`.
    #[cfg(unix)]
    #[test]
    fn a_settled_needle_stops_the_clock() {
        let _attached = crate::stats_deck::tests::attach_stats();
        let (mut app, wid, t1) = sweeping();
        app.windows
            .get_mut(&wid)
            .expect("the window")
            .deck_clock
            .port = inert_link;
        let period = Duration::from_micros(8_333);
        let (mut now, mut ticks, mut frames, mut runs) = (t1, 0_usize, 0_usize, 0_usize);
        for _ in 0..100_000 {
            let before = redraws(&app, wid);
            app.service_deck_clocks(now);
            if redraws(&app, wid) > before {
                frames += 1;
                present(&mut app, wid, now);
            }
            app.sync_deck_clocks(now);
            if app.windows[&wid].deck_clock.link_running() {
                let before = redraws(&app, wid);
                app.on_deck_frame_tick(wid, now + period, period, now);
                ticks += 1;
                if redraws(&app, wid) > before {
                    frames += 1;
                    present(&mut app, wid, now);
                }
                now += period;
                continue;
            }
            runs += 1;
            match app.deck_clock_deadline(now) {
                Some(at) => now = at.max(now + Duration::from_nanos(1)),
                None => break,
            }
        }
        assert!(frames > 5, "CONTROL: the sweep draws frames ({frames})");
        assert!(runs > 1, "CONTROL: the link paused across a gap ({runs})");
        assert!(
            ticks <= (LINK_LEAD as usize + 2) * frames,
            "{ticks} ticks for {frames} frames"
        );
        assert_eq!(asked().last(), Some(&false), "the settled clock is paused");
        assert!(!app.windows[&wid].deck_clock.link_running());
        assert_eq!(app.deck_clock_deadline(now), None, "and asks no deadline");
        let facts = app.live_facts(wid).expect("the deck draws");
        let (n, _) = facts.needle.expect("a needle");
        let drawn = app.windows[&wid].deck_needle_drawn.get().expect("drawn");
        assert_eq!(drawn.at, n.target(), "the last frame drew the reading");
        let before = redraws(&app, wid);
        app.on_deck_frame_tick(wid, now + period, period, now);
        assert_eq!(redraws(&app, wid), before, "a tick at rest asks no frame");
    }

    /// No window whose deck the clock cannot show wants a tick: one occluded (a hidden app's and
    /// a minimised window read as occluded), one unfocused, one under Reduce Motion, and one with
    /// the bars off; each pauses a running link and arms no deadline.
    #[cfg(unix)]
    #[test]
    fn an_occluded_hidden_or_unfocused_window_never_wants_a_tick() {
        let _attached = crate::stats_deck::tests::attach_stats();
        let (mut app, wid, t1) = sweeping();
        app.windows
            .get_mut(&wid)
            .expect("the window")
            .deck_clock
            .port = inert_link;
        app.sync_deck_clocks(t1);
        assert!(
            app.windows[&wid].deck_clock.link_running(),
            "CONTROL: a focused sweep runs the link"
        );
        /// Turns one reason the window cannot show its deck on (`true`) or off.
        type Toggle = fn(&mut crate::App, crate::WindowId, bool);
        let cases: [(&str, Toggle); 4] = [
            ("occluded", |app, wid, on| {
                app.windows.get_mut(&wid).expect("the window").occluded = on;
            }),
            ("unfocused", |app, wid, on| {
                app.windows.get_mut(&wid).expect("the window").focused = !on;
            }),
            ("Reduce Motion", |app, _, on| app.system_reduce_motion = on),
            ("the bars off", |app, _, on| {
                app.config
                    .sys
                    .get_or_insert_with(Default::default)
                    .stats_bars = Some(if on { "off" } else { "deck" }.into());
            }),
        ];
        for (name, set) in cases {
            set(&mut app, wid, true);
            app.sync_deck_clocks(t1);
            assert!(
                !app.windows[&wid].deck_clock.link_running(),
                "{name}: the link pauses"
            );
            assert_eq!(app.deck_clock_deadline(t1), None, "{name}: no deadline");
            set(&mut app, wid, false);
            app.sync_deck_clocks(t1);
            assert!(
                app.windows[&wid].deck_clock.link_running(),
                "CONTROL {name}: back on, it runs again"
            );
        }
    }

    /// A tick samples the needle at the vsync it targets, not at the moment it arrives: the frame
    /// it asks draws the needle there. A frame drawn between ticks samples the next vsync the
    /// link predicts.
    #[cfg(unix)]
    #[test]
    fn the_needle_is_drawn_where_the_spring_is_at_the_target_vsync() {
        let _attached = crate::stats_deck::tests::attach_stats();
        let (mut app, wid, t1) = sweeping();
        app.windows
            .get_mut(&wid)
            .expect("the window")
            .deck_clock
            .port = inert_link;
        let period = Duration::from_micros(16_667);
        // Draw the sweep's first frame, then tick just before its next step, for a vsync just
        // after it.
        app.sync_deck_clocks(t1);
        assert!(app.windows[&wid].deck_clock.link_running());
        app.on_deck_frame_tick(wid, t1, period, t1);
        present(&mut app, wid, t1);
        let facts = app.live_facts(wid).expect("the deck draws");
        let first = facts.needle_at(t1);
        let step = (1..2_000_u64)
            .map(|ms| t1 + Duration::from_millis(ms))
            .find(|&t| facts.needle_at(t) != first)
            .expect("CONTROL: the needle steps within two seconds");
        let (now, target) = (
            step - Duration::from_millis(3),
            step + Duration::from_millis(2),
        );
        assert_ne!(
            facts.needle_at(target),
            facts.needle_at(now),
            "CONTROL: the needle moves between the tick and its vsync"
        );
        app.on_deck_frame_tick(wid, target, period, now);
        assert_eq!(present(&mut app, wid, now), Some(target));
        assert_eq!(
            app.windows[&wid].deck_needle_drawn.get(),
            facts.needle_at(target),
            "the frame draws the needle at its vsync"
        );
        let ws = &app.windows[&wid];
        assert_eq!(
            ws.deck_clock.sample_at(target + period / 3, period),
            target + period,
            "a frame between ticks samples the next vsync"
        );
        assert_eq!(ws.deck_clock.sample_at(now, period), target);
    }

    /// A tick whose frame is asked and not yet shown takes the next tick's vsync instead of
    /// asking a second: one present shows the newer sample.
    #[cfg(unix)]
    #[test]
    fn a_tick_with_a_pending_redraw_presents_once() {
        let _attached = crate::stats_deck::tests::attach_stats();
        let (mut app, wid, t1) = sweeping();
        app.windows
            .get_mut(&wid)
            .expect("the window")
            .deck_clock
            .port = inert_link;
        let period = Duration::from_micros(8_333);
        let before = redraws(&app, wid);
        app.on_deck_frame_tick(wid, t1 + period, period, t1);
        assert_eq!(redraws(&app, wid), before + 1, "the tick asks a frame");
        let later = t1 + period * 2;
        app.on_deck_frame_tick(wid, later + period, period, later);
        assert_eq!(
            redraws(&app, wid),
            before + 1,
            "the next tick rides on the frame asked"
        );
        assert_eq!(present(&mut app, wid, later), Some(later + period));
        let facts = app.live_facts(wid).expect("the deck draws");
        assert_eq!(
            app.windows[&wid].deck_needle_drawn.get(),
            facts.needle_at(later + period),
            "the one present shows the newer vsync"
        );
        let last = later + period * 6;
        app.on_deck_frame_tick(wid, last + period, period, last);
        assert_eq!(
            redraws(&app, wid),
            before + 2,
            "CONTROL: once shown, the next tick asks again"
        );
    }

    /// A frame that lays the deck out where the clock asked serves the tick, whether it presents
    /// or proves the card unchanged (a soak step no pixel shows: the render's early-out). The
    /// next tick that moves the needle asks its own frame at once, rather than riding on one no
    /// present will ever show until some other redraw comes.
    #[cfg(unix)]
    #[test]
    fn a_frame_that_shows_nothing_new_still_serves_its_tick() {
        let _attached = crate::stats_deck::tests::attach_stats();
        let (mut app, wid, t1) = sweeping();
        app.windows
            .get_mut(&wid)
            .expect("the window")
            .deck_clock
            .port = inert_link;
        let period = Duration::from_micros(16_667);
        let before = redraws(&app, wid);
        app.on_deck_frame_tick(wid, t1 + period, period, t1);
        assert_eq!(redraws(&app, wid), before + 1, "the tick asks a frame");
        // The frame lays the deck out at the tick's vsync and proves it unchanged.
        let facts = app.live_facts(wid).expect("the deck draws");
        let ws = app.windows.get_mut(&wid).expect("the window");
        ws.deck_needle_drawn.set(facts.needle_at(t1 + period));
        ws.deck_clock.served();
        let facts = app.live_facts(wid).expect("the deck draws");
        let drawn = crate::stats_live::shows(app.windows[&wid].deck_needle_drawn.get());
        let next = (2..200_u32)
            .map(|k| t1 + period * k)
            .find(|&t| crate::stats_live::shows(facts.needle_at(t)) != drawn)
            .expect("CONTROL: the needle moves on");
        app.on_deck_frame_tick(wid, next, period, next - period);
        assert_eq!(
            redraws(&app, wid),
            before + 2,
            "served, the next tick asks its own frame"
        );
        // CONTROL: unserved, a moving tick rides on the frame asked.
        let later = next + period;
        app.on_deck_frame_tick(wid, later + period, period, later);
        assert_eq!(redraws(&app, wid), before + 2, "CONTROL: it rides");
    }

    /// A window whose frames never draw (a headless window outside a capture, a present that
    /// keeps failing) stops its clock once the frame asked goes [`LINK_GRACE`] periods
    /// unserved, though the needle still moves; the window's next frame starts it again. A clock
    /// that never stalled (the mutant) would tick every period to the end of the sweep.
    #[cfg(unix)]
    #[test]
    fn a_window_whose_frames_never_draw_stops_its_clock() {
        let _attached = crate::stats_deck::tests::attach_stats();
        let (mut app, wid, t1) = sweeping();
        app.sync_deck_clocks(t1);
        let mut now = app
            .deck_clock_deadline(t1)
            .expect("the sweep's first frame");
        let before = redraws(&app, wid);
        let mut ticks = 0_u32;
        while let Some(at) = app.deck_clock_deadline(now) {
            now = at.max(now);
            app.service_deck_clocks(now);
            app.sync_deck_clocks(now);
            ticks += 1;
            assert!(ticks < 1_000, "the clock never stops");
        }
        assert_eq!(redraws(&app, wid), before + 1, "one frame asked");
        assert!(
            ticks <= LINK_GRACE + 2,
            "{ticks} ticks for a frame nothing draws"
        );
        let facts = app.live_facts(wid).expect("the deck draws");
        assert_ne!(
            crate::stats_live::shows(facts.needle_at(now)),
            crate::stats_live::shows(app.windows[&wid].deck_needle_drawn.get()),
            "CONTROL: the needle still moves"
        );
        present(&mut app, wid, now);
        app.sync_deck_clocks(now);
        assert!(
            app.deck_clock_deadline(now).is_some(),
            "the window's next frame starts the clock again"
        );
    }

    /// What [`drive_link`] saw.
    #[cfg(unix)]
    struct Drive {
        /// When the window's clock fell silent, if it did.
        fell: Option<Instant>,
        /// How long its link was asked to run.
        ran: Duration,
        /// The loop's wakes for its deadline.
        wakes: u32,
        /// Where the drive stopped.
        now: Instant,
    }

    /// Drives `wid`'s sweep from `from` as the loop drives it (its deadline, the redraws it asks
    /// presented), with the link ticking each `period` while it runs where `ticks` and never
    /// where not, until the needle rests, continuing on the grid if the link falls silent.
    #[cfg(unix)]
    fn drive_link(
        app: &mut crate::App,
        wid: crate::WindowId,
        from: Instant,
        period: Duration,
        ticks: bool,
    ) -> Drive {
        let mut d = Drive {
            fell: None,
            ran: Duration::ZERO,
            wakes: 0,
            now: from,
        };
        for _ in 0..100_000 {
            let now = d.now;
            app.sync_deck_clocks(now);
            let clock = &app.windows[&wid].deck_clock;
            if clock.silent() {
                d.fell.get_or_insert(now);
            }
            let running = clock.link_running();
            let before = redraws(app, wid);
            let next = if running && ticks {
                app.on_deck_frame_tick(wid, now + period, period, now);
                now + period
            } else {
                let Some(at) = app.deck_clock_deadline(now) else {
                    break;
                };
                d.wakes += 1;
                let at = at.max(now + Duration::from_nanos(1));
                app.service_deck_clocks(at);
                at
            };
            if redraws(app, wid) > before {
                present(app, wid, next);
            }
            if running {
                d.ran += next - now;
            }
            d.now = next;
        }
        d
    }

    #[cfg(unix)]
    #[test]
    fn a_finite_sweep_finishes_even_when_the_native_link_never_ticks() {
        let _attached = crate::stats_deck::tests::attach_stats();
        let (mut app, wid, t1) = sweeping();
        app.windows.get_mut(&wid).unwrap().deck_clock.port = inert_link;
        let period = app.deck_clock_period(&app.windows[&wid]);
        let d = drive_link(&mut app, wid, t1, period, false);
        assert!(redraws(&app, wid) > 5, "CONTROL: the sweep drew frames");
        let most = u32::try_from(d.now.duration_since(t1).as_nanos() / period.as_nanos())
            .expect("a short sweep")
            + 2;
        assert!(d.wakes <= most, "the timer wakes at most once a period");
        assert!(!app.windows[&wid].deck_clock.link_running());
        assert_eq!(app.deck_clock_deadline(d.now), None, "the sweep settled");
    }

    /// A link kept running without ticks falls silent at [`LINK_SILENCE`], never sooner.
    /// Unlike a finite sparse sweep, this fixture actually asks it to run for that duration.
    /// Then the window's clock keeps the loop's timer,
    /// as a window with no link does (the synthetic grid, and a windowed deck the effect lane's
    /// interval), it arms no further detector wake, and the link's first tick puts it back on
    /// the link.
    #[cfg(unix)]
    #[test]
    fn a_link_kept_running_without_ticks_falls_back_to_the_timer() {
        let _attached = crate::stats_deck::tests::attach_stats();
        let (mut app, wid, t1) = sweeping();
        app.windows
            .get_mut(&wid)
            .expect("the window")
            .deck_clock
            .port = inert_link;
        let period = app.deck_clock_period(&app.windows[&wid]);
        let clock = &mut app.windows.get_mut(&wid).unwrap().deck_clock;
        clock.ask_link(None);
        clock.steer(None, true, t1);
        let fell = clock.silence_deadline().expect("a running link is watched");
        assert_eq!(fell, t1 + LINK_SILENCE);
        assert!(!clock.watch_link(fell - Duration::from_nanos(1)));
        assert!(clock.watch_link(fell), "a link kept running falls silent");
        let clock = &app.windows[&wid].deck_clock;
        assert_eq!(
            clock.timing_link(),
            Some(false),
            "timed as a window with none"
        );
        assert!(!clock.on_link());
        assert!(
            !deck_clock_at_panel_rate(true, clock.timing_link(), false),
            "a windowed deck frames at the effect lane's interval"
        );
        assert_eq!(clock.silence_deadline(), None, "no further detector wake");
        // On the grid now: a new reading's sweep wakes on its vsyncs, one after another.
        let mut now = fell + Duration::from_secs(1);
        let _ = crate::stats_live::tests::busier(&mut app, 0.2, now);
        for _ in 0..4 {
            app.sync_deck_clocks(now);
            assert!(app.windows[&wid].deck_clock.silent(), "it stays silent");
            let at = app.deck_clock_deadline(now).expect("the sweep goes on");
            assert!(at > now, "the deadline moves on");
            assert_eq!(grid_ceil(at, period), at, "on the grid");
            let before = redraws(&app, wid);
            app.service_deck_clocks(at);
            if redraws(&app, wid) > before {
                present(&mut app, wid, at);
            }
            now = at;
        }
        app.on_deck_frame_tick(wid, now + period, period, now);
        let clock = &app.windows[&wid].deck_clock;
        assert!(
            !clock.silent() && clock.on_link(),
            "a tick puts it back on the link"
        );
        assert_eq!(clock.timing_link(), Some(true));
    }

    /// The detector's CONTROL: a link that ticks each period through successive sweeps never
    /// falls silent, though it runs longer than [`LINK_SILENCE`] in all.
    #[cfg(unix)]
    #[test]
    fn a_link_that_ticks_never_falls_back() {
        let _attached = crate::stats_deck::tests::attach_stats();
        let (mut app, wid, t1) = sweeping();
        app.windows
            .get_mut(&wid)
            .expect("the window")
            .deck_clock
            .port = inert_link;
        let period = Duration::from_micros(8_333);
        let mut d = drive_link(&mut app, wid, t1, period, true);
        assert_eq!(d.fell, None, "a ticking link never falls silent");
        let mut ran = d.ran;
        for share in [0.2, 0.95] {
            let next = d.now + Duration::from_secs(1);
            let _ = crate::stats_live::tests::busier(&mut app, share, next);
            d = drive_link(&mut app, wid, next, period, true);
            assert_eq!(d.fell, None, "a ticking link never falls silent");
            ran += d.ran;
        }
        assert!(
            ran > LINK_SILENCE,
            "CONTROL: the link ran {:?}, long enough to fall silent unheard",
            ran
        );
        assert!(
            app.deck_clock_deadline(d.now).is_none(),
            "CONTROL: the sweep ended"
        );
    }

    /// A link's silence counts only while it is asked to run, adds up over its runs since its
    /// last tick, and starts again at each tick; it falls silent once, and its next tick resumes
    /// it.
    #[cfg(unix)]
    #[test]
    fn a_links_silence_counts_its_runs_since_its_last_tick() {
        let ms = Duration::from_millis;
        let mut clock = DeckClock {
            port: inert_link,
            ..DeckClock::default()
        };
        clock.ask_link(None);
        assert_eq!(
            clock.has_link(),
            Some(true),
            "CONTROL: the inert port has a link"
        );
        let t0 = Instant::now();
        clock.steer(None, true, t0);
        assert_eq!(clock.silence_deadline(), Some(t0 + LINK_SILENCE));
        clock.steer(None, false, t0 + ms(100));
        assert_eq!(
            clock.silence_deadline(),
            None,
            "a paused link is not watched"
        );
        assert!(!clock.watch_link(t0 + ms(10_000)));
        let t1 = t0 + ms(10_000);
        clock.steer(None, true, t1);
        assert_eq!(
            clock.silence_deadline(),
            Some(t1 + LINK_SILENCE - ms(100)),
            "the runs since the last tick add up"
        );
        assert!(
            !clock.link_ticked(t1 + ms(50)),
            "CONTROL: not silent, nothing to resume"
        );
        assert!(
            !clock.watch_link(t1 + LINK_SILENCE),
            "a tick starts the silence again"
        );
        assert!(clock.watch_link(t1 + ms(50) + LINK_SILENCE));
        assert!(clock.silent());
        assert!(
            !clock.watch_link(t1 + ms(50) + LINK_SILENCE * 2),
            "it falls silent once"
        );
        assert!(
            clock.link_ticked(t1 + ms(1_000)),
            "the next tick resumes it"
        );
        assert!(!clock.silent() && clock.on_link());
    }

    /// The clock runs at its link's period: a window that moves from a 60 Hz display to a
    /// 120 Hz one draws the needle at 120 Hz from its first tick there.
    #[cfg(unix)]
    #[test]
    fn the_link_follows_the_window_to_another_display() {
        let _attached = crate::stats_deck::tests::attach_stats();
        let (mut app, wid, t1) = sweeping();
        app.windows
            .get_mut(&wid)
            .expect("the window")
            .deck_clock
            .port = inert_link;
        let interval = |app: &crate::App| {
            app.live_facts(wid)
                .and_then(|f| f.needle)
                .map(|(_, view)| view.interval)
        };
        let (hz60, hz120) = (Duration::from_micros(16_667), Duration::from_micros(8_333));
        app.on_deck_frame_tick(wid, t1 + hz60, hz60, t1);
        present(&mut app, wid, t1);
        assert_eq!(interval(&app), Some(hz60), "on the 60 Hz display");
        let moved = t1 + hz60 * 3;
        app.on_deck_frame_tick(wid, moved + hz120, hz120, moved);
        assert_eq!(interval(&app), Some(hz120), "on the 120 Hz display");
        assert_eq!(
            app.deck_clock_period(&app.windows[&wid]),
            hz120,
            "the clock's period is its link's"
        );
    }

    /// A reading that lands between two of the synthetic grid's vsyncs, at a sweep's start or in
    /// its middle, is ticked at the grid's next vsync, never at the one that passed before it
    /// landed: that vsync's sample reads the new episode's start as a frame due (the CONTROL),
    /// and a tick for it would draw at once, off the grid, beside the reading's own frame.
    #[cfg(unix)]
    #[test]
    fn a_reading_ticks_the_grid_at_its_next_vsync_never_one_before_it_landed() {
        use crate::stats_live::tests::busier;
        let _attached = crate::stats_deck::tests::attach_stats();
        let (mut app, wid, t1) = sweeping();
        let period = app.deck_clock_period(&app.windows[&wid]);
        let due_before = |app: &crate::App, landed: Instant| {
            app.live_facts(wid)
                .expect("the deck draws")
                .needle_due(grid_floor(landed, period), landed)
        };
        assert!(
            due_before(&app, t1).is_some_and(|d| d < t1),
            "CONTROL: the vsync before the reading samples a frame due"
        );
        app.sync_deck_clocks(t1);
        assert_eq!(
            app.deck_clock_deadline(t1),
            Some(grid_ceil(t1, period)),
            "the sweep's first tick is the grid's next vsync"
        );
        let mut now = grid_ceil(t1, period);
        for _ in 0..4 {
            app.service_deck_clocks(now);
            present(&mut app, wid, now);
            app.sync_deck_clocks(now);
            now = app.deck_clock_deadline(now).expect("still sweeping");
        }
        // A reading two and a half periods after the last tick, with none between.
        let landed = now + period * 2 + period / 2;
        let _ = busier(&mut app, 0.2, landed);
        assert!(
            due_before(&app, landed).is_some_and(|d| d < landed),
            "CONTROL: mid-sweep, the vsync before the reading samples a frame due"
        );
        app.sync_deck_clocks(landed);
        assert_eq!(
            app.deck_clock_deadline(landed),
            Some(grid_ceil(landed, period)),
            "a mid-sweep reading's first tick is the grid's next vsync"
        );
    }

    /// A window with no OS window to answer for (headless, or one not yet attached) keeps no
    /// answer about its link: each sync asks again, and the window runs on the synthetic grid
    /// meanwhile, so one given its OS window later takes its display link from its next sync.
    #[cfg(unix)]
    #[test]
    fn a_clock_asks_again_for_a_link_until_a_window_can_answer() {
        thread_local! {
            static NO_LINK_ASKS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        }
        fn no_window_yet(window: Option<&aterm_window::window::Window>, _: bool) -> bool {
            assert!(window.is_none(), "the test's window has no OS window");
            NO_LINK_ASKS.with(|n| n.set(n.get() + 1));
            false
        }
        let _attached = crate::stats_deck::tests::attach_stats();
        let (mut app, wid, t1) = sweeping();
        app.windows
            .get_mut(&wid)
            .expect("the window")
            .deck_clock
            .port = no_window_yet;
        app.sync_deck_clocks(t1);
        app.sync_deck_clocks(t1);
        assert_eq!(
            NO_LINK_ASKS.with(std::cell::Cell::get),
            2,
            "asked each sync"
        );
        let ws = &app.windows[&wid];
        assert_eq!(ws.deck_clock.link, None, "no answer kept");
        let period = app.deck_clock_period(ws);
        let between = grid_ceil(t1, period) + period / 2;
        assert_eq!(
            ws.deck_clock.sample_at(between, period),
            between - period / 2,
            "it samples the synthetic grid meanwhile"
        );
        app.windows
            .get_mut(&wid)
            .expect("the window")
            .deck_clock
            .port = inert_link;
        app.sync_deck_clocks(t1);
        assert_eq!(
            app.windows[&wid].deck_clock.link,
            Some(true),
            "attached, the window's link is taken"
        );
        assert!(app.windows[&wid].deck_clock.link_running());
    }

    /// Without a display link (a headless window, a Mac before 14) the clock is the synthetic
    /// grid the loop's deadline drives: each of the sweep's frames lands on one of its vsyncs, a
    /// whole number of periods after the one before.
    #[cfg(unix)]
    #[test]
    fn the_synthetic_grid_lands_each_frame_on_a_vsync() {
        let _attached = crate::stats_deck::tests::attach_stats();
        let (mut app, wid, t1) = sweeping();
        let period = app.deck_clock_period(&app.windows[&wid]);
        let mut now = t1;
        let mut shown = Vec::new();
        for _ in 0..10_000 {
            app.sync_deck_clocks(now);
            let Some(at) = app.deck_clock_deadline(now) else {
                break;
            };
            now = at.max(now);
            let before = redraws(&app, wid);
            app.service_deck_clocks(now);
            if redraws(&app, wid) > before {
                shown.extend(present(&mut app, wid, now));
            }
        }
        assert!(
            shown.len() > 5,
            "CONTROL: the sweep draws frames ({})",
            shown.len()
        );
        for (k, pair) in shown.windows(2).enumerate() {
            assert_eq!(grid_ceil(pair[0], period), pair[0], "frame {k} on a vsync");
            let gap = pair[1].saturating_duration_since(pair[0]).as_nanos();
            assert!(
                gap > 0 && gap % period.as_nanos() == 0,
                "frame {k}: {gap} ns after the one before, at {period:?}"
            );
        }
    }

    /// A frame that would show the needle where the card already shows it is no frame due: a
    /// reading that lands where the needle rests begins a new episode the card's key cannot
    /// tell from the last (the splice keeps what it drew), and a clock that asked for it would
    /// ask forever. And a frame asked that never draws still costs at most one tick a period.
    #[cfg(unix)]
    #[test]
    fn the_clock_asks_no_frame_that_shows_what_is_drawn() {
        use crate::stats_live::tests::busier;
        let _attached = crate::stats_deck::tests::attach_stats();
        let (mut app, wid, t1) = sweeping();
        let period = app.deck_clock_period(&app.windows[&wid]);
        app.sync_deck_clocks(t1);
        let g0 = app
            .deck_clock_deadline(t1)
            .expect("the sweep's first frame");
        app.service_deck_clocks(g0);
        let after = app.deck_clock_deadline(g0).expect("a frame still due");
        assert!(
            after >= g0 + period,
            "a frame asked and never drawn: the next tick {:?} after the last",
            after.saturating_duration_since(g0)
        );
        // Draw the sweep to its rest, then land the same reading again.
        let rest = t1 + Duration::from_secs(30);
        let facts = app.live_facts(wid).expect("the deck draws");
        app.windows[&wid]
            .deck_needle_drawn
            .set(facts.needle_at(rest));
        assert_eq!(app.deck_clock_deadline(rest), None, "CONTROL: at rest");
        let was = app.vitals.needle.get().expect("a needle");
        let again = busier(&mut app, 0.95, rest);
        assert_ne!(again.t0, was.t0, "CONTROL: a new episode");
        let facts = app.live_facts(wid).expect("the deck draws");
        assert_ne!(
            facts.needle_at(rest),
            app.windows[&wid].deck_needle_drawn.get(),
            "CONTROL: what the window drew is of the last episode"
        );
        app.sync_deck_clocks(rest);
        assert_eq!(
            app.deck_clock_deadline(rest),
            None,
            "it shows the same: nothing due"
        );
    }

    /// A deck frame of a run that waited a park's worth for its drawable runs the rest of the
    /// run at two periods; the run's end clears it (SPECS `deck-frame-interval`).
    #[cfg(unix)]
    #[test]
    fn a_parked_deck_frame_halves_the_clock_until_the_run_ends() {
        let _attached = crate::stats_deck::tests::attach_stats();
        let (mut app, wid, t1) = sweeping();
        let period = app.deck_clock_period(&app.windows[&wid]);
        app.sync_deck_clocks(t1);
        let ws = app.windows.get_mut(&wid).expect("the window");
        ws.deck_card = Some(crate::SettingsCard {
            rgba: vec![0; 4],
            pw: 1,
            ph: 1,
            dx: 0,
            dy: 0,
            fp: 1,
            geom: 1,
            raster_fingerprint_once: std::sync::OnceLock::new(),
            scrim: None,
            serial: 0,
            delta: None,
        });
        ws.deck_motion.card_changed((0, 1));
        app.note_deck_present(wid, t1, t1, crate::EFFECT_LANE_ACQUIRE_PARK_NS - 1);
        assert_eq!(app.deck_frame_interval(&app.windows[&wid]), period);
        app.windows
            .get_mut(&wid)
            .expect("the window")
            .deck_motion
            .card_changed((0, 1));
        app.note_deck_present(wid, t1, t1, crate::EFFECT_LANE_ACQUIRE_PARK_NS);
        assert!(app.windows[&wid].deck_clock.parked());
        assert_eq!(app.deck_frame_interval(&app.windows[&wid]), period * 2);
        app.config.motion = Some("reduced".into());
        app.sync_deck_clocks(t1 + period);
        assert!(!app.windows[&wid].deck_clock.parked(), "the run ended");
        app.config.motion = None;
        assert_eq!(app.deck_frame_interval(&app.windows[&wid]), period);
    }

    /// A reading redraws each window that draws the stats bars on screen, and no other: not an
    /// occluded one (it redraws when it shows again), and none with the bars off.
    #[cfg(unix)]
    #[test]
    fn an_occluded_window_is_not_redrawn_for_a_reading() {
        let _attached = crate::stats_deck::tests::attach_stats();
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let asks = |app: &crate::App| app.windows[&wid].redraw_requests_for_test.get();
        let before = asks(&app);
        app.redraw_stats_decks();
        assert_eq!(
            asks(&app),
            before + 1,
            "CONTROL: a window on screen redraws for a reading"
        );
        app.windows.get_mut(&wid).expect("the window").occluded = true;
        app.redraw_stats_decks();
        assert_eq!(asks(&app), before + 1, "an occluded window is not redrawn");
        app.windows.get_mut(&wid).expect("the window").occluded = false;
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_bars = Some("off".into());
        app.redraw_stats_decks();
        assert_eq!(asks(&app), before + 1, "nor one with the bars off");
    }

    /// Over the gallery's live frames (still, the glide half way, a reading's beat, stalled, and
    /// the needle through its sweep, `deck_live_gallery`), in both forms at the mockups' widths:
    /// the rows a live frame's patch says it drew, and the band the deck card's stamp carries
    /// from the frame before, cover every row where the frame's whole raster differs from the
    /// card the renderer holds, the renderer's debug check (`aterm_gpu`'s `tray_card_update`).
    #[test]
    fn the_dirty_band_covers_every_changed_row() {
        use crate::stats_deck::paint_tests::{AT, Scene, scene, scene_input};
        let mut snap = scene(Scene::Calm);
        snap.at_unix_ms = AT - 7_000;
        let flowing = |ran| DeckLive {
            flow: Some(FlowAt {
                opened_ms: 1_000,
                ran,
            }),
            ..DeckLive::STILL
        };
        let needle = |deg: f64, soak: f64| DeckLive {
            needle: Some((
                (deg * crate::stats_needle::ANGLE_PER_DEG).round() as i32,
                (soak * crate::stats_needle::SOAK_PER_ONE).round() as i32,
            )),
            ..DeckLive::STILL
        };
        let frames = [
            DeckLive::STILL,
            flowing(0.5),
            DeckLive {
                beat: crate::stats_live::beat_phase(0),
                blend: crate::stats_live::tween_phase(0),
                ..flowing(0.25)
            },
            DeckLive::STALE,
            DeckLive::STILL,
            needle(-72.0, 0.0),
            needle(-40.5, 0.1),
            needle(28.8, 0.4),
            needle(75.0, 0.6),
            needle(60.0, 0.7),
            DeckLive::STILL,
        ];
        let changed_rows = |old: &[u8], new: &[u8], stride: usize| {
            let rows: Vec<usize> = (0..new.len() / stride)
                .filter(|&y| old[y * stride..(y + 1) * stride] != new[y * stride..(y + 1) * stride])
                .collect();
            rows.first()
                .zip(rows.last())
                .map(|(a, b)| (*a as u32, *b as u32 + 1))
        };
        let mut checked = 0;
        for (w, s) in [(1119.0_f64, 2.0_f64), (550.0, 2.0), (600.0, 1.0)] {
            for form in [DeckForm::Full, DeckForm::Folded] {
                let h = crate::stats_deck::deck_px(
                    crate::stats_deck::StatsBars::Deck,
                    form,
                    s,
                    (14.0 * s) as usize,
                );
                let g = crate::stats_deck::DeckGeom {
                    w: (w * s) as f32,
                    h: h as f32,
                    pad: (12.0 * s) as f32,
                    scale: s as f32,
                    font_px: (13.0 * s) as f32,
                };
                let raster = |prims: &[crate::widget::DrawPrim]| {
                    crate::tray_raster::rasterize_tray(prims, g.w as u32, g.h as u32, 1.0, [0; 4])
                };
                let mut input = scene_input(&snap, form);
                input.live = frames[0];
                let (prims, ..) = crate::stats_deck::deck_paint_live(&g, &input);
                let (rgba, pw, ph) = raster(&prims);
                let stride = pw as usize * 4;
                let mut card = crate::SettingsCard {
                    rgba,
                    pw,
                    ph,
                    dx: 0,
                    dy: 0,
                    fp: 1,
                    geom: 1,
                    raster_fingerprint_once: std::sync::OnceLock::new(),
                    scrim: None,
                    serial: 0,
                    delta: None,
                };
                let mut motion = DeckMotion::default();
                motion.card_changed((0, ph));
                let _ = motion.stamp();
                for pair in frames.windows(2) {
                    let (was, now) = (pair[0], pair[1]);
                    let at = format!("{form:?} {w} pt {s}x, {was:?} to {now:?}");
                    input.live = now;
                    let (after, _, out) = crate::stats_deck::deck_paint_live(&g, &input);
                    let (want, ..) = raster(&after);
                    let held = card.rgba.clone();
                    let Some(rows) = crate::stats_deck::patch_live_rows(
                        &mut card, &after, &out.rects, &was, &now,
                    ) else {
                        panic!("{at}: the patch fits the card");
                    };
                    motion.card_changed(rows);
                    let (_, dirty) = motion.stamp();
                    let band = dirty.expect("a stamp after the first").rows;
                    if let Some((c0, c1)) = changed_rows(&held, &want, stride) {
                        assert!(
                            rows.0 <= c0 && c1 <= rows.1,
                            "{at}: rows {c0}..{c1} changed, the patch drew {rows:?}"
                        );
                        assert!(
                            band.0 <= c0 && c1 <= band.1,
                            "{at}: rows {c0}..{c1} changed, the stamp says {band:?}"
                        );
                        checked += 1;
                    }
                    if let Some((c0, c1)) = changed_rows(&held, &card.rgba, stride) {
                        assert!(
                            band.0 <= c0 && c1 <= band.1,
                            "{at}: the patch changed rows {c0}..{c1} outside {band:?}"
                        );
                    }
                }
            }
        }
        assert!(
            checked > 30,
            "CONTROL: the frames change the card ({checked})"
        );
    }

    #[test]
    fn each_card_change_is_booked_by_one_present() {
        let mut m = DeckMotion::default();
        assert!(!m.take_unbooked(), "no change, no deck frame");
        m.card_changed((0, 4));
        m.card_changed((2, 3));
        assert!(m.take_unbooked(), "the present after a change books it");
        assert!(
            !m.take_unbooked(),
            "a second present of the same card books nothing"
        );
        m.card_changed((5, 5));
        assert!(!m.take_unbooked(), "an empty band changed nothing");
        m.card_changed((1, 2));
        assert!(m.take_unbooked());
    }

    /// Each stamp names the generation the one before carried and every row changed since, so
    /// a renderer that drew that generation needs only the band.
    #[test]
    fn a_stamp_carries_every_row_changed_since_the_last() {
        let mut m = DeckMotion::default();
        m.card_changed((0, 124));
        let (first, dirty) = m.stamp();
        assert!(
            first != 0 && dirty.is_none(),
            "the first card names no base"
        );
        let (again, dirty) = m.stamp();
        assert_eq!(again, first);
        assert_eq!(
            dirty,
            Some(aterm_gpu::TrayDelta {
                since: first,
                rows: (0, 0)
            })
        );
        m.card_changed((40, 60));
        m.card_changed((10, 20));
        let (next, dirty) = m.stamp();
        assert!(next != first);
        assert_eq!(
            dirty,
            Some(aterm_gpu::TrayDelta {
                since: first,
                rows: (10, 60)
            }),
            "two changes between presents: their union, from the generation presented"
        );
        let mut other = DeckMotion::default();
        other.card_changed((0, 1));
        assert_ne!(other.stamp().0, next, "no two cards share a generation");
    }
}
