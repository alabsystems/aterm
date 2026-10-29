// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The headless App `lab trail-diff`'s dump drives (`docs/design/PRISM-GLASS.md` §15.1 P0.4,
//! §15.7.2): the test App on the repo's font fixture, fed through the paths a person's session
//! takes (the config admission, the control socket's resize, a person's keys, the PTY reader's
//! `process_at` and its output wake, the window's focus, the present-cost feed, the pointer),
//! and drawn through the live present onto a scripted CPU surface, as the event loop's redraw
//! draws a window ([`App::presented_frame_at`]: the single-pane frame through
//! `cursor_frame_for_present`, the pet ticked live with the pointer). Every host clock read on
//! the driving thread is pinned to the dump's injected instant while the rig works
//! ([`crate::metrics::pin_host_clock`]), and a handler stamps what it keeps with its event's own
//! instant (a press on the pet), so a script's words are the same however fast it runs: a test
//! runs scripts paced to the wall clock (`pace_for_test`). Nothing here draws or decides: each
//! call hands its input to the production path. Debug builds of the tests and of the
//! `bench-support` feature, on Unix (the sessions write to `/dev/null`).

use super::*;
use crate::input::{InputEvent, InputOutcome, Source};
use crate::trail_frame_reads::WindowTrailReads;
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_effects::trail_sound::SoundEvent;
use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};
use std::os::fd::AsRawFd;

/// A frame's time after its rig's `t0`, and the centre of the pet's hit rect in window pixels
/// when the pet is drawn.
#[cfg(test)]
type PetSeen = (Duration, Option<(f64, f64)>);

#[cfg(test)]
thread_local! {
    /// Whether this thread's rigs wait for the wall clock to reach each injected instant before
    /// they act at it ([`pace_for_test`]).
    static PACED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Each frame this thread's rigs drew, after its rig's `t0`, with the centre of the pet's
    /// hit rect in window pixels when the pet is drawn ([`pet_seen_for_test`]).
    static PET_SEEN: std::cell::RefCell<Vec<PetSeen>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Run `f` with this thread's rigs paced to the wall clock: each acts at an injected instant only
/// once the wall clock reaches it, so a script runs no faster than real time. A script's words
/// must not move with how fast it runs.
#[cfg(test)]
pub(crate) fn pace_for_test<R>(f: impl FnOnce() -> R) -> R {
    PACED.set(true);
    let out = f();
    PACED.set(false);
    out
}

/// Run `f`, then every frame this thread's rigs drew in it with the pet's hit-rect centre, which
/// a test aims a press at the cat by.
#[cfg(test)]
pub(crate) fn pet_seen_for_test(f: impl FnOnce()) -> Vec<PetSeen> {
    PET_SEEN.with_borrow_mut(Vec::clear);
    f();
    PET_SEEN.take()
}

/// The headless App one dump script drives, and the window it reads.
#[doc(hidden)]
pub struct TrailRig {
    app: App,
    wid: WindowId,
    /// The script's start: a whole microsecond of the host clock's anchor, so a stamp the host
    /// floors to its microsecond ([`crate::metrics::now_us`]) lands on the same instant relative
    /// to `t0` in every run.
    t0: Instant,
    /// Every pane's session in pane order, the first the App's own.
    panes: Vec<u64>,
    /// The sessions' PTY: `/dev/null` takes every key, so each is delivered, and answers nothing.
    null: std::fs::File,
    /// Where the config the rig admits is written ([`Self::configure`]) and the themes it
    /// installs ([`Self::install_theme`]).
    dir: aterm_tempfile::TempDir,
}

/// One presented frame's pixels and the trail sound cues pushed since the one before.
#[doc(hidden)]
pub struct TrailFrame {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// The glass after the present, `0x00RRGGBB`, row-major.
    pub pixels: Vec<u32>,
    /// The cues pushed to the audio host since the frame before (at a key, at this frame's
    /// cursor step), in push order.
    pub sounds: Vec<SoundEvent>,
}

impl TrailRig {
    /// A focused headless window at [`Self::t0`] whose renderer draws `font` (the repo's
    /// fixture) at the App's own size, its lazy fallback faces loaded first so glyph routing is
    /// final, which presents onto a scripted CPU surface, and whose audio host records every cue.
    ///
    /// # Errors
    /// When `/dev/null`, a scratch directory or the font does not open.
    pub fn new(font: &std::path::Path) -> Result<Self, String> {
        let null = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .map_err(|error| format!("/dev/null: {error}"))?;
        let dir = aterm_tempfile::TempDir::new()
            .map_err(|error| format!("no scratch directory for the rig: {error}"))?;
        let sink = Arc::new(SinkWriter::new(null.as_raw_fd()));
        let mut app = App::headless_for_test_with_sink(sink);
        let face = font
            .to_str()
            .ok_or_else(|| format!("{}: not UTF-8", font.display()))?;
        let mut renderer = Renderer::from_system_with_family(Some(face), FONT_PX, app.theme)
            .ok_or_else(|| format!("{face} does not load"))?;
        renderer.set_line_height(Config::default().line_height_or_default());
        renderer.seal_admitted_font_sources();
        renderer.debug_block_on_lazy_fallbacks();
        app.backend = BackendSlot::Ready(Backend::Cpu(renderer));
        // The chrome faces (a split's pane titles draw in the UI face), installed as a headless
        // instance's boot installs them. The store is the process's, not the App's, and a
        // windowed App installs it only after its first present: without this a process's first
        // rig titles frame 0's panes on a cold store and every later rig on a warm one.
        app.sync_chrome_fonts();
        if let Some(ws) = app.windows.get_mut(&WindowId(0)) {
            ws.present = Some(crate::PresentTarget::Cpu {
                surface: crate::present::CpuSurface::Scripted(Default::default()),
            });
        }
        app.trail_audio = crate::trail_audio::TrailAudio::capturing_for_test();
        let t0 = crate::metrics::instant_at_us(crate::metrics::us_at(Instant::now()) + 1)
            .ok_or("the host clock's anchor overflows")?;
        let mut rig = Self {
            app,
            wid: WindowId(0),
            t0,
            panes: vec![0],
            null,
            dir,
        };
        rig.at(t0, |app, wid| app.on_focus(wid, true));
        Ok(rig)
    }

    /// The script's start, every instant the dump prints relative to it.
    #[must_use]
    pub const fn t0(&self) -> Instant {
        self.t0
    }

    /// Run `f` on the App with this thread's host clock pinned to `at`.
    fn at<R>(&mut self, at: Instant, f: impl FnOnce(&mut App, WindowId) -> R) -> R {
        #[cfg(test)]
        if PACED.get() {
            std::thread::sleep(at.saturating_duration_since(Instant::now()));
        }
        crate::metrics::pin_host_clock(Some(at));
        let out = f(&mut self.app, self.wid);
        crate::metrics::pin_host_clock(None);
        out
    }

    /// [`Self::at`] for the crate's tests, which reach the App's paths the dump does not drive
    /// (the live present, a tab's own theme) on the rig's App and clock.
    #[cfg(test)]
    pub(crate) fn with_app<R>(
        &mut self,
        at: Instant,
        f: impl FnOnce(&mut App, WindowId) -> R,
    ) -> R {
        self.at(at, f)
    }

    /// Install theme `text` as `<id>.conf` in the themes folder the next [`Self::configure`]
    /// discovers, as `aterm theme install` writes a pack theme.
    ///
    /// # Errors
    /// When the file cannot be written.
    pub fn install_theme(&mut self, id: &str, text: &str) -> Result<(), String> {
        let themes = self.dir.path().join("themes");
        std::fs::create_dir_all(&themes)
            .map_err(|error| format!("{}: {error}", themes.display()))?;
        let path = themes.join(format!("{id}.conf"));
        std::fs::write(&path, text).map_err(|error| format!("{}: {error}", path.display()))
    }

    /// Admit `toml` as the whole `aterm.toml` at `at`, through the prepared-generation admission
    /// a config reload takes, its `theme` resolved against the installed themes.
    ///
    /// # Errors
    /// When the file cannot be written or read back, or `toml` is not a config.
    pub fn configure(&mut self, toml: &str, at: Instant) -> Result<(), String> {
        let path = self.dir.path().join("aterm.toml");
        std::fs::write(&path, toml).map_err(|error| format!("{}: {error}", path.display()))?;
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)?;
        let config: Config = aterm_toml::from_str(toml).map_err(|error| error.to_string())?;
        let themes = crate::app_config::ThemeCatalog::discover_in(&self.dir.path().join("themes"));
        let assets = config.resolve_asset_catalog_with_themes(Arc::new(themes));
        let path_feeds = config.prepare_path_feed_generation();
        self.at(at, |app, _| {
            app.apply_prepared_config_generation(
                crate::native_font_catalog::PreparedConfigGeneration {
                    observation,
                    config,
                    values: std::collections::BTreeMap::new(),
                    disk_faults: Arc::default(),
                    assets,
                    path_feed_fps: path_feeds.fingerprints,
                    sparkle: path_feeds.sparkle,
                    fonts: None,
                    warnings: Vec::new(),
                    font_refusals: Vec::new(),
                    reason: crate::app_colorscheme::ThemeChange::ConfigReload,
                },
            );
        });
        Ok(())
    }

    /// Re-grid the window to `rows` x `cols` at `at`, as the control socket's `resize px` does.
    ///
    /// # Errors
    /// When the App refuses the size or lands on another grid.
    pub fn grid(&mut self, rows: u16, cols: u16, at: Instant) -> Result<(), String> {
        let px = self.app.window_frame_px_for(self.wid, rows, cols);
        let asked = InputEvent::ResizeWindowPx {
            width: px.width,
            height: px.height,
        };
        let outcome = self.at(at, |app, wid| app.input(wid, asked, Source::Controller));
        if outcome != InputOutcome::Ok {
            return Err(format!("grid {rows} {cols}: {outcome:?}"));
        }
        let ws = &self.app.windows[&self.wid];
        if (ws.rows, ws.cols) != (rows, cols) {
            return Err(format!(
                "grid {rows} {cols}: the window took {} {}",
                ws.rows, ws.cols
            ));
        }
        Ok(())
    }

    /// The panel's refresh rate, as the window's monitor would report it.
    pub fn panel_hz(&mut self, hz: u32) {
        if let Some(ws) = self.app.windows.get_mut(&self.wid) {
            ws.frame_interval = Some(Duration::from_nanos(1_000_000_000 / u64::from(hz.max(1))));
        }
    }

    /// The window's effect present interval: the cadence its trail is drawn and asked at.
    #[must_use]
    pub fn present_interval(&self) -> Duration {
        self.app.windows[&self.wid].effect_present_interval()
    }

    /// The glow's ramp cost: `None` the shipped one (aterm-render's `RAMP_COST`), `Some(true)`
    /// every run a ramp, `Some(false)` every ramp row laid as its quad.
    pub fn ramp_cost(&mut self, always: Option<bool>) {
        let Some(ws) = self.app.windows.get_mut(&self.wid) else {
            return;
        };
        let glow = &mut ws.fx.cursor_mut().glow;
        match always {
            None => glow.set_glow_ramps(true),
            Some(true) => glow.set_ramp_cost(aterm_render::RampCost::ALWAYS),
            Some(false) => glow.set_glow_ramps(false),
        }
    }

    /// Preview `live` in place of `prism_glass::LIVE` on every frame the window's light latch
    /// admits from now on, or restore `LIVE` with `None` (a script's `live <level>`): a frame the
    /// latch refuses stays off whatever the level.
    pub fn live(&mut self, live: Option<aterm_effects::cursor_glow::Live>) {
        if let Some(ws) = self.app.windows.get_mut(&self.wid) {
            ws.trail_ground.force_live(live);
        }
    }

    /// The launch kitty's look, which the pet wears, from `seed`.
    pub fn kitty_seed(&mut self, seed: u64) {
        self.app.launch_kitty = aterm_effects::kitty_registry::KittyLook::for_launch(seed);
    }

    /// Split the focused pane at `at`, a new session on the same PTY stand-in to its right.
    ///
    /// # Errors
    /// When the App refuses the split.
    pub fn split(&mut self, at: Instant) -> Result<(), String> {
        let session = self.app.next_session_id;
        let stub = crate::stub_session_with_sink(
            session,
            Arc::new(SinkWriter::new(self.null.as_raw_fd())),
        );
        self.at(at, |app, wid| {
            app.install_split_pane(wid, pane::SplitDir::Vertical, stub)
        })?;
        self.panes.push(session);
        Ok(())
    }

    /// How many panes the window shows.
    #[must_use]
    pub fn panes(&self) -> usize {
        self.panes.len()
    }

    /// Focus pane `pane` at `at`, moving the pane focus left or right as the keyboard does.
    ///
    /// # Errors
    /// When there is no such pane or the focus does not reach it.
    pub fn focus_pane(&mut self, pane: usize, at: Instant) -> Result<(), String> {
        let target = *self
            .panes
            .get(pane)
            .ok_or_else(|| format!("focus-pane {pane}: {} panes", self.panes.len()))?;
        for _ in 0..self.panes.len() {
            let Some(here) = self
                .app
                .focused_session_id(self.wid)
                .and_then(|s| self.panes.iter().position(|&p| p == s))
            else {
                break;
            };
            let dir = match here.cmp(&pane) {
                std::cmp::Ordering::Equal => return Ok(()),
                std::cmp::Ordering::Less => pane::FocusDir::Right,
                std::cmp::Ordering::Greater => pane::FocusDir::Left,
            };
            if !self.at(at, |app, wid| app.focus_pane_in(wid, dir)) {
                break;
            }
        }
        if self.app.focused_session_id(self.wid) == Some(target) {
            Ok(())
        } else {
            Err(format!("focus-pane {pane}: the focus did not reach it"))
        }
    }

    /// The focused pane's terminal reads `bytes` at `at`, as its PTY reader hands them over, and
    /// the App takes the reader's output wake.
    pub fn pty(&mut self, bytes: &[u8], at: Instant) {
        let Some(mirror) = self.app.front_terminal(self.wid) else {
            return;
        };
        let (term, session) = (mirror.term.clone(), mirror.session);
        term_lock(&term).process_at(
            bytes,
            aterm_core::terminal::ClockReading {
                monotonic: at,
                wall_ms: None,
            },
        );
        self.at(at, |app, _| app.on_output_wake(session, at));
    }

    /// Pane `pane`'s terminal (in pane order, focused or not) reads `bytes` at `at`, as
    /// [`Self::pty`] hands the focused one its bytes.
    #[cfg(test)]
    pub(crate) fn pty_in(&mut self, pane: usize, bytes: &[u8], at: Instant) {
        let session = self.panes[pane];
        let term = self
            .app
            .pool
            .get(session)
            .expect("the pane's session")
            .term
            .clone();
        term_lock(&term).process_at(
            bytes,
            aterm_core::terminal::ClockReading {
                monotonic: at,
                wall_ms: None,
            },
        );
        self.at(at, |app, _| app.on_output_wake(session, at));
    }

    /// Pane `pane`'s session (in pane order).
    #[cfg(test)]
    pub(crate) fn pane_session(&self, pane: usize) -> u64 {
        self.panes[pane]
    }

    /// Whether `key` is spelled as the control socket's `key` verb takes it.
    #[must_use]
    pub fn key_parses(key: &str) -> bool {
        crate::control::parse_key(key).is_some()
    }

    /// Press `key` at `at`, spelled as the control socket's `key` verb takes it
    /// (`crate::control::parse_key`), through the App's input path from a person.
    ///
    /// # Errors
    /// When `key` does not parse or the App refuses it.
    pub fn key(&mut self, key: &str, at: Instant) -> Result<(), String> {
        let ev = crate::control::parse_key(key).ok_or_else(|| format!("key {key}"))?;
        match self.at(at, |app, wid| app.input(wid, ev, Source::Human)) {
            InputOutcome::Ok => Ok(()),
            other => Err(format!("key {key}: {other:?}")),
        }
    }

    /// The window gains or loses focus at `at`.
    pub fn focus(&mut self, focused: bool, at: Instant) {
        self.at(at, |app, wid| app.on_focus(wid, focused));
    }

    /// One present that cost `present_ns` at `at`, fed to the load-shed latch at the window's
    /// frame interval, as a present's outcome feeds it.
    pub fn present_cost(&mut self, present_ns: u64, at: Instant) {
        let interval = self.app.windows[&self.wid]
            .frame_interval
            .unwrap_or(self.app.frame_interval);
        self.at(at, |app, _| app.note_present_cost(present_ns, interval, at));
    }

    /// The pointer moves to window pixel `(x, y)` at `at`, or (`None`) leaves the window.
    pub fn pointer(&mut self, to: Option<(f64, f64)>, at: Instant) {
        self.at(at, |app, wid| match to {
            Some((x, y)) => app.on_cursor_moved(wid, x, y),
            None => app.on_cursor_left(wid),
        });
    }

    /// A left click at window pixel `(x, y)` at `at`: the pointer moves there, the button goes
    /// down and comes up.
    pub fn press(&mut self, x: f64, y: f64, at: Instant) {
        self.at(at, |app, wid| {
            app.on_cursor_moved(wid, x, y);
            app.on_mouse_input_at(wid, ElementState::Pressed, WinitMouseButton::Left, at);
            app.on_mouse_input_at(wid, ElementState::Released, WinitMouseButton::Left, at);
        });
    }

    /// Select the focused pane's viewport cells `from` through `to`, or (`None`) clear its
    /// selection, as a finished drag leaves the terminal's selection.
    pub fn select(&mut self, span: Option<((u16, u16), (u16, u16))>) {
        let Some(mirror) = self.app.front_terminal(self.wid) else {
            return;
        };
        let term = mirror.term.clone();
        let mut term = term_lock(&term);
        match span {
            Some(((r0, c0), (r1, c1))) => {
                let offset = term.grid().display_offset() as i32;
                let selection = term.text_selection_mut();
                selection.start_selection(
                    i32::from(r0) - offset,
                    c0,
                    SelectionSide::Left,
                    SelectionType::Simple,
                );
                selection.update_selection(i32::from(r1) - offset, c1, SelectionSide::Right);
                selection.complete_selection();
                crate::app_mouse::note_selection_custody(&mut term, true);
            }
            None => {
                term.text_selection_mut().clear();
                crate::app_mouse::note_selection_custody(&mut term, false);
            }
        }
    }

    /// The window's frame at `now` through the live present: `None` while a synchronized update
    /// holds it.
    ///
    /// # Errors
    /// When the window lost its scripted surface.
    pub fn frame(&mut self, now: Instant) -> Result<Option<TrailFrame>, String> {
        let Some(((width, height), pixels)) =
            self.at(now, |app, wid| app.presented_frame_at(wid, now))?
        else {
            // The held frame's cues stay for the frame that draws.
            return Ok(None);
        };
        #[cfg(test)]
        {
            let seen = (now.saturating_duration_since(self.t0), self.pet_centre());
            PET_SEEN.with_borrow_mut(|frames| frames.push(seen));
        }
        Ok(Some(TrailFrame {
            width,
            height,
            pixels,
            sounds: self.app.trail_audio.take_captured_for_test(),
        }))
    }

    /// The centre of the pet's hit rect in window pixels, when the pet is drawn.
    #[cfg(test)]
    fn pet_centre(&self) -> Option<(f64, f64)> {
        let (x0, x1, y0, y1) = self.app.windows[&self.wid].fx.companion().hit_rect()?;
        let (ox, oy) = self.app.frame_origin(self.wid);
        Some((
            f64::from(x0 + x1) / 2.0 + ox as f64,
            f64::from(y0 + y1) / 2.0 + oy as f64,
        ))
    }

    /// The window's last frame's render input: the streams the trail laid.
    #[must_use]
    pub fn input(&self) -> &aterm_render::RenderInput {
        &self.app.windows[&self.wid].input_scratch
    }

    /// The window's trail reads ([`App::trail_frame_reads`]), each time relative to
    /// [`Self::t0`] and the glow's next change asked at `now`.
    #[must_use]
    pub fn reads(&self, now: Instant) -> WindowTrailReads {
        self.app
            .trail_frame_reads(self.t0, now)
            .into_iter()
            .find(|read| read.window == self.wid)
            .expect("the rig's window is the App's")
    }
}
