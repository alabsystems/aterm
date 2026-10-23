// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Window frame composition, split-pane redraw, tab-strip splice, and render geometry helpers for [`App`].

use std::num::NonZeroU32;
use std::ops::Range;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_core::render::SpanCell;
use aterm_core::terminal::{
    ContentScrollDelta, ContentScrollState, CursorStyle, RenderCell, Terminal,
};
use aterm_render::{DamageOutcome, RenderInput, Theme};
use aterm_window::dpi::PhysicalSize;
use aterm_window::window::Window;

use aterm_effects::companion::{
    CompanionDuty, cursor_cat_color_key, cursor_companion_duty, cursor_companion_on_glass,
    cursor_companion_presentable, pin_pet_mode_exit, resident_pet_owner_present,
    shed_companion_alpha, shed_companion_presentable, shed_envelope_transitioning,
};

use crate::app_input::{Deliveries, DeliveryTicket};
use crate::predict::GhostCell;
use crate::present::{CpuFrameBuffer as _, CpuPresenter as _};
use crate::{
    App, BLINK_INTERVAL, Backend, PresentDropAccounting, PresentTarget, RepaintKey,
    SelectionFingerprint, SyncObservation, WindowId, WindowState, chrome_band, metrics, pane,
    platform::AppRt, rearm_failed_gpu_recovery, request_gpu_recovery_redraw, tab_bar, term_lock,
    term_try_lock,
};
use aterm_effects::pipeline::TornBand;

#[cfg(clean_islands)]
macro_rules! trust_island {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(clean_islands))]
macro_rules! trust_island {
    ($($tt:tt)*) => {};
}

trust_island! {
    clean {
        namespace SingleHoldPredictionReconcile

        -- Formal model of single-pane predictive echo reconciliation in
        -- redraw_window_with_layout.
        -- engineEchoed: whether the PTY reader has written the echoed glyph into Terminal.
        -- scratchEchoed: whether ws.input_scratch.cells contains the echoed glyph.
        -- ghostPending: whether ws.predictor holds an unconfirmed prediction for the key.
        -- presentedGlyph: whether the composed frame has the glyph (either from
        -- scratchEchoed or from the painted prediction overlay).
        structure World where
          engineEchoed : Bool
          scratchEchoed : Bool
          ghostPending : Bool
          presentedGlyph : Bool

        inductive Event where
          | pressKey : Event
          | ptyEcho : Event
          | extractSnapshot : Event
          | reconcileSingleHold : Event
          | reconcileSecondLock : Event

        -- Under single-hold reconciliation, reconcile reads the same snapshot
        -- that extractSnapshot populated (scratchEchoed), so a prediction is
        -- retired if and only if scratchEchoed is true.
        def stepSingleHold (w : World) : Event -> World
          | Event.pressKey =>
              World.mk false false true true
          | Event.ptyEcho =>
              World.mk true w.scratchEchoed w.ghostPending w.presentedGlyph
          | Event.extractSnapshot =>
              World.mk w.engineEchoed w.engineEchoed w.ghostPending w.engineEchoed
          | Event.reconcileSingleHold =>
              let ghost := if w.scratchEchoed = true then false else w.ghostPending
              let shown := if w.scratchEchoed = true then true else ghost
              World.mk w.engineEchoed w.scratchEchoed ghost shown
          | Event.reconcileSecondLock =>
              let ghost := if w.engineEchoed = true then false else w.ghostPending
              let shown := if w.scratchEchoed = true then true else ghost
              World.mk w.engineEchoed w.scratchEchoed ghost shown

        def runSingleHold (w : World) : List Event -> World
          | [] => w
          | e :: es => runSingleHold (stepSingleHold w e) es

        -- Invariant: after extractSnapshot followed by reconcileSingleHold, even if
        -- ptyEcho interleaves between them, if a key was predicted (ghostPending = true)
        -- or already echoed, the presented frame never drops the glyph.
        theorem single_hold_never_blanks_interleaved_echo :
            (runSingleHold (World.mk false false false false)
              [Event.pressKey, Event.extractSnapshot, Event.ptyEcho, Event.reconcileSingleHold]).presentedGlyph = true := rfl

        -- Negative control (the two-lock race): if ptyEcho arrives AFTER
        -- extractSnapshot (so scratchEchoed = false) but BEFORE a second term_lock
        -- in reconcileSecondLock (so engineEchoed = true), reconcileSecondLock
        -- retires the prediction against the newer engine grid while presenting
        -- the older pre-echo scratch, producing a 1-frame blank flash (presentedGlyph = false).
        theorem two_lock_race_blanks_interleaved_echo :
            (runSingleHold (World.mk false false false false)
              [Event.pressKey, Event.extractSnapshot, Event.ptyEcho, Event.reconcileSecondLock]).presentedGlyph = false := rfl

        -- General theorem: for any engineEchoed state at extraction time,
        -- reconciling against scratchEchoed preserves glyph visibility whenever
        -- ghostPending is true.
        theorem single_hold_preserves_glyph_for_all_echo_states (echoed : Bool) :
            (stepSingleHold (World.mk echoed echoed true echoed) Event.reconcileSingleHold).presentedGlyph = true := by
          cases echoed with
          | false => rfl
          | true => rfl

        end SingleHoldPredictionReconcile
    }
}

const PERF_EMA_ALPHA: f64 = 0.5;
const PERF_SHED_FACTOR: f64 = 1.5;
const PERF_CLEAR_FACTOR: f64 = 0.8;
const PERF_HYSTERESIS_FRAMES: u32 = 3;
/// Minimum shed dwell.
pub(crate) const PERF_SHED_DWELL_MIN: std::time::Duration = std::time::Duration::from_millis(1500);
const PERF_SHED_DWELL_MAX: std::time::Duration = std::time::Duration::from_secs(30);
const PERF_RESHED_QUICK_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);
const SHED_FADE_OUT_SECS: f32 = 0.45;
const SHED_FADE_IN_SECS: f32 = 0.25;
const SYNC_HOLD_CAP: std::time::Duration = std::time::Duration::from_millis(150);

/// How long one key/text egress holds the cursor-effect typed wake ([`App::cursor_fx_focus`]) for its window.
pub(crate) const CURSOR_FX_TYPED_WAKE: std::time::Duration = std::time::Duration::from_secs(8);

/// What one content-scroll snapshot did to the window's cursor family.
pub(crate) type CursorEffectScrollChange = aterm_effects::pipeline::ScrollChange;

/// The `ATERM_TRACE_SPAWN` diagnostic gate for the per-present `SPAWNSRC` trace.
pub(crate) fn trace_spawn_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        aterm_types::dev_seam!("ATERM_TRACE_SPAWN").is_some_and(|v| !v.is_empty() && v != "0")
    })
}

/// The resize seam policy for the PTY behind `master`, asked of the PTY crate rather than the platform.
pub(crate) fn pty_resize_policy(master: i32) -> aterm_core::grid::ResizePolicy {
    #[cfg(windows)]
    {
        if aterm_pty::backend_is_conpty(master) {
            return aterm_core::grid::ResizePolicy::ConPty;
        }
    }
    let _ = master;
    aterm_core::grid::ResizePolicy::Native
}

fn retire_torn_cursor_fx(window: &mut WindowState) {
    window.fx.resight_torn(std::time::Instant::now());
    retire_torn_cursor_fx_host(window);
}

fn regrid_torn_cursor_fx(window: &mut WindowState) {
    window.fx.regrid_torn(std::time::Instant::now());
    retire_torn_cursor_fx_host(window);
}

fn retire_torn_cursor_fx_with(window: &mut WindowState, band: TornBand) {
    window.fx.retire_torn(band, std::time::Instant::now());
    retire_torn_cursor_fx_host(window);
}

/// Rebaseline every cursor-coordinate owner when the focused terminal switches between main and alternate screen without changing session identity.
pub(crate) fn sync_cursor_effect_coordinate_space(
    window: &mut WindowState,
    terminal_id: u64,
    alternate_screen: bool,
) -> bool {
    // The pipeline's owner fence retires its engines' marks, forgets the scroll
    // baseline and re-baselines the pet's probes on the new stream.
    if !window
        .fx
        .fence_owner((terminal_id, alternate_screen), std::time::Instant::now())
    {
        return false;
    }
    retire_torn_cursor_fx_host(window);
    window.last_blink_at = None;
    window.blink_reseed = true;
    true
}

fn retire_torn_cursor_fx_host(window: &mut WindowState) {
    window
        .fx
        .companions_mut()
        .cursor_cat
        .retire_unowned_cursor_motion();
    window.fx.companions_mut().cursor_cat.rebase_placement();
    window.composed_cursor_effect_valid = false;
    window.composed_cursor_fill = None;
    window.composed_cursor_style_override = None;
    window.block_fill = None;
    suppress_torn_cursor_projection(&mut window.input_scratch);
}

fn suppress_torn_cursor_projection(input: &mut aterm_core::render::RenderInput) {
    input.cursor_glow_add.clear();
    input.glow_halo.clear();
    input.fire_patch.clear();
    input.glow_under.clear();
    input.char_fg.clear();
    input.fire_halo.clear();
    input.cursor_trail.clear();
    input.cursor_effect_style_override = None;
    input.cursor_fill_override = None;
    input.cat_quads.clear();
    input.cat_atlas = None;
    input.free_sprites.clear();
    input.free_atlas = None;
}

#[cfg(test)]
#[path = "app_render_cursor_fx_generation_fence_tests.rs"]
mod cursor_fx_generation_fence_tests;

pub(crate) fn sync_cursor_effect_scroll(
    window: &mut WindowState,
    current: ContentScrollState,
) -> CursorEffectScrollChange {
    window.fx.sync_scroll(current, std::time::Instant::now())
}

#[cfg(test)]
#[path = "app_render_cursor_scroll_signal_tests.rs"]
mod cursor_scroll_signal_tests;

const fn causal_render_cost_ns(compose_ns: u64, raster_submit_ns: u64) -> u64 {
    compose_ns.saturating_add(raster_submit_ns)
}

fn gpu_backpressure_excess_ns(acquire_wait_ns: u64, frame_interval: std::time::Duration) -> u64 {
    let fi = u64::try_from(frame_interval.as_nanos()).unwrap_or(u64::MAX);
    acquire_wait_ns.saturating_sub(fi)
}

/// Which surface a piece of chrome paints on, and so which theme its palette ([`App::chrome_palette`]) takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChromeSurface {
    /// The window's header (tab strip, message band, presence, pane headers).
    Header,
    /// A card or band drawn over window `wid`'s grid (the find panel, the paste and close banners, the tab menu, the settings overlay, the badge).
    OverGrid(WindowId),
    /// What paints into the focused pane's cells (find's highlight on every match).
    Content(WindowId),
    /// The terminal grid itself (pane dividers, the active-pane stripe, rain, the drop overlay).
    Grid,
    /// Native pages (Settings, documents).
    NativePage,
}

/// Shipping decision at the failed-present seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailedPresentRoute {
    RetrySurface,
    RecoverGpu,
}

/// Testable projection of the failed CPU-builder branch of one already-routed GPU-loss transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GpuRecoveryRetryObservation {
    pub(crate) accounting: PresentDropAccounting,
    pub(crate) reason: metrics::PresentDropReason,
    pub(crate) parked: bool,
}

/// Exact source-window outcome of one routed GPU-loss transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GpuRecoveryOutcome {
    /// The CPU renderer itself could not be installed (or recovery raced an already-completed fallback).
    BackendUnavailable,
    /// The source CPU target exists and its first redraw edge was delivered.
    SourceReadyRequested,
    /// The source CPU target is not ready and owns this diagnosed bounded retry, including target construction failing after the global CPU backend was installed.
    SourceRetry(GpuRecoveryRetryObservation),
    /// No windowed presentation target exists for the source (headless or the logical window disappeared).
    SourceWithoutTarget,
}

#[must_use]
pub(crate) const fn failed_present_route(use_gpu: bool, device_lost: bool) -> FailedPresentRoute {
    if use_gpu && device_lost {
        FailedPresentRoute::RecoverGpu
    } else {
        FailedPresentRoute::RetrySurface
    }
}

/// Whether a CPU present must copy the whole frame into the retained surface instead of only its dirty rows.
pub(crate) fn cpu_present_needs_full_copy(
    forced: bool,
    translated: bool,
    translated_prev: bool,
    damage: DamageOutcome,
    retained_ok: bool,
) -> bool {
    forced
        || translated
        || translated_prev
        || damage == DamageOutcome::Full
        || matches!(damage, DamageOutcome::Scroll { .. })
        || !retained_ok
}

#[inline]
pub(crate) const fn present_band_bg(default_bg: u32, theme_bg: u32) -> u32 {
    if default_bg == aterm_core::render::COLOR_UNSET {
        theme_bg
    } else {
        default_bg
    }
}

fn cpu_client_capture(
    pixels: &[u32],
    width: usize,
    height: usize,
) -> Option<crate::app_introspect::CapturePixels> {
    crate::app_introspect::CapturePixels::softbuffer(pixels.to_vec(), width, height).ok()
}

pub(crate) use aterm_effects::cursor_fx::compose_caret_style_override;

const INACTIVE_CURSOR_FILL: u32 = 0x00FF_FFFF;

#[inline]
fn window_cursor_fill(
    cursor_override: Option<CursorStyle>,
    active_fill: Option<u32>,
) -> Option<u32> {
    if cursor_override == Some(CursorStyle::HollowBlock) {
        Some(INACTIVE_CURSOR_FILL)
    } else {
        active_fill
    }
}

#[inline]
fn presented_block_fill(
    cursor_override: Option<CursorStyle>,
    projected_fill: Option<u32>,
    candidate: Option<crate::cursor_glow::BlockFill>,
) -> Option<crate::cursor_glow::BlockFill> {
    if cursor_override == Some(CursorStyle::HollowBlock) {
        None
    } else {
        candidate.filter(|owned| projected_fill == Some(owned.fill))
    }
}

#[cfg(test)]
mod window_cursor_fill_tests {
    use super::{INACTIVE_CURSOR_FILL, presented_block_fill, window_cursor_fill};
    use aterm_core::terminal::CursorStyle;
    use aterm_effects::cursor_glow::{BlockFill, BlockFillOwner};

    #[test]
    fn status_owner_tracks_the_final_single_pane_fill() {
        assert_eq!(
            window_cursor_fill(Some(CursorStyle::Bolt), Some(0x00AA_44FF)),
            Some(0x00AA_44FF),
            "non-focus shape overrides keep their effect fill"
        );
        let owned = BlockFill {
            owner: BlockFillOwner::Rainbow,
            fill: 0x0050_FA7B,
            base: Some(0x0000_00FF),
        };
        let focused_fill = window_cursor_fill(None, Some(owned.fill));
        assert_eq!(
            presented_block_fill(None, focused_fill, Some(owned)),
            Some(owned)
        );

        let inactive_fill = window_cursor_fill(Some(CursorStyle::HollowBlock), Some(owned.fill));
        assert_eq!(inactive_fill, Some(INACTIVE_CURSOR_FILL));
        assert_eq!(
            presented_block_fill(Some(CursorStyle::HollowBlock), inactive_fill, Some(owned)),
            None,
            "the host's inactive outline, not the effect, owns the final caret"
        );
    }
}

#[cfg(test)]
mod window_renderer_state_binding_tests {
    use super::*;

    fn renderer_state(app: &App) -> (bool, Option<CursorStyle>, bool) {
        let Backend::Cpu(renderer) = app.backend.ready() else {
            panic!("headless test backend is CPU")
        };
        (
            renderer.cursor_blink_phase(),
            renderer.cursor_style_override(),
            renderer.selection_inactive(),
        )
    }

    #[test]
    fn opposite_two_window_cursor_state_is_rebound_before_each_encode() {
        let mut app = App::headless_for_test();
        let first = WindowId(0);
        let session = app.next_session_id;
        let second = app.insert_logical_window(crate::stub_session(session), 24, 80);
        app.render_knobs.selection_inactive = true;
        {
            let first = app.windows.get_mut(&first).unwrap();
            first.blink_phase = false;
            first.focused = false;
        }
        {
            let second = app.windows.get_mut(&second).unwrap();
            second.blink_phase = true;
            second.focused = true;
        }

        app.backend.set_cursor_blink_phase(true);
        app.backend
            .set_cursor_style_override(Some(CursorStyle::HollowBlock));
        app.backend.set_selection_inactive(false);

        app.bind_window_renderer_state(first);
        assert_eq!(
            renderer_state(&app),
            (false, None, true),
            "glass-less A owns its blink/inactive state and clears a stale OS-only style"
        );
        app.bind_window_renderer_state(second);
        assert_eq!(
            renderer_state(&app),
            (true, None, false),
            "B cannot inherit A's blink or inactive-selection state"
        );
    }
}

#[cfg(test)]
#[path = "app_render_canonical_layout_scheduler_tests.rs"]
mod canonical_layout_scheduler_tests;

fn cpu_surface_transaction<B, E>(
    acquired: Result<B, E>,
    paint_and_present: impl FnOnce(B) -> Result<u64, E>,
) -> Result<u64, metrics::PresentDropReason> {
    let buffer = acquired.map_err(|_| metrics::PresentDropReason::CpuAcquire)?;
    paint_and_present(buffer).map_err(|_| metrics::PresentDropReason::CpuCommit)
}

#[cfg(test)]
mod cpu_surface_transaction_tests {
    use super::{
        FailedPresentRoute, cpu_present_needs_full_copy, cpu_surface_transaction,
        failed_present_route,
    };
    use aterm_render::DamageOutcome;

    #[derive(Debug)]
    struct FakeBuffer;

    #[derive(Debug)]
    struct FakeSurfaceError;

    #[test]
    fn acquire_and_commit_failures_are_dropped_not_counted_as_presents() {
        use crate::metrics::PresentDropReason;
        let acquire = cpu_surface_transaction(Err::<FakeBuffer, _>(FakeSurfaceError), |_| Ok(11));
        assert_eq!(acquire, Err(PresentDropReason::CpuAcquire));
        let commit = cpu_surface_transaction(Ok(FakeBuffer), |_| Err::<u64, _>(FakeSurfaceError));
        assert_eq!(commit, Err(PresentDropReason::CpuCommit));
        let ok = cpu_surface_transaction(Ok::<_, FakeSurfaceError>(FakeBuffer), |_| Ok(17));
        assert_eq!(ok, Ok(17));
    }

    #[test]
    fn a_translated_cpu_frame_and_the_one_after_it_copy_in_full() {
        use DamageOutcome::{Full, GateHit};
        for (case, want) in [
            ((false, false, false, GateHit, true), false), // control
            ((false, true, false, GateHit, true), true),   // sub-row frame
            ((false, false, true, GateHit, true), true),   // settle frame
            ((false, false, false, Full, true), true),
            ((false, false, false, GateHit, false), true), // surface lost the frame
            ((true, false, false, GateHit, true), true),   // caller-forced
        ] {
            let (forced, translated, prev, damage, retained) = case;
            assert_eq!(
                cpu_present_needs_full_copy(forced, translated, prev, damage, retained),
                want,
                "{case:?}"
            );
        }
    }

    #[test]
    fn latched_gpu_loss_routes_to_fallback_before_surface_retry() {
        assert_eq!(
            failed_present_route(false, true),
            FailedPresentRoute::RetrySurface
        );
    }
}

pub(crate) use aterm_effects::sound_policy::{
    TrailSoundPolicy, drain_curse_bonk_cues, trail_sound_event,
};
use aterm_effects::sound_policy::{
    Voice, VoiceTerms, bonk_sound_gain, drain_trail_sound_cues, sing_riff_gain, voice_gain,
};

/// The host input clock in milliseconds.
#[inline]
pub(crate) fn input_clock_ms() -> u32 {
    ((crate::metrics::now_us() / 1000) as u32).max(1)
}

#[cfg(test)]
#[path = "app_render_curse_bonk_drain_tests.rs"]
mod curse_bonk_drain_tests;

pub(crate) fn take_due_trail_tick(
    next: &mut Option<Instant>,
    anchor: &mut Option<Instant>,
    now: Instant,
) -> bool {
    let Some(fired) = *next else {
        return false;
    };
    if now < fired {
        return false;
    }
    *next = None;
    *anchor = Some(fired);
    true
}

const fn rebase_pending_trail_tick_after_present(
    next: &mut Option<Instant>,
    anchor: &mut Option<Instant>,
    frame_started: Instant,
) {
    if next.take().is_some() {
        *anchor = Some(frame_started);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SuccessfulPresentWindowOutcome {
    frame_interval: Duration,
    offscreen: bool,
}

#[cfg(test)]
thread_local! {
    pub(crate) static ACQUIRE_WAIT_NS_FOR_TEST: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SuccessfulPresentRoute {
    Terminal,
    Heterogeneous,
    Native {
        view: crate::tab_model::ViewId,
        presented_stamp: Option<crate::app_native::NativeUiCompileStamp>,
    },
}

/// Window-wide present-time chrome shared by terminal, native, and heterogeneous routes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HostVisualState {
    pub(crate) invert: bool,
    pub(crate) overlay: Option<OverlayGlow>,
}

fn acknowledge_successful_present(
    state: &mut WindowState,
    app_frame_interval: Duration,
    frame_started: Instant,
    presented_at: Instant,
) -> SuccessfulPresentWindowOutcome {
    let offscreen = matches!(state.present, Some(PresentTarget::Virtual { .. }));
    rebase_pending_trail_tick_after_present(
        &mut state.next_trail_tick,
        &mut state.last_trail_fire,
        frame_started,
    );
    let frame_interval = state.frame_interval.unwrap_or(app_frame_interval);
    state.on_present_succeeded();
    state.note_present_backlog(presented_at, frame_interval);
    let content_presented = std::mem::take(&mut state.content_pending);
    if content_presented {
        state.on_content_presented(presented_at);
    }
    if let Some(registration) = aterm_gpu::present_glass::take_registration() {
        state.last_registration_ns = Some(registration.registered_ns);
    }
    SuccessfulPresentWindowOutcome {
        frame_interval,
        offscreen,
    }
}

/// Choose the next effect deadline without accumulating render/present cost.
pub(crate) fn phase_locked_effect_deadline(
    now: Instant,
    interval: Duration,
    last_fire: Option<Instant>,
    coarse_deadline: Option<Instant>,
    needs_frame_cadence: bool,
) -> Instant {
    if !needs_frame_cadence && let Some(deadline) = coarse_deadline {
        return deadline;
    }
    last_fire
        .map(|previous| previous + interval)
        .filter(|deadline| *deadline > now)
        .unwrap_or(now + interval)
}

#[cfg(test)]
mod config_asset_publication_tests {
    use std::sync::Arc;

    use crate::{App, WindowId};

    fn catalog(
        kitty_sprite: crate::app_config::KittySpriteAsset,
        themes: Arc<crate::app_config::ThemeCatalog>,
    ) -> Arc<crate::app_config::ConfigAssetCatalog> {
        Arc::new(crate::app_config::ConfigAssetCatalog {
            trail_style: crate::app_config::TrailStyleCache::empty(),
            kitty_sprite,
            wallpaper: crate::app_config::WallpaperAsset::None,
            themes,
            sparkle_spec_consumers: Default::default(),
        })
    }

    fn assert_exact_catalog_on_every_window(
        app: &App,
        expected: &Arc<crate::app_config::ConfigAssetCatalog>,
    ) {
        assert!(Arc::ptr_eq(&app.config_assets, expected));
        assert!(app.windows.values().all(|window| {
            window
                .installed_config_assets
                .as_ref()
                .is_some_and(|installed| Arc::ptr_eq(installed, expected))
        }));
    }

    #[test]
    fn authoritative_publication_installs_ready_invalid_and_builtin_before_effect_paths() {
        let mut app = App::headless_for_test();
        let sid = app.next_session_id;
        let _second = app.insert_logical_window(crate::stub_session(sid), 24, 80);

        let rgba: Arc<[u8]> = Arc::from([0x12, 0x34, 0x56, 0xff]);
        let ready = catalog(
            crate::app_config::KittySpriteAsset::Ready {
                source_id: Arc::from("same.png"),
                w: 1,
                h: 1,
                rgba: Arc::clone(&rgba),
                fp: 11,
            },
            crate::app_config::ThemeCatalog::empty(),
        );
        assert_eq!(app.publish_config_assets(Arc::clone(&ready)), 2);
        assert_exact_catalog_on_every_window(&app, &ready);
        assert!(
            app.windows
                .values()
                .all(|window| { window.fx.decos().kitty_sprite_source_fingerprint() == Some(11) })
        );
        assert!(!app.install_window_config_assets(WindowId(0)));

        let themes = crate::app_config::ThemeCatalog::from_schemes([(
            "Publication Test".to_string(),
            aterm_types::scheme::builtin("Dracula").expect("builtin theme"),
        )]);
        let theme_only = catalog(ready.kitty_sprite.clone(), themes);
        assert_eq!(app.publish_config_assets(Arc::clone(&theme_only)), 2);
        assert_exact_catalog_on_every_window(&app, &theme_only);

        let invalid = catalog(
            crate::app_config::KittySpriteAsset::Invalid {
                source_id: Arc::from("same.png"),
                bounded_reason: Arc::from("replacement is invalid"),
            },
            crate::app_config::ThemeCatalog::empty(),
        );
        assert_eq!(app.publish_config_assets(Arc::clone(&invalid)), 2);
        assert_exact_catalog_on_every_window(&app, &invalid);
        assert!(app.windows.values().all(|window| {
            window
                .fx
                .decos()
                .kitty_sprite_source_fingerprint()
                .is_none()
        }));

        let builtin = catalog(
            crate::app_config::KittySpriteAsset::BuiltIn,
            crate::app_config::ThemeCatalog::empty(),
        );
        assert_eq!(app.publish_config_assets(Arc::clone(&builtin)), 2);
        assert_exact_catalog_on_every_window(&app, &builtin);
        assert!(
            app.windows
                .values()
                .all(|window| { window.fx.decos().kitty_sprite_source_fingerprint() == Some(0) })
        );
        assert_eq!(
            app.publish_config_assets(Arc::clone(&builtin)),
            0,
            "re-publishing the identical outer Arc is a complete no-op"
        );
    }
}

#[cfg(test)]
#[path = "app_render_trail_present_pacing_tests.rs"]
mod trail_present_pacing_tests;

fn compiled_native_paint_damage(
    previous: &crate::native_ui::CompiledUi,
    current: &crate::native_ui::CompiledUi,
) -> Option<crate::native_app::DamageRegion> {
    if previous.bounds != current.bounds {
        return Some(crate::native_app::DamageRegion::All);
    }
    let mut union: Option<crate::native_ui::LogicalRect> = None;
    let mut include = |rect: crate::native_ui::LogicalRect| {
        if rect.is_empty() || !rect.is_valid() {
            return;
        }
        union = Some(match union {
            Some(existing) => {
                let x = existing.x.min(rect.x);
                let y = existing.y.min(rect.y);
                crate::native_ui::LogicalRect::new(
                    x,
                    y,
                    existing.right().max(rect.right()) - x,
                    existing.bottom().max(rect.bottom()) - y,
                )
            }
            None => rect,
        });
    };
    if previous.paint.len() != current.paint.len() {
        return Some(crate::native_app::DamageRegion::All);
    }
    for (before, after) in previous.paint.iter().zip(&current.paint) {
        if before.key != after.key || before.rect != after.rect || before.clip != after.clip {
            return Some(crate::native_app::DamageRegion::All);
        }
        if before.content == after.content {
            continue;
        }
        let changed = match (&before.content, &after.content) {
            (
                crate::native_ui::UiContent::SettingsPreview(was),
                crate::native_ui::UiContent::SettingsPreview(now),
            ) => {
                let repaint = was.repaint_rect(now, before.rect);
                if repaint.is_empty() {
                    continue;
                }
                repaint.intersect(before.clip).unwrap_or(before.clip)
            }
            _ => before.clip,
        };
        include(changed);
    }
    let rect = union?.intersect(current.bounds)?;
    let x = rect.x.floor().max(0.0) as u32;
    let y = rect.y.floor().max(0.0) as u32;
    let right = rect.right().ceil().max(0.0) as u32;
    let bottom = rect.bottom().ceil().max(0.0) as u32;
    (right > x && bottom > y).then_some(crate::native_app::DamageRegion::Rect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    })
}

fn union_native_render_damage(
    requested: crate::native_app::DamageRegion,
    derived: crate::native_app::DamageRegion,
) -> crate::native_app::DamageRegion {
    use crate::native_app::DamageRegion;
    if requested == DamageRegion::All || derived == DamageRegion::All {
        return DamageRegion::All;
    }
    let (
        DamageRegion::Rect {
            x: first_x,
            y: first_y,
            width: first_width,
            height: first_height,
        },
        DamageRegion::Rect {
            x: second_x,
            y: second_y,
            width: second_width,
            height: second_height,
        },
    ) = (requested, derived)
    else {
        unreachable!("All damage returned above")
    };
    let x = first_x.min(second_x);
    let y = first_y.min(second_y);
    let right = first_x
        .saturating_add(first_width)
        .max(second_x.saturating_add(second_width));
    let bottom = first_y
        .saturating_add(first_height)
        .max(second_y.saturating_add(second_height));
    DamageRegion::Rect {
        x,
        y,
        width: right.saturating_sub(x),
        height: bottom.saturating_sub(y),
    }
}

fn pending_native_leaf_damage(
    cache: &crate::LeafRenderCache,
    stamp: crate::app_native::NativeUiCompileStamp,
    width: u32,
    height: u32,
) -> Option<crate::native_app::DamageRegion> {
    let raster = cache.native.as_ref();
    let stale = raster.is_none_or(|raster| {
        raster.stamp != stamp || raster.width != width || raster.height != height
    }) || cache.native_damage.is_some();
    stale.then(|| {
        if raster.is_some_and(|raster| {
            raster.width == width
                && raster.height == height
                && stamp.accepts_regional_damage_from(raster.stamp)
        }) {
            cache
                .native_damage
                .unwrap_or(crate::native_app::DamageRegion::All)
        } else {
            crate::native_app::DamageRegion::All
        }
    })
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativePreviewFontConvergence",
        action = "Paint",
        project = "aterm_gui::app_render::native_damage_tests::project_native_preview_font_convergence"
    )
)]
fn retain_native_leaf_raster(
    cache: &mut crate::LeafRenderCache,
    scene: crate::app_native::NativeLeafScene,
    width: u32,
    height: u32,
    scale: f32,
    chrome: crate::chrome_band::ChromePalette,
    terminal_theme: Theme,
) {
    let effective_damage = cache
        .native
        .as_ref()
        .and_then(|raster| compiled_native_paint_damage(&raster.compiled, &scene.compiled))
        .map_or(scene.damage, |derived| {
            union_native_render_damage(scene.damage, derived)
        });
    let tray = scene
        .compiled
        .tray_with_chrome(chrome, terminal_theme, 13.0);
    let ime_caret = scene.compiled.ime_caret_rect();
    let patched = match (cache.native.as_mut(), effective_damage) {
        (
            Some(raster),
            crate::native_app::DamageRegion::Rect {
                x,
                y,
                width: damage_width,
                height: damage_height,
            },
        ) if raster.width == width
            && raster.height == height
            && scene.stamp.accepts_regional_damage_from(raster.stamp) =>
        {
            crate::tray_raster::RasterRect::from_logical(
                x,
                y,
                damage_width,
                damage_height,
                scale,
                width,
                height,
            )
            .filter(|region| region.pixels() < u64::from(width) * u64::from(height))
            .is_some_and(|region| {
                let patch = crate::tray_raster::rasterize_tray_region(
                    &tray.prims,
                    scale,
                    [0, 0, 0, 0],
                    region,
                );
                if !crate::tray_raster::apply_raster_patch(
                    &mut raster.rgba,
                    raster.width,
                    raster.height,
                    region,
                    &patch,
                ) {
                    return false;
                }
                raster.stamp = scene.stamp;
                raster.compiled = scene.compiled.clone();
                raster.ime_caret = ime_caret;
                raster.presented = false;
                raster.raster_fingerprint_once.take();
                raster.last_work = crate::NativeRasterWork::Region {
                    rect: region,
                    pixels: region.pixels(),
                };
                raster.regional_rasters = raster.regional_rasters.saturating_add(1);
                true
            })
        }
        _ => false,
    };
    if !patched {
        let (full_rasters, regional_rasters) = cache.native.as_ref().map_or((1, 0), |raster| {
            (
                raster.full_rasters.saturating_add(1),
                raster.regional_rasters,
            )
        });
        let rgba = crate::tray_raster::rasterize_tray_pixels(
            &tray.prims,
            width,
            height,
            scale,
            [0, 0, 0, 0],
        );
        let pixels = u64::from(width) * u64::from(height);
        cache.native = Some(crate::NativeLeafRaster {
            stamp: scene.stamp,
            compiled: scene.compiled,
            ime_caret,
            rgba,
            width,
            height,
            presented_x: 0,
            presented_y: 0,
            presented: false,
            last_work: crate::NativeRasterWork::Full { pixels },
            full_rasters,
            regional_rasters,
            raster_fingerprint_once: std::sync::OnceLock::new(),
        });
    }
    cache.native_damage = None;
}

#[cfg(test)]
#[path = "app_render_native_damage_tests.rs"]
mod native_damage_tests;

pub(crate) const BLINK_RECENT_MAX: Duration = Duration::from_secs(1);

pub(crate) const fn row_probe_trust(
    is_alt: bool,
    blink_recent: bool,
) -> aterm_effects::cursor_glow::ProbeTrust {
    if !is_alt || blink_recent {
        aterm_effects::cursor_glow::ProbeTrust::Full
    } else {
        aterm_effects::cursor_glow::ProbeTrust::ContentOnly
    }
}

/// Whether ordinary forward-typing momentum may own the cursor cat.
#[inline]
pub(crate) const fn ordinary_kitty_cursor_enabled(
    cursor_trail_enabled: bool,
    style: crate::cursor_glow::GlowStyle,
) -> bool {
    cursor_trail_enabled && matches!(style, crate::cursor_glow::GlowStyle::RainbowKitty)
}

/// Consume one echo-correlated cursor pulse and forward it only when the rainbow kitty companion's trail owner is enabled.
#[inline]
pub(crate) fn forward_kitty_cursor_motion(
    cursor_trail_enabled: bool,
    pet_mode: bool,
    style: crate::cursor_glow::GlowStyle,
    pulse: Option<aterm_effects::cursor_glow::CursorCatMotionPulse>,
    cat: &mut crate::kitty_cursor::CursorCat,
) {
    if ordinary_kitty_cursor_enabled(cursor_trail_enabled, style)
        && let Some(pulse) = pulse
    {
        if pet_mode {
            cat.on_pet_mode_motion_pulse(pulse);
        } else {
            cat.on_motion_pulse(pulse);
        }
    }
}

/// Ground an ordinary flying episode as soon as its trail owner goes away (hot reload disabling the master or switching styles), so an invisible sprite's fade/bob state buys no frame cadence.
#[inline]
pub(crate) fn retire_kitty_cursor_without_owner(
    cursor_trail_enabled: bool,
    pet_mode: bool,
    style: crate::cursor_glow::GlowStyle,
    sing_drive: f32,
    cat: &mut crate::kitty_cursor::CursorCat,
) {
    if !ordinary_kitty_cursor_enabled(cursor_trail_enabled, style)
        || (pet_mode && sing_drive <= 0.0)
    {
        cat.retire_unowned_cursor_motion();
    }
}

/// Exact host presentation gate shared by glass and styled captures.
#[inline]
pub(crate) fn cursor_cat_presentation_enabled(
    animate_cat: bool,
    cursor_trail_enabled: bool,
    style: crate::cursor_glow::GlowStyle,
    collection_hello: bool,
) -> bool {
    collection_hello || (animate_cat && ordinary_kitty_cursor_enabled(cursor_trail_enabled, style))
}

/// Reserve what the host splices into the pane after the console observation, by what it is on the glass ([`aterm_effects::pet_world::PetReserve`]).
pub(crate) fn pet_console_exclusions(
    input: &aterm_core::render::RenderInput,
    preedit: bool,
    predictions: Option<(usize, usize, usize, usize)>,
    find_open: bool,
    chrome: &[aterm_effects::pet_world::PetRect],
    menu: &[aterm_effects::pet_world::PetRect],
) -> PetExclusions {
    use aterm_effects::pet_world::PetRect;
    let mut ink = [PetRect::default(); PET_INK_SLOTS];
    let mut typed = 0;
    if preedit && input.display_offset == 0 && input.cursor_row < input.rows && input.cols > 0 {
        ink[typed] = PetRect::new(input.cursor_row as f32, 0.0, 1.0, input.cols as f32);
        typed += 1;
    }
    if input.display_offset == 0
        && let Some((r0, c0, r1, c1)) = predictions
    {
        let r1 = r1.min(input.rows);
        let c1 = c1.min(input.cols);
        if r0 < r1 && c0 < c1 {
            ink[typed] = PetRect::new(r0 as f32, c0 as f32, (r1 - r0) as f32, (c1 - c0) as f32);
            typed += 1;
        }
    }
    let mut rects = [PetRect::default(); PET_EXCLUSIONS];
    let mut count = 0;
    if find_open && input.rows > 0 && input.cols > 0 {
        let rows = crate::find_bar::FIND_BAR_ROWS.min(input.rows);
        rects[count] = PetRect::new(0.0, 0.0, rows as f32, input.cols as f32);
        rects[count + 1] = PetRect::new(
            (input.rows - rows) as f32,
            0.0,
            rows as f32,
            input.cols as f32,
        );
        count += 2;
    }
    if input.display_offset == 0 {
        for &rect in menu.iter().filter(|r| r.valid()) {
            fold_into(&mut rects, &mut count, PET_OVERLAY_SLOTS, rect);
        }
    }
    let overlays = count;
    if input.display_offset == 0 {
        for &rect in chrome.iter().filter(|r| r.valid()) {
            fold_into(&mut rects, &mut count, PET_EXCLUSIONS, rect);
        }
    }
    PetExclusions {
        rects,
        overlays,
        count,
        ink,
        typed,
    }
}

fn fold_into(
    rects: &mut [aterm_effects::pet_world::PetRect; PET_EXCLUSIONS],
    count: &mut usize,
    limit: usize,
    rect: aterm_effects::pet_world::PetRect,
) {
    use aterm_effects::pet_world::PetRect;
    if *count < limit {
        rects[*count] = rect;
        *count += 1;
    } else {
        let last = &mut rects[limit - 1];
        let (row, col) = (last.row.min(rect.row), last.col.min(rect.col));
        *last = PetRect::new(
            row,
            col,
            (last.row + last.rows).max(rect.row + rect.rows) - row,
            (last.col + last.cols).max(rect.col + rect.cols) - col,
        );
    }
}

/// What [`pet_console_exclusions`] reserves.
pub(crate) struct PetExclusions {
    rects: [aterm_effects::pet_world::PetRect; PET_EXCLUSIONS],
    overlays: usize,
    count: usize,
    ink: [aterm_effects::pet_world::PetRect; PET_INK_SLOTS],
    typed: usize,
}

impl PetExclusions {
    /// The find bar's bands and the Autopilot menu's card.
    pub(crate) fn overlays(&self) -> &[aterm_effects::pet_world::PetRect] {
        &self.rects[..self.overlays]
    }

    /// The host's chrome over the grid (`crate::pet_chrome`).
    pub(crate) fn chrome(&self) -> &[aterm_effects::pet_world::PetRect] {
        &self.rects[self.overlays..self.count]
    }

    /// The preedit row and the predictions' hull.
    pub(crate) fn ink(&self) -> &[aterm_effects::pet_world::PetRect] {
        &self.ink[..self.typed]
    }

    /// All three, as the pet's world observes them.
    pub(crate) fn reserve(&self) -> aterm_effects::pet_world::PetReserve<'_> {
        aterm_effects::pet_world::PetReserve {
            protected: self.chrome(),
            overlays: self.overlays(),
            ink: self.ink(),
        }
    }
}

/// The exclusion slots [`pet_console_exclusions`] fills.
pub(crate) const PET_EXCLUSIONS: usize = 16;

const PET_OVERLAY_SLOTS: usize = 3;

const PET_INK_SLOTS: usize = 2;

#[cfg(test)]
mod pet_console_observation_tests {
    use super::pet_console_exclusions;
    use aterm_core::terminal::Terminal;
    use aterm_effects::pet_world::{PetPane, PetRect, PetWorld, PetWorldFacts};

    #[test]
    fn late_native_overlays_reserve_real_cells_before_pet_planning() {
        let mut term = Terminal::new(20, 60);
        term.process(b"\x1b]11;#000000\x07\x1b[6;20H");
        let input = term.cell_frame(20, 60);
        let facts = PetWorldFacts::read(&term, 7);
        let chrome = [PetRect::new(15.0, 30.0, 1.0, 5.0)];
        let menu = [PetRect::new(10.0, 50.0, 2.0, 6.0)];
        let exclusions =
            pet_console_exclusions(&input, true, Some((7, 3, 9, 12)), true, &chrome, &menu);
        assert_eq!(exclusions.ink().len(), 2, "the preedit row and the guesses");
        assert_eq!(
            exclusions.overlays().len(),
            3,
            "two find bands and the menu"
        );
        assert_eq!(exclusions.chrome(), &chrome[..]);
        let mut world = PetWorld::default();
        assert!(world.observe_with_reserve(
            &input,
            &facts,
            PetPane::full(&input),
            exclusions.reserve(),
        ));
        for (row, col) in [
            (5.0, 40.0),
            (7.0, 6.0),
            (0.0, 40.0),
            (19.0, 40.0),
            (15.0, 31.0),
            (10.0, 52.0),
        ] {
            assert!(!world.clear(PetRect::new(row, col, 1.0, 1.0), 0.0));
        }
        assert!(world.clear(PetRect::new(12.0, 40.0, 2.0, 6.0), 0.2));
        for (row, col) in [(5.0, 40.0), (7.0, 6.0), (8.0, 11.0)] {
            let cell = PetRect::new(row, col, 1.0, 1.0);
            assert!(
                world.under_text_clear(cell, 0.0),
                "typed ink at ({row},{col})"
            );
            assert_eq!(world.ink_clearance(cell, 0.0), Some(false), "drawn under");
        }
        for (row, col) in [(0.0, 40.0), (19.0, 40.0), (15.0, 31.0), (10.0, 52.0)] {
            let cell = PetRect::new(row, col, 1.0, 1.0);
            assert!(
                !world.under_text_clear(cell, 0.0),
                "protected at ({row},{col})"
            );
        }
        let none = pet_console_exclusions(&input, false, None, false, &[], &[]);
        assert!(none.overlays().is_empty() && none.chrome().is_empty() && none.ink().is_empty());

        let mut term = Terminal::new(10, 40);
        term.process("line\r\n".repeat(15).as_bytes());
        term.scroll_display(3);
        let input = term.cell_frame(10, 40);
        assert_ne!(input.display_offset, 0);
        let history = pet_console_exclusions(&input, true, Some((3, 4, 4, 5)), false, &[], &[]);
        assert!(
            history.overlays().is_empty()
                && history.chrome().is_empty()
                && history.ink().is_empty()
        );
    }
}

#[cfg(test)]
pub(crate) fn pet_tick_for_test(
    ws: &mut WindowState,
    capture: aterm_effects::host::CaptureMode,
    sense: aterm_effects::kitty_pet::PetSense,
) -> aterm_effects::kitty_pet::PetFrame {
    pet_owner_tick_for_test(ws, capture, sense).pet
}

#[cfg(test)]
pub(crate) fn pet_owner_tick_for_test(
    ws: &mut WindowState,
    capture: aterm_effects::host::CaptureMode,
    sense: aterm_effects::kitty_pet::PetSense,
) -> aterm_effects::companion::CompanionFrame {
    let facts = aterm_effects::host::TerminalFacts {
        session: 0,
        caret: sense.caret,
        cursor_visible: sense.caret_drawn,
        frame_unfinished: false,
        sync_end_seq: 0,
        sync_end_caret: None,
        display_offset: 0,
        live_viewport: true,
        content_seq: 0,
        wrap_serial: 0,
        scrolled: false,
        shell_executing: false,
        cmd_done: None,
        alt_screen: false,
    };
    let host = aterm_effects::host::HostFrameInput {
        now: sense.now,
        visibility: aterm_effects::host::Visibility::Focused,
        reduced_motion: sense.reduced_motion,
        serious: false,
        shed_envelope: 1.0,
        shed_active: false,
        pointer_px: sense
            .pointer
            .map(|(col, row)| (col * f32::from(sense.cell_w), row * f32::from(sense.cell_h))),
        capture,
        geometry: aterm_effects::host::FrameGeom {
            rows: sense.rows,
            cols: sense.cols,
            cell_w: sense.cell_w,
            cell_h: sense.cell_h,
            origin_px: (0, 0),
        },
    };
    let tick = ws.fx.pet_prepare(aterm_effects::companion::PetFacts {
        facts: &facts,
        host: &host,
        glow: aterm_effects::companion::GlowOwnership {
            enabled: true,
            style: crate::cursor_glow::GlowStyle::RainbowKitty,
            style_raw_names_pet: true,
        },
        sing: aterm_effects::host::SingFacts::default(),
        focused: true,
        obscured: false,
        pane: None,
        room: None,
    });
    ws.fx.companion_mut().tick(tick)
}

#[cfg(test)]
pub(crate) fn pet_prepare_for_test(
    ws: &mut WindowState,
    now: Instant,
    session: u64,
    caret: Option<(u16, u16)>,
    cmd_done: Option<(u64, i32, Option<u64>)>,
) -> aterm_effects::companion::PetTick {
    pet_prepare_shed_for_test(ws, now, session, caret, cmd_done, (1.0, false))
}

#[cfg(test)]
pub(crate) fn pet_prepare_shed_for_test(
    ws: &mut WindowState,
    now: Instant,
    session: u64,
    caret: Option<(u16, u16)>,
    cmd_done: Option<(u64, i32, Option<u64>)>,
    (shed_envelope, shed_active): (f32, bool),
) -> aterm_effects::companion::PetTick {
    let facts = aterm_effects::host::TerminalFacts {
        session,
        caret,
        cursor_visible: caret.is_some(),
        frame_unfinished: false,
        sync_end_seq: 0,
        sync_end_caret: None,
        display_offset: 0,
        live_viewport: true,
        content_seq: 0,
        wrap_serial: 0,
        scrolled: false,
        shell_executing: false,
        cmd_done,
        alt_screen: false,
    };
    let host = aterm_effects::host::HostFrameInput {
        now,
        visibility: aterm_effects::host::Visibility::Focused,
        reduced_motion: false,
        serious: false,
        shed_envelope,
        shed_active,
        pointer_px: None,
        capture: aterm_effects::host::CaptureMode::Present,
        geometry: aterm_effects::host::FrameGeom {
            rows: 24,
            cols: 80,
            cell_w: 8,
            cell_h: 16,
            origin_px: (0, 0),
        },
    };
    ws.fx.pet_prepare(aterm_effects::companion::PetFacts {
        facts: &facts,
        host: &host,
        glow: aterm_effects::companion::GlowOwnership {
            enabled: true,
            style: crate::cursor_glow::GlowStyle::RainbowKitty,
            style_raw_names_pet: true,
        },
        sing: aterm_effects::host::SingFacts::default(),
        focused: true,
        obscured: false,
        pane: None,
        room: None,
    })
}

#[cfg(test)]
pub(crate) fn pet_fails_a_command_for_test(
    ws: &mut WindowState,
    now: Instant,
    dur_ms: Option<u64>,
) -> bool {
    let baseline = pet_prepare_for_test(ws, now, 0, None, None);
    let _ = ws.fx.companion_mut().tick(baseline);
    let failed = pet_prepare_for_test(ws, now, 0, None, Some((1, 1, dur_ms)));
    let grieving = ws.fx.companion_mut().grieving();
    let _ = ws.fx.companion_mut().tick(failed);
    grieving
}

#[cfg(test)]
pub(crate) fn seed_resident_pet_mid_flight_for_test(
    ws: &mut WindowState,
    start: Instant,
    rows: u16,
    cols: u16,
    cell_w: u16,
    cell_h: u16,
    target: (u16, u16),
) -> (Instant, aterm_effects::kitty_pet::PetFrame) {
    use aterm_effects::host::CaptureMode;
    let sense = |now, caret| aterm_effects::kitty_pet::PetSense {
        caret_drawn: true,
        now,
        caret,
        rows,
        cols,
        cell_w,
        cell_h,
        reduced_motion: false,
        output_burst: false,
        pointer: None,
        wrapped: false,
    };
    ws.fx.companion_mut().drain();
    let first = pet_tick_for_test(ws, CaptureMode::StaticCapture, sense(start, Some((2, 2))));
    assert!(first.alpha > 0, "fixture materializes the resident");

    let mut now = start;
    for caret in [(2, 3), (2, 4)] {
        now += Duration::from_millis(50);
        let _ = pet_tick_for_test(ws, CaptureMode::Present, sense(now, Some(caret)));
    }
    for _ in 0..120 {
        now += Duration::from_millis(16);
        let _ = pet_tick_for_test(ws, CaptureMode::Present, sense(now, Some((2, 4))));
    }
    for _ in 0..180 {
        now += Duration::from_millis(16);
        let frame = pet_tick_for_test(ws, CaptureMode::Present, sense(now, Some(target)));
        if frame.action == aterm_effects::kitty_pet::PetAction::Leap && frame.lift > 0.0 {
            return (now, frame);
        }
    }
    panic!("fixture failed to earn a resident-pet flight");
}

/// Which rendered-cell snapshot the v2 companion router probes for ink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompanionInk {
    /// `WindowState::input_scratch`.
    Single,
    /// `WindowState::composed_focus_scratch`.
    Composed,
}

impl CompanionInk {
    const fn canvas(self) -> aterm_effects::pipeline::CanvasInk {
        match self {
            Self::Single => aterm_effects::pipeline::CanvasInk::Window,
            Self::Composed => aterm_effects::pipeline::CanvasInk::Pane,
        }
    }
}

/// The companion seam (`docs/design/RAINBOW-KITTY-V2.md` §7.2, D13).
pub(crate) fn route_v2_companion(
    ws: &mut WindowState,
    ink: CompanionInk,
    duty: CompanionDuty,
    now: Instant,
    geom: aterm_effects::word_decorations::EffectGeom,
    cat_frame: &mut crate::kitty_cursor::CatFrame,
    animate: bool,
) -> aterm_effects::pipeline::FlyingSeat {
    let cells: &[Vec<RenderCell>] = match ink {
        CompanionInk::Single => &ws.input_scratch.cells,
        CompanionInk::Composed => &ws.composed_focus_scratch.cells,
    };
    ws.fx
        .route_flying_head(duty, cells, now, geom, cat_frame, animate)
}

#[cfg(test)]
#[path = "app_render_v2_companion_router_tests.rs"]
mod v2_companion_router_tests;

#[cfg(test)]
#[path = "app_render_escort_caret_tests.rs"]
mod escort_caret_tests;

#[derive(Clone, Copy)]
pub(crate) struct RoutedCompanion {
    pub(crate) duty: CompanionDuty,
    pub(crate) cat_frame: crate::kitty_cursor::CatFrame,
    pub(crate) seat: aterm_effects::pipeline::FlyingSeat,
    placement: Option<aterm_effects::word_decorations::KittyCursorPlacement>,
}

impl RoutedCompanion {
    /// What the ambient word-cats yield to this frame ([`cursor_companion_on_glass`]).
    pub(crate) fn on_glass(
        &self,
        cursor: Option<(u16, u16)>,
        pet_body_px: Option<(i32, i32, i32, i32)>,
    ) -> Option<aterm_effects::word_decorations::CompanionOnGlass> {
        cursor_companion_on_glass(
            self.duty,
            cursor,
            self.placement
                .and_then(|placement| placement.drawn_footprint())
                .map(|rect| {
                    (
                        rect.x,
                        rect.x.saturating_add(i32::from(rect.w)),
                        rect.y,
                        rect.y.saturating_add(i32::from(rect.h)),
                    )
                }),
            pet_body_px,
        )
    }
}

fn prepare_routed_head(
    ws: &mut WindowState,
    geom: crate::word_decorations::EffectGeom,
    duty: CompanionDuty,
    cat_frame: crate::kitty_cursor::CatFrame,
    seat: aterm_effects::pipeline::FlyingSeat,
    now: Instant,
) -> Option<aterm_effects::word_decorations::KittyCursorPlacement> {
    let CompanionDuty::FlyingHead { cell } = duty else {
        return None;
    };
    let layout = aterm_effects::word_decorations::KittyCursorLayout {
        geom,
        cursor: cell,
        look: cat_frame.render_look(),
        bob: cat_frame.bob,
        lift: seat.lift,
    };
    let motion = ws.fx.companions_mut().cursor_cat.placement_frame(now);
    ws.fx
        .decos_mut()
        .prepare_kitty_cursor_placement(layout, motion, cat_frame.pose)
}

/// Route the single grid's cursor companion for this frame ([`route_v2_companion`], called for every duty: the impulse is consumed even on an idle or pet frame).
#[allow(
    clippy::too_many_arguments,
    reason = "the companion's custody inputs and the frame it routes"
)]
pub(crate) fn route_single_cursor_companion(
    ws: &mut WindowState,
    geom: crate::word_decorations::EffectGeom,
    cursor: Option<(u16, u16)>,
    pet: &aterm_effects::companion::CompanionFrame,
    mut cat_frame: crate::kitty_cursor::CatFrame,
    animate_head: bool,
    kitty_alpha: u8,
    now: Instant,
) -> RoutedCompanion {
    let duty = cursor_companion_duty(pet.on_glass, kitty_alpha, cursor);
    let seat = route_v2_companion(
        ws,
        CompanionInk::Single,
        duty,
        now,
        geom,
        &mut cat_frame,
        animate_head,
    );
    let placement = prepare_routed_head(ws, geom, duty, cat_frame, seat, now);
    RoutedCompanion {
        duty,
        cat_frame,
        seat,
        placement,
    }
}

/// Emit the cursor-owned companion sprites for a single-grid frame with no word pass between its route and its emission.
#[allow(
    clippy::too_many_arguments,
    reason = "one resolved companion frame carries geometry, palette, motion and custody inputs"
)]
pub(crate) fn emit_single_cursor_companion(
    ws: &mut WindowState,
    geom: crate::word_decorations::EffectGeom,
    cursor: Option<(u16, u16)>,
    pet: &aterm_effects::companion::CompanionFrame,
    present: bool,
    cat_frame: crate::kitty_cursor::CatFrame,
    animate_head: bool,
    kitty_alpha: u8,
    now: Instant,
    notes_reduced: bool,
    default_bg: u32,
    cursor_color: u32,
    accent: u32,
) -> u64 {
    let routed = route_single_cursor_companion(
        ws,
        geom,
        cursor,
        pet,
        cat_frame,
        animate_head,
        kitty_alpha,
        now,
    );
    emit_routed_cursor_companion(
        ws,
        geom,
        pet,
        present,
        routed,
        kitty_alpha,
        now,
        notes_reduced,
        default_bg,
        cursor_color,
        accent,
    )
}

/// Emit the cursor-owned companion sprites for a single-grid frame, as routed ([`RoutedCompanion`]).
#[allow(
    clippy::too_many_arguments,
    reason = "one resolved companion frame carries geometry, palette, motion and custody inputs"
)]
pub(crate) fn emit_routed_cursor_companion(
    ws: &mut WindowState,
    geom: crate::word_decorations::EffectGeom,
    pet: &aterm_effects::companion::CompanionFrame,
    present: bool,
    routed: RoutedCompanion,
    kitty_alpha: u8,
    now: Instant,
    notes_reduced: bool,
    default_bg: u32,
    cursor_color: u32,
    accent: u32,
) -> u64 {
    let mut fp = 0;
    let RoutedCompanion {
        duty,
        cat_frame,
        seat,
        placement,
    } = routed;
    let seat_lift = seat.lift;
    if let CompanionDuty::Pet = duty {
        fp ^= emit_resident_pet(
            ws,
            pet,
            cat_frame.look.normalized(),
            CompanionInk::Single,
            present,
            now,
            aterm_effects::companion::ContrastFallback {
                bg: default_bg,
                cursor: cursor_color,
                accent,
            },
        );
    }

    if let CompanionDuty::FlyingHead { cell } = duty {
        let layout = aterm_effects::word_decorations::KittyCursorLayout {
            geom,
            cursor: cell,
            look: cat_frame.render_look(),
            bob: cat_frame.bob,
            lift: seat_lift,
        };
        if let Some(placement) = placement {
            let colors = ws
                .fx
                .companions_mut()
                .cursor_cat
                .episode_colors()
                .unwrap_or_else(|| {
                    let sampled = cursor_cat_color_key(
                        &ws.input_scratch.cells,
                        geom,
                        placement.sample,
                        default_bg,
                        cursor_color,
                        accent,
                    );
                    ws.fx
                        .companions_mut()
                        .cursor_cat
                        .colors_for_episode(sampled)
                });
            let notes = {
                let c = ws.fx.companions_mut();
                c.music_notes
                    .update(now, c.kitty_sing.is_armed(now), c.kitty_sing.beat(now));
                c.music_notes.frame_array(now, notes_reduced)
            };
            if let Some(cursor_art_fp) = ws.fx.draw_flying_head(
                CompanionInk::Single.canvas(),
                aterm_effects::word_decorations::KittyCursorFrame {
                    geom,
                    cursor: cell,
                    look: layout.look,
                    colors,
                    bob: cat_frame.bob,
                    lift: seat_lift,
                    alpha: kitty_alpha,
                    pose: cat_frame.pose,
                    sing: cat_frame.sing,
                    notes,
                },
                placement,
            ) {
                fp ^= cursor_art_fp.rotate_left(29);
            }
        }
    }
    fp
}

/// The performance seam, host half.
pub(crate) fn emit_resident_pet(
    ws: &mut WindowState,
    pet: &aterm_effects::companion::CompanionFrame,
    look: aterm_effects::kitty_registry::KittyLook,
    ink: CompanionInk,
    present: bool,
    now: Instant,
    fallback: aterm_effects::companion::ContrastFallback,
) -> u64 {
    let cells: &[Vec<RenderCell>] = match ink {
        CompanionInk::Single => &ws.input_scratch.cells,
        CompanionInk::Composed => &ws.composed_focus_scratch.cells,
    };
    let arrival = ws.fx.look().tenure().arrival();
    let rung = ws.fx.look().rung();
    ws.fx
        .companion_mut()
        .set_look((look.coat, look.iris), arrival, rung);
    let (fp, outcome) = ws.fx.draw_pet(ink.canvas(), pet, cells, fallback);
    if ws.fx.companion().commit_hello_due(present, outcome) {
        ws.fx.look_mut().tenure_mut().commit_hello(now);
    }
    fp
}

#[cfg(test)]
mod resident_pet_presentation_tests {
    use aterm_effects::host::CaptureMode;
    use aterm_effects::kitty_pet::PetSense;
    use std::time::{Duration, Instant};

    #[test]
    fn a_reduced_motion_resident_keeps_full_body_cursor_home_and_no_frame_debt() {
        let mut app = crate::App::headless_for_test();
        let geom = aterm_effects::word_decorations::EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 30,
            cols: 80,
        };
        let ws = app.windows.get_mut(&crate::WindowId(0)).unwrap();
        let start = Instant::now();
        let mut tick = 0;
        for caret_col in [12_u16, 60] {
            let caret = (4, caret_col);
            let mut previous = None;
            for _ in 0..3 {
                let now = start + Duration::from_millis(tick * 16);
                tick += 1;
                let pet = crate::app_render::pet_owner_tick_for_test(
                    ws,
                    CaptureMode::Present,
                    PetSense {
                        caret_drawn: true,
                        now,
                        caret: Some(caret),
                        rows: geom.rows,
                        cols: geom.cols,
                        cell_w: geom.cell_w,
                        cell_h: geom.cell_h,
                        reduced_motion: true,
                        output_burst: false,
                        pointer: None,
                        wrapped: false,
                    },
                );
                assert!(pet.on_glass);
                let body = pet.body_px.expect("the static resident is drawn");
                let cursor_x = i32::from(caret.1) * i32::from(geom.cell_w);
                assert!(
                    (body.0 - cursor_x).max(cursor_x - body.1).max(0)
                        <= 10 * i32::from(geom.cell_w),
                    "the stationary posture follows the caret"
                );
                assert!(
                    !ws.fx.companion_mut().needs_frames(),
                    "a retained static body owes no idle animation"
                );
                if let Some(previous) = previous {
                    assert_eq!(pet.fp, previous, "idle frames remain byte-stable");
                }
                previous = Some(pet.fp);
                ws.fx.open_words();
                let head = ws.fx.companions_mut().cursor_cat.static_frame(now);
                let fp = super::emit_single_cursor_companion(
                    ws,
                    geom,
                    Some(caret),
                    &pet,
                    false,
                    head,
                    false,
                    0,
                    now,
                    true,
                    0x0010_1010,
                    0x00FF_FFFF,
                    0x0050_FA7B,
                );
                assert_ne!(fp, 0);
                assert_eq!(
                    ws.fx.free_sprites().len(),
                    1,
                    "the static posture emits only its full body"
                );
                assert!(ws.fx.free_sprites()[0].w >= 5 * geom.cell_w);
            }
        }
    }
}

/// The redraw early-out decision, pure for unit tests.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AsymmetricPadLayout",
        action = "RenderWithLayoutCache",
        project = "aterm_gui::app_render::project_asymmetric_pad_layout_key"
    )
)]
pub(crate) fn should_repaint(prev: Option<RepaintKey>, cur: RepaintKey) -> bool {
    prev != Some(cur)
}

/// A requested recovery frame bypasses the content-key cache until a real present/drop acknowledges it.
#[inline]
pub(crate) fn should_repaint_or_recover(
    prev: Option<RepaintKey>,
    cur: RepaintKey,
    recovery_redraw_outstanding: bool,
) -> bool {
    recovery_redraw_outstanding || should_repaint(prev, cur)
}

#[cfg(test)]
pub(crate) fn project_asymmetric_pad_layout_key(key: &RepaintKey) -> usize {
    key.grid_top
}

/// Async fallback-face convergence, pure for unit tests.
#[must_use]
pub(crate) const fn fallback_convergence_action(pending: bool, was: bool) -> (bool, bool) {
    let converging = pending || was;
    (converging, converging && !pending)
}

#[must_use]
fn sync_frame_hold(
    observation: SyncObservation,
    was_active: bool,
    armed: Option<std::time::Instant>,
    armed_end_seq: u64,
    now: std::time::Instant,
    timeout: std::time::Duration,
) -> (Option<std::time::Instant>, bool, u64) {
    if !observation.active {
        return (None, false, observation.end_seq);
    }
    if was_active && observation.end_seq != armed_end_seq {
        return (
            Some(now + timeout),
            observation.open_dirty,
            observation.end_seq,
        );
    }
    let mut deadline = if !was_active {
        // Distinct branch.
        Some(now + timeout) // rising edge: arm
    } else {
        // Distinct branch.
        armed // still active mid-episode: keep the armed deadline (may be expired)
    };
    let hold = deadline.is_some_and(|d| now < d);
    if !hold {
        // Distinct branch.
        deadline = None; // released: no stale past-deadline for about_to_wait to fold
    }
    (deadline, hold, observation.end_seq)
}

fn note_sync_hold_transition(
    sync_active: bool,
    was_active: bool,
    previous_deadline: Option<Instant>,
    deadline: Option<Instant>,
    sync_end_seq: u64,
    armed_end_seq: u64,
) {
    let closed_and_rearmed = sync_active && was_active && sync_end_seq != armed_end_seq;
    if (sync_active && !was_active) || closed_and_rearmed {
        metrics::note_sync_armed();
    }
    if closed_and_rearmed {
        metrics::note_sync_release_end();
    } else if previous_deadline.is_some() && deadline.is_none() {
        if sync_active {
            metrics::note_sync_release_timeout();
        } else {
            metrics::note_sync_release_end();
        }
    }
}

fn step_sync_pane_hold(
    holds: &mut Vec<crate::ComposedSyncPaneHold>,
    observation: SyncObservation,
    now: Instant,
    timeout: Duration,
) -> (bool, Option<Instant>) {
    let index = holds
        .iter()
        .position(|state| state.terminal_id == observation.terminal_id)
        .unwrap_or_else(|| {
            holds.push(crate::ComposedSyncPaneHold {
                terminal_id: observation.terminal_id,
                was_active: false,
                hold_until: None,
                armed_end_seq: observation.end_seq,
            });
            holds.len() - 1
        });
    let state = &mut holds[index];
    let (deadline, hold, armed_seq) = sync_frame_hold(
        observation,
        state.was_active,
        state.hold_until,
        state.armed_end_seq,
        now,
        timeout,
    );
    note_sync_hold_transition(
        observation.active,
        state.was_active,
        state.hold_until,
        deadline,
        observation.end_seq,
        state.armed_end_seq,
    );
    state.was_active = observation.active;
    state.hold_until = deadline;
    state.armed_end_seq = armed_seq;
    (hold, deadline)
}

#[must_use]
fn update_sync_frame_hold(
    window: &mut WindowState,
    observation: SyncObservation,
    now: Instant,
    timeout: Duration,
) -> bool {
    update_sync_frame_holds(window, &[observation], now, timeout)
}

#[must_use]
fn update_composed_sync_frame_hold(
    window: &mut WindowState,
    observations: &[SyncObservation],
    now: Instant,
    timeout: Duration,
) -> bool {
    update_sync_frame_holds(window, observations, now, timeout)
}

#[must_use]
fn update_sync_frame_holds(
    window: &mut WindowState,
    observations: &[SyncObservation],
    now: Instant,
    timeout: Duration,
) -> bool {
    window.sync_holds.retain(|state| {
        observations
            .iter()
            .any(|observation| observation.terminal_id == state.terminal_id)
    });

    let mut any_hold = false;
    let mut earliest_deadline: Option<Instant> = None;
    for (observation_index, &observation) in observations.iter().enumerate() {
        // The same terminal can be visible through more than one view. Its
        // protocol edge is still one event; folding it twice would turn the
        // close+rearm Allow decision into an immediate Hold just like a second
        // extraction recheck. The first occurrence owns the state transition.
        if observations[..observation_index]
            .iter()
            .any(|seen| seen.terminal_id == observation.terminal_id)
        {
            continue;
        }
        let (hold, deadline) =
            step_sync_pane_hold(&mut window.sync_holds, observation, now, timeout);
        any_hold |= hold;
        if let Some(deadline) = deadline {
            earliest_deadline = Some(earliest_deadline.map_or(deadline, |old| old.min(deadline)));
        }
    }
    window.sync_hold_until = earliest_deadline;
    window.sync_release_retry_at = None;
    window.sync_holding_gauge.set(any_hold);
    any_hold
}

#[must_use]
fn recheck_sync_frame_hold(
    window: &mut WindowState,
    preflight: SyncObservation,
    current: SyncObservation,
    now: Instant,
    timeout: Duration,
) -> bool {
    // Re-applying an identical observation is not idempotent at the one-frame
    // close+reopen boundary: preflight deliberately allows that completed
    // bracket while arming the next episode, and a second transition would
    // immediately hold the newly armed episode instead. Only a terminal state
    // that actually changed in the lock gap needs another transition.
    if preflight == current {
        return false;
    }
    if preflight.terminal_id != current.terminal_id {
        // The pane changed terminal identity after the plan snapshot: do not
        // authorize extraction against a different owner. The next redraw
        // rebuilds the visible set.
        return true;
    }
    let (hold, _) = step_sync_pane_hold(&mut window.sync_holds, current, now, timeout);
    window.sync_hold_until = window
        .sync_holds
        .iter()
        .filter_map(|state| state.hold_until)
        .min();
    window.sync_release_retry_at = None;
    let any_hold = window
        .sync_holds
        .iter()
        .any(|state| state.hold_until.is_some_and(|deadline| now < deadline));
    window.sync_holding_gauge.set(any_hold);
    hold
}

#[cfg(test)]
#[path = "app_render_sync_hold_tests.rs"]
mod sync_hold_tests;

#[cfg(test)]
mod fallback_convergence_tests {
    use super::fallback_convergence_action;

    #[test]
    fn fallback_convergence_truth_table() {
        for (pending, was, want, why) in [
            (true, false, (true, false), "first pending frame"),
            (true, true, (true, false), "steady pending frame"),
            (false, true, (true, true), "landing edge"),
            (false, false, (false, false), "not converging"),
        ] {
            assert_eq!(fallback_convergence_action(pending, was), want, "{why}");
        }
    }
}

#[cfg(all(test, any(windows, target_os = "linux")))]
mod strip_band_landing_tests {
    use super::*;
    use crate::tab_bar::TabHit;

    fn compose(app: &mut App, wid: WindowId) {
        let (rows, cols) = {
            let ws = &app.windows[&wid];
            (ws.rows, ws.cols)
        };
        let terminal = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        {
            let ws = app.windows.get_mut(&wid).expect("window");
            let mut term = term_lock(&terminal);
            term.resize(rows, cols);
            term.cell_frame_into(&mut ws.input_scratch, usize::from(rows), usize::from(cols));
        }
        app.splice_tab_strip(wid);
    }

    fn present_raster(app: &mut App, wid: WindowId) {
        let App {
            windows, backend, ..
        } = app;
        let ws = windows.get(&wid).expect("window");
        let Backend::Cpu(renderer) = backend.ready_mut() else {
            panic!("headless test backend is CPU")
        };
        let _ = renderer.render_input(&ws.input_scratch);
    }

    fn band_covers(app: &App, wid: WindowId, col: usize) -> bool {
        let band = &app.windows[&wid].cached_strip_band;
        let Some(row) = band.first() else {
            return false;
        };
        let Some((_, first)) = row.first() else {
            return false;
        };
        row.iter()
            .any(|(c, r)| *c == col && Arc::ptr_eq(&r.image, &first.image))
    }

    fn finalize(app: &mut App, wid: WindowId) {
        let plan = app.active_visible_leaf_plan(wid).expect("visible plan");
        app.finalize_successful_present(
            wid,
            crate::metrics::StartupPresentTiming::collapsed(Instant::now()),
            0,
            None,
            SuccessfulPresentRoute::Terminal,
            crate::VisibleContentRoute::Terminal { composed: false },
            HostVisualState::default(),
            &plan,
        );
    }

    #[test]
    fn a_cjk_title_is_a_band_title_on_its_first_frame_and_asks_one_frame_more() {
        let Some(path) = crate::tray_raster::cjk_chain_face_path_for_test() else {
            aterm_spec::host::could_not_run("no CJK chain face here");
        };
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        app.first_present_done = true;
        let session = crate::stub_session(app.next_session_id);
        app.push_stub_tab(wid, session);
        let terminal = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        {
            let mut term = term_lock(&terminal);
            term.process("\x1b[2 q\x1b]0;日本語 shell\x07".as_bytes());
            assert!(
                !crate::blink_clock_armed(term.cursor_visible(), term.cursor_style(), false),
                "a steady cursor: the blink clock is not armed"
            );
        }
        let seed = {
            let App {
                backend,
                font_px,
                theme,
                ..
            } = &mut app;
            let Backend::Cpu(renderer) = backend.ready_mut() else {
                panic!("headless test backend is CPU")
            };
            renderer.set_runtime_font_discovery(false);
            renderer.set_config_fallback_fonts(&[path.to_string()]);
            let _ = renderer.seal_admitted_font_sources();
            renderer
                .fork_semantic_surface(*font_px, *theme)
                .expect("a sealed generation forks")
        };
        crate::tray_raster::install_dormant_chrome_fonts_for_test(seed);
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        if !crate::tray_raster::strip_band_ui_ready() {
            crate::tray_raster::clear_ui_fonts_for_test();
            aterm_spec::host::could_not_run("no system UI face here for the strip band");
        }

        assert!(
            !crate::tray_raster::chrome_cascade_reaches_for_test('日'),
            "CONTROL: nothing has parsed the CJK face before frame 1"
        );
        compose(&mut app, wid);
        let seg = app.windows[&wid]
            .tab_segments
            .iter()
            .find(|s| matches!(s.kind, TabHit::Select(1)))
            .copied()
            .expect("the CJK tab's segment");
        let probe = usize::from(seg.start_col) + 1;
        let first_key = app.windows[&wid].strip_band_key;
        assert!(
            band_covers(&app, wid, probe),
            "frame 1: the CJK segment is band-covered on its first frame"
        );
        present_raster(&mut app, wid);
        {
            let key =
                crate::early_out_tests::frame_key(&mut term_lock(&terminal), false, false, None);
            app.windows
                .get_mut(&wid)
                .expect("window")
                .stamp_present_decision(key);
        }
        assert!(app.windows[&wid].last_present.is_some());
        finalize(&mut app, wid);
        assert!(
            app.windows[&wid].last_present.is_none(),
            "the follow-up cleared the retained key after the first present"
        );
        let mut requests = 0;
        assert!(app.rearm_for_strip_band_landing(wid, || requests += 1));
        assert_eq!(
            requests, 1,
            "a redraw was requested after the first present"
        );

        compose(&mut app, wid);
        let second_key = app.windows[&wid].strip_band_key;
        assert_eq!(
            (first_key.0, first_key.1, first_key.2, first_key.3),
            (second_key.0, second_key.1, second_key.2, second_key.3),
            "the geometry terms did not move"
        );
        assert_ne!(
            first_key.4, second_key.4,
            "the landing moved the band key's epoch term"
        );
        assert!(
            band_covers(&app, wid, probe),
            "frame 2: the CJK segment is band-covered"
        );
        let mut requests = 0;
        assert!(!app.rearm_for_strip_band_landing(wid, || requests += 1));
        assert_eq!(requests, 0, "a settled band requests no frame");
        crate::tray_raster::clear_ui_fonts_for_test();
    }

    #[test]
    fn no_landing_no_rearm() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let renderer = aterm_render::Renderer::from_bytes(
            aterm_render::embedded_font(),
            14.0,
            aterm_render::Theme::default(),
        )
        .expect("embedded renderer");
        crate::tray_raster::install_dormant_chrome_fonts_for_test(renderer);
        let mut requests = 0;
        app.tab_strip_rows = 0;
        assert!(
            !app.rearm_for_strip_band_landing(wid, || requests += 1),
            "no strip: nothing to land"
        );
        app.tab_strip_rows = 1;
        compose(&mut app, wid);
        assert!(
            !app.rearm_for_strip_band_landing(wid, || requests += 1),
            "a settled band's re-read is not a landing"
        );
        assert_eq!(requests, 0);
        crate::tray_raster::clear_ui_fonts_for_test();
    }
}

/// Coalesce a per-row dirty set into inclusive `(first, last)` runs of consecutive dirty rows.
pub(crate) fn dirty_row_runs(dirty: &[bool]) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut row = 0usize;
    std::iter::from_fn(move || {
        while row < dirty.len() && !dirty[row] {
            row += 1;
        }
        if row >= dirty.len() {
            return None;
        }
        let start = row;
        while row < dirty.len() && dirty[row] {
            row += 1;
        }
        Some((start, row - 1))
    })
}

fn cpu_damage_rects_into(
    out: &mut Vec<crate::present::DamageRect>,
    dirty: &[bool],
    x0: usize,
    span: usize,
    off_y: i64,
    surface_height: usize,
    mut row_pixel_band: impl FnMut(usize) -> (usize, usize),
) -> bool {
    out.clear();
    out.reserve(dirty.len().div_ceil(2).max(1));
    let Some(width) = NonZeroU32::new(span as u32) else {
        let one = NonZeroU32::new(1).expect("one is non-zero");
        out.push(crate::present::DamageRect {
            x: 0,
            y: 0,
            width: one,
            height: one,
        });
        return false;
    };
    let surface_height = i64::try_from(surface_height).unwrap_or(i64::MAX);
    for (r0, r1) in dirty_row_runs(dirty) {
        let (fy0, _) = row_pixel_band(r0);
        let (_, fy1) = row_pixel_band(r1);
        let lo = i64::try_from(fy0)
            .unwrap_or(i64::MAX)
            .saturating_add(off_y)
            .clamp(0, surface_height);
        let hi = i64::try_from(fy1)
            .unwrap_or(i64::MAX)
            .saturating_add(off_y)
            .clamp(0, surface_height);
        if lo < hi
            && let Some(height) = NonZeroU32::new(u32::try_from(hi - lo).unwrap_or(u32::MAX))
        {
            out.push(crate::present::DamageRect {
                x: x0 as u32,
                y: lo as u32,
                width,
                height,
            });
        }
    }
    let copied = !out.is_empty();
    if !copied {
        let one = NonZeroU32::new(1).expect("one is non-zero");
        out.push(crate::present::DamageRect {
            x: 0,
            y: 0,
            width: one,
            height: one,
        });
    }
    copied
}

#[cfg(test)]
mod cpu_damage_rect_tests {
    use super::cpu_damage_rects_into;

    fn tuples(rects: &[crate::present::DamageRect]) -> Vec<(u32, u32, u32, u32)> {
        rects
            .iter()
            .map(|rect| (rect.x, rect.y, rect.width.get(), rect.height.get()))
            .collect()
    }

    #[test]
    fn dirty_damage_geometry_is_exact() {
        let dirty = [true, true, false, true, false, true, true];
        let mut rects = Vec::new();
        assert!(cpu_damage_rects_into(
            &mut rects,
            &dirty,
            3,
            8,
            -5,
            55,
            |row| (row * 10, (row + 1) * 10)
        ));
        assert_eq!(
            tuples(&rects),
            vec![(3, 0, 8, 15), (3, 25, 8, 10), (3, 45, 8, 10)],
            "contiguous rows coalesce and top/bottom cropping stays exact",
        );

        for (dirty, span) in [([false, false], 8), ([true, true], 0)] {
            let mut rects = Vec::new();
            assert!(!cpu_damage_rects_into(
                &mut rects,
                &dirty,
                3,
                span,
                0,
                20,
                |row| (row * 10, (row + 1) * 10),
            ));
            assert_eq!(tuples(&rects), vec![(0, 0, 1, 1)]);
        }
    }
}

/// The `RepaintKey::system_dark` term (and the overlay card raster's geom_key term) from the App-tracked OS appearance (`sync_app_theme_to_appearance`), the one source for every key construction site.
pub(crate) fn repaint_system_dark(appearance: aterm_types::Appearance) -> bool {
    appearance == aterm_types::Appearance::Dark
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PlacedFrame {
    surface_width: usize,
    surface_height: usize,
    origin_x: i64,
    origin_y: i64,
    frame_width: usize,
    frame_height: usize,
}

fn apply_bell_invert_at(pixels: &mut [u32], placement: PlacedFrame, active: bool) {
    let PlacedFrame {
        surface_width,
        surface_height,
        origin_x,
        origin_y,
        frame_width,
        frame_height,
    } = placement;
    if !active || surface_width == 0 || surface_height == 0 {
        return;
    }
    for row in 0..frame_height {
        let y = origin_y.saturating_add(row as i64);
        if y < 0 || y >= surface_height as i64 {
            continue;
        }
        for col in 0..frame_width {
            let x = origin_x.saturating_add(col as i64);
            if x < 0 || x >= surface_width as i64 {
                continue;
            }
            if let Some(pixel) = pixels.get_mut(y as usize * surface_width + x as usize) {
                *pixel ^= 0x00ff_ffff;
            }
        }
    }
}

fn drop_border_px(w: usize, h: usize) -> usize {
    (w.min(h) / 200).clamp(2, 6)
}

const DROP_WASH_ALPHA: u32 = 28; // ~11% — readable content underneath
const DROP_BORDER_ALPHA: u32 = 235; // ~92% — a crisp but not harsh frame

/// The parameters of one inset-accent-border overlay pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OverlayGlow {
    /// Overlay accent (packed `0x00RRGGBB`; the top byte is ignored).
    pub(crate) accent: u32,
    /// Interior full-grid wash alpha (0..255).
    pub(crate) wash_a: u8,
    /// Inset-border alpha (0..255).
    pub(crate) border_a: u8,
    /// Border thickness as a multiple of the size-derived law ([`drop_border_px`]), in 1/16 units.
    pub(crate) border_scale_q4: u8,
}

fn scaled_border_px(base: usize, scale_q4: u8) -> usize {
    if scale_q4 == 0 {
        return base;
    }
    ((base * usize::from(scale_q4)) / 16).max(1)
}

#[cfg(test)]
mod scaled_border_px_tests {
    use super::scaled_border_px;

    #[test]
    fn scaled_border_px_is_sixteenths_never_thinner_than_a_pixel() {
        for (base, q4, want) in [(6, 0, 6), (6, 16, 6), (6, 24, 9), (1, 1, 1), (0, 16, 1)] {
            assert_eq!(scaled_border_px(base, q4), want, "base {base} × {q4}/16");
        }
    }
}

const fn blend_rgb(bg: u32, fg: u32, a: u32) -> u32 {
    let inv = 255 - a;
    let r = (((bg >> 16) & 0xff) * inv + ((fg >> 16) & 0xff) * a) / 255;
    let g = (((bg >> 8) & 0xff) * inv + ((fg >> 8) & 0xff) * a) / 255;
    let b = ((bg & 0xff) * inv + (fg & 0xff) * a) / 255;
    (r << 16) | (g << 8) | b
}

#[cfg(test)]
pub(crate) fn apply_drop_overlay(pixels: &mut [u32], w: usize, h: usize, accent: u32) {
    apply_drop_overlay_at(pixels, w, h, 0, 0, w, h, accent);
}

#[cfg(test)]
#[allow(
    clippy::too_many_arguments,
    reason = "a raw surface + a placed frame rect is irreducibly 7 geometry scalars; bundling them into a struct only relocates the list"
)]
pub(crate) fn apply_drop_overlay_at(
    pixels: &mut [u32],
    sw: usize,
    sh: usize,
    ox: i64,
    oy: i64,
    fw: usize,
    fh: usize,
    accent: u32,
) {
    apply_overlay_at(
        pixels,
        sw,
        sh,
        ox,
        oy,
        fw,
        fh,
        OverlayGlow {
            accent,
            wash_a: DROP_WASH_ALPHA as u8,
            border_a: DROP_BORDER_ALPHA as u8,
            border_scale_q4: 0,
        },
    );
}

/// The alpha-parametrized core of the inset-accent-border overlay (the drop target's fixed alphas or the upgrade rim's pulsing ones).
#[allow(
    clippy::too_many_arguments,
    reason = "a raw surface + a placed frame rect is irreducibly 7 geometry scalars plus the glow; bundling them into a struct only relocates the list"
)]
pub(crate) fn apply_overlay_at(
    pixels: &mut [u32],
    sw: usize,
    sh: usize,
    ox: i64,
    oy: i64,
    fw: usize,
    fh: usize,
    glow: OverlayGlow,
) {
    if fw == 0 || fh == 0 || sw == 0 || sh == 0 {
        return;
    }
    let border = scaled_border_px(drop_border_px(fw, fh), glow.border_scale_q4)
        .min(fw / 2)
        .min(fh / 2)
        .max(1);
    let accent = glow.accent & 0x00ff_ffff;
    let (wash_a, border_a) = (u32::from(glow.wash_a), u32::from(glow.border_a));
    let y0 = ox_clamp(oy, sh);
    let y1 = (oy + fh as i64).clamp(0, sh as i64) as usize;
    let x0 = ox_clamp(ox, sw);
    let x1 = (ox + fw as i64).clamp(0, sw as i64) as usize;
    for y in y0..y1 {
        let fy = (y as i64 - oy) as usize; // frame-local row
        let edge_row = fy < border || fy >= fh - border;
        let Some(row) = pixels.get_mut(y * sw + x0..y * sw + x1) else {
            break;
        };
        for (i, px) in row.iter_mut().enumerate() {
            let fx = (x0 + i) as i64 - ox; // frame-local column
            let fx = fx as usize;
            let on_border = edge_row || fx < border || fx >= fw - border;
            let a = if on_border { border_a } else { wash_a };
            let transmittance = *px & 0xff00_0000;
            *px = transmittance | blend_rgb(*px & 0x00ff_ffff, accent, a);
        }
    }
}

fn ox_clamp(off: i64, len: usize) -> usize {
    off.clamp(0, len as i64) as usize
}

#[cfg(test)]
pub(crate) fn composite_tray(
    pixels: &mut [u32],
    fb_w: usize,
    fb_h: usize,
    card: &crate::SettingsCard,
) {
    composite_tray_at(pixels, fb_w, fb_h, 0, 0, card);
}

/// Project one cached tray card into the portion at or below `floor_y`.
pub(crate) fn tray_quad_below_y(
    card: &crate::SettingsCard,
    floor_y: u32,
) -> Option<aterm_gpu::TrayQuad<'_>> {
    if card.pw == 0 || card.ph == 0 {
        return None;
    }
    let skipped_rows = floor_y.saturating_sub(card.dy).min(card.ph);
    if skipped_rows == card.ph {
        return None;
    }
    let stride = usize::try_from(card.pw).ok()?.checked_mul(4)?;
    let start = usize::try_from(skipped_rows).ok()?.checked_mul(stride)?;
    let remaining_rows = usize::try_from(card.ph - skipped_rows).ok()?;
    let end = start.checked_add(remaining_rows.checked_mul(stride)?)?;
    Some(aterm_gpu::TrayQuad {
        rgba: card.rgba.get(start..end)?,
        pw: card.pw,
        ph: card.ph - skipped_rows,
        dx: card.dx,
        dy: card.dy.saturating_add(skipped_rows),
        scrim: card
            .scrim
            .map(|rgba| aterm_gpu::TrayScrim { rgba, top: floor_y }),
    })
}

#[cfg(test)]
pub(crate) fn composite_tray_at(
    pixels: &mut [u32],
    fb_w: usize,
    fb_h: usize,
    ox: i64,
    oy: i64,
    card: &crate::SettingsCard,
) {
    let Some(card) = tray_quad_below_y(card, 0) else {
        return;
    };
    composite_tray_quad_at(pixels, fb_w, fb_h, ox, oy, card);
}

/// Raw-quad twin used after [`tray_quad_below_y`] has cropped a cached card.
pub(crate) fn composite_tray_quad_at(
    pixels: &mut [u32],
    fb_w: usize,
    fb_h: usize,
    ox: i64,
    oy: i64,
    card: aterm_gpu::TrayQuad<'_>,
) {
    if let Some(scrim) = card.scrim {
        composite_tray_scrim_at(pixels, fb_w, fb_h, oy, scrim);
    }
    let (cw, ch) = (card.pw as usize, card.ph as usize);
    for ty in 0..ch {
        let py = i64::from(card.dy) + ty as i64 + oy;
        if py < 0 {
            continue;
        }
        let py = py as usize;
        if py >= fb_h {
            break;
        }
        let base = py * fb_w;
        for tx in 0..cw {
            let px = i64::from(card.dx) + tx as i64 + ox;
            if px < 0 || px >= fb_w as i64 {
                continue;
            }
            let px = px as usize;
            let i = (ty * cw + tx) * 4;
            let a = u32::from(card.rgba[i + 3]);
            if a == 0 {
                continue;
            }
            let Some(slot) = pixels.get_mut(base + px) else {
                continue;
            };
            let (sr, sg, sb) = (
                u32::from(card.rgba[i]),
                u32::from(card.rgba[i + 1]),
                u32::from(card.rgba[i + 2]),
            );
            let d = *slot;
            let (dr, dg, db) = ((d >> 16) & 0xff, (d >> 8) & 0xff, d & 0xff);
            let r = (sr * a + dr * (255 - a) + 127) / 255;
            let g = (sg * a + dg * (255 - a) + 127) / 255;
            let b = (sb * a + db * (255 - a) + 127) / 255;
            let dst_t = (d >> 24) & 0xff;
            let out_t = (dst_t * (255 - a) + 127) / 255;
            *slot = (out_t << 24) | (r << 16) | (g << 8) | b;
        }
    }
}

fn composite_tray_scrim_at(
    pixels: &mut [u32],
    fb_w: usize,
    fb_h: usize,
    oy: i64,
    scrim: aterm_gpu::TrayScrim,
) {
    let a = u32::from(scrim.rgba[3]);
    if a == 0 {
        return;
    }
    let first = (i64::from(scrim.top) + oy).clamp(0, fb_h as i64) as usize;
    let Some(rows) = pixels.get_mut(first * fb_w..(fb_h * fb_w).min(pixels.len())) else {
        return;
    };
    let lit = |s: u8| u32::from(s) * a + 127;
    let (sr, sg, sb) = (lit(scrim.rgba[0]), lit(scrim.rgba[1]), lit(scrim.rgba[2]));
    let keep = 255 - a;
    for slot in rows {
        let d = *slot;
        let (dr, dg, db) = ((d >> 16) & 0xff, (d >> 8) & 0xff, d & 0xff);
        let r = (sr + dr * keep) / 255;
        let g = (sg + dg * keep) / 255;
        let b = (sb + db * keep) / 255;
        let out_t = (((d >> 24) & 0xff) * keep + 127) / 255;
        *slot = (out_t << 24) | (r << 16) | (g << 8) | b;
    }
}

/// Canonical CPU/capture frontend order, matching the GPU present pipeline.
#[allow(
    clippy::too_many_arguments,
    reason = "the placed renderer frame and raw destination require two dimensions plus an origin"
)]
pub(crate) fn apply_host_chrome_at(
    pixels: &mut [u32],
    surface_width: usize,
    surface_height: usize,
    origin_x: i64,
    origin_y: i64,
    frame_width: usize,
    frame_height: usize,
    edge_rows: std::ops::Range<usize>,
    tray: Option<aterm_gpu::TrayQuad<'_>>,
    invert: bool,
    overlay: Option<OverlayGlow>,
) {
    if let Some(tray) = tray {
        composite_tray_quad_at(
            pixels,
            surface_width,
            surface_height,
            origin_x,
            origin_y,
            tray,
        );
        aterm_render::continue_chrome_edges(
            pixels,
            surface_width,
            surface_height,
            frame_width,
            frame_height,
            edge_rows,
        );
    }
    apply_bell_invert_at(
        pixels,
        PlacedFrame {
            surface_width,
            surface_height,
            origin_x,
            origin_y,
            frame_width,
            frame_height,
        },
        invert,
    );
    if let Some(overlay) = overlay {
        apply_overlay_at(
            pixels,
            surface_width,
            surface_height,
            origin_x,
            origin_y,
            frame_width,
            frame_height,
            overlay,
        );
    }
}

#[allow(
    clippy::items_after_test_module,
    reason = "these unit tests sit next to the drop-overlay helpers they cover; the rest of the file is the App render inherent-impl, not stray items"
)]
#[cfg(test)]
#[path = "app_render_drop_overlay_tests.rs"]
mod drop_overlay_tests;

#[allow(
    clippy::too_many_arguments,
    reason = "pure pixel->cell geometry over independent scalar inputs (x, y, dims, pad, pad_top, head, strip rows); a struct would not clarify the mapping"
)]
pub(crate) fn pixel_to_term_cell(
    x: f64,
    y: f64,
    cw: usize,
    ch: usize,
    rows: u16,
    cols: u16,
    strip_rows: u16,
    pad: usize,
    pad_top: usize,
    head: usize,
) -> (u16, u16) {
    let gx = (x as usize).saturating_sub(pad);
    let gy = (y as usize).saturating_sub(pad_top + head);
    let strip_px = strip_rows as usize * ch.max(1);
    let term_y = gy.saturating_sub(strip_px);
    let col = (gx / cw.max(1)).min(cols.saturating_sub(1) as usize) as u16;
    let row = (term_y / ch.max(1)).min(rows.saturating_sub(1) as usize) as u16;
    (row, col)
}

/// Pure "is this pixel in the tab strip, and if so which strip column?" (the body of [`App::strip_col_at`]).
#[allow(
    clippy::too_many_arguments,
    reason = "pure pixel->strip-column geometry over independent scalar inputs (x, y, dims, pad, pad_top, head, strip rows); a struct would not clarify the mapping"
)]
pub(crate) fn strip_col_for_pixel(
    x: f64,
    y: f64,
    cw: usize,
    ch: usize,
    cols: u16,
    strip_rows: u16,
    pad: usize,
    pad_top: usize,
    head: usize,
) -> Option<u16> {
    let gx = (x as usize).saturating_sub(pad);
    let gy = (y as usize).saturating_sub(pad_top + head);
    let strip_px = strip_rows as usize * ch.max(1);
    if gy >= strip_px {
        return None;
    }
    Some((gx / cw.max(1)).min(cols.saturating_sub(1) as usize) as u16)
}

/// Selection-drag autoscroll.
pub(crate) fn selection_autoscroll_lines(
    y: f64,
    pad: usize,
    strip_px: usize,
    ch: usize,
    rows: u16,
) -> i32 {
    let ch = ch.max(1);
    let top = (pad + strip_px) as f64; // first device pixel of terminal row 0
    let bottom = top + (rows as usize * ch) as f64; // one past the last terminal row
    if y < top {
        // Above the top edge → scroll into history.
        autoscroll_step(top - y, ch)
    } else if y >= bottom {
        // Below the bottom edge → scroll toward the live bottom (negative offset).
        -autoscroll_step(y - bottom, ch)
    } else {
        0
    }
}

fn autoscroll_step(overshoot: f64, ch: usize) -> i32 {
    (overshoot / ch.max(1) as f64 + 1.0).min(i32::MAX as f64) as i32
}

/// Shift the composed frame `dst` down by `strip_rows.len()` rows and prepend those tab-strip rows, keeping every per-row vector aligned and moving the cursor and row count with the content.
pub(crate) fn prepend_strip_rows(
    dst: &mut RenderInput,
    strip_rows: &[Vec<RenderCell>],
    cell_h: usize,
    grid_top: usize,
    pool: &mut Vec<Vec<RenderCell>>,
) {
    prepend_strip_row_slices(
        dst,
        strip_rows.len(),
        strip_rows.iter().map(Vec::as_slice),
        cell_h,
        grid_top,
        pool,
    );
}

/// The chrome bleed's per-row gutter tones for the message band's metered rows (`aterm_render::band`).
pub(crate) use aterm_render::band::band_row_edges;

/// [`prepend_strip_rows`] over borrowed rows.
pub(crate) fn prepend_strip_row_slices<'a>(
    dst: &mut RenderInput,
    strip: usize,
    strip_rows: impl Iterator<Item = &'a [RenderCell]>,
    cell_h: usize,
    grid_top: usize,
    pool: &mut Vec<Vec<RenderCell>>,
) {
    dst.prepend_host_rows(
        strip,
        strip_rows,
        cell_h,
        grid_top,
        pool,
        aterm_core::render::HostRowPixels::Placed,
    );
}

fn narrow_clip_below_band(
    clip: aterm_core::render::SelectionClip,
    rows: &std::ops::Range<usize>,
) -> Option<aterm_core::render::SelectionClip> {
    let band_covers_top = rows.start <= clip.row_start && rows.end > clip.row_start;
    let band_covers_bottom = rows.start < clip.row_end && rows.end >= clip.row_end;
    if band_covers_top && band_covers_bottom {
        return None;
    }
    let narrowed = if band_covers_top {
        aterm_core::render::SelectionClip::new(rows.end, clip.row_end, clip.col_start, clip.col_end)
    } else if band_covers_bottom {
        aterm_core::render::SelectionClip::new(
            clip.row_start,
            rows.start,
            clip.col_start,
            clip.col_end,
        )
    } else {
        // Strictly interior — not expressible as one rectangle.
        return None;
    };
    (narrowed.row_start < narrowed.row_end).then_some(narrowed)
}

fn claim_chrome_rows(ws: &mut crate::WindowState, band: std::ops::Range<usize>) {
    let strip = ws
        .input_scratch
        .cells
        .len()
        .saturating_sub(usize::from(ws.rows));
    let band = band.start.saturating_sub(strip)..band.end.saturating_sub(strip);
    if !band.is_empty() {
        ws.chrome_rows.push(band);
    }
}

fn chrome_owns_terminal_row_of(ws: &crate::WindowState, row: usize) -> bool {
    ws.chrome_rows.iter().any(|band| band.contains(&row))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FindRegion {
    row0: usize,
    col0: usize,
    rows: usize,
    cols: usize,
    display_offset: i32,
}

fn find_rail(
    row: u16,
    span: Range<usize>,
    ink: [u8; 3],
    rail_h: u16,
) -> aterm_render::ChromeRaster {
    let ink = aterm_render::rgb_to_u32(ink);
    let keep = aterm_render::ChromeRaster::KEEP;
    aterm_render::ChromeRaster {
        row,
        ground: Arc::from(Vec::new()),
        rail: (0..span.end)
            .map(|x| if x < span.start { keep } else { ink })
            .collect(),
        rail_h,
        clear_rail: true,
        own: Vec::new(),
        split: None,
        rings: Vec::new(),
        icons: Vec::new(),
    }
}

fn unlifted_rail_h(want: u16, room: (usize, usize)) -> Option<u16> {
    (1..=want)
        .rev()
        .find(|&h| aterm_render::chrome_fit(&find_rail(0, 0..1, [0; 3], h), room).0 == 0)
}

fn clear_find_row_lists(input: &mut RenderInput, frame_row: usize) {
    if let Some(c) = input.clusters.get_mut(frame_row) {
        c.clear();
    }
    if let Some(c) = input.combining.get_mut(frame_row) {
        c.clear();
    }
    if let Some(m) = input.visual_to_logical.get_mut(frame_row) {
        m.clear();
    }
    if let Some(im) = input.images.get_mut(frame_row) {
        im.clear();
    }
    if let Some(ls) = input.line_sizes.get_mut(frame_row) {
        *ls = aterm_core::grid::LineSize::SingleWidth;
    }
    if let Some(spans) = input.line_size_spans.get_mut(frame_row) {
        spans.clear();
    }
    if let Some(spans) = input.default_bg_spans.get_mut(frame_row) {
        spans.clear();
    }
}

fn splice_find_row_span(
    input: &mut RenderInput,
    frame_row: usize,
    col0: usize,
    built: Vec<RenderCell>,
) {
    let span = col0..col0 + built.len();
    let Some(row) = input.cells.get_mut(frame_row) else {
        return;
    };
    for (col, cell) in span.clone().zip(built) {
        if let Some(slot) = row.get_mut(col) {
            *slot = cell;
        }
    }
    if let Some(c) = input.clusters.get_mut(frame_row) {
        c.retain(|(col, _)| !span.contains(col));
    }
    if let Some(c) = input.combining.get_mut(frame_row) {
        c.retain(|(col, _)| !span.contains(col));
    }
    if let Some(im) = input.images.get_mut(frame_row) {
        im.retain(|(col, _)| !span.contains(col));
    }
    if let Some(map) = input.visual_to_logical.get_mut(frame_row) {
        for v in span.clone() {
            if let Some(slot) = map.get_mut(v) {
                *slot = u16::try_from(v).unwrap_or(u16::MAX);
            }
        }
    }
    if let Some(spans) = input.line_size_spans.get_mut(frame_row) {
        let mut kept = Vec::with_capacity(spans.len() + 1);
        for run in spans.drain(..) {
            if run.start_col < span.start {
                kept.push(aterm_core::render::LineSizeSpan::new(
                    run.start_col,
                    run.end_col.min(span.start),
                    run.line_size,
                ));
            }
            if run.end_col > span.end {
                kept.push(aterm_core::render::LineSizeSpan::new(
                    run.start_col.max(span.end),
                    run.end_col,
                    run.line_size,
                ));
            }
        }
        *spans = kept;
        if spans.is_empty()
            && let Some(ls) = input.line_sizes.get_mut(frame_row)
        {
            *ls = aterm_core::grid::LineSize::SingleWidth;
        }
    }
}

fn scrub_overlay_row_band(
    input: &mut RenderInput,
    rows: std::ops::Range<usize>,
    bands: aterm_render::RowBands,
) {
    if rows.start >= rows.end {
        return;
    }
    let cell_h = bands.cell_h;
    let (oy0, oy1) = bands.rows_px(rows.clone());
    input.fx_occluders.push(aterm_core::render::PxRect::new(
        i32::MIN,
        oy0,
        i32::MAX,
        oy1,
    ));
    input
        .chrome_rasters
        .retain(|raster| !rows.contains(&usize::from(raster.row)));
    let start16 = u16::try_from(rows.start).unwrap_or(u16::MAX);
    let end16 = u16::try_from(rows.end).unwrap_or(u16::MAX);
    let tagged = |row: u16| row >= start16 && row < end16;
    let y0 = i64::try_from(rows.start.saturating_mul(cell_h)).unwrap_or(i64::MAX);
    let y1 = i64::try_from(rows.end.saturating_mul(cell_h)).unwrap_or(i64::MAX);
    let cell_h_i64 = i64::try_from(cell_h).unwrap_or(0);

    if input.cursor_visible && rows.contains(&input.cursor_row) {
        input.cursor_visible = false;
    }
    let mut scalar_intersects = false;
    let mut entry_intersects = vec![false; input.selections.len()];
    if input.any_selection() {
        let mut undecided = input
            .selections
            .iter()
            .filter(|entry| entry.clip.row_start < rows.end && entry.clip.row_end > rows.start)
            .count();
        if input.selections.is_empty() {
            undecided = 1;
        }
        if undecided > 0 {
            let cols = input.cols.min(usize::from(u16::MAX) + 1);
            'band: for frame_row in rows.clone() {
                let row_cells = input.cells.get(frame_row).map(Vec::as_slice).unwrap_or(&[]);
                for col in 0..cols {
                    let is_wide_lead = row_cells.get(col + 1).is_some_and(|cell| cell.wide);
                    let is_wide_continuation = row_cells.get(col).is_some_and(|cell| cell.wide);
                    let Some(hit) =
                        input.selection_hit(frame_row, col, is_wide_lead, is_wide_continuation)
                    else {
                        continue;
                    };
                    let first = match entry_intersects.get_mut(hit) {
                        Some(slot) => !std::mem::replace(slot, true),
                        None => !std::mem::replace(&mut scalar_intersects, true),
                    };
                    if first {
                        undecided -= 1;
                        if undecided == 0 {
                            break 'band;
                        }
                    }
                }
            }
        }
    }
    if scalar_intersects {
        let clip = input.selection_clip.unwrap_or_else(|| {
            aterm_core::render::SelectionClip::new(0, input.rows, 0, input.cols)
        });
        match narrow_clip_below_band(clip, &rows) {
            Some(clip) => input.selection_clip = Some(clip),
            None => {
                input.selection = aterm_core::selection::TextSelection::new();
                input.selection_clip = None;
            }
        }
    }
    if entry_intersects.iter().any(|&hit| hit) {
        let mut kept = 0;
        for (i, &intersects) in entry_intersects.iter().enumerate() {
            if intersects {
                match narrow_clip_below_band(input.selections[i].clip, &rows) {
                    Some(clip) => input.selections[i].clip = clip,
                    None => continue,
                }
            }
            input.selections.swap(kept, i);
            kept += 1;
        }
        input.selections.truncate(kept);
        if kept == 0 {
            input.selection = aterm_core::selection::TextSelection::new();
            input.selection_clip = None;
        }
    }
    input
        .cursor_trail
        .retain(|trail| !rows.contains(&trail.row));
    input.char_fg.retain(|cell| !tagged(cell.row));
    input.fire_halo.retain(|cell| !tagged(cell.row));
    input.cursor_glow_add.retain(|quad| !tagged(quad.row));
    input.glow_under.retain(|quad| !tagged(quad.row));
    input.fire_patch.retain(|quad| !tagged(quad.row));
    input.glow_halo.retain(|quad| !tagged(quad.row));
    input.ink.retain(|cell| !tagged(cell.row));
    input.word_decorations.retain(|decoration| {
        let top = i64::from(decoration.row)
            .saturating_mul(cell_h_i64)
            .saturating_add(i64::from(decoration.dy));
        let bottom = top.saturating_add(cell_h_i64);
        bottom <= y0 || top >= y1
    });
    input.nova_add.retain(|quad| !tagged(quad.row));
    input.cat_quads.retain(|quad| !tagged(quad.row));
    input.rain_quads.retain(|quad| !tagged(quad.row));
    input.rain_add.retain(|quad| !tagged(quad.row));
    input.free_sprites.retain(|sprite| {
        let top = i64::from(sprite.y);
        let bottom = top.saturating_add(i64::from(sprite.h));
        bottom <= y0 || top >= y1
    });

    if input.cat_quads.is_empty() {
        input.cat_atlas = None;
    }
    if input.free_sprites.is_empty() {
        input.free_atlas = None;
    }
    if input.rain_quads.is_empty() {
        input.rain_atlas = None;
    }
}

/// The card is on top: a menu card the host painted into the frame's cells at `card` (frame cells, row 0 the strip's first row), after the overlays were published, takes its cells back from every overlay that would paint over them.
pub(crate) fn scrub_overlay_card(
    input: &mut RenderInput,
    card: crate::tab_menu::MenuRect,
    (cell_w, cell_h): (usize, usize),
    (pad, grid_top): (usize, usize),
) {
    if card.w == 0 || card.h == 0 {
        return;
    }
    let edge = |origin: usize, cells: usize, size: usize| {
        i32::try_from(origin.saturating_add(cells.saturating_mul(size)))
            .unwrap_or(i32::MAX - 1)
            .min(i32::MAX - 1)
    };
    input.fx_occluders.push(aterm_core::render::PxRect::new(
        edge(pad, card.col, cell_w),
        edge(grid_top, card.row, cell_h),
        edge(pad, card.col + card.w, cell_w),
        edge(grid_top, card.row + card.h, cell_h),
    ));
    let rows = card.row..card.row + card.h;
    let cols = card.col..card.col + card.w;
    let on_card = |row: usize, col: usize| rows.contains(&row) && cols.contains(&col);
    if input.cursor_visible {
        let wide = input
            .cells
            .get(input.cursor_row)
            .and_then(|row| row.get(input.cursor_col + 1))
            .is_some_and(|next| next.wide);
        if on_card(input.cursor_row, input.cursor_col)
            || (wide && on_card(input.cursor_row, input.cursor_col + 1))
        {
            input.cursor_visible = false;
        }
    }
    clip_selections_off_card(input, card);
    cut_chrome_rasters(&mut input.chrome_rasters, &input.cells, card);
    let tagged = |row: u16, col: u16| on_card(usize::from(row), usize::from(col));
    input.cursor_trail.retain(|t| !on_card(t.row, t.col));
    input.char_fg.retain(|c| !tagged(c.row, c.col));
    input.fire_halo.retain(|c| !tagged(c.row, c.col));
    input.ink.retain(|c| !tagged(c.row, c.col));
    let px =
        |cells: usize, size: usize| i64::try_from(cells.saturating_mul(size)).unwrap_or(i64::MAX);
    let (x0, x1) = (px(card.col, cell_w), px(card.col + card.w, cell_w));
    let (y0, y1) = (px(card.row, cell_h), px(card.row + card.h, cell_h));
    let line_sizes = &input.line_sizes;
    input.word_decorations.retain(|d| {
        let size = line_sizes
            .get(usize::from(d.row))
            .copied()
            .unwrap_or_default();
        let w = aterm_render::row_cell_w(size, cell_w);
        let left = px(usize::from(d.col), w).saturating_add(i64::from(d.dx));
        let top = px(usize::from(d.row), cell_h).saturating_add(i64::from(d.dy));
        let (right, bottom) = (
            left.saturating_add(px(1, w)),
            top.saturating_add(px(1, cell_h)),
        );
        right <= x0 || left >= x1 || bottom <= y0 || top >= y1
    });
    let pad = i64::try_from(pad).unwrap_or(0);
    let meets_x = |x: u16, w: u16, shift: i64| {
        let left = i64::from(x) - shift;
        left < x1 && left + i64::from(w) > x0
    };
    let tag_in = |row: u16| rows.contains(&usize::from(row));
    input
        .cursor_glow_add
        .retain(|q| !(tag_in(q.row) && meets_x(q.x, q.w, pad)));
    input
        .glow_under
        .retain(|q| !(tag_in(q.row) && meets_x(q.x, q.w, pad)));
    input
        .fire_patch
        .retain(|q| !(tag_in(q.row) && meets_x(q.x, q.w, pad)));
    input
        .glow_halo
        .retain(|q| !(tag_in(q.row) && meets_x(q.x, q.w, pad)));
    input
        .nova_add
        .retain(|q| !(tag_in(q.row) && meets_x(q.x, q.w, 0)));
    input
        .rain_add
        .retain(|q| !(tag_in(q.row) && meets_x(q.x, q.w, 0)));
    let card_px = (x0, y0, x1, y1);
    cut_sprite_quads(&mut input.cat_quads, card_px);
    cut_sprite_quads(&mut input.rain_quads, card_px);
    let mut kept = Vec::with_capacity(input.free_sprites.len());
    for sprite in input.free_sprites.drain(..) {
        cut_free_sprite(sprite, card_px, &mut kept);
    }
    input.free_sprites = kept;
    if input.cat_quads.is_empty() {
        input.cat_atlas = None;
    }
    if input.free_sprites.is_empty() {
        input.free_atlas = None;
    }
    if input.rain_quads.is_empty() {
        input.rain_atlas = None;
    }
}

/// [`scrub_overlay_card`]'s chrome rule, run once the card's cells are in
/// `cells`: a raster on the card's rows loses what spans the row (ground,
/// rail, rings, the ink split and the glyph lift, which cannot be cut at the
/// card's edge) and keeps the drawn icons it carries beside the card, which
/// are per cell; an icon on the card's cells goes, and so does one that would
/// spill into a blank cell of the card (`aterm_render::chrome_icon_spills`,
/// the renderers' own rule: a band's badge between blanks, never a glass mark,
/// which keeps to its own cell). A raster left with nothing to draw goes. A
/// raster off the card's rows is untouched.
fn cut_chrome_rasters(
    rasters: &mut Vec<aterm_render::ChromeRaster>,
    cells: &[Vec<RenderCell>],
    card: crate::tab_menu::MenuRect,
) {
    let rows = card.row..card.row + card.h;
    let cols = card.col..card.col + card.w;
    rasters.retain_mut(|raster| {
        let row = usize::from(raster.row);
        if !rows.contains(&row) {
            return true;
        }
        let line = cells.get(row).map_or(&[][..], Vec::as_slice);
        raster.icons.retain(|icon| {
            let c = usize::from(icon.col);
            let meets = |n: Option<usize>| n.is_some_and(|n| cols.contains(&n));
            !cols.contains(&c)
                && !(aterm_render::chrome_icon_spills(icon.icon, line, c)
                    && (meets(c.checked_sub(1)) || meets(c.checked_add(1))))
        });
        if raster.icons.is_empty() {
            return false;
        }
        if !raster.ground.is_empty() {
            raster.ground = Arc::from([]);
        }
        if !raster.rail.is_empty() {
            raster.rail = Arc::from([]);
        }
        raster.rail_h = 0;
        raster.clear_rail = false;
        raster.own.clear();
        raster.split = None;
        raster.rings.clear();
        true
    });
}

fn clip_selections_off_card(input: &mut RenderInput, card: crate::tab_menu::MenuRect) {
    use aterm_core::render::SelectionClip;
    if !input.any_selection() {
        return;
    }
    let (rows, cols) = (input.cells.len().min(input.rows), input.cols);
    let wide = |input: &RenderInput, row: usize, col: usize| {
        let cells = input.cells.get(row).map_or(&[][..], Vec::as_slice);
        (
            cells.get(col + 1).is_some_and(|c| c.wide),
            cells.get(col).is_some_and(|c| c.wide),
        )
    };
    let hit = |input: &RenderInput, row: usize, col: usize| {
        let (lead, cont) = wide(input, row, col);
        input.selection_hit(row, col, lead, cont)
    };
    let scalar = input.selections.is_empty();
    let slots = if scalar { 1 } else { input.selections.len() };
    let mut covered = vec![false; slots];
    for row in card.row..(card.row + card.h).min(rows) {
        for col in card.col..(card.col + card.w).min(cols) {
            if let Some(slot) = hit(input, row, col).and_then(|e| covered.get_mut(e)) {
                *slot = true;
            }
        }
    }
    if !covered.contains(&true) {
        return;
    }
    let whole = SelectionClip::new(0, input.rows, 0, input.cols);
    let clip_of = |input: &RenderInput, e: usize| {
        if scalar {
            input.selection_clip.unwrap_or(whole)
        } else {
            input.selections[e].clip
        }
    };
    let (card_bottom, card_right) = (card.row + card.h, card.col + card.w);
    let sides = |c: SelectionClip| {
        [
            SelectionClip::new(c.row_start, c.row_end.min(card.row), c.col_start, c.col_end),
            SelectionClip::new(
                c.row_start.max(card_bottom),
                c.row_end,
                c.col_start,
                c.col_end,
            ),
            SelectionClip::new(c.row_start, c.row_end, c.col_start, c.col_end.min(card.col)),
            SelectionClip::new(
                c.row_start,
                c.row_end,
                c.col_start.max(card_right),
                c.col_end,
            ),
        ]
    };
    let mut kept = vec![[0usize; 4]; slots];
    for row in 0..rows {
        for col in 0..cols {
            let Some(e) = hit(input, row, col).filter(|&e| covered.get(e) == Some(&true)) else {
                continue;
            };
            for (n, side) in sides(clip_of(input, e)).iter().enumerate() {
                if side.contains(row, col) {
                    kept[e][n] += 1;
                }
            }
        }
    }
    let narrowed: Vec<Option<SelectionClip>> = (0..slots)
        .map(|e| {
            let sides = sides(clip_of(input, e));
            let best = (0..4).max_by_key(|&n| (kept[e][n], std::cmp::Reverse(n)))?;
            (kept[e][best] > 0).then_some(sides[best])
        })
        .collect();
    if scalar {
        match narrowed[0] {
            Some(clip) => input.selection_clip = Some(clip),
            None => {
                input.selection = aterm_core::selection::TextSelection::new();
                input.selection_clip = None;
            }
        }
        return;
    }
    let mut at = 0;
    input.selections.retain_mut(|entry| {
        let e = at;
        at += 1;
        if !covered[e] {
            return true;
        }
        match narrowed[e] {
            Some(clip) => {
                entry.clip = clip;
                true
            }
            None => false,
        }
    });
    if input.selections.is_empty() {
        input.selection = aterm_core::selection::TextSelection::new();
        input.selection_clip = None;
    }
}

fn off_card_parts(
    (x, y, w, h): (i64, i64, i64, i64),
    (x0, y0, x1, y1): (i64, i64, i64, i64),
) -> Vec<(i64, i64, i64, i64)> {
    let (right, bottom) = (x + w, y + h);
    if w <= 0 || h <= 0 || right <= x0 || x >= x1 || bottom <= y0 || y >= y1 {
        return vec![(x, y, w, h)];
    }
    let mut parts = Vec::with_capacity(4);
    if y < y0 {
        parts.push((x, y, w, y0 - y));
    }
    if bottom > y1 {
        parts.push((x, y1, w, bottom - y1));
    }
    let (mid0, mid1) = (y.max(y0), bottom.min(y1));
    if x < x0 {
        parts.push((x, mid0, x0 - x, mid1 - mid0));
    }
    if right > x1 {
        parts.push((x1, mid0, right - x1, mid1 - mid0));
    }
    parts
}

fn part_ax(ax: u16, aw: u16, offset: i64, part_w: i64, flip_x: bool) -> Option<u16> {
    let start = if flip_x {
        i64::from(ax) + i64::from(aw) - offset - part_w
    } else {
        i64::from(ax) + offset
    };
    u16::try_from(start).ok()
}

fn cut_free_sprite(
    sprite: aterm_core::render::FreeSprite,
    card: (i64, i64, i64, i64),
    out: &mut Vec<aterm_core::render::FreeSprite>,
) {
    let rect = (
        i64::from(sprite.x),
        i64::from(sprite.y),
        i64::from(sprite.w),
        i64::from(sprite.h),
    );
    let parts = off_card_parts(rect, card);
    if parts == [rect] {
        out.push(sprite);
        return;
    }
    if sprite.w != sprite.aw || sprite.h != sprite.ah {
        return;
    }
    for (x, y, w, h) in parts {
        let (Ok(nx), Ok(ny), Ok(nw), Ok(nh)) = (
            i32::try_from(x),
            i32::try_from(y),
            u16::try_from(w),
            u16::try_from(h),
        ) else {
            continue;
        };
        let Some(ax) = part_ax(sprite.ax, sprite.aw, x - rect.0, w, sprite.flip_x) else {
            continue;
        };
        let Ok(ay) = u16::try_from(i64::from(sprite.ay) + (y - rect.1)) else {
            continue;
        };
        out.push(aterm_core::render::FreeSprite {
            x: nx,
            y: ny,
            w: nw,
            h: nh,
            ax,
            ay,
            aw: nw,
            ah: nh,
            ..sprite
        });
    }
}

fn cut_sprite_quads(quads: &mut Vec<aterm_core::render::SpriteQuad>, card: (i64, i64, i64, i64)) {
    if quads.is_empty() {
        return;
    }
    let mut kept = Vec::with_capacity(quads.len());
    for q in quads.drain(..) {
        let rect = (
            i64::from(q.x),
            i64::from(q.y),
            i64::from(q.w),
            i64::from(q.h),
        );
        let parts = off_card_parts(rect, card);
        if parts == [rect] {
            kept.push(q);
            continue;
        }
        if q.w != q.aw || q.h != q.ah {
            continue;
        }
        for (x, y, w, h) in parts {
            let (Ok(nx), Ok(ny), Ok(nw), Ok(nh)) = (
                u16::try_from(x),
                u16::try_from(y),
                u16::try_from(w),
                u16::try_from(h),
            ) else {
                continue;
            };
            let Some(ax) = part_ax(q.ax, q.aw, x - rect.0, w, q.flip_x) else {
                continue;
            };
            let Ok(ay) = u16::try_from(i64::from(q.ay) + (y - rect.1)) else {
                continue;
            };
            kept.push(aterm_core::render::SpriteQuad {
                x: nx,
                y: ny,
                w: nw,
                h: nh,
                ax,
                ay,
                aw: nw,
                ah: nh,
                ..q
            });
        }
    }
    *quads = kept;
}

#[cfg(test)]
mod overlay_scrub_selection_tests {
    use super::*;
    use aterm_core::selection::{SelectionSide, SelectionType};

    #[test]
    fn scrub_detects_block_selection_snapped_onto_wide_lead() {
        let blank = crate::chrome_band::blank_cell(aterm_render::Theme::default());
        let mut continuation = blank;
        continuation.wide = true;

        let mut input = RenderInput::empty();
        input.rows = 1;
        input.cols = 1;
        input.cells = vec![vec![blank, continuation]];
        input
            .selection
            .start_selection(0, 1, SelectionSide::Left, SelectionType::Block);
        input.selection.update_selection(0, 1, SelectionSide::Right);
        input.selection.complete_selection();
        assert!(
            input.selection_contains_cell(0, 0, true, false),
            "negative-control precondition: wide snapping reaches the visible lead"
        );
        assert!(
            !input.selection_contains_cell(0, 0, false, false),
            "a scalar-only probe would miss this intersection"
        );

        scrub_overlay_row_band(&mut input, 0..1, aterm_render::RowBands::new(0, 16));

        assert!(
            !input.selection.has_selection(),
            "chrome replacement must scrub the same wide-aware selection the renderer paints"
        );
    }

    #[test]
    fn scrub_still_clears_overlays_when_selection_clip_is_disjoint_from_band() {
        let blank = crate::chrome_band::blank_cell(aterm_render::Theme::default());
        let mut input = RenderInput::empty();
        input.rows = 4;
        input.cols = 4;
        input.cells = vec![vec![blank; 4]; 4];

        let mut selection = aterm_core::selection::TextSelection::new();
        selection.start_selection(2, 0, SelectionSide::Left, SelectionType::Simple);
        selection.update_selection(2, 2, SelectionSide::Right);
        selection.complete_selection();
        input.selections.push(aterm_core::render::PaneSelection {
            selection,
            clip: aterm_core::render::SelectionClip::new(2, 4, 0, 4),
            bg: 0x334455,
            fg: 0xffffff,
            inactive: false,
        });

        input.cursor_trail.push(aterm_core::render::TrailCell {
            row: 0,
            col: 1,
            alpha: 255,
        });
        input.cursor_glow_add.push(aterm_core::render::GlowQuad {
            x: 0,
            y: 0,
            w: 8,
            h: 16,
            color: 0xffffff,
            alpha: 0,
            color2: 0xffffff,
            alpha2: 0,
            row: 0,
        });
        input.cat_quads.push(aterm_core::render::SpriteQuad {
            row: 0,
            x: 0,
            y: 0,
            w: 8,
            h: 16,
            ax: 0,
            ay: 0,
            aw: 8,
            ah: 16,
            tint: 0x00ff_ffff,
            alpha: 255,
            flip_x: false,
        });

        scrub_overlay_row_band(&mut input, 0..1, aterm_render::RowBands::new(0, 16));

        assert_eq!(
            input.selections.len(),
            1,
            "a selection in rows 2..4 is untouched by a top-row chrome band"
        );
        assert!(
            input.cursor_trail.is_empty()
                && input.cursor_glow_add.is_empty()
                && input.cat_quads.is_empty(),
            "overlay effects in the spliced band must still be scrubbed when selection clips are disjoint"
        );
    }
}

fn rain_refresh_needed(
    enabled: bool,
    suspended: bool,
    sync_hold: bool,
    display_offset: usize,
    engine: Option<&crate::matrix_rain::MatrixRain>,
    epoch: u64,
) -> bool {
    enabled
        && !suspended
        && !sync_hold
        && display_offset == 0
        && engine.is_none_or(|e| e.needs_rescan(epoch) || e.needs_material_sample())
}

fn effect_only_snapshot_reusable(
    uncontended: bool,
    scratch: &RenderInput,
    term: &Terminal,
    live_epoch: u64,
    rows: usize,
    cols: usize,
) -> bool {
    uncontended
        && !term.has_damage()
        && scratch.snapshot_seq == live_epoch
        && scratch.engine_fill_seq == scratch.snapshot_seq
        && scratch.process_sequence == term.pipeline_timestamps().process_sequence
        && scratch.terminal_id != 0
        && scratch.terminal_id == term.render_identity()
        && scratch.engine_alt == term.is_alternate_screen()
        && scratch.row_shift == 0
        && scratch.rows == rows
        && scratch.cols == cols
        && scratch.cells.len() == rows
        && rows == usize::from(term.rows())
        && cols == usize::from(term.cols())
        && i64::from(scratch.display_offset)
            == i64::try_from(term.grid().display_offset()).unwrap_or(i64::MAX)
        && scratch.base_y == i64::try_from(term.grid().base_y()).unwrap_or(i64::MAX)
        && scratch.absolute_row_revision == term.absolute_row_revision()
        && scratch.selection == *term.text_selection()
}

#[cfg(test)]
#[path = "app_render_effect_only_reuse_tests.rs"]
mod effect_only_reuse_tests;

pub(crate) fn update_rain_hidden_band(
    band: &mut Vec<u16>,
    dmg: &aterm_core::grid::damage::Damage,
    rows: usize,
) {
    if dmg.is_full() {
        return;
    }
    let rows = u16::try_from(rows).unwrap_or(u16::MAX);
    for r in dmg.damaged_rows(rows) {
        // Promote to most-recent-first; ascending iteration leaves the frame's
        // bottom-most damaged rows at the ring's head (a bottom-region bias).
        if let Some(p) = band.iter().position(|&x| x == r) {
            band.remove(p);
        }
        band.insert(0, r);
        band.truncate(crate::matrix_rain::HIDDEN_CURSOR_BAND_ROWS);
    }
}

pub(crate) use aterm_effects::pane::PanePlace;

const VERDICT_MIN_MS: u64 = 2_000;
const VERDICT_ICE_MS: u64 = 30_000;

/// The arm: `true` iff this `D` was run by a keystroke, judged from the window's spend latch and the session's last accepted plain-Enter turn boundary, and spent by the answer whatever it is.
pub(crate) fn verdict_armed(
    spent: &mut Option<(u64, std::time::Instant)>,
    session: u64,
    boundary: Option<std::time::Instant>,
) -> bool {
    let Some(at) = boundary else {
        return false;
    };
    if *spent == Some((session, at)) {
        return false;
    }
    *spent = Some((session, at));
    true
}

fn keyed_celebration_completion(
    sing: &mut aterm_effects::kitty_sing::KittySing,
    spent: &mut Option<(u64, std::time::Instant)>,
    session: u64,
    boundary: Option<std::time::Instant>,
    now: std::time::Instant,
    code: i32,
) -> bool {
    let keyed = verdict_armed(spent, session, boundary);
    if keyed && code == 0 {
        sing.note_green_block(now, session);
    }
    keyed
}

/// What the verdict does with one `D`, as a pure function of the facts the shell handed over:.
pub(crate) fn verdict_voice(failed: bool, dur_ms: Option<u64>) -> Option<(bool, bool)> {
    let dur = dur_ms.unwrap_or(0);
    if failed {
        return Some((true, false));
    }
    (dur >= VERDICT_MIN_MS).then_some((false, dur >= VERDICT_ICE_MS))
}

/// The resident pet's frame grid in the host contract's terms.
pub(crate) fn pet_frame_geom(
    cell: (usize, usize),
    grid: (usize, usize),
    origin_px: (i32, i32),
) -> aterm_effects::host::FrameGeom {
    let clamp = |v: usize| v.min(usize::from(u16::MAX)) as u16;
    aterm_effects::host::FrameGeom {
        rows: clamp(grid.0),
        cols: clamp(grid.1),
        cell_w: clamp(cell.0),
        cell_h: clamp(cell.1),
        origin_px,
    }
}

/// The window's presentability as the pet owner reads it.
pub(crate) const fn pet_visibility(focused: bool) -> aterm_effects::host::Visibility {
    if focused {
        aterm_effects::host::Visibility::Focused
    } else {
        aterm_effects::host::Visibility::VisibleUnfocused
    }
}

fn room_facts(
    session: Option<&crate::Session>,
    status: Option<&crate::session_status::Status>,
    sibling: Option<(f32, f32)>,
    landed: Option<u32>,
) -> aterm_effects::companion::RoomFacts {
    aterm_effects::companion::RoomFacts {
        quiet: status.is_some_and(|s| s.phase == crate::session_status::Phase::Quiet),
        sibling,
        session: session.map(|session| {
            let ctx = &session.ctx;
            let now_us = crate::metrics::now_us();
            let lease = ctx
                .turn_lease
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
                .is_some_and(|l| l.is_live(now_us));
            let turn = ctx
                .turns
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .newest_not_harness()
                .map_or((0, false), |r| (r.id, r.status == "settled"));
            let (unseen, overdue, hold) = ctx.fabric.room_facts(crate::turn_ledger::now_ms());
            aterm_effects::companion::SessionRoom {
                lease,
                turn,
                inbox: aterm_effects::kitty_pet::RoomInbox {
                    unseen,
                    overdue,
                    hold,
                },
                landed,
            }
        }),
    }
}

fn mark_end_cheered(
    glass: &mut std::collections::HashMap<u64, crate::harness_glass::SessionGlass>,
    seen: Option<(u64, u32)>,
) {
    if let Some((session, serial)) = seen
        && let Some(glass) = glass.get_mut(&session)
    {
        glass.cheer_end(serial);
    }
}

const fn sing_riff_event(bar: u64, gain: f32, sig: u32) -> aterm_effects::trail_sound::SoundEvent {
    aterm_effects::trail_sound::SoundEvent {
        style: crate::cursor_glow::GlowStyle::RainbowKitty,
        voice: aterm_effects::trail_sound::SoundVoice::Style,
        kind: aterm_effects::trail_sound::SoundGesture::Celebration(
            aterm_effects::trail_sound::CelebrationGesture::riff_bar((bar & 0xffff) as u16, sig),
        ),
        pan: 0.0,
        heat: 1.0,
        hue: 0.0,
        gain,
        tone: aterm_effects::tone::Tone::Technical,
        bed: false,
        shifted: false, // an authored riff bar, not a keystroke
    }
}

fn sing_outro_event(gain: f32, sig: u32) -> aterm_effects::trail_sound::SoundEvent {
    aterm_effects::trail_sound::SoundEvent {
        kind: aterm_effects::trail_sound::SoundGesture::Celebration(
            aterm_effects::trail_sound::CelebrationGesture::outro(sig),
        ),
        ..sing_riff_event(0, gain, sig)
    }
}

/// Whether the Sparkle Words engine may present (spend births rather than bank them).
#[must_use]
pub(crate) const fn sparkle_words_presentable(word_focus: bool, suspended: bool) -> bool {
    word_focus && !suspended
}

#[cfg(test)]
#[path = "app_render_pet_sing_swap_tests.rs"]
mod pet_sing_swap_tests;

#[derive(Clone, Copy)]
struct ComposedOutputStreakConfig {
    enabled: bool,
    intensity: f32,
    tail: u16,
    max_streaks: u8,
    idle_secs: f32,
    sound_gain: Option<f32>,
    style: crate::cursor_glow::GlowStyle,
    voice: aterm_effects::trail_sound::SoundVoice,
    animate: bool,
    now: Instant,
}

/// Whether a host route must drop Prism Wake before allocating an engine or inspecting frame cells.
#[must_use]
pub(crate) const fn output_streak_inert(
    enabled: bool,
    animate: bool,
    intensity: f32,
    suppressed: bool,
) -> bool {
    !enabled || !animate || intensity <= 0.0 || suppressed
}

impl ComposedOutputStreakConfig {
    const fn inert(self) -> bool {
        output_streak_inert(self.enabled, self.animate, self.intensity, false)
    }
}

#[derive(Clone, Copy)]
struct ComposedOutputStreakPane {
    session: u64,
    slot: usize,
    place: PanePlace,
    may_speak: bool,
    echo: crate::app_input::OutputEchoSample,
}

fn placed_output_streak_fp(
    pane_fp: u64,
    pane: ComposedOutputStreakPane,
    paint_index: usize,
) -> u64 {
    if pane_fp == 0 {
        return 0;
    }
    let mut placement = aterm_effects::genome::mix(pane.session);
    for value in [
        u64::from(pane.place.row_off),
        u64::from(pane.place.col_off),
        u64::from(pane.place.rows),
        u64::from(pane.place.cols),
    ] {
        placement = aterm_effects::genome::mix(placement ^ value);
    }
    aterm_effects::genome::mix(pane_fp ^ placement).rotate_left((paint_index % 64) as u32)
}

fn tick_composed_output_streak_pane(
    fx: &mut aterm_effects::pipeline::EffectsPipeline,
    into: aterm_effects::pipeline::NovaInto<'_>,
    snapshot: &RenderInput,
    content_seq: u64,
    pane: ComposedOutputStreakPane,
    resolved: ComposedOutputStreakConfig,
) -> (u64, Option<aterm_effects::trail_sound::SoundEvent>) {
    let speaks = pane.may_speak && resolved.sound_gain.is_some();
    let (fp, cue) = fx.streak_pane(
        &aterm_effects::pipeline::StreakPane {
            session: pane.session,
            slot: pane.slot,
            place: pane.place,
            echo: pane.echo.streak(),
            snapshot,
            content_seq,
            sound: speaks,
        },
        aterm_effects::pipeline::StreakKnobs {
            now: resolved.now,
            intensity: resolved.intensity,
            tail: resolved.tail,
            max_streaks: resolved.max_streaks,
            idle_secs: resolved.idle_secs,
        },
        into,
    );
    let sound = cue.and_then(|cue| {
        resolved
            .sound_gain
            .map(|gain| aterm_effects::trail_sound::SoundEvent {
                style: resolved.style,
                voice: resolved.voice,
                kind: aterm_effects::trail_sound::SoundGesture::Output(cue.sound.output_gesture()),
                pan: cue.pan,
                heat: 0.0,
                hue: cue.hue,
                gain,
                shifted: false,
                tone: aterm_effects::tone::Tone::Technical,
                bed: false,
            })
    });
    (fp, sound)
}

/// Everything the composed sparkle pass needs that `redraw_compose` already resolved.
pub(crate) struct ComposeDecoCtx<'a> {
    pub(crate) panes: &'a [(pane::PaneRect, Arc<Mutex<Terminal>>)],
    pub(crate) focus: u64,
    /// Canonical focused-view slot.
    pub(crate) focus_index: usize,
    pub(crate) win_rows: u16,
    pub(crate) win_cols: u16,
    pub(crate) cell_w: u32,
    pub(crate) cell_h: u32,
    /// The caret the cursor companions escort, in the focused pane's cells ([`aterm_effects::companion::PetTick::escort_caret`], the single-pane path's rule).
    pub(crate) escort_caret: Option<(u16, u16)>,
    /// The window focus the cursor effects fold.
    pub(crate) win_focused: bool,
    /// The unwoken focus (`App::motion_focus`: raw focus or the recording pin, no typed wake), the Sparkle Words presentability input ([`sparkle_words_presentable`]).
    pub(crate) word_focus: bool,
    /// Whether the motion policy still animates word sparkles.
    pub(crate) animate_sparkles: bool,
    /// Whether the motion policy still animates Prism Wake's output streak.
    pub(crate) animate_streak: bool,
    /// Whether the independent cursor-companion motion policy is animated.
    pub(crate) animate_cat: bool,
    /// This frame's companion opacity (0 ⇒ no companion is drawn) and pose.
    pub(crate) kitty_alpha: u8,
    pub(crate) cat_frame: crate::kitty_cursor::CatFrame,
    /// The pet companion's resolved frame, already ticked by its owner for this pass.
    pub(crate) pet: aterm_effects::companion::CompanionFrame,
    /// Whether this composed pass is a drawn present (`redraw_compose`) or a capture splice (`app_introspect`'s composed arm) re-using the same deco path.
    pub(crate) present: bool,
    pub(crate) accent: u32,
    pub(crate) cursor_color: u32,
    pub(crate) now: Instant,
}

fn composed_companion_geom(
    ctx: &ComposeDecoCtx<'_>,
    focus_place: Option<PanePlace>,
) -> crate::word_decorations::EffectGeom {
    focus_place.map_or(
        crate::word_decorations::EffectGeom {
            cell_w: 0,
            cell_h: 0,
            rows: 0,
            cols: 0,
        },
        |place| crate::word_decorations::EffectGeom {
            cell_w: ctx.cell_w as u16,
            cell_h: ctx.cell_h as u16,
            rows: place.rows,
            cols: place.cols,
        },
    )
}

/// One wallpaper cover-scale generation for one window (see [`crate::WindowState::wallpaper_scaled`]).
pub(crate) struct WallpaperScaled {
    fp: u64,
    dim_bits: u32,
    bg: u32,
    w: u32,
    h: u32,
    rows: usize,
    cols: usize,
    atlas: std::sync::Arc<aterm_core::render::SceneAtlas>,
    fg: u32,
    tint: Vec<Option<[u8; 3]>>,
}

/// A divider cell for the gaps between split panes.
pub(crate) fn divider_cell(p: impl Into<crate::chrome_band::ChromePalette>) -> RenderCell {
    let p = p.into();
    if crate::pane_chrome::split_chrome() == crate::pane_chrome::SplitChrome::Fluent {
        return crate::pane_chrome::seam_ground_cell(p);
    }
    let seam = match crate::chrome_band::forced_chrome() {
        Some(hc) => hc.window_text,
        None => p.tokens().seam_on(crate::settings::u32_rgb(p.theme.bg)),
    };
    RenderCell {
        ch: ' ',
        fg: seam,
        bg: seam,
        wide: false,
        emoji_presentation: false,
        text_presentation: false,
        bold: false,
        italic: false,
        underline: aterm_core::terminal::UnderlineStyle::None,
        strikethrough: false,
        overline: false,
        underline_color: None,
        overline_color: None,
        script: aterm_core::terminal::Script::Normal,
    }
}

/// Which side of the active pane a divider run marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActivePaneEdge {
    Left,
    Right,
    Top,
    Bottom,
}

const fn active_pane_edge_ink(theme: Theme) -> [u8; 3] {
    [
        ((theme.fg >> 16) & 0xff) as u8,
        ((theme.fg >> 8) & 0xff) as u8,
        (theme.fg & 0xff) as u8,
    ]
}

/// One divider cell carrying the active pane's edge stripe.
pub(crate) fn active_pane_edge_cell(
    p: impl Into<crate::chrome_band::ChromePalette>,
    edge: ActivePaneEdge,
) -> RenderCell {
    let p = p.into();
    let theme = p.theme;
    let seam = divider_cell(p);
    RenderCell {
        ch: match edge {
            ActivePaneEdge::Top => '▁',
            ActivePaneEdge::Bottom => '▔',
            ActivePaneEdge::Left => '▐',
            ActivePaneEdge::Right => '▌',
        },
        fg: active_pane_edge_ink(theme),
        ..seam
    }
}

/// The pane that takes keystrokes, as a rectangle of composite cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ActivePaneMark {
    pub(crate) row: usize,
    pub(crate) col: usize,
    pub(crate) rows: usize,
    pub(crate) cols: usize,
}

impl ActivePaneMark {
    /// The mark a composed frame carries, or `None` for a frame that marks no pane.
    pub(crate) fn resolve(
        enabled: bool,
        window_focused: bool,
        rect: (usize, usize, usize, usize),
    ) -> Option<Self> {
        let (row, col, rows, cols) = rect;
        (enabled && window_focused && rows > 0 && cols > 0).then_some(ActivePaneMark {
            row,
            col,
            rows,
            cols,
        })
    }

    /// This frame's term for [`crate::RepaintKey`].
    pub(crate) fn key_term(mark: Option<Self>) -> u64 {
        use std::hash::{Hash, Hasher};

        let Some(mark) = mark else {
            return 0;
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (mark.row, mark.col, mark.rows, mark.cols).hash(&mut hasher);
        hasher.finish() | 1
    }
}

fn mark_active_pane_edges(
    dst: &mut RenderInput,
    rows: usize,
    cols: usize,
    palette: crate::chrome_band::ChromePalette,
    mark: ActivePaneMark,
) {
    let row_end = mark.row.saturating_add(mark.rows).min(rows);
    let col_end = mark.col.saturating_add(mark.cols).min(cols);
    let across = mark.col.saturating_sub(1)..col_end.saturating_add(1).min(cols);
    let stamp_row = |dst: &mut RenderInput, r: usize, edge| {
        let cell = active_pane_edge_cell(palette, edge);
        if let Some(dst_row) = dst.cells.get_mut(r) {
            let span = across.start.min(dst_row.len())..across.end.min(dst_row.len());
            dst_row[span].fill(cell);
        }
    };
    if let Some(above) = mark.row.checked_sub(1) {
        stamp_row(dst, above, ActivePaneEdge::Top);
    }
    if mark.row.saturating_add(mark.rows) < rows {
        stamp_row(dst, row_end, ActivePaneEdge::Bottom);
    }
    let stamp_col = |dst: &mut RenderInput, c: usize, edge| {
        let cell = active_pane_edge_cell(palette, edge);
        for r in mark.row..row_end {
            if let Some(slot) = dst.cells.get_mut(r).and_then(|row| row.get_mut(c)) {
                *slot = cell;
            }
        }
    };
    if let Some(left) = mark.col.checked_sub(1) {
        stamp_col(dst, left, ActivePaneEdge::Left);
    }
    if mark.col.saturating_add(mark.cols) < cols {
        stamp_col(dst, col_end, ActivePaneEdge::Right);
    }
}

#[cfg(test)]
#[path = "app_render_active_pane_mark_tests.rs"]
mod active_pane_mark_tests;

#[cfg(test)]
#[path = "app_render_active_pane_mark_compose_tests.rs"]
mod active_pane_mark_compose_tests;

pub(crate) fn terminal_blank_cell(term: &Terminal) -> RenderCell {
    term.implicit_blank_render_cell()
}

/// Resolve the cursor color the terminal actually presents.
pub(crate) fn terminal_cursor_rgb(term: &Terminal) -> [u8; 3] {
    let color = term
        .cursor_color()
        .unwrap_or_else(|| term.default_foreground());
    [color.r, color.g, color.b]
}

/// Packed twin of [`terminal_cursor_rgb`] for [`RenderInput::cursor_color`].
pub(crate) fn terminal_cursor_color(term: &Terminal) -> u32 {
    aterm_render::rgb_to_u32(terminal_cursor_rgb(term))
}

fn terminal_frame_colors(term: &Terminal) -> (u32, u32, u32) {
    let blank = terminal_blank_cell(term);
    let default_bg = aterm_render::rgb_to_u32(blank.bg);
    let default_fg = aterm_render::rgb_to_u32(blank.fg);
    (default_bg, default_fg, terminal_cursor_color(term))
}

#[cfg(test)]
#[path = "app_render_tiled_seam_oracle_tests.rs"]
mod tiled_seam_oracle_tests;

#[cfg(test)]
#[path = "app_render_cursor_body_master_switch_tests.rs"]
mod cursor_body_master_switch_tests;

#[cfg(test)]
#[path = "app_render_terminal_cursor_color_tests.rs"]
mod terminal_cursor_color_tests;

#[cfg(test)]
thread_local! {
    pub(crate) static COMPOSE_SEAM_UNTILED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

#[cfg(test)]
thread_local! {
    pub(crate) static COMPOSE_SEAM_ARMS: std::cell::Cell<(u64, u64)> =
        const { std::cell::Cell::new((0, 0)) };
}

fn seed_seam_cells<T>(
    dst: &mut RenderInput,
    rows: usize,
    cols: usize,
    seam: RenderCell,
    tiles: T,
    keep: &[bool],
) where
    T: Iterator<Item = pane::PaneRect> + Clone,
{
    dst.cells.resize_with(rows, Vec::new);
    let mut count = 0usize;
    for tile in tiles.clone() {
        if usize::from(tile.row_off).saturating_add(usize::from(tile.rows)) > rows
            || usize::from(tile.col_off).saturating_add(usize::from(tile.cols)) > cols
        {
            count = 0;
            break;
        }
        count += 1;
    }
    let tiled = count > 0 && count.saturating_mul(count.saturating_add(1)) <= cols;
    #[cfg(test)]
    COMPOSE_SEAM_ARMS.with(|arms| {
        let (t, w) = arms.get();
        arms.set(if tiled { (t + 1, w) } else { (t, w + 1) });
    });
    for (r, row) in dst.cells.iter_mut().enumerate() {
        if row.len() != cols {
            row.clear();
            row.resize(cols, seam);
            continue;
        }
        if row_retained(keep, r) {
            continue;
        }
        if !tiled {
            row.fill(seam);
            continue;
        }
        let on_this_row = |tile: &pane::PaneRect| {
            let top = usize::from(tile.row_off);
            r >= top && r < top.saturating_add(usize::from(tile.rows))
        };
        let mut c = 0usize;
        while c < cols {
            // `.max(c + 1)` cannot be reached by a real `PaneRect` (its width is
            // documented `>= 1`) and is here so a degenerate one cannot spin.
            if let Some(tile) = tiles.clone().find(|tile| {
                on_this_row(tile)
                    && c >= usize::from(tile.col_off)
                    && c < usize::from(tile.col_off).saturating_add(usize::from(tile.cols))
            }) {
                c = usize::from(tile.col_off)
                    .saturating_add(usize::from(tile.cols))
                    .max(c.saturating_add(1));
                continue;
            }
            let next = tiles
                .clone()
                .filter(|tile| on_this_row(tile) && usize::from(tile.col_off) > c)
                .map(|tile| usize::from(tile.col_off))
                .min()
                .unwrap_or(cols)
                .min(cols);
            row[c..next].fill(seam);
            c = next;
        }
    }
}

fn seam_declared_shortfall(
    dst: &mut RenderInput,
    palette: crate::chrome_band::ChromePalette,
    rect: pane::PaneRect,
    src_rows: usize,
    src_cols: usize,
    keep: &[bool],
) {
    let rows = usize::from(rect.rows);
    let cols = usize::from(rect.cols);
    if src_rows >= rows && src_cols >= cols {
        return;
    }
    let seam = divider_cell(palette);
    let top = usize::from(rect.row_off);
    let left = usize::from(rect.col_off);
    for dr in top..top.saturating_add(rows) {
        // A retained row already carries this seam from the frame that wrote it:
        // the retention gate pins the rectangle, the snapshot's dimensions and
        // the theme's seam cell.
        if row_retained(keep, dr) {
            continue;
        }
        let Some(row) = dst.cells.get_mut(dr) else {
            break;
        };
        let covered = if dr < top.saturating_add(src_rows) {
            src_cols.min(cols)
        } else {
            0
        };
        let start = left.saturating_add(covered).min(row.len());
        let end = left.saturating_add(cols).min(row.len());
        if start < end {
            row[start..end].fill(seam);
        }
    }
}

#[inline]
fn row_retained(keep: &[bool], r: usize) -> bool {
    matches!(keep.get(r), Some(true))
}

fn reset_sparse_rows<T>(channel: &mut Vec<Vec<T>>, rows: usize, keep: &[bool]) {
    channel.resize_with(rows, Vec::new);
    if keep.is_empty() {
        for row in channel.iter_mut() {
            row.clear();
        }
        return;
    }
    for (r, row) in channel.iter_mut().enumerate() {
        if !row_retained(keep, r) {
            row.clear();
        }
    }
}

#[derive(Default, Clone, Debug)]
struct RetainPane {
    rect: Option<pane::PaneRect>,
    src_rows: usize,
    src_cols: usize,
    blank: Option<RenderCell>,
    lane: u64,
    base_y: i64,
    absolute_row_revision: u64,
    engine_alt: bool,
    engine_clean: bool,
    row_rev: Vec<u64>,
}

/// The composed-retention ledger.
#[derive(Default, Debug)]
pub(crate) struct ComposedRetain {
    seq: u64,
    rows: usize,
    cols: usize,
    seam: Option<RenderCell>,
    mark: Option<ActivePaneMark>,
    focus: usize,
    panes: Vec<RetainPane>,
    row_keep: Vec<bool>,
}

#[cfg(test)]
thread_local! {
    pub(crate) static COMPOSE_RETAIN_OFF: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

#[cfg(test)]
thread_local! {
    pub(crate) static COMPOSE_RETAIN_ARMS: std::cell::Cell<(u64, u64)> =
        const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
thread_local! {
    pub(crate) static COMPOSE_RETAIN_UNSOUND: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn note_retain_arms(retained: usize, rebuilt: usize) {
    COMPOSE_RETAIN_ARMS.with(|arms| {
        let (r, w) = arms.get();
        let widen = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
        arms.set((r + widen(retained), w + widen(rebuilt)));
    });
}

#[allow(
    clippy::too_many_arguments,
    reason = "the retention gate is a conjunction of independently-sourced frame facts; bundling them into a struct would hide which clause refused"
)]
fn plan_composed_retention(
    ws: &mut WindowState,
    rows: usize,
    cols: usize,
    seam: RenderCell,
    mark: Option<ActivePaneMark>,
    panes: &[(pane::PaneRect, Arc<Mutex<Terminal>>)],
    focus: usize,
    focus_blank: RenderCell,
    focus_quiet: bool,
    tiles_ready: bool,
) {
    let ComposedRetain {
        seq: l_seq,
        rows: l_rows,
        cols: l_cols,
        seam: l_seam,
        mark: l_mark,
        focus: l_focus,
        panes: l_panes,
        row_keep: keep,
    } = &mut ws.composed_retain;
    keep.clear();
    #[cfg(test)]
    if COMPOSE_RETAIN_OFF.with(std::cell::Cell::get) {
        note_retain_arms(0, rows);
        return;
    }
    let inp = &ws.input_scratch;
    let gate = tiles_ready
        && inp.composed_fill_intact()
        && *l_seq != 0
        && *l_seq == inp.composed_fill_seq
        && *l_rows == rows
        && *l_cols == cols
        && *l_seam == Some(seam)
        && *l_mark == mark
        && *l_focus == focus
        && l_panes.len() == panes.len()
        && inp.rows == rows
        && inp.cols == cols
        && inp.cells.len() == rows
        && inp.row_shift == 0;
    if !gate {
        #[cfg(test)]
        note_retain_arms(0, rows);
        return;
    }
    keep.resize(rows, true);
    for (i, (r, _)) in panes.iter().enumerate() {
        let prev = &l_panes[i];
        let src = if i == focus {
            &ws.composed_focus_scratch
        } else if let Some(buf) = ws.unfocused_pane_scratch.get(&i) {
            buf
        } else {
            keep.clear();
            #[cfg(test)]
            note_retain_arms(0, rows);
            return;
        };
        let blank = if i == focus {
            Some(focus_blank)
        } else {
            ws.composed_pane_stage_meta
                .get(i)
                .and_then(Option::as_ref)
                .map(|(_, blank)| *blank)
        };
        let comparable = prev.engine_clean
            && prev.rect == Some(*r)
            && prev.src_rows == src.rows
            && prev.src_cols == src.cols
            && prev.blank.is_some()
            && prev.blank == blank
            && prev.lane != 0
            && prev.lane == src.row_rev_lane
            && prev.base_y == src.base_y
            && prev.absolute_row_revision == src.absolute_row_revision
            && prev.engine_alt == src.engine_alt
            && prev.row_rev.len() == src.rows
            && src.row_rev.len() == src.rows
            && src.snapshot_seq == src.engine_fill_seq
            && src.display_offset == 0
            && src.row_shift == 0
            && src.engine_row_order == aterm_core::render::RowOrder::Logical
            && (i != focus || focus_quiet);
        let top = usize::from(r.row_off);
        let reach = usize::from(r.rows).max(src.rows);
        let end = top.saturating_add(reach).min(rows);
        for (sr, slot) in keep[top..end].iter_mut().enumerate() {
            if !*slot {
                continue;
            }
            let unchanged = comparable
                && src.row_rev.get(sr).copied().is_some_and(|rev| rev != 0)
                && src.row_rev.get(sr) == prev.row_rev.get(sr);
            if !unchanged {
                *slot = false;
            }
        }
    }
    for (r, row) in ws.input_scratch.cells.iter().enumerate() {
        if row.len() != cols
            && let Some(slot) = keep.get_mut(r)
        {
            *slot = false;
        }
    }
    let retained = keep.iter().filter(|k| **k).count();
    if retained == 0 {
        // The whole-composite rebuild, as the empty slice every other caller
        // passes: one length compare per row over the plain fill and blit.
        keep.clear();
    }
    #[cfg(test)]
    note_retain_arms(retained, rows - retained);
}

impl ComposedRetain {
    #[cfg(test)]
    pub(crate) fn retained_rows(&self) -> usize {
        self.row_keep.iter().filter(|keep| **keep).count()
    }
}

#[cfg(test)]
fn note_retain_unsound_if_kept(keep: &[bool], rect: pane::PaneRect, host_wrote: bool) {
    if !host_wrote {
        return;
    }
    let end = usize::from(rect.row_off)
        .saturating_add(usize::from(rect.rows))
        .min(keep.len());
    let top = usize::from(rect.row_off).min(end);
    if keep[top..end].iter().any(|k| *k) {
        COMPOSE_RETAIN_UNSOUND.with(|c| c.set(c.get().saturating_add(1)));
    }
}

fn record_retain_pane(
    ledger: &mut ComposedRetain,
    index: usize,
    src: &RenderInput,
    rect: pane::PaneRect,
    blank: RenderCell,
    host_wrote: bool,
) {
    if ledger.panes.len() <= index {
        ledger.panes.resize_with(index + 1, RetainPane::default);
    }
    let engine_clean = !host_wrote
        && src.snapshot_seq == src.engine_fill_seq
        && src.row_rev_lane != 0
        && src.row_rev.len() == src.rows
        && src.display_offset == 0
        && src.row_shift == 0
        && src.engine_row_order == aterm_core::render::RowOrder::Logical;
    let slot = &mut ledger.panes[index];
    slot.rect = Some(rect);
    slot.src_rows = src.rows;
    slot.src_cols = src.cols;
    slot.blank = Some(blank);
    slot.lane = src.row_rev_lane;
    slot.base_y = src.base_y;
    slot.absolute_row_revision = src.absolute_row_revision;
    slot.engine_alt = src.engine_alt;
    slot.engine_clean = engine_clean;
    if engine_clean {
        slot.row_rev.clone_from(&src.row_rev);
    } else {
        slot.row_rev.clear();
    }
}

/// Split-pane composition.
pub(crate) fn fill_divider_grid(
    dst: &mut RenderInput,
    rows: usize,
    cols: usize,
    palette: impl Into<crate::chrome_band::ChromePalette>,
    mark: Option<ActivePaneMark>,
) {
    fill_divider_grid_tiled(dst, rows, cols, palette, mark, std::iter::empty());
}

/// [`fill_divider_grid`] told which pane rectangles the caller is about to blit, so the seam pass can skip the cells those blits will overwrite.
pub(crate) fn fill_divider_grid_tiled<T>(
    dst: &mut RenderInput,
    rows: usize,
    cols: usize,
    palette: impl Into<crate::chrome_band::ChromePalette>,
    mark: Option<ActivePaneMark>,
    tiles: T,
) where
    T: Iterator<Item = pane::PaneRect> + Clone,
{
    fill_divider_grid_retained(dst, rows, cols, palette, mark, tiles, &[]);
}

/// [`fill_divider_grid_tiled`] also told which composed rows this frame may leave as the last committed composite left them.
pub(crate) fn fill_divider_grid_retained<T>(
    dst: &mut RenderInput,
    rows: usize,
    cols: usize,
    palette: impl Into<crate::chrome_band::ChromePalette>,
    mark: Option<ActivePaneMark>,
    tiles: T,
    keep: &[bool],
) where
    T: Iterator<Item = pane::PaneRect> + Clone,
{
    fill_divider_grid_cells(dst, rows, cols, palette.into(), mark, tiles, keep);
    clear_effect_overlays_for_compose(dst);
}

fn fill_divider_grid_cells<T>(
    dst: &mut RenderInput,
    rows: usize,
    cols: usize,
    palette: crate::chrome_band::ChromePalette,
    mark: Option<ActivePaneMark>,
    tiles: T,
    keep: &[bool],
) where
    T: Iterator<Item = pane::PaneRect> + Clone,
{
    let seam = divider_cell(palette);
    dst.invalidate_row_revisions();
    dst.rows = rows;
    dst.cols = cols;
    seed_seam_cells(dst, rows, cols, seam, tiles, keep);
    if let Some(mark) = mark
        && crate::pane_chrome::split_chrome() == crate::pane_chrome::SplitChrome::Classic
    {
        mark_active_pane_edges(dst, rows, cols, palette, mark);
    }
    reset_sparse_rows(&mut dst.clusters, rows, keep);
    reset_sparse_rows(&mut dst.combining, rows, keep);
    reset_sparse_rows(&mut dst.visual_to_logical, rows, keep);
    reset_sparse_rows(&mut dst.images, rows, keep);
    dst.line_sizes
        .resize(rows, aterm_core::grid::LineSize::SingleWidth);
    for (r, size) in dst.line_sizes.iter_mut().enumerate() {
        if !row_retained(keep, r) {
            *size = aterm_core::grid::LineSize::SingleWidth;
        }
    }
    reset_sparse_rows(&mut dst.line_size_spans, rows, keep);
    reset_sparse_rows(&mut dst.default_bg_spans, rows, keep);
    dst.cursor_visible = false;
    dst.cursor_row = 0;
    dst.cursor_col = 0;
    dst.cursor_style = CursorStyle::default();
    dst.cursor_effect_style_override = None;
    dst.cursor_fill_override = None;
    dst.cursor_trail_color = 0;
    dst.display_offset = 0;
    dst.base_y = 0;
    dst.absolute_row_revision = 0;
    dst.history_renumber_epoch = 0;
    dst.scroll_frac_px = 0;
    dst.grid_top_row = 0;
    dst.grid_bot_row = 0;
    dst.selection = aterm_core::selection::TextSelection::new();
    dst.selection_clip = None;
    dst.selection_bg = aterm_core::render::COLOR_UNSET;
    dst.selection_fg = aterm_core::render::COLOR_UNSET;
    dst.selections.clear();
    dst.default_bg = aterm_core::render::COLOR_UNSET;
    dst.default_fg = aterm_core::render::COLOR_UNSET;
    dst.cursor_color = aterm_core::render::COLOR_UNSET;
    dst.input_hot = false;
}

fn hand_fx_prefetch(pending: &mut Vec<Arc<aterm_core::render::FxArt>>, input: &mut RenderInput) {
    input.fx_prefetch.clear();
    std::mem::swap(pending, &mut input.fx_prefetch);
}

fn settle_fx_prefetch(
    pending: &mut Vec<Arc<aterm_core::render::FxArt>>,
    input: &mut RenderInput,
    presented: bool,
) {
    if !presented {
        input.fx_prefetch.append(pending);
        std::mem::swap(pending, &mut input.fx_prefetch);
    }
    input.fx_prefetch.clear();
}

/// The effect-overlay half of [`fill_divider_grid`].
pub(crate) fn clear_effect_overlays_for_compose(dst: &mut RenderInput) {
    dst.cursor_glow_add.clear();
    dst.glow_halo.clear();
    dst.fire_patch.clear();
    dst.glow_under.clear();
    dst.char_fg.clear();
    dst.fire_halo.clear();
    dst.cursor_trail.clear();
    dst.word_decorations.clear();
    dst.ink.clear();
    dst.cat_quads.clear();
    dst.cat_atlas = None;
    dst.free_sprites.clear();
    dst.free_atlas = None;
    dst.nova_add.clear();
    dst.rain_quads.clear();
    dst.rain_atlas = None;
    dst.rain_add.clear();
    dst.fx_layers.clear();
    dst.fx_occluders.clear();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ComposedCursorEffectPane {
    row: usize,
    col: usize,
    pane_rows: usize,
    pane_cols: usize,
    cell_w: usize,
    cell_h: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ComposedCursorEffectClip {
    row0: usize,
    row1: usize,
    col0: usize,
    col1: usize,
    x0: u16,
    y0: u16,
    x1: u16,
    y1: u16,
    fx: PaneFxPlacement,
}

/// Where one pane's fx layers land in a composed frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PaneFxPlacement {
    /// The pane's top-left cell corner in window pixels (Window space).
    pub(crate) window_origin: (i32, i32),
    /// The same corner in grid-interior pixels (Grid space).
    pub(crate) grid_origin: (i32, i32),
    /// The pane's box in window pixels.
    pub(crate) window_box: aterm_core::render::PxRect,
}

impl PaneFxPlacement {
    /// `layer`, authored pane-local, placed in the composed frame.
    pub(crate) fn place(&self, layer: &aterm_core::render::FxLayer) -> aterm_core::render::FxLayer {
        let (origin, pane_box) = match layer.space {
            aterm_core::render::FxSpace::Window => (self.window_origin, self.window_box),
            aterm_core::render::FxSpace::Grid => (
                self.grid_origin,
                self.window_box.translate(
                    self.grid_origin.0.saturating_sub(self.window_origin.0),
                    self.grid_origin.1.saturating_sub(self.window_origin.1),
                ),
            ),
        };
        let mut placed = layer.clone();
        placed.offset = (
            layer.offset.0.saturating_add(origin.0),
            layer.offset.1.saturating_add(origin.1),
        );
        placed.clip = layer.clip.translate(origin.0, origin.1).intersect(pane_box);
        placed
    }

    /// Install a pane's `layers` into `dst`, each placed.
    pub(crate) fn install(
        &self,
        dst: &mut Vec<aterm_core::render::FxLayer>,
        layers: &[aterm_core::render::FxLayer],
    ) {
        withdraw_fx_layers(dst, layers);
        dst.extend(layers.iter().map(|l| self.place(l)));
    }
}

fn withdraw_fx_layers(
    dst: &mut Vec<aterm_core::render::FxLayer>,
    layers: &[aterm_core::render::FxLayer],
) {
    if !layers.is_empty() {
        dst.retain(|l| !layers.iter().any(|n| n.id == l.id));
    }
}

struct FocusedComposedCursorEffects<'a> {
    glow_add: &'a [aterm_core::render::GlowQuad],
    glow_halo: &'a [aterm_core::render::RainHalo],
    fire_patch: &'a [aterm_core::render::FirePatch],
    glow_under: &'a [aterm_core::render::GlowQuad],
    char_fg: &'a [aterm_core::render::CharFg],
    fire_halo: &'a [aterm_core::render::FireHaloCell],
    trail: &'a [aterm_core::render::TrailCell],
    trail_color: u32,
    active_cursor_fill: Option<u32>,
    fx_layers: &'a [aterm_core::render::FxLayer],
}

fn project_focused_composed_cursor_effects(
    dst: &mut RenderInput,
    effects: FocusedComposedCursorEffects<'_>,
    clip: Option<ComposedCursorEffectClip>,
    focus_cursor_override: Option<CursorStyle>,
    body_style_override: Option<CursorStyle>,
) {
    dst.cursor_glow_add.clear();
    dst.glow_halo.clear();
    dst.fire_patch.clear();
    dst.glow_under.clear();
    dst.char_fg.clear();
    dst.fire_halo.clear();
    dst.cursor_trail.clear();
    dst.cursor_effect_style_override = None;

    let Some(clip) = clip else {
        withdraw_fx_layers(&mut dst.fx_layers, effects.fx_layers);
        dst.cursor_fill_override = None;
        dst.cursor_trail_color = effects.trail_color;
        return;
    };
    clip.fx.install(&mut dst.fx_layers, effects.fx_layers);

    dst.cursor_glow_add.extend_from_slice(effects.glow_add);
    dst.glow_halo.extend_from_slice(effects.glow_halo);
    dst.fire_patch.extend_from_slice(effects.fire_patch);
    dst.glow_under.extend_from_slice(effects.glow_under);
    dst.char_fg.extend_from_slice(effects.char_fg);
    dst.fire_halo.extend_from_slice(effects.fire_halo);
    dst.cursor_trail.extend_from_slice(effects.trail);
    dst.cursor_trail_color = effects.trail_color;
    dst.cursor_fill_override =
        window_cursor_fill(focus_cursor_override, effects.active_cursor_fill);
    dst.cursor_effect_style_override = body_style_override;

    let clip_quad = |x: u16, y: u16, w: u16, h: u16| {
        let x0 = x.max(clip.x0);
        let y0 = y.max(clip.y0);
        let x1 = x.saturating_add(w).min(clip.x1);
        let y1 = y.saturating_add(h).min(clip.y1);
        (x1 > x0 && y1 > y0).then(|| (x0, y0, x1 - x0, y1 - y0))
    };
    dst.cursor_glow_add
        .retain_mut(|quad| match clip_quad(quad.x, quad.y, quad.w, quad.h) {
            Some((x, y, w, h)) => {
                (quad.x, quad.y, quad.w, quad.h) = (x, y, w, h);
                true
            }
            None => false,
        });
    dst.glow_under
        .retain_mut(|quad| match clip_quad(quad.x, quad.y, quad.w, quad.h) {
            Some((x, y, w, h)) => {
                (quad.x, quad.y, quad.w, quad.h) = (x, y, w, h);
                true
            }
            None => false,
        });
    dst.glow_halo
        .retain_mut(|quad| match clip_quad(quad.x, quad.y, quad.w, quad.h) {
            Some((x, y, w, h)) => {
                (quad.x, quad.y, quad.w, quad.h) = (x, y, w, h);
                true
            }
            None => false,
        });
    dst.fire_patch
        .retain_mut(|quad| match clip_quad(quad.x, quad.y, quad.w, quad.h) {
            Some((x, y, w, h)) => {
                (quad.x, quad.y, quad.w, quad.h) = (x, y, w, h);
                true
            }
            None => false,
        });
    dst.char_fg.retain(|cell| {
        (clip.row0..clip.row1).contains(&usize::from(cell.row))
            && (clip.col0..clip.col1).contains(&usize::from(cell.col))
    });
    dst.fire_halo.retain(|cell| {
        (clip.row0..clip.row1).contains(&usize::from(cell.row))
            && (clip.col0..clip.col1).contains(&usize::from(cell.col))
    });
    dst.cursor_trail.retain(|cell| {
        (clip.row0..clip.row1).contains(&cell.row) && (clip.col0..clip.col1).contains(&cell.col)
    });
}

#[cfg(test)]
#[path = "app_render_composed_cursor_effect_projection_tests.rs"]
mod composed_cursor_effect_projection_tests;

#[cfg(test)]
#[path = "app_render_composed_cursor_effect_advance_tests.rs"]
mod composed_cursor_effect_advance_tests;

const fn take_focused_pane_scratch(ws: &mut WindowState) {
    std::mem::swap(&mut ws.pane_scratch, &mut ws.composed_focus_scratch);
}

const fn return_focused_pane_scratch(ws: &mut WindowState, host_wrote: bool) {
    if host_wrote {
        ws.pane_scratch.snapshot_seq = ws.pane_scratch.snapshot_seq.wrapping_add(1);
    }
    std::mem::swap(&mut ws.pane_scratch, &mut ws.composed_focus_scratch);
}

pub(crate) fn push_pane_selection(
    dst: &mut RenderInput,
    src: &RenderInput,
    row_off: usize,
    col_off: usize,
    pane_rows: usize,
    pane_cols: usize,
    focused: bool,
) {
    // An unfocused pane with no selection contributes nothing to `dst`.
    if !focused && !src.selection.has_selection() {
        return;
    }
    let mut selection = src.selection.clone();
    let start = selection.start();
    let end = selection.end();
    let row_delta = i32::try_from(row_off)
        .unwrap_or(i32::MAX)
        .saturating_add(src.display_offset);
    let col_delta = u16::try_from(col_off).unwrap_or(u16::MAX);
    selection.relocate(
        start.row.saturating_add(row_delta),
        start.col.saturating_add(col_delta),
        end.row.saturating_add(row_delta),
        end.col.saturating_add(col_delta),
    );

    let row_end = row_off
        .saturating_add(pane_rows.min(src.rows))
        .min(dst.rows);
    let col_end = col_off
        .saturating_add(pane_cols.min(src.cols))
        .min(dst.cols);
    let clip = aterm_core::render::SelectionClip::new(
        row_off.min(dst.rows),
        row_end,
        col_off.min(dst.cols),
        col_end,
    );
    if focused {
        dst.selection.clone_from(&selection);
        dst.selection_clip = src.selection.has_selection().then_some(clip);
        dst.selection_bg = src.selection_bg;
        dst.selection_fg = src.selection_fg;
    }
    if src.selection.has_selection() {
        dst.selections.push(aterm_core::render::PaneSelection {
            selection,
            clip,
            bg: src.selection_bg,
            fg: src.selection_fg,
            inactive: !focused,
        });
    }
}

#[cfg(test)]
mod selection_projection_tests {
    use super::*;
    use aterm_core::selection::{SelectionSide, SelectionType};

    fn selected_pane(rows: usize, cols: usize) -> RenderInput {
        let mut src = RenderInput::empty();
        src.rows = rows;
        src.cols = cols;
        src.selection
            .start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        src.selection.update_selection(
            i32::try_from(rows).unwrap() - 1,
            u16::try_from(cols).unwrap() - 1,
            SelectionSide::Right,
        );
        src.selection.complete_selection();
        src
    }

    #[test]
    fn off_frame_focused_pane_selection_fails_closed() {
        let src = selected_pane(2, 2);
        let mut dst = RenderInput::empty();
        dst.rows = 2;
        dst.cols = 4;
        push_pane_selection(&mut dst, &src, 0, 8, 2, 2, true);

        assert_eq!(
            dst.selection_clip,
            Some(aterm_core::render::SelectionClip::new(0, 2, 4, 4)),
            "active but clipped-away selection keeps explicit empty authority"
        );
        for row in 0..dst.rows {
            for col in 0..dst.cols {
                assert!(
                    !dst.selection_contains_cell(row, col, false, false),
                    "off-frame pane selection leaked to frame cell ({row}, {col})"
                );
            }
        }
    }

    #[test]
    fn both_panes_paint_and_the_focused_one_keeps_the_scalar_anchor() {
        let mut dst = RenderInput::empty();
        dst.rows = 2;
        dst.cols = 9;
        let mut focused = selected_pane(2, 4);
        focused.selection_bg = 0x0011_2233;
        push_pane_selection(&mut dst, &focused, 0, 0, 2, 4, true);
        let mut sibling = selected_pane(2, 4);
        sibling.selection_bg = 0x0044_5566;
        push_pane_selection(&mut dst, &sibling, 0, 5, 2, 4, false);

        assert_eq!(
            dst.selection_bg, 0x0011_2233,
            "the scalar colour is the FOCUSED pane's, unchanged"
        );
        assert_eq!(dst.selections.len(), 2);
        assert_eq!(
            (dst.selections[0].bg, dst.selections[1].bg),
            (0x0011_2233, 0x0044_5566),
            "each entry carries its OWN live selection colour"
        );
        assert_eq!(
            (dst.selections[0].inactive, dst.selections[1].inactive),
            (false, true)
        );
        for col in 0..dst.cols {
            let hit = dst.selection_hit(0, col, false, false);
            let expected = match col {
                0..=3 => Some(0),
                4 => None, // the divider column belongs to no pane
                _ => Some(1),
            };
            assert_eq!(hit, expected, "column {col} resolved to the wrong pane");
        }
    }
}

/// Blit one pane's snapshot `src` (sized to the pane's sub-rect) into the composite `dst` at `(row_off, col_off)`.
pub(crate) fn blit_pane_into(
    dst: &mut RenderInput,
    src: &RenderInput,
    row_off: usize,
    col_off: usize,
    blank: RenderCell,
) {
    blit_pane_into_retained(dst, src, row_off, col_off, blank, &[]);
}

/// [`blit_pane_into`] told which composed rows this frame leaves exactly as the last committed composite left them.
pub(crate) fn blit_pane_into_retained(
    dst: &mut RenderInput,
    src: &RenderInput,
    row_off: usize,
    col_off: usize,
    blank: RenderCell,
    keep: &[bool],
) {
    for sr in 0..src.rows {
        let Some(dr) = row_off.checked_add(sr) else {
            break;
        };
        if row_retained(keep, dr) {
            continue;
        }
        let Some(dst_row) = dst.cells.get_mut(dr) else {
            break;
        };
        let src_row = src.cells.get(sr);
        let materialized = src_row.map_or(0, |row| row.len().min(src.cols));
        let clipped_wide_lead = src.cols.checked_sub(1).filter(|_| {
            src_row
                .and_then(|row| row.get(src.cols))
                .is_some_and(|cell| cell.wide)
        });
        if let Some(src_row) = src_row {
            for (sc, cell) in src_row.iter().take(materialized).enumerate() {
                let Some(dc) = col_off.checked_add(sc) else {
                    break;
                };
                if let Some(slot) = dst_row.get_mut(dc) {
                    *slot = if Some(sc) == clipped_wide_lead {
                        blank
                    } else {
                        *cell
                    };
                }
            }
        }
        let blank_start = col_off.saturating_add(materialized).min(dst_row.len());
        let pane_end = col_off.saturating_add(src.cols).min(dst_row.len());
        if blank_start < pane_end {
            dst_row[blank_start..pane_end].fill(blank);
        }
        let src_spans = src
            .line_size_spans
            .get(sr)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut pane_line_size = None;
        if col_off < pane_end
            && let Some(spans) = dst.line_size_spans.get_mut(dr)
        {
            if src_spans.is_empty() {
                if let Some(ls) = src.line_sizes.get(sr).copied()
                    && ls != aterm_core::grid::LineSize::SingleWidth
                {
                    spans.push(aterm_core::render::LineSizeSpan::new(col_off, pane_end, ls));
                    pane_line_size = Some(ls);
                }
            } else {
                for span in src_spans {
                    if span.line_size == aterm_core::grid::LineSize::SingleWidth {
                        continue;
                    }
                    let Some(start) = col_off.checked_add(span.start_col.min(src.cols)) else {
                        continue;
                    };
                    let Some(end) = col_off.checked_add(span.end_col.min(src.cols)) else {
                        continue;
                    };
                    let (start, end) = (start.min(pane_end), end.min(pane_end));
                    if start < end {
                        spans.push(aterm_core::render::LineSizeSpan::new(
                            start,
                            end,
                            span.line_size,
                        ));
                        pane_line_size = Some(span.line_size);
                    }
                }
            }
            if pane_line_size.is_some() {
                // Panes are not guaranteed to blit left-to-right, and
                // `line_size_run_at` binary-searches by `start_col`.
                spans.sort_unstable_by_key(|s| s.start_col);
            }
        }
        if let Some(ls) = pane_line_size
            && let Some(dst_ls) = dst.line_sizes.get_mut(dr)
        {
            *dst_ls = ls;
        }
        if col_off < pane_end
            && let Some(dst_spans) = dst.default_bg_spans.get_mut(dr)
        {
            dst_spans.push(aterm_core::render::DefaultBgSpan::new(
                col_off,
                pane_end,
                aterm_render::rgb_to_u32(blank.bg),
            ));
            dst_spans.sort_unstable_by_key(|span| span.start_col);
        }
        let src_map = src.visual_to_logical_row(sr);
        let map_end = col_off.saturating_add(src_map.len());
        if !src_map.is_empty()
            && src_map.len() <= src.cols
            && map_end <= pane_end
            && let Some(dst_map) = dst.visual_to_logical.get_mut(dr)
        {
            if dst_map.len() < map_end {
                let identity_from = dst_map.len();
                dst_map
                    .extend((identity_from..map_end).map(|c| u16::try_from(c).unwrap_or(u16::MAX)));
            }
            for (v, &l) in src_map.iter().enumerate() {
                dst_map[col_off + v] = u16::try_from(col_off + usize::from(l)).unwrap_or(u16::MAX);
            }
        }
        if let Some(dst_clusters) = dst.clusters.get_mut(dr)
            && let Some(src_clusters) = src.clusters.get(sr)
        {
            for (c, s) in src_clusters {
                if Some(*c) != clipped_wide_lead
                    && *c < src.cols
                    && let Some(dc) = col_off.checked_add(*c)
                    && dc < pane_end
                {
                    dst_clusters.push((dc, s.clone()));
                }
            }
            dst_clusters.sort_unstable_by_key(|(c, _)| *c);
        }
        if let Some(dst_comb) = dst.combining.get_mut(dr)
            && let Some(src_comb) = src.combining.get(sr)
        {
            for (c, m) in src_comb {
                if Some(*c) != clipped_wide_lead
                    && *c < src.cols
                    && let Some(dc) = col_off.checked_add(*c)
                    && dc < pane_end
                {
                    dst_comb.push((dc, m.clone()));
                }
            }
            dst_comb.sort_unstable_by_key(|(c, _)| *c);
        }
        if let Some(dst_imgs) = dst.images.get_mut(dr)
            && let Some(src_imgs) = src.images.get(sr)
        {
            for (c, img) in src_imgs {
                if Some(*c) != clipped_wide_lead
                    && *c < src.cols
                    && let Some(dc) = col_off.checked_add(*c)
                    && dc < pane_end
                {
                    dst_imgs.push((dc, img.clone()));
                }
            }
            dst_imgs.sort_by_key(|(c, _)| *c);
        }
    }
}

/// Paint a non-terminal pane's full cell rectangle into a divider-seeded composite and register a `SingleWidth` pane span for hard x clipping.
pub(crate) fn fill_blank_pane_into(
    dst: &mut RenderInput,
    row_off: usize,
    col_off: usize,
    rows: usize,
    cols: usize,
    blank: RenderCell,
) {
    for sr in 0..rows {
        let Some(dr) = row_off.checked_add(sr) else {
            break;
        };
        let Some(dst_row) = dst.cells.get_mut(dr) else {
            break;
        };
        let start = col_off.min(dst_row.len());
        let end = col_off.saturating_add(cols).min(dst_row.len());
        if start >= end {
            continue;
        }
        dst_row[start..end].fill(blank);
        if let Some(spans) = dst.line_size_spans.get_mut(dr) {
            spans.push(aterm_core::render::LineSizeSpan::new(
                start,
                end,
                aterm_core::grid::LineSize::SingleWidth,
            ));
            spans.sort_unstable_by_key(|span| span.start_col);
        }
        if let Some(spans) = dst.default_bg_spans.get_mut(dr) {
            spans.push(aterm_core::render::DefaultBgSpan::new(
                start,
                end,
                aterm_render::rgb_to_u32(blank.bg),
            ));
            spans.sort_unstable_by_key(|span| span.start_col);
        }
    }
}

/// One terminal leaf captured while building an introspection framebuffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TerminalCaptureLeaf {
    pub(crate) view: crate::tab_model::ViewId,
    pub(crate) session: u64,
    pub(crate) focused: bool,
    pub(crate) rows: usize,
    pub(crate) cols: usize,
    pub(crate) snapshot_seq: u64,
    /// Content-only arrival clock coherent with this leaf's cells.
    pub(crate) content_seq: u64,
}

/// Focused terminal coordinate sample owned by the same lock as a capture's cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TerminalCaptureFocus {
    /// Canonical view identity.
    pub(crate) view: crate::tab_model::ViewId,
    pub(crate) session: u64,
    pub(crate) terminal_id: u64,
    /// Content-only arrival clock captured under the same terminal lock as the cells.
    pub(crate) content_seq: u64,
    pub(crate) alternate_screen: bool,
    pub(crate) live_viewport: bool,
    /// Rows scrolled back into history (`0` on the live viewport).
    pub(crate) display_offset: usize,
    /// The caret on the live viewport, hidden or not (`None` while scrolled into history), as a present hands it to the pet.
    pub(crate) cursor: Option<(u16, u16)>,
    /// The emulator paints the caret (dectcem shown), same lock.
    pub(crate) cursor_visible: bool,
    /// The program's own account that the captured frame is unfinished ([`aterm_core::terminal::Terminal::sync_frame_unfinished`]), same lock.
    pub(crate) frame_unfinished: bool,
    /// The terminal's count of synchronized-output closes ([`aterm_core::terminal::Terminal::sync_end_seq`]), same lock.
    pub(crate) sync_end_seq: u64,
    /// The latest close's cell, from the same terminal hold.
    pub(crate) sync_end_caret: Option<(u16, u16)>,
    /// Bounded console facts paired with these exact captured cells.
    pub(crate) pet_world: aterm_effects::pet_world::PetWorldFacts,
    /// The advancing headless capture consumed the exact damage session while it still held the authorized extraction lock.
    pub(crate) damage_consumed: bool,
}

/// Result of the damage-neutral terminal capture preparation pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalCaptureGrid {
    /// `true` when the active terminal tab owns a split tree, including its one-visible-leaf zoom presentation.
    pub(crate) composed: bool,
    /// Exact focused coordinate/caret observation paired with `leaves`.
    pub(crate) focus: Option<TerminalCaptureFocus>,
    /// Every visible terminal leaf, in canonical paint order.
    pub(crate) leaves: Vec<TerminalCaptureLeaf>,
}

/// Exact terminal-capture preparation verdict.
#[derive(Debug)]
pub(crate) enum CapturePreparation<T> {
    Ready(T),
    Held { retry_at: Instant },
    Unavailable,
}

impl<T> CapturePreparation<T> {
    #[cfg(any(test, feature = "bench-support"))]
    fn map<U>(self, f: impl FnOnce(T) -> U) -> CapturePreparation<U> {
        match self {
            Self::Ready(value) => CapturePreparation::Ready(f(value)),
            Self::Held { retry_at } => CapturePreparation::Held { retry_at },
            Self::Unavailable => CapturePreparation::Unavailable,
        }
    }

    #[cfg(any(test, feature = "bench-support"))]
    fn into_option(self) -> Option<T> {
        match self {
            Self::Ready(grid) => Some(grid),
            Self::Held { .. } | Self::Unavailable => None,
        }
    }
}

#[inline]
fn blend_rgba_pixel_over(destination: &mut [u8], source: &[u8]) {
    debug_assert_eq!(destination.len(), 4);
    debug_assert_eq!(source.len(), 4);
    let sa = u32::from(source[3]);
    if sa == 255 {
        destination.copy_from_slice(source);
        return;
    }
    let da = u32::from(destination[3]);
    let out_a = sa + (da * (255 - sa) + 127) / 255;
    if out_a == 0 {
        destination.fill(0);
        return;
    }
    for channel in 0..3 {
        let source_premultiplied = u32::from(source[channel]) * sa;
        let destination_premultiplied = u32::from(destination[channel]) * da * (255 - sa) / 255;
        destination[channel] =
            ((source_premultiplied + destination_premultiplied) / out_a).min(255) as u8;
    }
    destination[3] = out_a.min(255) as u8;
}

fn blit_rgba_over(
    dst: &mut [u8],
    dst_size: (u32, u32),
    src: &[u8],
    src_size: (u32, u32),
    origin: (u32, u32),
) {
    let (dst_width, dst_height) = dst_size;
    let (src_width, src_height) = src_size;
    let (x, y) = origin;
    let span = src_width.min(dst_width.saturating_sub(x));
    for sy in 0..src_height.min(dst_height.saturating_sub(y)) {
        let src_row = (sy as usize) * (src_width as usize) * 4;
        let dst_row = ((y + sy) as usize * dst_width as usize + x as usize) * 4;
        let len = span as usize * 4;
        if let (Some(source_row), Some(destination_row)) = (
            src.get(src_row..src_row.saturating_add(len)),
            dst.get_mut(dst_row..dst_row.saturating_add(len)),
        ) {
            for (destination, source) in destination_row
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(source_row.as_chunks::<4>().0)
            {
                blend_rgba_pixel_over(destination, source);
            }
            continue;
        }
        for sx in 0..span {
            let si = ((sy * src_width + sx) * 4) as usize;
            let di = (((y + sy) * dst_width + x + sx) * 4) as usize;
            let Some(source) = src.get(si..si + 4) else {
                return;
            };
            let Some(destination) = dst.get_mut(di..di + 4) else {
                return;
            };
            blend_rgba_pixel_over(destination, source);
        }
    }
}

fn with_borrowed_prediction_overlay<R>(
    predictor: &mut crate::predict::Predictor,
    now: Instant,
    consume: impl FnOnce(&[crate::predict::Prediction], Option<(u16, u16)>) -> R,
) -> R {
    let (cells, chain_cursor) = predictor.overlay_frame(now);
    consume(cells, chain_cursor)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PredictionPainted {
    cursor: Option<(u16, u16)>,
    shown_through: Option<u64>,
    proven: bool,
}

impl PredictionPainted {
    fn closes_input_through(self) -> Option<u64> {
        self.shown_through.filter(|_| self.proven)
    }
}

fn paint_prediction_overlay(
    predictor: &mut crate::predict::Predictor,
    scratch: &mut RenderInput,
    now: Instant,
    cols: usize,
    blank: RenderCell,
) -> PredictionPainted {
    #[cfg(any(test, debug_assertions))]
    let mut under = None;
    let drawn = with_borrowed_prediction_overlay(predictor, now, |preds, chain| {
        #[cfg(any(test, debug_assertions))]
        {
            under = Some(crate::predict_trace::Under::read(scratch, preds, chain));
        }
        paint_predictions(scratch, preds, chain, cols, blank)
    });
    #[cfg(any(test, debug_assertions))]
    if let (Some(cursor), Some(under)) = (drawn.cursor, under.as_ref()) {
        crate::predict_trace::frame(predictor, under, scratch, cursor, now);
    }
    PredictionPainted {
        cursor: drawn.cursor,
        shown_through: if drawn.cursor.is_some() && drawn.whole {
            predictor.shown_through()
        } else {
            None
        },
        proven: predictor.mode() == crate::predict::PredictMode::Adaptive,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OverlayPaint {
    cursor: Option<(u16, u16)>,
    whole: bool,
}

fn paint_predictions(
    scratch: &mut RenderInput,
    preds: &[crate::predict::Prediction],
    chain_cursor: Option<(u16, u16)>,
    cols: usize,
    blank: RenderCell,
) -> OverlayPaint {
    match chain_cursor {
        Some(cursor) => {
            let cursor = paint_prediction_chain(scratch, preds, cursor, cols, blank);
            OverlayPaint {
                cursor,
                whole: cursor.is_some(),
            }
        }
        None => {
            let ghosts = paint_prediction_ghosts(scratch, preds, cols, blank);
            let last = u16::try_from(cols.saturating_sub(1)).unwrap_or(u16::MAX);
            OverlayPaint {
                cursor: ghosts
                    .newest
                    .map(|(row, col)| (row, col.saturating_add(1).min(last))),
                whole: ghosts.refused == 0,
            }
        }
    }
}

fn paint_prediction_chain(
    scratch: &mut RenderInput,
    cells: &[crate::predict::Prediction],
    cursor: (u16, u16),
    cols: usize,
    blank: RenderCell,
) -> Option<(u16, u16)> {
    let (row, col) = (usize::from(cursor.0), usize::from(cursor.1));
    if col >= cols || cells.iter().any(|p| p.row != cursor.0) {
        return None;
    }
    let painted_here = cells.iter().any(|p| usize::from(p.col) == col);
    let on_continuation = !painted_here
        && scratch
            .cells
            .get(row)
            .and_then(|cells| cells.get(scratch.visual_col(row, col)))
            .is_some_and(|cell| cell.wide);
    if on_continuation {
        return None;
    }
    let span = cells.iter().map(|p| (usize::from(p.col), span_cell(p)));
    scratch
        .paint_prediction_span(row, span, cols, blank)
        .then_some(cursor)
}

fn span_cell(p: &crate::predict::Prediction) -> SpanCell {
    match p.ghost() {
        GhostCell::Typed => SpanCell::Typed(p.ch),
        GhostCell::Moved { from } => SpanCell::Moved {
            from: usize::from(from),
            ch: p.ch,
        },
        GhostCell::Vacated => SpanCell::Vacated,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GhostsPainted {
    newest: Option<(u16, u16)>,
    refused: usize,
}

fn paint_prediction_ghosts(
    scratch: &mut RenderInput,
    preds: &[crate::predict::Prediction],
    cols: usize,
    blank: RenderCell,
) -> GhostsPainted {
    let mut painted = GhostsPainted {
        newest: None,
        refused: 0,
    };
    for p in preds {
        if scratch.paint_prediction_ghost(usize::from(p.row), usize::from(p.col), p.ch, cols, blank)
        {
            painted.newest = Some((p.row, p.col));
        } else {
            painted.refused += 1;
        }
    }
    painted
}

fn field_preedit_display(
    text: &str,
    caret: usize,
    preedit: &str,
    preedit_caret: Option<usize>,
) -> (String, usize, std::ops::Range<usize>) {
    let mut at = caret.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    let mut display = String::with_capacity(text.len() + preedit.len());
    display.push_str(&text[..at]);
    display.push_str(preedit);
    display.push_str(&text[at..]);
    let in_pre = preedit_caret.map_or(preedit.len(), |a| {
        let mut b = a.min(preedit.len());
        while b > 0 && !preedit.is_char_boundary(b) {
            b -= 1;
        }
        b
    });
    (display, at + in_pre, at..at + preedit.len())
}

#[cfg(test)]
mod ime_preedit_field_tests {
    use super::field_preedit_display;

    #[test]
    fn field_preedit_display_rows() {
        enum Span {
            Is(std::ops::Range<usize>),
            Empty,
            Unchecked,
        }
        let rows = [
            (
                "splices at the caret",
                ("find", 2, "中文", Some("中".len())),
                ("fi中文nd", 2 + "中".len(), Span::Is(2.."fi中文".len())),
            ),
            (
                "no platform caret",
                ("ab", 2, "に", None),
                ("abに", "abに".len(), Span::Unchecked),
            ),
            (
                "interior byte offsets floor to char boundaries",
                ("中", 1, "日", Some(2)),
                ("日中", 0, Span::Is(0.."日".len())),
            ),
            (
                "an empty composition is the identity",
                ("query", 3, "", None),
                ("query", 3, Span::Empty),
            ),
        ];
        for (label, (text, caret, preedit, preedit_caret), (want_text, want_caret, want_span)) in
            rows
        {
            let (got_text, got_caret, got_span) =
                field_preedit_display(text, caret, preedit, preedit_caret);
            assert_eq!(got_text, want_text, "{label}");
            assert_eq!(got_caret, want_caret, "{label}");
            match want_span {
                Span::Is(span) => assert_eq!(got_span, span, "{label}"),
                Span::Empty => assert!(got_span.is_empty(), "{label}"),
                Span::Unchecked => {}
            }
        }
    }
}

#[allow(
    clippy::items_after_test_module,
    reason = "this module drives the composed present path it is written against; the rest of the file is the App render inherent-impl, not stray items"
)]
#[cfg(test)]
mod ime_candidate_anchor_tests {
    use crate::{App, WindowId};
    use std::time::{Duration, Instant};

    fn split_fixture() -> (App, WindowId, usize, usize) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let _ = app.split_active_stub_tab(wid);
        let (rows, cols) = {
            let ws = app.windows.get(&wid).expect("test window");
            (usize::from(ws.rows), usize::from(ws.cols))
        };
        (app, wid, rows, cols)
    }

    fn present(
        app: &mut App,
        wid: WindowId,
        size: (usize, usize),
        frame: u64,
    ) -> Option<(i32, i32, u32, u32)> {
        let at = Instant::now() + Duration::from_millis(16 * frame);
        assert!(
            app.redraw_compose(wid, size.0, size.1, false, false, None, 0, at)
                .is_some(),
            "fixture: the composed present path ran"
        );
        app.windows[&wid].last_ime_rect
    }

    #[test]
    fn a_split_anchors_the_candidate_list_on_the_composition_not_its_start() {
        let (mut app, wid, rows, cols) = split_fixture();
        let engine = present(&mut app, wid, (rows, cols), 0)
            .expect("the engine caret rect is reported on an ordinary frame");

        app.on_ime_preedit(wid, "にほん".to_string(), None);
        let composing = present(&mut app, wid, (rows, cols), 1).expect("the composing caret rect");
        assert!(
            app.windows[&wid].preedit_shown,
            "fixture: the composed frame really painted the composition"
        );
        let cw = i32::try_from(app.win_cell_size(wid).0).expect("cell width");
        assert_eq!(
            (composing.0 - engine.0, composing.1),
            (6 * cw, engine.1),
            "six columns of kana on the same row: the candidate list follows the \
             caret, not the column the composition started at"
        );

        app.on_ime_preedit(wid, String::new(), None);
        assert_eq!(present(&mut app, wid, (rows, cols), 2), Some(engine));
    }

    #[test]
    fn a_hidden_terminal_cursor_still_anchors_the_composition() {
        let (mut app, wid, rows, cols) = split_fixture();
        let session = app.windows[&wid].layouts[app.windows[&wid].tabs.active].focus();
        let term = app
            .pool
            .get(session)
            .expect("focused leaf terminal")
            .term
            .clone();
        crate::term_lock(&term).process(b"\x1b[?25l");
        assert_eq!(
            present(&mut app, wid, (rows, cols), 0),
            None,
            "control: a hidden cursor with no composition reports nothing"
        );
        app.on_ime_preedit(wid, "に".to_string(), None);
        assert!(
            present(&mut app, wid, (rows, cols), 1).is_some(),
            "the composition is on screen, so its caret rectangle must go out"
        );
    }
}

#[allow(
    clippy::items_after_test_module,
    reason = "these unit tests sit next to the ghost painter they cover; the rest of the file is the App render inherent-impl, not stray items"
)]
#[cfg(test)]
mod overlay_card_theme_tests {
    use crate::{App, WindowId};

    #[test]
    fn overlay_card_tracks_live_background_and_theme_fallback() {
        let mut app = App::headless_for_test();
        app.theme.bg = 0x00FF_FFFF; // a light user theme
        let wid0 = WindowId(0);
        if let Some(ws) = app.windows.get_mut(&wid0) {
            ws.input_scratch.default_bg = 0x0012_3456;
        }
        assert_eq!(
            app.overlay_card_theme(wid0).bg,
            0x0012_3456,
            "a terminal window's overlay still tracks the live OSC-11 bg"
        );
        if let Some(ws) = app.windows.get_mut(&wid0) {
            ws.input_scratch.default_bg = aterm_core::render::COLOR_UNSET;
        }
        assert_eq!(app.overlay_card_theme(wid0).bg, 0x00FF_FFFF);
    }
}

#[cfg(test)]
mod prediction_ghost_tests {
    use super::{
        paint_prediction_chain, paint_prediction_ghosts, paint_prediction_overlay,
        paint_predictions, terminal_blank_cell,
    };
    use crate::predict::{GhostCell, Prediction};
    use aterm_core::terminal::{RenderCell, Terminal};
    use aterm_render::RenderInput;

    fn blank() -> RenderCell {
        RenderCell {
            ch: ' ',
            fg: [200, 200, 200],
            bg: [0, 0, 0],
            wide: false,
            emoji_presentation: false,
            text_presentation: false,
            bold: false,
            italic: false,
            underline: aterm_core::terminal::UnderlineStyle::None,
            strikethrough: false,
            overline: false,
            underline_color: None,
            overline_color: None,
            script: aterm_core::terminal::Script::Normal,
        }
    }

    fn scratch(rows: usize) -> RenderInput {
        let mut s = RenderInput::empty();
        s.cells = vec![Vec::new(); rows];
        s
    }

    #[test]
    fn ghost_extends_row_and_dims() {
        let mut s = scratch(2);
        let preds = [Prediction::test_at(1, 3, 'x')];
        assert_eq!(
            paint_prediction_ghosts(&mut s, &preds, 10, blank()).newest,
            Some((1, 3))
        );
        assert_eq!(s.cells[1].len(), 4, "row extended to hold the ghost");
        let c = &s.cells[1][3];
        assert_eq!(c.ch, 'x');
        assert_eq!(c.fg, [100, 100, 100], "fg dimmed halfway to bg");
        assert!(!c.bold && !c.wide, "inherited SGR/geometry dropped");
    }

    #[test]
    fn ghost_sparse_padding_uses_the_live_terminal_blank_not_last_sgr() {
        let mut term = Terminal::new(1, 5);
        term.process(b"\x1b]10;rgb:11/22/33\x07\x1b]11;rgb:44/55/66\x07\x1b[?5h");
        let live_blank = terminal_blank_cell(&term);
        assert_eq!(
            (live_blank.fg, live_blank.bg),
            ([0x44, 0x55, 0x66], [0x11, 0x22, 0x33]),
            "DECSCNM-resolved live defaults"
        );
        let mut s = scratch(1);
        let mut styled = blank();
        styled.ch = '!';
        styled.bg = [180, 20, 30];
        styled.bold = true;
        s.cells[0].push(styled);

        assert!(
            paint_prediction_ghosts(&mut s, &[Prediction::test_at(0, 3, 'x')], 5, live_blank)
                .newest
                .is_some()
        );
        assert_eq!(s.cells[0][0], styled, "materialized SGR cell is preserved");
        assert_eq!(s.cells[0][1..3], [live_blank, live_blank], "implicit tail");
        assert_eq!(s.cells[0][3].bg, live_blank.bg);
        assert!(!s.cells[0][3].bold);
    }

    #[test]
    fn ghost_clips_to_pane_rect() {
        let mut s = scratch(2);
        let preds = [
            Prediction::test_at(0, 5, 'a'), // at cols → clipped
            Prediction::test_at(0, 9, 'b'), // past cols → clipped
            Prediction::test_at(7, 0, 'c'), // past rows → ignored
        ];
        assert_eq!(
            paint_prediction_ghosts(&mut s, &preds, 5, blank()).newest,
            None,
            "nothing painted when everything clips"
        );
        assert!(s.cells[0].is_empty(), "clipped ghosts never extend the row");
        let ok = [Prediction::test_at(0, 4, 'd')]; // last in-rect column paints
        assert_eq!(
            paint_prediction_ghosts(&mut s, &ok, 5, blank()).newest,
            Some((0, 4))
        );
        assert_eq!(s.cells[0][4].ch, 'd');
    }

    #[test]
    fn the_cursor_goes_past_the_newest_painted_guess_never_a_refused_one() {
        let row = || {
            let mut lead = blank();
            lead.ch = '日';
            let mut continuation = blank();
            continuation.wide = true;
            vec![blank(), lead, continuation]
        };
        let mut s = scratch(1);
        s.cells[0] = row();
        let preds = [
            Prediction::test_at(0, 0, 'b'),
            Prediction::test_at(0, 1, 'c'),
        ];
        assert_eq!(
            paint_prediction_ghosts(&mut s, &preds, 5, blank()),
            super::GhostsPainted {
                newest: Some((0, 0)),
                refused: 1,
            },
            "the newest PAINTED guess, not the refused newest, which is counted"
        );
        assert_eq!(s.cells[0][0].ch, 'b');
        assert_eq!(s.cells[0][1].ch, '日');
        assert!(s.cells[0][2].wide, "the glyph keeps its right half");

        let mut s = scratch(1);
        s.cells[0] = row();
        assert_eq!(
            paint_prediction_ghosts(&mut s, &preds[1..], 5, blank()).newest,
            None,
            "every guess refused: nothing to draw the cursor past"
        );
        assert_eq!(s.cells[0], row(), "and nothing painted");
    }

    #[test]
    fn a_frame_that_refused_an_older_guess_shows_no_key_through_the_newest() {
        let mut predictor = crate::predict::Predictor::new(crate::predict::PredictMode::Always);
        let now = std::time::Instant::now();
        for (col, ch) in (0u16..).zip(['a', 'b', 'c', 'd']) {
            assert!(predictor.predict_char(ch, (0, col), 0, 10, now));
        }
        let newest = predictor.shown_through();
        assert!(newest.is_some(), "fixture: Always shows all four");
        let wide = || {
            let mut lead = blank();
            lead.ch = '日';
            let mut continuation = blank();
            continuation.wide = true;
            vec![blank(), lead, continuation, blank()]
        };
        let mut s = scratch(1);
        s.cells[0] = wide();
        let painted = paint_prediction_overlay(&mut predictor, &mut s, now, 10, blank());
        assert_eq!(painted.cursor, Some((0, 4)), "one past the newest painted");
        assert_eq!(
            (s.cells[0][0].ch, s.cells[0][1].ch, s.cells[0][3].ch),
            ('a', '日', 'd'),
            "fixture: `b` and `c` refused, `a` and `d` painted"
        );
        assert_eq!(
            painted.shown_through, None,
            "the keys of `b` and `c` are not on glass"
        );
        let mut s = scratch(1);
        s.cells[0] = vec![blank(); 4];
        let painted = paint_prediction_overlay(&mut predictor, &mut s, now, 10, blank());
        assert_eq!(painted.shown_through, newest, "every guess painted");
        assert_eq!(
            painted.closes_input_through(),
            None,
            "`always` shows them with no proof: their keys wait for their echoes"
        );
    }

    fn wide_row() -> Vec<RenderCell> {
        let cell = |ch: char| RenderCell { ch, ..blank() };
        let mut continuation = blank();
        continuation.wide = true;
        vec![cell('a'), cell('b'), cell('日'), continuation]
    }

    #[test]
    fn a_state_chain_is_painted_all_or_none() {
        let mut s = scratch(1);
        s.cells[0] = wide_row();
        let refused = [
            Prediction::test_at(0, 1, 'x'),
            Prediction::test_at(0, 3, 'y'),
        ];
        assert_eq!(
            paint_prediction_chain(&mut s, &refused, (0, 1), 6, blank()),
            None
        );
        assert_eq!(s.cells[0], wide_row(), "nothing painted");
        assert_eq!(
            paint_prediction_chain(&mut s, &[], (0, 3), 6, blank()),
            None
        );
        assert_eq!(
            paint_prediction_chain(&mut s, &[], (0, 1), 6, blank()),
            Some((0, 1)),
            "a pure motion: its cursor, no cell"
        );
        assert_eq!(s.cells[0], wide_row());
        let erase = [Prediction::test_at(0, 1, ' ')];
        assert_eq!(
            paint_predictions(&mut s, &erase, Some((0, 1)), 6, blank()),
            super::OverlayPaint {
                cursor: Some((0, 1)),
                whole: true,
            },
            "the chain's cursor, on its vacated cell, the chain painted whole"
        );
        assert_eq!(s.cells[0][1].ch, ' ', "the vacated cell painted blank");
        let mut s = scratch(1);
        s.cells[0] = wide_row();
        assert_eq!(
            paint_predictions(&mut s, &erase, None, 6, blank()),
            super::OverlayPaint {
                cursor: Some((0, 2)),
                whole: true,
            },
            "the glyph lane: one past the newest painted guess, every guess painted"
        );
    }

    #[test]
    fn a_shift_is_painted_as_the_text_it_moves() {
        let cell = |ch: char| RenderCell { ch, ..blank() };
        let mut bold = cell('c');
        bold.bold = true;
        bold.fg = [200, 0, 0];
        let mut s = scratch(1);
        s.cells[0] = vec![cell('a'), cell('b'), bold, cell('d')];
        let shift = [
            Prediction::test_ghost(0, 1, 'x', GhostCell::Typed),
            Prediction::test_ghost(0, 2, 'b', GhostCell::Moved { from: 1 }),
            Prediction::test_ghost(0, 3, 'c', GhostCell::Moved { from: 2 }),
            Prediction::test_ghost(0, 4, 'd', GhostCell::Moved { from: 3 }),
        ];
        assert_eq!(
            paint_predictions(&mut s, &shift, Some((0, 2)), 6, blank()).cursor,
            Some((0, 2)),
            "the chain's cursor"
        );
        let text: String = s.cells[0].iter().map(|c| c.ch).collect();
        assert_eq!(text, "axbcd");
        assert_eq!(s.cells[0][3], bold, "`c` moved, in its own style");
        assert!(
            s.cells[0][1].fg != blank().fg && !s.cells[0][1].bold,
            "the typed glyph: a ghost"
        );

        let mut s = scratch(1);
        s.cells[0] = wide_row();
        let refused = [Prediction::test_ghost(
            0,
            1,
            '日',
            GhostCell::Moved { from: 2 },
        )];
        assert_eq!(
            paint_predictions(&mut s, &refused, Some((0, 1)), 6, blank()),
            super::OverlayPaint {
                cursor: None,
                whole: false,
            }
        );
        assert_eq!(s.cells[0], wide_row(), "nothing painted");
    }
}

#[cfg(test)]
#[path = "app_render_comet_trail_tests.rs"]
mod comet_trail_tests;

#[cfg(test)]
mod gpu_backpressure_tests {
    use super::gpu_backpressure_excess_ns;
    use std::time::Duration;

    #[test]
    fn backpressure_charges_only_the_wait_past_one_refresh() {
        let hz120 = Duration::from_micros(8333);
        let hz60 = Duration::from_micros(16667);
        for (wait_ns, fi, want) in [
            (0, hz120, 0),
            (1_000_000, hz120, 0),
            (8_333_000, hz120, 0),
            (10_333_000, hz120, 2_000_000),
            (25_000_000, hz120, 16_667_000),
            (25_000_000, hz60, 8_333_000),
        ] {
            assert_eq!(
                gpu_backpressure_excess_ns(wait_ns, fi),
                want,
                "{wait_ns} ns at {fi:?}"
            );
        }
    }
}

#[cfg(test)]
#[path = "app_render_motion_policy_tests.rs"]
mod motion_policy_tests;

pub(crate) fn captured_witness_neighbor<'a>(
    row: u16,
    caret_row: u16,
    above: Option<&'a [char]>,
    below: Option<&'a [char]>,
) -> Option<&'a [char]> {
    if caret_row.checked_sub(1) == Some(row) {
        return above;
    }
    if caret_row.checked_add(1) == Some(row) {
        return below;
    }
    None
}

fn needs_fresh_witness_read(
    rows: &[u16],
    caret_row: u16,
    pane_row: usize,
    pane_rows: usize,
    above: Option<&[char]>,
    below: Option<&[char]>,
    captured: &ComposedWitnessRows,
) -> bool {
    rows.iter().any(|&row| {
        row != caret_row
            && usize::from(row)
                .checked_sub(pane_row)
                .is_some_and(|local| local < pane_rows)
            && captured_witness_neighbor(row, caret_row, above, below).is_none()
            && captured.get(row).is_none()
    })
}

#[cfg(test)]
mod witness_row_capture_tests {
    use super::{ComposedWitnessRows, captured_witness_neighbor, needs_fresh_witness_read};

    #[test]
    fn composed_witness_rows_come_from_the_capture_or_a_fresh_read() {
        let above = ['═', 'A'];
        let below = [' ', 'B'];
        let none = ComposedWitnessRows::default();
        let (a, b) = (Some(&above[..]), Some(&below[..]));
        assert_eq!(captured_witness_neighbor(4, 5, a, b), a);
        assert_eq!(captured_witness_neighbor(6, 5, a, b), b);
        assert_eq!(captured_witness_neighbor(8, 5, a, b), None);
        assert_eq!(captured_witness_neighbor(4, 5, None, b), None);
        assert!(!needs_fresh_witness_read(&[4, 5, 6], 5, 4, 4, a, b, &none));
        assert!(needs_fresh_witness_read(
            &[4, 5, 6, 7],
            5,
            4,
            4,
            a,
            b,
            &none
        ));
        assert!(needs_fresh_witness_read(
            &[4, 5, 6],
            5,
            4,
            4,
            None,
            b,
            &none
        ));
    }
}

/// One frame's terminal-state snapshot for [`App::cursor_frame`].
pub(crate) struct CursorFxInputs {
    /// The animation clock (one `Instant` per redraw / capture).
    pub now: Instant,
    /// Window grid rows (terminal content — no tab-strip rows).
    pub rows: usize,
    /// Window grid cols.
    pub cols: usize,
    /// The cursor cell.
    pub cur: Option<(u16, u16)>,
    /// Whether this frame presents the active grid rather than retained scrollback.
    pub live_viewport: bool,
    /// Raw cursor visibility (dectcem).
    pub cursor_visible: bool,
    /// [`aterm_effects::host::hidden_caret_withheld`].
    pub hidden_withheld: bool,
    /// Live cursor shape (the rainbow cursor requires a block).
    pub cursor_style: CursorStyle,
    /// This window's GUI blink phase.
    pub blink_phase: bool,
    /// Effective live cursor colour.
    pub live_cursor_rgb: [u8; 3],
    /// Whether `live_cursor_rgb` was pinned by the user (`cursor_color` in the config) or a program (a live OSC 12, `Terminal::cursor_color_recoloured`) rather than seeded by the theme.
    pub cursor_color_pinned: bool,
    /// Live default background (decscnm-folded).
    pub default_bg: u32,
    /// Row probe `(row, caret, trust)` for this frame.
    pub row_probe: Option<(u16, u16, aterm_effects::cursor_glow::ProbeTrust)>,
    /// Flanking-row presence as [`capture_cursor_neighbor_rows`] answered it under the same lock as `row_probe`.
    pub row_probe_neighbors: Option<(bool, bool)>,
    /// The live default foreground (`0x00RRGGBB`), the twin of `default_bg`.
    pub default_fg: u32,
    /// The echo anchor (`Terminal::print_anchor`).
    pub print_anchor: Option<(u16, u16, u64)>,
    /// The glyph at that print run's last cell (`Terminal::print_anchor_glyph`), same hold and live-viewport guard.
    pub print_anchor_glyph: Option<char>,
}

#[cfg(test)]
impl CursorFxInputs {
    /// A neutral frame for tests that drive [`App::cursor_frame`] without caring about the terminal's exact state.
    pub(crate) fn sample_for_test(now: Instant) -> Self {
        Self {
            now,
            rows: 24,
            cols: 80,
            cur: Some((0, 0)),
            live_viewport: true,
            cursor_visible: true,
            hidden_withheld: false,
            cursor_style: CursorStyle::SteadyBlock,
            blink_phase: true,
            live_cursor_rgb: [255, 255, 255],
            cursor_color_pinned: false,
            default_bg: Theme::default().bg,
            row_probe: None,
            row_probe_neighbors: None,
            default_fg: Theme::default().fg,
            print_anchor: None,
            print_anchor_glyph: None,
        }
    }
}

#[cfg(test)]
#[path = "app_render_sync_cursor_fx_hold_tests.rs"]
mod sync_cursor_fx_hold_tests;

pub(crate) struct CursorFxTick {
    pub win_focused: bool,
    pub motion: crate::motion::MotionPolicy,
    pub cursor_motion: crate::motion::MotionPolicy,
    pub scroll_motion: crate::motion::MotionPolicy,
    pub shed_envelope: f32,
    pub glow_cfg: crate::cursor_glow::GlowConfig,
    pub trail_color: u32,
    pub glow_cw: usize,
    pub glow_ch: usize,
    pub glow_geom: crate::cursor_glow::Geom,
    pub glow_fp: u64,
    pub trail_fp: u64,
    pub block_fill: Option<crate::cursor_glow::BlockFill>,
    pub bolt_cursor: bool,
    pub twinkle_cursor: bool,
    pub momentum_steady: Option<CursorStyle>,
}

/// Clock ownership for one composed cursor-effect projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ComposedCursorFxClock {
    Advance(Instant),
    Retain { observed_at: Instant },
}

impl ComposedCursorFxClock {
    const fn observed_at(self) -> Instant {
        match self {
            Self::Advance(now) | Self::Retain { observed_at: now } => now,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ComposedWitnessStamp {
    terminal_id: u64,
    content_seq: u64,
    display_offset: usize,
    alternate_screen: bool,
    scroll: ContentScrollState,
    sync_active: bool,
    sync_end_seq: u64,
}

impl ComposedWitnessStamp {
    const fn read(terminal: &Terminal) -> Self {
        Self {
            terminal_id: terminal.render_identity(),
            content_seq: terminal.content_seq(),
            display_offset: terminal.grid().display_offset(),
            alternate_screen: terminal.is_alternate_screen(),
            scroll: terminal.content_scroll_state(),
            sync_active: terminal.modes().synchronized_output(),
            sync_end_seq: terminal.sync_end_seq(),
        }
    }

    fn matches(self, terminal: &Terminal) -> bool {
        self == Self::read(terminal)
    }
}

struct FocusedComposedCursorFxSample {
    session: u64,
    witness_stamp: ComposedWitnessStamp,
    witness_rows: ComposedWitnessRows,
    terminal_id: u64,
    cursor: (u16, u16),
    cursor_visible: bool,
    cursor_style: CursorStyle,
    display_offset: usize,
    live_cursor_rgb: [u8; 3],
    cursor_recoloured: bool,
    default_bg: u32,
    default_fg: u32,
    alt: bool,
    blink_epoch: u64,
    content_scroll_state: ContentScrollState,
    row_probe: Vec<char>,
    row_above_probe: Vec<char>,
    row_below_probe: Vec<char>,
    row_above_present: bool,
    row_below_present: bool,
    print_anchor: Option<(u16, u16, u64)>,
    print_anchor_glyph: Option<char>,
}

#[derive(Default)]
pub(crate) struct ComposedWitnessRows {
    slots: Vec<ComposedWitnessRow>,
    active: usize,
}

#[derive(Default)]
struct ComposedWitnessRow {
    row: u16,
    cols: Vec<char>,
}

impl ComposedWitnessRows {
    fn capture(
        &mut self,
        terminal: &Terminal,
        requested: &[u16],
        carried: u16,
        caret_row: u16,
        pane_row: usize,
        pane_col: usize,
    ) {
        self.active = 0;
        let grid_rows = usize::from(terminal.grid().rows());
        let caret_row = pane_row.saturating_add(usize::from(caret_row));
        let shifted = requested
            .iter()
            .filter(|_| carried > 0)
            .filter_map(|&row| row.checked_sub(carried));
        for row in requested.iter().copied().chain(shifted) {
            if self.get(row).is_some() {
                continue;
            }
            let row_index = usize::from(row);
            let Some(local) = row_index.checked_sub(pane_row) else {
                continue;
            };
            if local >= grid_rows
                || row_index == caret_row
                || caret_row.checked_sub(1) == Some(row_index)
                || caret_row.checked_add(1) == Some(row_index)
            {
                continue;
            }
            if self.active == self.slots.len() {
                self.slots.push(ComposedWitnessRow::default());
            }
            let slot = &mut self.slots[self.active];
            slot.row = row;
            terminal.row_cols_into(local, &mut slot.cols);
            if pane_col > 0 {
                let len = slot.cols.len();
                slot.cols.resize(len.saturating_add(pane_col), ' ');
                slot.cols.rotate_right(pane_col);
            }
            self.active += 1;
        }
    }

    fn get(&self, row: u16) -> Option<&[char]> {
        self.slots[..self.active]
            .iter()
            .find(|slot| slot.row == row)
            .map(|slot| slot.cols.as_slice())
    }
}

/// The flanking rows feed Rainbow Kitty (v2's text-first star landing gate, the seam's content witnesses, the abandoned band's witness) and every other style's over-text rules (Fire's over-ink ledger and light flame cap, the particles that keep off the text).
pub(crate) fn capture_cursor_neighbor_rows(
    terminal: &Terminal,
    row: usize,
    rows: usize,
    reads: bool,
    above: &mut Vec<char>,
    below: &mut Vec<char>,
) -> (bool, bool) {
    let above_present = reads && row > 0;
    let below_present = reads && row < rows.saturating_sub(1);
    if above_present {
        terminal.row_cols_into(row - 1, above);
    } else {
        above.clear();
    }
    if below_present {
        terminal.row_cols_into(row + 1, below);
    } else {
        below.clear();
    }
    (above_present, below_present)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SinglePaneRowProbeKey {
    terminal_id: u64,
    alternate_screen: bool,
    content_seq: u64,
    grid_content_gen: u64,
    process_sequence: u32,
    display_offset: usize,
    base_y: usize,
    absolute_row_revision: u64,
    history_renumber_epoch: u64,
    scroll_up_rows: u64,
    scroll_invalidation_epoch: u64,
    scroll_band_seq: u64,
    host_rows: usize,
    host_cols: usize,
    term_rows: u16,
    term_cols: u16,
    cursor_row: usize,
    reads_neighbor_rows: bool,
}

impl SinglePaneRowProbeKey {
    fn read(
        terminal: &Terminal,
        cursor_row: usize,
        host_rows: usize,
        host_cols: usize,
        reads_neighbor_rows: bool,
    ) -> Self {
        let grid = terminal.grid();
        let scroll = terminal.content_scroll_state();
        Self {
            terminal_id: terminal.render_identity(),
            alternate_screen: terminal.is_alternate_screen(),
            content_seq: terminal.content_seq(),
            grid_content_gen: grid.content_gen(),
            process_sequence: terminal.pipeline_timestamps().process_sequence,
            display_offset: grid.display_offset(),
            base_y: grid.base_y(),
            absolute_row_revision: terminal.absolute_row_revision(),
            history_renumber_epoch: grid.history_renumber_epoch(),
            scroll_up_rows: scroll.uniform_up_rows,
            scroll_invalidation_epoch: scroll.invalidation_epoch,
            scroll_band_seq: scroll.band_seq,
            host_rows,
            host_cols,
            term_rows: terminal.rows(),
            term_cols: terminal.cols(),
            cursor_row,
            reads_neighbor_rows,
        }
    }
}

/// A private copy, since composed rendering and headless capture may move or overwrite the window's `poof_*` scratch.
#[derive(Debug, Default)]
pub(crate) struct SinglePaneRowProbeCache {
    key: Option<SinglePaneRowProbeKey>,
    caret: Vec<char>,
    above: Vec<char>,
    below: Vec<char>,
    above_present: bool,
    below_present: bool,
}

impl SinglePaneRowProbeCache {
    const fn invalidate(&mut self) {
        self.key = None;
    }

    fn sample(
        &mut self,
        terminal: &Terminal,
        cursor_row: usize,
        host_rows: usize,
        host_cols: usize,
        reads_neighbor_rows: bool,
        probes: (&mut Vec<char>, &mut Vec<char>, &mut Vec<char>),
    ) -> (bool, bool, bool) {
        let (caret, above, below) = probes;
        let key = SinglePaneRowProbeKey::read(
            terminal,
            cursor_row,
            host_rows,
            host_cols,
            reads_neighbor_rows,
        );
        if self.key == Some(key) {
            caret.clone_from(&self.caret);
            above.clone_from(&self.above);
            below.clone_from(&self.below);
            return (true, self.above_present, self.below_present);
        }

        terminal.row_cols_into(cursor_row, caret);
        let (above_present, below_present) = capture_cursor_neighbor_rows(
            terminal,
            cursor_row,
            host_rows,
            reads_neighbor_rows,
            above,
            below,
        );
        self.caret.clone_from(caret);
        self.above.clone_from(above);
        self.below.clone_from(below);
        self.above_present = above_present;
        self.below_present = below_present;
        self.key = Some(key);
        (false, above_present, below_present)
    }
}

#[cfg(test)]
mod single_pane_row_probe_cache_tests {
    use super::SinglePaneRowProbeCache;
    use aterm_core::terminal::Terminal;

    fn sample(
        cache: &mut SinglePaneRowProbeCache,
        term: &Terminal,
        row: usize,
        host_rows: usize,
        host_cols: usize,
        v2: bool,
        probes: &mut [Vec<char>; 3],
    ) -> (bool, bool, bool) {
        let [caret, above, below] = probes;
        cache.sample(term, row, host_rows, host_cols, v2, (caret, above, below))
    }

    #[test]
    fn the_row_probe_cache_restores_on_a_hit_and_every_key_term_breaks_reuse() {
        let mut term = Terminal::new(4, 12);
        term.process(b"above\r\ncaret\r\nbelow");
        let mut cache = SinglePaneRowProbeCache::default();
        let mut probes = [Vec::new(), Vec::new(), Vec::new()];
        let rows = |probes: &[Vec<char>; 3]| {
            probes
                .iter()
                .map(|p| p.iter().take(5).collect::<String>())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            sample(&mut cache, &term, 1, 4, 12, true, &mut probes),
            (false, true, true)
        );
        assert_eq!(rows(&probes), ["caret", "above", "below"]);
        for probe in &mut probes {
            probe.clear();
            probe.extend(['X'; 12]);
        }
        assert_eq!(
            sample(&mut cache, &term, 1, 4, 12, true, &mut probes),
            (true, true, true)
        );
        assert_eq!(
            rows(&probes),
            ["caret", "above", "below"],
            "restored on a hit"
        );

        let mut hit = |term: &Terminal, row, host: (usize, usize), v2| {
            sample(&mut cache, term, row, host.0, host.1, v2, &mut probes).0
        };
        assert!(!hit(&term, 2, (4, 12), true), "a different cursor row");
        assert!(!hit(&term, 2, (4, 13), true), "host geometry");
        assert!(!hit(&term, 2, (4, 13), false), "neighbor ownership");
        assert!(hit(&term, 2, (4, 13), false), "control: unchanged");
        term.process(b"Z");
        assert!(!hit(&term, 2, (4, 13), false), "new content");
        term.resize(5, 13);
        assert!(!hit(&term, 2, (5, 13), false), "a resize");
        term.process(b"\r\none\r\ntwo\r\nthree\r\nfour\r\nfive\r\n");
        assert!(!hit(&term, 1, (5, 13), false));
        term.scroll_display(1);
        assert!(term.grid().display_offset() > 0);
        assert!(!hit(&term, 1, (5, 13), false), "a different viewport");
        let other = Terminal::new(5, 13);
        assert!(
            !hit(&other, 1, (5, 13), false),
            "another terminal (a tab switch)"
        );
    }
}

#[cfg(test)]
mod composed_witness_scroll_capture_tests {
    use super::{ComposedWitnessRows, focused_composed_cursor_fx_sample};
    use aterm_core::terminal::Terminal;

    fn witness_rows(
        term: &Terminal,
        previous: aterm_core::terminal::ContentScrollState,
        requested: &[u16],
    ) -> ComposedWitnessRows {
        focused_composed_cursor_fx_sample(
            1,
            term,
            true,
            true,
            true,
            (0, 0),
            requested,
            Some(previous),
            ComposedWitnessRows::default(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .witness_rows
    }

    fn row_text(term: &Terminal, row: u16) -> Vec<char> {
        let mut cols = Vec::new();
        term.row_cols_into(usize::from(row), &mut cols);
        cols
    }

    #[test]
    fn a_whole_grid_scroll_reads_each_witness_row_where_asked_and_where_carried() {
        let mut term = Terminal::new(10, 20);
        for i in 0..10 {
            term.process(format!("line {i}\r\n").as_bytes());
        }
        let still = term.content_scroll_state();
        let rows = witness_rows(&term, still, &[5]);
        assert_eq!(rows.get(5), Some(&row_text(&term, 5)[..]));
        assert_eq!(rows.get(2), None, "no scroll: the asked row alone");

        term.process(b"step 1\r\nstep 2\r\nstep 3\r\n");
        let rows = witness_rows(&term, still, &[5]);
        assert_eq!(rows.get(5), Some(&row_text(&term, 5)[..]), "where asked");
        assert_eq!(
            rows.get(2),
            Some(&row_text(&term, 2)[..]),
            "and the scroll's three rows higher"
        );

        let banded = term.content_scroll_state();
        term.process(b"\x1b[2;6r\x1b[6;1H\r\n\x1b[r");
        let rows = witness_rows(&term, banded, &[5]);
        assert_eq!(
            (rows.get(5), rows.get(2)),
            (None, None),
            "a band move: none"
        );
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "one frame's capture inputs plus the three probe buffers it reuses, moved in and \
              handed back in the sample"
)]
fn focused_composed_cursor_fx_sample(
    session: u64,
    terminal: &Terminal,
    capture_row_probes: bool,
    v2_owns_frame: bool,
    reads_neighbor_rows: bool,
    pane_origin: (usize, usize),
    requested_witness_rows: &[u16],
    previous_scroll: Option<ContentScrollState>,
    mut witness_rows: ComposedWitnessRows,
    mut row_probe: Vec<char>,
    mut row_above_probe: Vec<char>,
    mut row_below_probe: Vec<char>,
) -> FocusedComposedCursorFxSample {
    let cursor = terminal.cursor();
    let display_offset = terminal.grid().display_offset();
    let content_scroll_state = terminal.content_scroll_state();
    let row = usize::from(cursor.row);
    let rows = usize::from(terminal.grid().rows());
    let probe_live = capture_row_probes && display_offset == 0;
    let (row_above_present, row_below_present) = if probe_live {
        terminal.row_cols_into(row, &mut row_probe);
        capture_cursor_neighbor_rows(
            terminal,
            row,
            rows,
            reads_neighbor_rows,
            &mut row_above_probe,
            &mut row_below_probe,
        )
    } else {
        row_probe.clear();
        row_above_probe.clear();
        row_below_probe.clear();
        (false, false)
    };
    let carried = match ContentScrollState::delta_since(previous_scroll, content_scroll_state) {
        ContentScrollDelta::Baseline | ContentScrollDelta::Unchanged => Some(0),
        ContentScrollDelta::Translate(rows) => Some(rows),
        ContentScrollDelta::Bands { .. } | ContentScrollDelta::Invalidate => None,
    };
    if probe_live
        && v2_owns_frame
        && let Some(carried) = carried
    {
        witness_rows.capture(
            terminal,
            requested_witness_rows,
            carried,
            cursor.row,
            pane_origin.0,
            pane_origin.1,
        );
    } else {
        witness_rows.active = 0;
    }
    let (print_anchor, print_anchor_glyph) = aterm_effects::host::motion_print_anchor(terminal);
    FocusedComposedCursorFxSample {
        session,
        witness_stamp: ComposedWitnessStamp::read(terminal),
        witness_rows,
        terminal_id: terminal.render_identity(),
        cursor: (cursor.row, cursor.col),
        cursor_visible: terminal.cursor_visible(),
        cursor_style: terminal.cursor_style(),
        display_offset,
        live_cursor_rgb: terminal_cursor_rgb(terminal),
        cursor_recoloured: terminal.cursor_color_recoloured(),
        default_bg: aterm_render::rgb_to_u32(terminal_blank_cell(terminal).bg),
        default_fg: aterm_render::rgb_to_u32(terminal_blank_cell(terminal).fg),
        alt: terminal.is_alternate_screen(),
        blink_epoch: terminal.repaint_blink_epoch(),
        content_scroll_state,
        row_probe,
        row_above_probe,
        row_below_probe,
        row_above_present,
        row_below_present,
        print_anchor,
        print_anchor_glyph,
    }
}

fn lower_bound_match_row(matches: &[(i32, u16, u32)], target: i64) -> (usize, usize) {
    let mut lo = 0;
    let mut hi = matches.len();
    let mut comparisons = 0;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        comparisons += 1;
        if i64::from(matches[mid].0) < target {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    (lo, comparisons)
}

fn visible_match_range(
    matches: &[(i32, u16, u32)],
    offset: i64,
    term_rows: usize,
    span_rows: i32,
) -> (Range<usize>, usize) {
    let lower_row = offset
        .saturating_neg()
        .saturating_sub(i64::from(span_rows.max(0)));
    let upper_row = i64::try_from(term_rows)
        .unwrap_or(i64::MAX)
        .saturating_sub(offset);
    let (start, lower_comparisons) = lower_bound_match_row(matches, lower_row);
    let (end, upper_comparisons) = lower_bound_match_row(matches, upper_row);
    (start..end, lower_comparisons + upper_comparisons)
}

fn delivery_for_window(
    id: WindowId,
    at: Instant,
    ticket: DeliveryTicket,
) -> aterm_effects::pipeline::Delivery {
    aterm_effects::pipeline::Delivery {
        at,
        dispatched_at: ticket.dispatched_at,
        insert: ticket.insert,
        key: ticket.key.filter(|_| ticket.window == Some(id)),
    }
}

fn send_window_reports(s: &crate::Session, reports: Option<Vec<u8>>) {
    if let Some(report) = reports {
        crate::app_input::write_ui_report(
            &s.term,
            &s.ctx.modes,
            &s.ctx.sink,
            &crate::input::InputEvent::KeySequence(report),
        );
    }
}

impl App {
    /// The predictive-echo mode, resolved once per config generation (the cache is invalidated by `reload_config`) rather than re-parsed per keystroke and per presented frame.
    pub(crate) fn predict_mode(&mut self) -> crate::predict::PredictMode {
        match self.predict_mode_cache {
            Some(m) => m,
            None => {
                let m =
                    crate::predict::PredictMode::parse(self.config.predictive_echo_or_default());
                self.predict_mode_cache = Some(m);
                m
            }
        }
    }

    /// Metrics for a logical window that has not reached glass attach yet.
    pub(crate) fn unattached_window_metrics(&self) -> crate::MetricsView {
        debug_assert!(
            !self.backend.is_pending(),
            "post-startup logical window creation requires the ready renderer"
        );
        crate::MetricsView::applied(
            self.font_px,
            self.backend.pad(),
            self.backend.pad_top(),
            self.backend.head(),
        )
    }

    /// Resize every pane of every tab of window `wid`'s engine and PTY to its computed sub-rect.
    #[track_caller]
    pub(crate) fn resize_panes(&mut self, wid: WindowId) {
        self.resize_panes_scoped(wid, self.resize_live_drag);
    }

    /// The body of [`Self::resize_panes`].
    #[track_caller]
    pub(crate) fn resize_panes_scoped(&mut self, wid: WindowId, active_only: bool) {
        let site = crate::resize_ledger::Site::caller(
            crate::resize_ledger::Cause::Pass,
            std::panic::Location::caller(),
        );
        self.resize_panes_scoped_with_active_plan(wid, active_only, None, site);
    }

    fn resize_panes_scoped_with_active_plan(
        &mut self,
        wid: WindowId,
        active_only: bool,
        active_plan: Option<&crate::tab_model::VisibleLeafPlan>,
        site: crate::resize_ledger::Site,
    ) {
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        let (rows, cols) = (ws.rows, ws.cols);
        let attribution = crate::resize_ledger::Attribution {
            site: crate::resize_ledger::SiteScope::current().unwrap_or(site),
            win: Some((cols, rows)),
            chrome: Some(self.chrome_rows(wid)),
        };
        let active = ws.tab_set.active_index().unwrap_or(0);
        let ntabs = ws.tab_set.len();
        let pane_cell_px = self.spawn_cell_px(wid);
        let mut targets: Vec<(usize, u64, u16, u16)> = Vec::new();
        for (ti, tab) in ws.tab_set.tabs().iter().enumerate() {
            // Under `active_only`, skip laying out background tabs the second loop
            // would discard anyway (`visible_plan` allocates per leaf and divider,
            // and `redraw_window` makes this call every frame of a live drag). The
            // predicate is the second loop's (`pool.views(id) > 1`): a background
            // tab is laid out iff some leaf holds a shared session. The per-pane
            // `continue` below stays, since such a tab can also hold non-shared
            // siblings.
            if active_only
                && ti != active
                && !tab.root.any_leaf(&mut |view| {
                    self.view_store
                        .get(*view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        .and_then(|session| self.pool.views(session))
                        .is_some_and(|views| views > 1)
                })
            {
                continue;
            }
            let owned_plan;
            let plan = if ti == active
                && let Some(plan) = active_plan
            {
                plan
            } else {
                owned_plan = self.plan_tab(tab, rows.max(1), cols.max(1));
                &owned_plan
            };
            for leaf in &plan.leaves {
                let Some(session) = self
                    .view_store
                    .get(leaf.view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                else {
                    continue;
                };
                let (sub_rows, sub_cols) = Self::leaf_grid_cells(&leaf.rect);
                targets.push((ti, session, sub_rows, sub_cols));
            }
        }
        let reflow_wake_proxy = self.proxy.clone();
        let mut shared_changed: Vec<u64> = Vec::new();
        let split_hold_now = (!self.split_regrid_holds.is_empty()).then(Instant::now);
        let split_hold_cause = self.split_regrid_cause;
        for (ti, id, sub_rows, sub_cols) in targets {
            let shared = self.pool.views(id).is_some_and(|v| v > 1);
            if active_only && ti != active && !shared {
                continue;
            }
            let (sub_rows, sub_cols) = self.pane_target_grid(id, (sub_rows, sub_cols));
            if let Some((term, ui_waiting)) = self
                .pool
                .get(id)
                .map(|s| (s.term.clone(), s.ctx.ui_waiting.clone()))
            {
                let gliding: Vec<WindowId> = self
                    .windows
                    .iter()
                    .filter(|(_, ws)| {
                        ws.scroll_glide
                            .as_ref()
                            .is_some_and(|st| Arc::ptr_eq(&st.term, &term))
                    })
                    .map(|(glide_wid, _)| *glide_wid)
                    .collect();
                if !gliding.is_empty()
                    && crate::term_lock_ui(&term, &ui_waiting, crate::metrics::TermWaitSite::Redraw)
                        .cols()
                        != sub_cols
                {
                    let now = Instant::now();
                    for glide_wid in gliding {
                        self.settle_scroll_motion_at_target(glide_wid, now);
                    }
                }
            }
            let Some(s) = self.pool.get(id) else { continue };
            let policy = pty_resize_policy(s.master);
            let (pending, journal, cast_stamp) = {
                let site = crate::metrics::TermWaitSite::Redraw;
                let mut term = crate::term_lock_ui(&s.term, &s.ctx.ui_waiting, site);
                if let Some((cw, ch)) = pane_cell_px {
                    term.set_cell_pixel_size(cw, ch);
                    s.ctx.crash_journal.host_settings(&term, None);
                }
                if term.rows() == sub_rows && term.cols() == sub_cols {
                    let reports = term.take_reports();
                    drop(term);
                    send_window_reports(s, reports);
                    continue;
                }
                if split_hold_now.is_some_and(|now| {
                    self.split_regrid_holds
                        .judge(id, (sub_rows, sub_cols), split_hold_cause, now)
                        == crate::split_regrid_hold::HoldVerdict::Withhold
                }) {
                    continue;
                }
                let pending = term
                    .resize_offloading_scrollback_with_policy(sub_rows, sub_cols, policy)
                    .map(|pending| {
                        let rewrap = s.ctx.rewrap_gauge.begin_job(pending.lines_total());
                        (pending, rewrap)
                    });
                s.ctx.crash_journal.host_resized(&term);
                (
                    pending,
                    crate::resize_ledger::JournalCopy::take(&term),
                    crate::cast::ResizeStamp::take(&term),
                )
            };
            if let Some((pending, rewrap)) = pending {
                let job = std::sync::Arc::new(std::sync::Mutex::new(Some((
                    pending,
                    s.term.clone(),
                    reflow_wake_proxy.clone(),
                    rewrap,
                ))));
                let cancel = s.reflow_cancel.clone();
                let run = {
                    let job = job.clone();
                    move || {
                        if let Some((pending, term, proxy, rewrap)) =
                            job.lock().unwrap_or_else(|p| p.into_inner()).take()
                        {
                            let _running = reflow_gauge::enter();
                            let mut next = Some(pending);
                            let mut ended = aterm_messages::WaitEnd::Done;
                            while let Some(active) = next.take() {
                                let pass_lines = active.lines_total();
                                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    drive_reflow_job_gauged(
                                        active,
                                        &cancel,
                                        REFLOW_WORKER_STEP_LINES,
                                        Some(rewrap.gauge()),
                                    )
                                })) {
                                    Ok(Some(reflowed)) => {
                                        next = term_lock(&term).finish_resize_offload(reflowed);
                                        rewrap.gauge().pass_done(
                                            pass_lines,
                                            next.as_ref().map(|n| n.lines_total()),
                                        );
                                    }
                                    Ok(None) => {
                                        ended = aterm_messages::WaitEnd::Gone;
                                        aterm_log::info!(
                                            "reflow worker cancelled rewrapping session {id} \
                                             scrollback (session teardown); aborting the \
                                             offload (detached history released, grid \
                                             recovered to ring-only)"
                                        );
                                        term_lock(&term).abort_resize_offload();
                                    }
                                    Err(_) => {
                                        ended = aterm_messages::WaitEnd::Failed;
                                        aterm_log::error!(
                                            "reflow worker panicked rewrapping session {id} \
                                             scrollback; aborting the offload (tiered history \
                                             lost, grid recovered to ring-only)"
                                        );
                                        term_lock(&term).abort_resize_offload();
                                    }
                                }
                            }
                            rewrap.finish(ended);
                            if let Some(proxy) = proxy {
                                let _ = proxy.send_event(crate::Wake::Output {
                                    session: id,
                                    window: wid,
                                });
                            }
                        }
                    }
                };
                reflow_gauge::submit();
                let inline = move || {
                    let Some((pending, term, proxy, rewrap)) =
                        job.lock().unwrap_or_else(|p| p.into_inner()).take()
                    else {
                        // Distinct branch.
                        return; // a pooled worker already took this job
                    };
                    const INLINE_REFLOW_MAX_LINES: usize = 20_000;
                    let mut ended = aterm_messages::WaitEnd::Done;
                    if pending.line_count() > INLINE_REFLOW_MAX_LINES {
                        ended = aterm_messages::WaitEnd::Failed;
                        aterm_log::error!(
                            "no reflow worker could be started and session {id} history \
                             ({} lines) is too large to rewrap inline without a main-thread \
                             freeze; dropping tiered scrollback (grid recovered to ring-only)",
                            pending.line_count()
                        );
                        drop(pending);
                        term_lock(&term).abort_resize_offload();
                    } else {
                        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            pending.reflow()
                        })) {
                            Ok(reflowed) => {
                                let mut next = term_lock(&term).finish_resize_offload(reflowed);
                                while let Some(follow) = next {
                                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                                        || follow.reflow(),
                                    )) {
                                        Ok(again) => {
                                            next = term_lock(&term).finish_resize_offload(again);
                                        }
                                        Err(_) => {
                                            ended = aterm_messages::WaitEnd::Failed;
                                            aterm_log::error!(
                                                "inline convergence rewrap of session {id} \
                                                 panicked; aborting the offload (grid \
                                                 recovered)"
                                            );
                                            term_lock(&term).abort_resize_offload();
                                            break;
                                        }
                                    }
                                }
                                aterm_log::warn!(
                                    "no reflow worker could be started; rewrapped session \
                                     {id} scrollback synchronously on the main thread \
                                     (history preserved)"
                                );
                            }
                            Err(_) => {
                                ended = aterm_messages::WaitEnd::Failed;
                                aterm_log::error!(
                                    "no reflow worker could be started AND the inline rewrap \
                                     of session {id} panicked; aborting the offload (grid \
                                     recovered)"
                                );
                                term_lock(&term).abort_resize_offload();
                            }
                        }
                    }
                    reflow_gauge::note_inline();
                    rewrap.finish(ended);
                    if let Some(proxy) = proxy {
                        let _ = proxy.send_event(crate::Wake::Output {
                            session: id,
                            window: wid,
                        });
                    }
                };
                for stranded in reflow_pool_submit(ReflowHandoff {
                    run: Box::new(run),
                    inline: Box::new(inline),
                    priority: ti == active,
                }) {
                    (stranded.inline)();
                }
            }
            if shared {
                shared_changed.push(id);
            }
            aterm_pty::resize_with_cell_px(s.master, sub_rows, sub_cols, pane_cell_px);
            s.ctx
                .cast
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record_resize_stamped(cast_stamp, sub_cols, sub_rows);
            s.ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .ingest_resizes(&journal, Some(attribution), crate::turn_ledger::now_ms());
        }
        for id in shared_changed {
            let viewers = self
                .windows
                .keys()
                .copied()
                .filter(|window| self.window_contains_session(*window, id))
                .collect::<Vec<_>>();
            for window in viewers {
                if let Some(w) = self
                    .windows
                    .get(&window)
                    .and_then(|state| state.os_window.as_ref())
                {
                    w.request_redraw();
                }
            }
        }
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.panes_stale = active_only && ntabs > 1;
        }
    }

    /// The glyph cell size in pixels, from the live rasterizer (GPU's internal CPU face, or the standalone CPU renderer).
    pub(crate) fn cell_size(&self) -> (usize, usize) {
        self.backend.cell_size()
    }

    /// The cell size (px) for window `wid` from its own [`MetricsView`] (`font_px`), resolved against the shared faces via [`crate::Backend::cell_geometry`], so it is right even while the shared renderer is activated to another window's size.
    pub(crate) fn win_cell_size(&self, wid: WindowId) -> (usize, usize) {
        match self.windows.get(&wid) {
            Some(ws) => {
                let (cw, ch, _) = self.backend.cell_geometry(ws.metrics.font_px);
                (cw, ch)
            }
            None => self.backend.cell_size(),
        }
    }

    /// Window `wid`'s cell size as the engine wants it, a `(w, h)` pixel pair clamped into `u16`, or `None` while the backend build is still in flight (a `Pending` slot panics on any metric read, and the Boot session is spawned before the join on a windowed launch).
    pub(crate) fn spawn_cell_px(&self, wid: WindowId) -> Option<(u16, u16)> {
        if self.backend.is_pending() {
            return None;
        }
        let (cw, ch) = self.win_cell_size(wid);
        Some((
            u16::try_from(cw).unwrap_or(u16::MAX).max(1),
            u16::try_from(ch).unwrap_or(u16::MAX).max(1),
        ))
    }

    /// Report window `wid`'s real cell pixel size to every engine it hosts, so DEC 1016 (SGR-pixel) mouse reports and OSC 1337 `File=` images use the font on glass.
    pub(crate) fn sync_cell_pixel_size(&mut self, wid: WindowId) {
        let Some(px) = self.spawn_cell_px(wid) else {
            return;
        };
        if self
            .windows
            .get(&wid)
            .is_none_or(|ws| ws.cell_px_reported == Some(px))
        {
            return;
        }
        for id in self.window_terminal_sessions(wid) {
            if let Some(s) = self.pool.get(id) {
                let (grid, pulse_holds, reports) = {
                    let site = crate::metrics::TermWaitSite::Redraw;
                    let mut term = crate::term_lock_ui(&s.term, &s.ctx.ui_waiting, site);
                    term.set_cell_pixel_size(px.0, px.1);
                    s.ctx.crash_journal.host_settings(&term, None);
                    (
                        (term.rows(), term.cols()),
                        s.adopt_pulse_hold
                            .load(std::sync::atomic::Ordering::Acquire),
                        term.take_reports(),
                    )
                };
                if !pulse_holds {
                    // The PTY learns it too, with the same rows/cols and new pixels,
                    // so `TIOCGWINSZ` is right for the boot session (spawned at 0x0
                    // before the backend build joined) and after a font zoom or DPI
                    // change that keeps rows/cols (where `apply_term_resize` returns
                    // early). Memoized on `cell_px_reported`, so it cannot churn
                    // SIGWINCH.
                    aterm_pty::resize_with_cell_px(s.master, grid.0, grid.1, Some(px));
                }
                send_window_reports(s, reports);
            }
        }
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.cell_px_reported = Some(px);
        }
        if self
            .window_terminal_sessions(wid)
            .into_iter()
            .any(|id| self.card_drawer_published(id))
        {
            self.prefetch_card_arts(wid);
        }
    }

    /// Interior padding (device px per edge) for window `wid`, from its own per-window [`MetricsView`] (mixed-DPI).
    pub(crate) fn win_pad(&self, wid: WindowId) -> usize {
        self.windows
            .get(&wid)
            .map_or_else(|| self.backend.pad(), |ws| ws.metrics.pad)
    }

    /// Top interior padding (device px) for window `wid`.
    pub(crate) fn win_pad_top(&self, wid: WindowId) -> usize {
        self.windows
            .get(&wid)
            .map_or_else(|| self.backend.pad_top(), |ws| ws.metrics.pad_top)
    }

    /// Chrome headroom (device px) above window `wid`'s padded grid (the titlebar band the effects layer may draw into), from its own per-window [`MetricsView`].
    pub(crate) fn win_head(&self, wid: WindowId) -> usize {
        match self.windows.get(&wid) {
            Some(ws) if ws.os_window.is_some() => ws.metrics.head,
            _ => {
                let h = crate::headroom_override();
                if h != 0 { h } else { self.backend.head() }
            }
        }
    }

    /// The program cat's raw claim.
    pub(crate) fn app_kitty_claim(
        &mut self,
        wid: WindowId,
        session: u64,
    ) -> Option<aterm_effects::companion_look::AppIdentity> {
        let s = self.pool.get(session)?;
        let term = crate::term_lock_ui(
            &s.term,
            &s.ctx.ui_waiting,
            crate::metrics::TermWaitSite::Redraw,
        );
        let ws = self.windows.get_mut(&wid)?;
        ws.fx.look_mut().claim(
            session,
            term.current_block().or_else(|| term.all_blocks().last()),
        )
    }

    /// The one companion verdict.
    pub(crate) fn companion_verdict(
        &mut self,
        wid: WindowId,
        focus_session: u64,
        now: std::time::Instant,
    ) -> aterm_effects::kitty_registry::KittyLook {
        let raw = self.app_kitty_claim(wid, focus_session);
        self.companion_verdict_with_claim(wid, raw, now)
    }

    /// [`Self::companion_verdict`] with the focused pane's raw program claim already read.
    pub(crate) fn companion_verdict_with_claim(
        &mut self,
        wid: WindowId,
        raw: Option<aterm_effects::companion_look::AppIdentity>,
        now: std::time::Instant,
    ) -> aterm_effects::kitty_registry::KittyLook {
        let favourite = self.kitty_log.favourite_look();
        let launch = self.launch_kitty;
        match self.windows.get_mut(&wid) {
            Some(ws) => ws
                .fx
                .look_mut()
                .verdict(raw.as_ref(), favourite, launch, now),
            None => aterm_effects::companion_look::companion_precedence(favourite, None, launch).0,
        }
    }

    /// Window-space effects geometry for `wid`'s effect streams (fire/glow/halo/nova).
    pub(crate) fn effects_origin_win(
        &self,
        wid: WindowId,
        rows: usize,
        cols: usize,
        ch: usize,
    ) -> (u16, u16, u16, u16, u16) {
        let pad = self.win_pad(wid);
        let pad_top = self.win_pad_top(wid);
        let head = self.win_head(wid);
        let chrome = self.chrome_rows(wid);
        let strip_px = usize::from(chrome) * ch.max(1);
        let (rows16, cols16) = (
            u16::try_from(rows).unwrap_or(u16::MAX),
            u16::try_from(cols).unwrap_or(u16::MAX),
        );
        let size = self.frame_px(rows16.saturating_add(chrome), cols16);
        (
            pad.min(u16::MAX as usize) as u16,
            (pad_top + head + strip_px).min(u16::MAX as usize) as u16,
            size.width.min(u32::from(u16::MAX)) as u16,
            size.height.min(u32::from(u16::MAX)) as u16,
            if strip_px > 0 {
                0
            } else {
                head.min(u16::MAX as usize) as u16
            },
        )
    }

    /// Glyph rasterization size (physical px) for window `wid`, from its own per-window [`MetricsView`] (mixed-DPI).
    pub(crate) fn win_font_px(&self, wid: WindowId) -> f32 {
        self.windows
            .get(&wid)
            .map_or(self.font_px, |ws| ws.metrics.font_px)
    }

    /// One zoom-aware coordinate transform for every host modal painter and pointer consumer.
    pub(crate) fn overlay_coordinate_transform(
        &self,
        wid: WindowId,
    ) -> Option<crate::OverlayCoordinateTransform> {
        let plan = self.active_visible_leaf_plan(wid)?;
        self.overlay_coordinate_transform_from_plan(wid, &plan)
    }

    fn overlay_coordinate_transform_from_plan(
        &self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> Option<crate::OverlayCoordinateTransform> {
        let window = self.windows.get(&wid)?;
        let route = self.visible_content_route_from_plan(plan)?;
        let panel_rows = if route.has_visible_native() {
            window.overlay_rows_with_available(usize::from(window.rows))
        } else {
            window.overlay_rows()
        };
        if panel_rows == 0 {
            return None;
        }
        let (cw, ch) = self.win_cell_size(wid);
        if route.has_visible_native() {
            let scale = window.scale.max(f64::EPSILON) as f32;
            Some(crate::OverlayCoordinateTransform {
                geom: crate::settings::SettingsGeom {
                    cw: cw as f32 / scale,
                    ch: ch as f32 / scale,
                    font_px: 13.0,
                    cols: window.cols as usize,
                    panel_rows,
                },
                scale,
                origin_x: self.win_pad(wid) as f64,
                origin_y: self.native_content_origin_y(wid) as f64,
            })
        } else {
            Some(crate::OverlayCoordinateTransform {
                geom: crate::settings::SettingsGeom {
                    cw: cw as f32,
                    ch: ch as f32,
                    font_px: self.win_font_px(wid),
                    cols: window.cols as usize,
                    panel_rows,
                },
                scale: 1.0,
                origin_x: self.win_pad(wid) as f64,
                origin_y: (self.win_pad_top(wid) + self.win_head(wid)) as f64,
            })
        }
    }

    /// The window/swapchain pixel size for a `total_rows`×`cols` grid, including the interior padding (`2·pad` horizontally, `pad_top + pad` vertically).
    pub(crate) fn frame_px(&self, total_rows: u16, cols: u16) -> PhysicalSize<u32> {
        let (w, h) = self.backend.frame_size(total_rows as usize, cols as usize);
        PhysicalSize::new(w as u32, h as u32)
    }

    /// The window pixel size for the current terminal grid.
    pub(crate) fn window_frame_px_for(
        &self,
        wid: WindowId,
        rows: u16,
        cols: u16,
    ) -> PhysicalSize<u32> {
        self.frame_px(rows.saturating_add(self.chrome_rows(wid)), cols)
    }

    /// Resolve the motion policy for a window with focus `focused`.
    pub(crate) fn motion_policy(&self, focused: bool) -> crate::motion::MotionPolicy {
        let mode = self.config.motion_mode();
        if self.load_shed_active_with(mode) {
            return crate::motion::MotionPolicy::Reduced;
        }
        crate::motion::MotionPolicy::resolve(mode, self.system_reduce_motion, focused)
    }

    /// The motion policy for one effect.
    pub(crate) fn effect_policy(
        &self,
        effect: crate::motion::MotionEffect,
        focused: bool,
    ) -> crate::motion::MotionPolicy {
        if effect.load_shed_exempt() {
            self.accessibility_motion_policy(focused)
        } else {
            self.motion_policy(focused)
        }
    }

    /// [`Self::motion_policy`] without the load-shed latch.
    pub(crate) fn accessibility_motion_policy(&self, focused: bool) -> crate::motion::MotionPolicy {
        crate::motion::MotionPolicy::resolve(
            self.config.motion_mode(),
            self.system_reduce_motion,
            focused,
        )
    }

    /// The resident pet's motion gate for a focused window's scheduler.
    pub(crate) fn resident_pet_motion(&self) -> bool {
        self.serious_mode_policy()
            .allows(crate::motion::SeriousEffect::CursorCat)
            && self
                .accessibility_motion_policy(true)
                .animate(crate::motion::MotionEffect::CursorGlow)
    }

    /// Whether the SmoothScroll effect (the glide, the sub-row band translate, the overscroll bounce) animates for window `wid`.
    pub(crate) fn smooth_scroll_animates(&self, wid: WindowId) -> bool {
        let focused = self.motion_focus(wid, self.windows.get(&wid).is_some_and(|ws| ws.focused));
        self.effect_policy(crate::motion::MotionEffect::SmoothScroll, focused)
            .animate(crate::motion::MotionEffect::SmoothScroll)
    }

    /// The focus bit the motion seam consumes for window `id`.
    pub(crate) fn motion_focus(&self, id: WindowId, focused: bool) -> bool {
        focused || self.video_rec.as_ref().is_some_and(|v| v.window == id)
    }

    /// The focus bit the cursor-effect seams (aurora, comet, rainbow-kitty companion) feed the motion policy for window `id`.
    pub(crate) fn cursor_fx_focus(&self, id: WindowId, focused: bool, now: Instant) -> bool {
        self.motion_focus(id, focused)
            || self
                .windows
                .get(&id)
                .is_some_and(|ws| ws.cursor_fx_typed_wake(now))
    }

    fn read_deliveries(&self, id: WindowId, now: Instant) -> Option<(u64, Deliveries)> {
        let ws = self.windows.get(&id)?;
        let seen = ws.delivery_seen;
        let session = ws.front_terminal()?.session;
        let after = match seen {
            Some((seen_session, serial)) if seen_session == session => Some(serial),
            _ => None,
        };
        let ctx = &self.pool.get(session)?.ctx;
        Some((
            session,
            ctx.output_echo.deliveries_after(&ctx.sink, after, now),
        ))
    }

    /// Whether the raw overload latch is allowed to suppress decorative work under the current user policy.
    pub(crate) fn load_shed_active(&self) -> bool {
        self.load_shed_active_with(self.config.motion_mode())
    }

    /// [`Self::load_shed_active`] with the caller's resolved [`MotionMode`](crate::motion::MotionMode), so the per-frame [`Self::motion_policy`] parses the `motion` config once.
    pub(crate) fn load_shed_active_with(&self, mode: crate::motion::MotionMode) -> bool {
        self.perf_reduced
            && mode.admits_load_shed()
            && self.config.load_adaptive_motion_or_default()
    }

    fn settle_motion_policy_transition(&mut self) {
        let native_windows = self
            .windows
            .keys()
            .copied()
            .filter(|wid| self.active_tab_contains_native(*wid))
            .collect::<Vec<_>>();
        for wid in native_windows {
            self.invalidate_native_ui_cache(wid);
        }
        self.request_redraw_all_windows();
    }

    fn gpu_backpressure_ns(&self, id: WindowId) -> u64 {
        let Some(ws) = self.windows.get(&id) else {
            return 0;
        };
        gpu_backpressure_excess_ns(
            self.last_acquire_wait_ns(id),
            ws.frame_interval.unwrap_or(self.frame_interval),
        )
    }

    fn last_acquire_wait_ns(&self, id: WindowId) -> u64 {
        #[cfg(test)]
        if let Some(ns) = ACQUIRE_WAIT_NS_FOR_TEST.with(std::cell::Cell::get) {
            return ns;
        }
        match self.windows.get(&id).and_then(|ws| ws.present.as_ref()) {
            Some(PresentTarget::Gpu { window_gpu, .. }) => window_gpu.last_acquire_wait_ns(),
            _ => 0,
        }
    }

    pub(crate) fn note_present_cost(
        &mut self,
        present_ns: u64,
        frame_interval: std::time::Duration,
        now: std::time::Instant,
    ) -> bool {
        let sample = present_ns as f64;
        let ema = match self.present_cost_ema_ns {
            Some(prev) => PERF_EMA_ALPHA * sample + (1.0 - PERF_EMA_ALPHA) * prev,
            None => sample,
        };
        self.present_cost_ema_ns = Some(ema);
        let fi = frame_interval.as_nanos() as f64;
        let before = self.perf_reduced;
        if self.perf_reduced {
            // Re-engage only after a sustained run comfortably under budget and
            // once the anti-flap dwell has elapsed: the post-shed cost excludes the
            // effects' own cost, so the clear threshold is nearly always met, and
            // without the dwell that asymmetry oscillates (see PERF_SHED_DWELL_MIN).
            if ema < PERF_CLEAR_FACTOR * fi {
                self.perf_run += 1;
            } else {
                self.perf_run = 0;
            }
            let dwell_ok = self
                .perf_flip_at
                .is_none_or(|t| now.saturating_duration_since(t) >= self.perf_shed_dwell);
            if self.perf_run >= PERF_HYSTERESIS_FRAMES && dwell_ok {
                self.flip_shed_latch(now, false);
            }
        } else {
            // Shed only after a sustained run over budget, judged on the raw
            // sample: one huge spike (a theme switch) keeps the decaying EMA over
            // threshold for a few frames and must not shed. The EMA still drives
            // the clear direction.
            if sample > PERF_SHED_FACTOR * fi {
                self.perf_run += 1;
            } else {
                self.perf_run = 0;
            }
            if self.perf_run >= PERF_HYSTERESIS_FRAMES {
                self.perf_shed_dwell = match self.perf_flip_at {
                    Some(t) if now.saturating_duration_since(t) < PERF_RESHED_QUICK_WINDOW => {
                        (self.perf_shed_dwell * 2).min(PERF_SHED_DWELL_MAX)
                    }
                    _ => PERF_SHED_DWELL_MIN,
                };
                self.flip_shed_latch(now, true);
            }
        }
        self.perf_reduced != before
    }

    fn flip_shed_latch(&mut self, now: std::time::Instant, shed: bool) {
        self.perf_env_at_flip = self.shed_envelope(now);
        self.perf_flip_at = Some(now);
        self.perf_reduced = shed;
        self.perf_run = 0;
    }

    /// The soft load-shed envelope in [0, 1] at `now`.
    pub(crate) fn shed_envelope(&self, now: std::time::Instant) -> f32 {
        let Some(flip) = self.perf_flip_at else {
            return 1.0;
        };
        let dt = now.saturating_duration_since(flip).as_secs_f32();
        if self.perf_reduced {
            (self.perf_env_at_flip - dt / SHED_FADE_OUT_SECS).max(0.0)
        } else {
            (self.perf_env_at_flip + dt / SHED_FADE_IN_SECS).min(1.0)
        }
    }

    /// The load-shed envelope this frame is actually allowed to apply.
    pub(crate) fn effective_shed_envelope(
        &self,
        mode: crate::motion::MotionMode,
        now: std::time::Instant,
    ) -> f32 {
        if !mode.admits_load_shed() || !self.config.load_adaptive_motion_or_default() {
            1.0
        } else {
            self.shed_envelope(now)
        }
    }

    /// Install the exact admitted config-asset generation into one window's effects state.
    pub(crate) fn install_window_config_assets(&mut self, id: WindowId) -> bool {
        let assets = std::sync::Arc::clone(&self.config_assets);
        let Some(window) = self.windows.get_mut(&id) else {
            return false;
        };
        if window
            .installed_config_assets
            .as_ref()
            .is_some_and(|installed| std::sync::Arc::ptr_eq(installed, &assets))
        {
            return false;
        }
        let kitty_asset_fp = assets.kitty_sprite.fingerprint();
        let source = match &assets.kitty_sprite {
            crate::app_config::KittySpriteAsset::BuiltIn => {
                aterm_effects::word_decorations::KittySpriteSource::BuiltIn
            }
            crate::app_config::KittySpriteAsset::Ready { w, h, rgba, fp, .. } => {
                aterm_effects::word_decorations::KittySpriteSource::Custom {
                    source_fp: *fp,
                    w: *w,
                    h: *h,
                    rgba: std::sync::Arc::clone(rgba),
                }
            }
            crate::app_config::KittySpriteAsset::Invalid { .. } => {
                aterm_effects::word_decorations::KittySpriteSource::Disabled
            }
        };
        window.fx.decos_mut().set_kitty_sprite_source(source);
        window.fx.companion_mut().invalidate_colors();
        window.installed_kitty_asset_fp = kitty_asset_fp;
        window.installed_config_assets = Some(assets);
        true
    }

    /// Resolve the cursor-aurora (additive light) config for this frame.
    pub(crate) fn glow_config(&self) -> crate::cursor_glow::GlowConfig {
        self.glow_config_for(self.trail_presentation())
    }

    /// The immutable config-generation presentation.
    pub(crate) fn trail_presentation(&self) -> crate::app_config::ResolvedTrailPresentation {
        let style_raw = self.config.cursor_trail_style_raw();
        self.config_assets.trail_style.presentation_for(style_raw)
    }

    pub(crate) fn glow_config_for(
        &self,
        presentation: crate::app_config::ResolvedTrailPresentation,
    ) -> crate::cursor_glow::GlowConfig {
        crate::app_config::resolve_cursor_glow(
            self.config.cursor_glow_inputs(
                self.serious_mode_policy()
                    .allows(crate::motion::SeriousEffect::CursorGlow),
                self.theme.cursor,
                presentation,
            ),
            presentation,
            self.theme.cursor,
            true,
            0x00C8_D3F5,
            0x001A_1B26,
            0.5,
        )
    }

    /// The resolved aurora style alone.
    pub(crate) fn glow_style(&self) -> crate::cursor_glow::GlowStyle {
        self.trail_presentation()
            .style
            .style
            .unwrap_or(crate::cursor_glow::GlowStyle::Lumen)
    }

    /// The trail-gesture voice for the current config generation ([`crate::app_config::Config::trail_sound_voice_for`]).
    pub(crate) fn trail_gesture_voice(&self) -> aterm_effects::trail_sound::SoundVoice {
        self.config.trail_sound_voice_for(self.trail_presentation())
    }

    #[cfg(test)]
    pub(crate) fn trail_is_comet(&self) -> bool {
        self.trail_presentation().comet
    }

    /// Whether the active trail style draws the full-body pet companion (`"rainbow kitty pet"` or `"rainbow dog pet"`) instead of the flying kitty.
    pub(crate) fn trail_is_kitty_pet(&self) -> bool {
        self.trail_presentation().pet_species.is_some()
    }

    #[cfg(test)]
    pub(crate) fn trail_config(&self) -> crate::cursor_trail::TrailConfig {
        self.trail_config_for(self.trail_presentation())
    }

    fn trail_config_for(
        &self,
        presentation: crate::app_config::ResolvedTrailPresentation,
    ) -> crate::cursor_trail::TrailConfig {
        let color = self
            .config
            .cursor_trail_color_u32()
            .unwrap_or(crate::cursor_glow::COMET_DEFAULT_COLOR);
        crate::cursor_trail::TrailConfig {
            enabled: presentation.comet
                && self
                    .serious_mode_policy()
                    .allows(crate::motion::SeriousEffect::CursorTrail),
            duration: std::time::Duration::from_millis(self.config.cursor_trail_ms_or_default()),
            max_len: self.config.cursor_trail_length_or_default(),
            color,
            intensity: 0.0,
            warmth: 0.0,
        }
    }

    /// The raw typing-momentum metric (`trail status momentum=`, from [`aterm_effects::cursor_glow::CursorGlow::typing_momentum`]) of the window hosting `session`, stamped with the instant it was read, for `await momentum` and `turn yield=`.
    pub(crate) fn typing_momentum_of(&self, session: u64) -> Result<(f32, Instant), String> {
        let now = Instant::now();
        let v = self
            .window_of_session(session)
            .and_then(|wid| self.windows.get(&wid))
            .map_or(0.0, |ws| ws.fx.cursor().glow.typing_momentum(now));
        Ok((v, now))
    }

    pub(crate) fn trail_status(&self) -> Result<String, String> {
        let Some(id) = self.frontmost_window else {
            return Err("no focused window".to_string());
        };
        let Some(ws) = self.windows.get(&id) else {
            return Err("no focused window".to_string());
        };
        let now = std::time::Instant::now();
        let win_focused = self.cursor_fx_focus(id, ws.focused, now);
        let mode = self.config.motion_mode();
        let policy =
            crate::motion::MotionPolicy::resolve(mode, self.system_reduce_motion, win_focused);
        let shed = self.effective_shed_envelope(mode, now);
        let presentation = self.trail_presentation();
        let glow_cfg = self.glow_config_for(presentation);
        let pet = ws.fx.companion().brain();
        let pet_action = if ws.fx.companion().hit_rect().is_some() {
            format!("{:?}", pet.action()).to_ascii_lowercase()
        } else {
            "none".to_string()
        };
        let comet_bed = &ws.fx.cursor().trail_scratch;
        Ok(crate::cursor_glow::TrailStatus {
            style_raw: self.config.cursor_trail_style_raw(),
            style: presentation.style.style,
            unknown: presentation.style.unknown,
            config_enabled: self.config.cursor_trail_drawn(),
            effective: glow_cfg.enabled,
            focused: win_focused,
            motion_stage: match self.motion_policy(win_focused) {
                crate::motion::MotionPolicy::Full => "full",
                crate::motion::MotionPolicy::Reduced => "reduced",
            },
            motion_mode: match mode {
                crate::motion::MotionMode::Auto => "auto",
                crate::motion::MotionMode::Full => "full",
                crate::motion::MotionMode::Reduced => "reduced",
            },
            shed,
            intensity: if glow_cfg.enabled {
                glow_cfg.intensity
                    * policy.amplitude(crate::motion::MotionEffect::CursorGlow)
                    * shed
            } else {
                0.0
            },
            sound_seam: ws.fx.cursor().glow.sound_seam_open(),
            ribbon_look: if presentation.style.style
                != Some(crate::cursor_glow::GlowStyle::RainbowKitty)
            {
                "none"
            } else if glow_cfg.ribbon_tall {
                "tall"
            } else {
                "underline"
            },
            tally: ws.fx.cursor().glow.admission_tally(),
            spawns: ws.fx.cursor().glow.spawns(),
            ribbon_segments: ws.fx.cursor().glow.ribbon_segments(),
            ribbon_hue_bands: ws.fx.cursor().glow.ribbon_hue_bands(),
            ribbon_drawn: ws.fx.cursor().glow.ribbon_drawn(),
            ribbon_curtain_ms: ws
                .fx
                .cursor()
                .glow
                .curtain_left_ms(std::time::Instant::now()),
            field: ws.fx.cursor().glow.rainbow_field(),
            sparks: ws.fx.cursor().glow.live_sparks(),
            momentum: ws.fx.cursor().glow.typing_momentum(now),
            momentum_display: ws.fx.cursor().glow.momentum_display(),
            momentum_glow: ws
                .fx
                .cursor()
                .momentum
                .value(now, aterm_effects::cursor_momentum::MOMENTUM_GLOW_TAU_S),
            glow_active: ws.fx.cursor().glow.is_active(),
            pet_active: self.trail_is_kitty_pet() && pet.is_active(),
            pet_action: &pet_action,
            pet_content: pet.content(),
            pet_pending: pet.pending_pets(),
            pet_focus: pet.console_attention().name(),
            pet_reason: pet.console_reason(),
            pet_anchor: pet.console_anchor_id(),
            pet_event_seq: pet.console_event_seq(),
            pet_pose: pet.pose_name(),
            pet_body: ws.fx.companion().hit_rect(),
            cat_active: ws.fx.companions().cursor_cat.is_active(),
            block_fill: ws.block_fill,
            flow: ws.fx.cursor().glow.flow_status(),
            inserts: ws.fx.cursor().glow.insert_tally(),
            in_flight: ws.fx.cursor().glow.in_flight_tally(),
            comet_cells: comet_bed.len(),
            comet_alpha_max: comet_bed.iter().map(|cell| cell.alpha).max().unwrap_or(0),
        }
        .line_v2(ws.fx.cursor().glow.v2_status()))
    }

    fn cursor_frame_for_present(
        &mut self,
        id: WindowId,
        sync_hold: bool,
        fx: CursorFxInputs,
    ) -> Option<CursorFxTick> {
        if sync_hold {
            None
        } else {
            self.cursor_frame(id, fx)
        }
    }

    /// One window's cursor-effect frame.
    pub(crate) fn cursor_frame(
        &mut self,
        id: WindowId,
        fx: CursorFxInputs,
    ) -> Option<CursorFxTick> {
        let CursorFxInputs {
            now: frame_started,
            rows,
            cols,
            cur,
            live_viewport,
            cursor_visible,
            hidden_withheld,
            cursor_style,
            blink_phase,
            live_cursor_rgb,
            cursor_color_pinned,
            default_bg,
            default_fg,
            row_probe,
            row_probe_neighbors,
            print_anchor,
            print_anchor_glyph,
        } = fx;
        self.reconcile_licence(id);
        let cur = aterm_effects::host::motion_caret(cur, cursor_visible, hidden_withheld);
        let raw_focused = self.windows.get(&id)?.focused;
        let win_focused = self.cursor_fx_focus(id, raw_focused, frame_started);
        let deliveries = self.read_deliveries(id, frame_started);
        let motion = self.motion_policy(win_focused);
        let scroll_motion = self.effect_policy(
            crate::motion::MotionEffect::SmoothScroll,
            self.motion_focus(id, raw_focused),
        );
        let cursor_body_allowed = self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::CursorBody);
        let terminal_sound_allowed = self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::TerminalSound);
        let mode = self.config.motion_mode();
        let cursor_motion =
            crate::motion::MotionPolicy::resolve(mode, self.system_reduce_motion, win_focused);
        let shed_env = self.effective_shed_envelope(mode, frame_started);
        let trail_presentation = self.trail_presentation();
        let trail_voice = self.config.trail_sound_voice_for(trail_presentation);
        let mut glow_cfg = self.glow_config_for(trail_presentation);
        crate::sound_seam::fold_window_audibility(
            &mut glow_cfg,
            cursor_motion.amplitude(crate::motion::MotionEffect::CursorGlow),
            shed_env,
            win_focused,
        );
        glow_cfg.dark_theme = aterm_render::theme_is_dark(default_bg);
        glow_cfg.theme_fg = default_fg;
        glow_cfg.theme_bg = default_bg;
        let mut trail_cfg = self.trail_config_for(trail_presentation);
        trail_cfg.enabled &=
            cursor_motion.animate(crate::motion::MotionEffect::CursorGlow) && shed_env > 0.0;
        trail_cfg.enabled &= !aterm_effects::cursor_fx::comet_tail_is_its_streak(&glow_cfg);
        let pet_mode = trail_presentation.pet_species.is_some();
        let (glow_cw, glow_ch) = self.win_cell_size(id);
        aterm_effects::cursor_fx::follow_live_cursor(
            &mut glow_cfg,
            &mut trail_cfg,
            aterm_render::rgb_to_u32(live_cursor_rgb),
            cursor_color_pinned,
            self.config.trail_color(),
            self.config.cursor_trail_accent_u32().is_some(),
            None,
        );
        if crate::app_render::trace_spawn_enabled() {
            static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
            static LAST: std::sync::Mutex<Option<Option<(u16, u16)>>> = std::sync::Mutex::new(None);
            let e = *EPOCH.get_or_init(std::time::Instant::now);
            let mut last = LAST.lock().unwrap_or_else(|p| p.into_inner());
            if *last != Some(cur) {
                crate::logging::stderr_line!(
                    "SPAWNSRC t_us={} cur={:?} vis={}",
                    frame_started.saturating_duration_since(e).as_micros(),
                    cur,
                    cursor_visible
                );
                *last = Some(cur);
            }
        }
        let (origin_x, origin_y, win_w, win_h, fx_head) =
            self.effects_origin_win(id, rows, cols, glow_ch);
        let glow_geom = crate::cursor_glow::Geom {
            cw: glow_cw,
            ch: glow_ch,
            rows,
            cols,
            origin_x,
            origin_y,
            win_w,
            win_h,
            head: fx_head,
        };
        let ws = self.windows.get_mut(&id)?;
        glow_cfg.head_dx = aterm_effects::cursor_fx::head_dx_for(cursor_style);
        let cursor_cell_bg = cur.and_then(|(row, col)| {
            ws.input_scratch
                .cells
                .get(usize::from(row))?
                .get(usize::from(col))
                .map(|cell| aterm_render::rgb_to_u32(cell.bg))
        });
        let mut receipts = [None; crate::app_input::DELIVERY_RING];
        if let Some((session, batch)) = deliveries {
            if batch.evicted > 0 {
                aterm_log::warn!(
                    "delivery register overran: {} receipt(s) evicted unread (session {session}, latest {})",
                    batch.evicted,
                    batch.latest
                );
            }
            for (slot, (at, ticket)) in receipts.iter_mut().zip(batch.items.iter().flatten()) {
                *slot = Some(delivery_for_window(id, *at, *ticket));
            }
            for ticket in batch.failed.iter().flatten() {
                crate::app_input::revoke_failed_delivery(ws, id, *ticket);
            }
            ws.delivery_seen = Some((session, batch.latest));
        }
        let step = ws.fx.cursor_step(
            &aterm_effects::pipeline::CursorStep {
                input: aterm_effects::cursor_fx::CursorFxInput {
                    now: frame_started,
                    cur,
                    live_viewport,
                    cursor_style,
                    blink_phase,
                    live_cursor: aterm_render::rgb_to_u32(live_cursor_rgb),
                    cursor_color_pinned,
                    default_bg,
                    cursor_cell_bg,
                    geom: glow_geom,
                    focused: win_focused,
                    shed_envelope: shed_env,
                    trail_brightness: self.config.cursor_trail_intensity_for(trail_presentation),
                    body_allowed: cursor_body_allowed,
                    momentum_glow: self.config.cursor_momentum_glow_or_default(),
                    user_tinted: self.config.trail_color().chosen(),
                },
                glow: &glow_cfg,
                trail: trail_cfg,
                animate: cursor_motion.animate(crate::motion::MotionEffect::CursorGlow),
                row_probe: row_probe.map(|(row, caret, trust)| {
                    let (above, below) =
                        row_probe_neighbors.unwrap_or((row > 0, usize::from(row) + 1 < rows));
                    aterm_effects::pipeline::RowProbe {
                        row,
                        caret,
                        trust,
                        cols: &ws.poof_row_buf,
                        flanks: Some((
                            above.then_some(ws.poof_row_above_buf.as_slice()),
                            below.then_some(ws.poof_row_below_buf.as_slice()),
                        )),
                    }
                }),
                frame_cells: Some(&ws.input_scratch.cells),
                pending_keys: 0,
                print_anchor,
                print_anchor_glyph,
                caret_drawn: cursor_visible,
            },
            receipts.into_iter().flatten(),
        );
        let frame = step.frame;
        forward_kitty_cursor_motion(
            glow_cfg.enabled,
            pet_mode,
            glow_cfg.style,
            step.motion_pulse,
            &mut ws.fx.companions_mut().cursor_cat,
        );
        {
            let gain = voice_gain(
                Voice::Keystroke,
                &VoiceTerms {
                    master: self.config.trail_sounds_or_default() && terminal_sound_allowed,
                    own: !ws.resize_sound_quiet(frame_started),
                    volume: self.config.trail_sound_volume(),
                    raw_focused,
                    reduced_motion: false,
                },
            );
            ws.fx
                .companions_mut()
                .tone_tracker
                .drain_pending_inference(frame_started);
            let tone = ws
                .fx
                .companions_mut()
                .tone_tracker
                .effective(self.config.tone_melody_or_default());
            let mut last_at_ms = 0u32;
            let flow_heat = ws.fx.cursor_mut().glow.flow_status().heat;
            drain_trail_sound_cues(
                &mut ws.fx.cursor_mut().glow,
                glow_cfg.style,
                cols.min(u16::MAX as usize) as u16,
                TrailSoundPolicy {
                    voice: trail_voice,
                    gain,
                    tone,
                    bed: self.config.trail_sound_bed_or_default(),
                },
                |event| {
                    let at_ms = input_clock_ms().max(last_at_ms.wrapping_add(1));
                    last_at_ms = at_ms;
                    self.trail_audio.push_meta(
                        event,
                        aterm_effects::trail_sound::EventMeta {
                            at_ms,
                            glyph_class: 0,
                            rank: 0,
                            pan_from: 0.0,
                            block_lead_s: 0.0,
                            flow: flow_heat,
                        },
                    );
                },
            );
        }
        if let Some(ws) = self.windows.get(&id) {
            metrics::note_fx_composed(
                ws.fx.cursor().glow_scratch.len(),
                ws.fx.cursor().glow.under_quads().len(),
                ws.fx.cursor().trail_scratch.len(),
            );
        }
        Some(CursorFxTick {
            win_focused,
            motion,
            cursor_motion,
            scroll_motion,
            shed_envelope: shed_env,
            glow_cfg,
            trail_color: frame.trail_color,
            glow_cw,
            glow_ch,
            glow_geom,
            glow_fp: frame.glow_fp,
            trail_fp: frame.trail_fp,
            block_fill: frame.block_fill,
            bolt_cursor: frame.bolt_cursor,
            twinkle_cursor: frame.twinkle_cursor,
            momentum_steady: frame.momentum_steady,
        })
    }

    /// Bind the complete renderer context to the window about to be encoded.
    pub(crate) fn bind_window_renderer_state(&mut self, id: WindowId) {
        self.apply_window_scale(id);
        let Some(window) = self.windows.get(&id) else {
            return;
        };
        let cursor_override =
            (!window.focused && window.os_window.is_some()).then_some(CursorStyle::HollowBlock);
        self.backend.set_cursor_blink_phase(window.blink_phase);
        self.backend.set_cursor_style_override(cursor_override);
        self.backend
            .set_selection_inactive(self.render_knobs.selection_inactive && !window.focused);
    }

    fn composed_cursor_effect_clip(
        &self,
        wid: WindowId,
        pane: ComposedCursorEffectPane,
    ) -> Option<ComposedCursorEffectClip> {
        let ComposedCursorEffectPane {
            row,
            col,
            pane_rows,
            pane_cols,
            cell_w,
            cell_h,
        } = pane;
        if pane_rows == 0 || pane_cols == 0 {
            return None;
        }
        let window = self.windows.get(&wid)?;
        let rows = usize::from(window.rows);
        let cols = usize::from(window.cols);
        let row1 = row.saturating_add(pane_rows).min(rows);
        let col1 = col.saturating_add(pane_cols).min(cols);
        if row >= row1 || col >= col1 {
            return None;
        }
        let (origin_x, origin_y, _, _, effect_head) =
            self.effects_origin_win(wid, rows, cols, cell_h);
        let x0 = u32::from(origin_x)
            .saturating_add(u32::try_from(col.saturating_mul(cell_w)).unwrap_or(u32::MAX));
        let mut y0 = u32::from(origin_y)
            .saturating_add(u32::try_from(row.saturating_mul(cell_h)).unwrap_or(u32::MAX));
        if row == 0 {
            y0 = y0.saturating_sub(u32::from(effect_head));
        }
        let x1 = u32::from(origin_x)
            .saturating_add(u32::try_from(col1.saturating_mul(cell_w)).unwrap_or(u32::MAX));
        let y1 = u32::from(origin_y)
            .saturating_add(u32::try_from(row1.saturating_mul(cell_h)).unwrap_or(u32::MAX));
        let (x0, y0, x1, y1) = (
            x0.min(u32::from(u16::MAX)) as u16,
            y0.min(u32::from(u16::MAX)) as u16,
            x1.min(u32::from(u16::MAX)) as u16,
            y1.min(u32::from(u16::MAX)) as u16,
        );
        let px = |v: usize| i32::try_from(v).unwrap_or(i32::MAX - 1).min(i32::MAX - 1);
        let corner = (col.saturating_mul(cell_w), row.saturating_mul(cell_h));
        Some(ComposedCursorEffectClip {
            row0: row,
            row1,
            col0: col,
            col1,
            x0,
            y0,
            x1,
            y1,
            fx: PaneFxPlacement {
                window_origin: (
                    px(usize::from(origin_x).saturating_add(corner.0)),
                    px(usize::from(origin_y).saturating_add(corner.1)),
                ),
                grid_origin: (px(corner.0), px(corner.1)),
                window_box: aterm_core::render::PxRect::new(
                    i32::from(x0),
                    i32::from(y0),
                    i32::from(x1),
                    i32::from(y1),
                ),
            },
        })
    }

    fn project_retained_composed_cursor_effects(
        &mut self,
        wid: WindowId,
        clip: Option<ComposedCursorEffectClip>,
        cursor_override: Option<CursorStyle>,
    ) -> bool {
        let Some(window) = self.windows.get_mut(&wid).map(Box::as_mut) else {
            return false;
        };
        let valid = window.composed_cursor_effect_valid;
        let active_cursor_fill = window.composed_cursor_fill;
        let body_style_override = valid
            .then_some(window.composed_cursor_style_override)
            .flatten();
        let trail_color = window.composed_cursor_trail_color;
        let WindowState {
            input_scratch, fx, ..
        } = window;
        let aterm_effects::cursor_fx::CursorFx {
            glow_scratch,
            glow: cursor_glow,
            trail_scratch,
            ..
        } = fx.cursor();
        if valid {
            project_focused_composed_cursor_effects(
                input_scratch,
                FocusedComposedCursorEffects {
                    glow_add: glow_scratch,
                    glow_halo: cursor_glow.halos(),
                    fire_patch: cursor_glow.patches(),
                    glow_under: cursor_glow.under_quads(),
                    char_fg: cursor_glow.charred(),
                    fire_halo: cursor_glow.halo_cells(),
                    trail: trail_scratch,
                    trail_color,
                    active_cursor_fill,
                    fx_layers: &[],
                },
                clip,
                cursor_override,
                body_style_override,
            );
        } else {
            project_focused_composed_cursor_effects(
                input_scratch,
                FocusedComposedCursorEffects {
                    glow_add: &[],
                    glow_halo: &[],
                    fire_patch: &[],
                    glow_under: &[],
                    char_fg: &[],
                    fire_halo: &[],
                    trail: &[],
                    trail_color: 0,
                    active_cursor_fill: None,
                    fx_layers: &[],
                },
                None,
                cursor_override,
                None,
            );
        }
        true
    }

    fn retire_composed_cursor_effects_for_history(&mut self, wid: WindowId) {
        if let Some(window) = self.windows.get_mut(&wid) {
            // History, not a new space: the live grid the band describes has not
            // moved, so the band is kept. This frame projects none of it
            // (`composed_cursor_effect_valid` is dropped below), but it is still
            // there when the live viewport returns. No one-frame hide is armed:
            // this arm advances no effect clock (see [`TornBand::Keep`]).
            retire_torn_cursor_fx_with(window, TornBand::Keep);
        }
    }

    #[cfg(test)]
    pub(crate) fn composed_cursor_frame(
        &mut self,
        wid: WindowId,
        clock: ComposedCursorFxClock,
    ) -> bool {
        let Some(plan) = self.active_visible_leaf_plan(wid) else {
            return false;
        };
        self.composed_cursor_frame_sampled(wid, clock, None, &plan)
    }

    fn composed_cursor_frame_sampled(
        &mut self,
        wid: WindowId,
        clock: ComposedCursorFxClock,
        exact_sample: Option<FocusedComposedCursorFxSample>,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> bool {
        let Some(leaf) = plan.leaves.iter().find(|leaf| leaf.focused) else {
            return false;
        };
        let Some(view) = self.view_store.get(leaf.view).copied() else {
            return false;
        };
        let crate::tab_model::View::Terminal(terminal_view) = view else {
            if let Some(window) = self.windows.get_mut(&wid) {
                window.composed_cursor_effect_valid = false;
                window.composed_cursor_effect_session = None;
            }
            return self.project_retained_composed_cursor_effects(wid, None, None);
        };
        let Some((term, ui_waiting)) = self
            .pool
            .get(terminal_view.session)
            .map(|s| (s.term.clone(), s.ctx.ui_waiting.clone()))
        else {
            return false;
        };
        let row = leaf.rect.origin.y.round().max(0.0) as usize;
        let col = leaf.rect.origin.x.round().max(0.0) as usize;
        let pane_rows = (leaf.rect.size.height.round() as usize).max(1);
        let pane_cols = (leaf.rect.size.width.round() as usize).max(1);
        let (cell_w, cell_h) = self.win_cell_size(wid);
        let clip = self.composed_cursor_effect_clip(
            wid,
            ComposedCursorEffectPane {
                row,
                col,
                pane_rows,
                pane_cols,
                cell_w,
                cell_h,
            },
        );
        let cursor_override = self.windows.get(&wid).and_then(|window| {
            (!window.focused && window.os_window.is_some()).then_some(CursorStyle::HollowBlock)
        });
        let site = crate::metrics::TermWaitSite::Redraw;
        if matches!(clock, ComposedCursorFxClock::Retain { .. }) {
            let (live_viewport, terminal_id, alternate_screen) = if let Some(sample) = exact_sample
            {
                if sample.session != terminal_view.session {
                    if let Some(window) = self.windows.get_mut(&wid) {
                        window.composed_cursor_effect_valid = false;
                        window.composed_cursor_effect_session = None;
                    }
                    return false;
                }
                (sample.display_offset == 0, sample.terminal_id, sample.alt)
            } else {
                let terminal = crate::term_lock_ui(&term, &ui_waiting, site);
                (
                    terminal.grid().display_offset() == 0,
                    terminal.render_identity(),
                    terminal.is_alternate_screen(),
                )
            };
            if let Some(window) = self.windows.get_mut(&wid) {
                sync_cursor_effect_coordinate_space(window, terminal_id, alternate_screen);
                if window.composed_cursor_effect_session != Some(terminal_view.session) {
                    window.composed_cursor_effect_valid = false;
                    window.composed_cursor_effect_session = None;
                }
            }
            if !live_viewport {
                self.retire_composed_cursor_effects_for_history(wid);
            }
            return self.project_retained_composed_cursor_effects(wid, clip, cursor_override);
        }
        let ComposedCursorFxClock::Advance(now) = clock else {
            unreachable!("retain handled above")
        };

        let Some((rows, cols)) = self
            .windows
            .get(&wid)
            .map(|window| (usize::from(window.rows), usize::from(window.cols)))
        else {
            return false;
        };
        let sample = match exact_sample {
            Some(sample) => {
                if sample.session != terminal_view.session {
                    if let Some(window) = self.windows.get_mut(&wid) {
                        window.poof_row_buf = sample.row_probe;
                        window.poof_row_above_buf = sample.row_above_probe;
                        window.poof_row_below_buf = sample.row_below_probe;
                        window.composed_witness_rows = sample.witness_rows;
                        window.composed_cursor_effect_valid = false;
                        window.composed_cursor_effect_session = None;
                    }
                    return false;
                }
                sample
            }
            None => {
                let (
                    row_probe,
                    row_above_probe,
                    row_below_probe,
                    (v2_owns_frame, reads_neighbor_rows),
                    requested_witness_rows,
                    witness_n,
                    previous_scroll,
                    witness_rows,
                ) = {
                    let Some(window) = self.windows.get_mut(&wid) else {
                        return false;
                    };
                    let mut requested_witness_rows =
                        [0u16; aterm_effects::cursor_glow::CURSOR_WITNESS_ROWS];
                    let witness_n = window
                        .fx
                        .cursor_mut()
                        .glow
                        .ribbon_rows(&mut requested_witness_rows);
                    (
                        std::mem::take(&mut window.poof_row_buf),
                        std::mem::take(&mut window.poof_row_above_buf),
                        std::mem::take(&mut window.poof_row_below_buf),
                        (
                            window.fx.cursor_mut().glow.v2_owns_frame(),
                            window.fx.cursor_mut().glow.reads_neighbor_rows(),
                        ),
                        requested_witness_rows,
                        witness_n,
                        window.fx.scroll_baseline(),
                        std::mem::take(&mut window.composed_witness_rows),
                    )
                };
                let terminal = crate::term_lock_ui(&term, &ui_waiting, site);
                focused_composed_cursor_fx_sample(
                    terminal_view.session,
                    &terminal,
                    true,
                    v2_owns_frame,
                    reads_neighbor_rows,
                    (row, col),
                    &requested_witness_rows[..witness_n],
                    previous_scroll,
                    witness_rows,
                    row_probe,
                    row_above_probe,
                    row_below_probe,
                )
            }
        };
        let FocusedComposedCursorFxSample {
            session: _,
            witness_stamp,
            witness_rows,
            terminal_id,
            cursor,
            cursor_visible,
            cursor_style,
            display_offset,
            live_cursor_rgb,
            cursor_recoloured,
            default_bg,
            default_fg,
            alt,
            blink_epoch,
            content_scroll_state,
            row_probe: sampled_row_probe,
            row_above_probe: sampled_row_above_probe,
            row_below_probe: sampled_row_below_probe,
            row_above_present,
            row_below_present,
            print_anchor: sampled_print_anchor,
            print_anchor_glyph: sampled_print_anchor_glyph,
        } = sample;
        let (row_probe, witness_caret_row, row_above_present, row_below_present) = {
            let Some(window) = self.windows.get_mut(&wid) else {
                return false;
            };
            window.poof_row_buf = sampled_row_probe;
            window.poof_row_above_buf = sampled_row_above_probe;
            window.poof_row_below_buf = sampled_row_below_probe;
            sync_cursor_effect_coordinate_space(window, terminal_id, alt);
            let scroll_change = sync_cursor_effect_scroll(window, content_scroll_state);
            if display_offset != 0 {
                // Scrollback is a different visible coordinate space from the
                // active-grid anchors both engines retain, so hard-clear at the
                // shared composed seam. Rainbow Kitty's ribbon is hidden instead:
                // kept, not composed, and back where its clocks say.
                if !window.fx.cursor_mut().glow.hide_v2() {
                    window.fx.cursor_mut().glow.reset();
                }
                window.fx.cursor_mut().trail.reset();
            }
            if window.blink_reseed {
                window.blink_reseed = false;
                window.blink_epoch_seen = blink_epoch;
            } else if blink_epoch != window.blink_epoch_seen {
                window.blink_epoch_seen = blink_epoch;
                window.last_blink_at = Some(now);
                window.fx.cursor_mut().glow.note_repaint_blink(now);
                window.fx.cursor_mut().trail.note_repaint_blink(now);
            }
            window.fx.cursor_mut().glow.note_context(alt);
            window.fx.cursor_mut().trail.note_context(alt);
            let pane_col0 = u16::try_from(col).unwrap_or(u16::MAX);
            window
                .fx
                .cursor_mut()
                .glow
                .note_pane_columns(pane_col0, pane_cols);
            window
                .fx
                .cursor_mut()
                .trail
                .note_pane_columns(pane_col0, pane_cols);
            let pane_row0 = u16::try_from(row).unwrap_or(u16::MAX);
            window
                .fx
                .cursor_mut()
                .glow
                .note_pane_rows(pane_row0, pane_rows);
            let blink_recent = window
                .last_blink_at
                .is_some_and(|t| now.saturating_duration_since(t) <= BLINK_RECENT_MAX);
            let probe_trust = row_probe_trust(alt, blink_recent);
            let row_probe = if display_offset == 0 && !scroll_change.changed() {
                Some((
                    cursor
                        .0
                        .saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
                    cursor
                        .1
                        .saturating_add(u16::try_from(col).unwrap_or(u16::MAX)),
                    probe_trust,
                ))
            } else {
                None
            };
            let witness_caret_row = row_probe.map(|(r, _, _)| r).or_else(|| {
                (display_offset == 0 && scroll_change.translated_only()).then(|| {
                    cursor
                        .0
                        .saturating_add(u16::try_from(row).unwrap_or(u16::MAX))
                })
            });
            (
                row_probe,
                witness_caret_row,
                row_above_present,
                row_below_present,
            )
        };
        if witness_caret_row.is_some() && col > 0 {
            let Some(window) = self.windows.get_mut(&wid) else {
                return false;
            };
            for probe in [
                &mut window.poof_row_buf,
                &mut window.poof_row_above_buf,
                &mut window.poof_row_below_buf,
            ] {
                let len = probe.len();
                probe.resize(len.saturating_add(col), ' ');
                probe.rotate_right(col);
            }
        }
        if let Some(caret_row) = witness_caret_row {
            let mut ribbon_rows = [0u16; aterm_effects::cursor_glow::CURSOR_WITNESS_ROWS];
            let (wanted, needs_read) = {
                let Some(window) = self.windows.get_mut(&wid).map(Box::as_mut) else {
                    return false;
                };
                let WindowState {
                    fx,
                    poof_row_buf,
                    poof_row_above_buf,
                    poof_row_below_buf,
                    ..
                } = window;
                let cursor_glow = &mut fx.cursor_mut().glow;
                cursor_glow.observe_ribbon_row(caret_row, poof_row_buf);
                let wanted = cursor_glow.ribbon_rows(&mut ribbon_rows);
                let needs_read = needs_fresh_witness_read(
                    &ribbon_rows[..wanted],
                    caret_row,
                    row,
                    pane_rows,
                    row_above_present.then_some(poof_row_above_buf.as_slice()),
                    row_below_present.then_some(poof_row_below_buf.as_slice()),
                    &witness_rows,
                );
                (wanted, needs_read)
            };
            if ribbon_rows[..wanted].iter().any(|&r| r != caret_row) {
                let terminal = needs_read.then(|| crate::term_lock_ui(&term, &ui_waiting, site));
                let same_generation = terminal
                    .as_ref()
                    .is_some_and(|terminal| witness_stamp.matches(terminal));
                let grid_rows = terminal
                    .as_ref()
                    .map_or(0, |terminal| usize::from(terminal.grid().rows()));
                for &r in &ribbon_rows[..wanted] {
                    if r == caret_row {
                        continue;
                    }
                    let Some(local) = usize::from(r).checked_sub(row) else {
                        continue;
                    };
                    if local >= pane_rows {
                        continue;
                    }
                    let Some(window) = self.windows.get_mut(&wid).map(Box::as_mut) else {
                        return false;
                    };
                    let WindowState {
                        fx,
                        poof_row_above_buf,
                        poof_row_below_buf,
                        ..
                    } = window;
                    let cursor_glow = &mut fx.cursor_mut().glow;
                    if let Some(captured) = captured_witness_neighbor(
                        r,
                        caret_row,
                        row_above_present.then_some(poof_row_above_buf.as_slice()),
                        row_below_present.then_some(poof_row_below_buf.as_slice()),
                    ) {
                        cursor_glow.observe_ribbon_row(r, captured);
                    } else if let Some(captured) = witness_rows.get(r) {
                        cursor_glow.observe_ribbon_row(r, captured);
                    } else if same_generation && local < grid_rows {
                        let terminal = terminal.as_ref().expect("far witness owns a lock");
                        cursor_glow.capture_ribbon_row(r, |cols| {
                            terminal.row_cols_into(local, cols);
                            if col > 0 {
                                let len = cols.len();
                                cols.resize(len.saturating_add(col), ' ');
                                cols.rotate_right(col);
                            }
                        });
                    }
                }
            }
        }
        if let Some(window) = self.windows.get_mut(&wid) {
            window.composed_witness_rows = witness_rows;
        }
        let effect_cursor = (cursor_visible && display_offset == 0).then_some((
            cursor
                .0
                .saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
            cursor
                .1
                .saturating_add(u16::try_from(col).unwrap_or(u16::MAX)),
        ));
        let Some(blink_phase) = self.windows.get(&wid).map(|window| window.blink_phase) else {
            return false;
        };
        let Some(fx) = self.cursor_frame(
            wid,
            CursorFxInputs {
                now,
                rows,
                cols,
                cur: effect_cursor,
                live_viewport: display_offset == 0,
                cursor_visible,
                hidden_withheld: false,
                cursor_style,
                blink_phase,
                live_cursor_rgb,
                cursor_color_pinned: self.config.cursor_color.is_some() || cursor_recoloured,
                default_bg,
                default_fg,
                row_probe,
                row_probe_neighbors: Some((row_above_present, row_below_present)),
                print_anchor: (display_offset == 0)
                    .then_some(sampled_print_anchor)
                    .flatten()
                    .map(|(ar, ac, seq)| {
                        (
                            ar.saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
                            ac.saturating_add(u16::try_from(col).unwrap_or(u16::MAX)),
                            seq,
                        )
                    }),
                print_anchor_glyph: (display_offset == 0)
                    .then_some(sampled_print_anchor_glyph)
                    .flatten(),
            },
        ) else {
            return false;
        };
        let block_fill = fx.block_fill;
        let active_fill = block_fill.map(|owned| owned.fill);
        let body_style_override =
            compose_caret_style_override(fx.bolt_cursor, fx.twinkle_cursor, fx.momentum_steady);
        let Some(window) = self.windows.get_mut(&wid) else {
            return false;
        };
        window.composed_cursor_effect_valid = true;
        window.composed_cursor_effect_session = Some(terminal_view.session);
        window.composed_cursor_fill = active_fill;
        window.composed_cursor_style_override = body_style_override;
        window.composed_cursor_trail_color = fx.trail_color;
        let projected = self.project_retained_composed_cursor_effects(wid, clip, cursor_override);
        if projected && let Some(window) = self.windows.get_mut(&wid) {
            window.block_fill = presented_block_fill(
                cursor_override,
                window.input_scratch.cursor_fill_override,
                block_fill,
            );
        }
        projected
    }

    /// Push the current blink phase into the rasterizer.
    pub(crate) fn sync_blink_phase(&mut self) {
        let phase = self.front().is_none_or(|ws| ws.blink_phase);
        self.backend.set_cursor_blink_phase(phase);
    }

    /// Force the blink phase on (cursor solid) and restart the blink period.
    pub(crate) fn reset_blink(&mut self, wid: WindowId) {
        let mut flipped = false;
        if let Some(ws) = self.windows.get_mut(&wid) {
            if ws.next_blink.is_some() {
                ws.next_blink = Some(Instant::now() + BLINK_INTERVAL);
            }
            if !ws.blink_phase {
                ws.blink_phase = true;
                flipped = true;
            }
        }
        if flipped {
            self.sync_blink_phase();
            if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                w.request_redraw();
            }
        }
    }

    /// Resolve the canonical active leaf only when it is a native tab app.
    pub(crate) fn active_native_view(
        &self,
        wid: WindowId,
    ) -> Option<(crate::tab_model::AppInstanceId, crate::tab_model::ViewId)> {
        self.windows.get(&wid)?.front_content?.native()
    }

    /// Resolve/request one Settings font candidate before semantic compilation.
    pub(crate) fn prepare_native_semantic_font(
        &self,
        wid: WindowId,
        view: crate::tab_model::ViewId,
        phase_ms: u64,
    ) -> Option<crate::tray_raster::PreparedSemanticFont> {
        let state = match self.native_runtime.view_state(view) {
            Some(crate::native_app::AppViewState::Settings(state)) => state,
            _ => return None,
        };
        let candidate = state.preview_font_candidate(
            phase_ms,
            self.native_view_motion_cx(wid, view),
            self.win_font_px(wid),
        )?;
        Some(crate::tray_raster::prepare_semantic_font(&candidate))
    }

    /// Sample a preview only when the park can use its cadence, not for unfocused, overlay-covered or unrecorded headless views whose result is discarded.
    pub(crate) fn native_settings_preview_for_tick(
        &self,
        wid: WindowId,
        phase_ms: u64,
        recorded: bool,
    ) -> Option<(
        crate::tab_model::ViewId,
        crate::settings_preview::PreviewAnimation,
    )> {
        let window = self.windows.get(&wid)?;
        if !crate::native_preview_may_tick(
            window.os_window.is_some(),
            window.focused,
            recorded,
            window.overlay.is_some(),
        ) {
            return None;
        }
        self.active_native_settings_preview(wid, phase_ms)
    }

    /// The active Settings workbench while it animates or awaits a font frame.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreviewFontConvergence",
            action = "Decide",
            project = "aterm_gui::app_render::native_damage_tests::project_native_preview_font_convergence"
        )
    )]
    pub(crate) fn active_native_settings_preview(
        &self,
        wid: WindowId,
        phase_ms: u64,
    ) -> Option<(
        crate::tab_model::ViewId,
        crate::settings_preview::PreviewAnimation,
    )> {
        let viewport = self.native_ui_viewport(wid).ok()?;
        self.active_native_settings_preview_in(wid, phase_ms, viewport)
    }

    /// [`Self::active_native_settings_preview`] in a `viewport` the caller already resolved.
    pub(crate) fn active_native_settings_preview_in(
        &self,
        wid: WindowId,
        phase_ms: u64,
        viewport: crate::native_ui::LogicalRect,
    ) -> Option<(
        crate::tab_model::ViewId,
        crate::settings_preview::PreviewAnimation,
    )> {
        let (_, view) = self.active_native_view(wid)?;
        match self.native_runtime.view_state(view) {
            Some(crate::native_app::AppViewState::Settings(state)) => {
                let motion = self.native_view_motion_cx(wid, view);
                let font_px = self.win_font_px(wid);
                let mut animation = if self
                    .serious_mode_policy()
                    .allows(crate::motion::SeriousEffect::SettingsPreview)
                {
                    state.preview_animation(
                        phase_ms,
                        motion,
                        font_px,
                        self.windows[&wid].scale.max(f64::EPSILON) as f32,
                        viewport,
                    )
                } else {
                    crate::settings_preview::PreviewAnimation::None
                };
                if self
                    .prepare_native_semantic_font(wid, view, phase_ms)
                    .is_some_and(|prepared| {
                        prepared.snapshot.pending
                            || self.windows.get(&wid)
                                .and_then(|window| window.leaf_render_cache.get(&view))
                                .and_then(|cache| cache.native.as_ref())
                                .is_some_and(|raster| raster.compiled.paint.iter().any(|paint| {
                                    matches!(&paint.content,
                                        crate::native_ui::UiContent::SettingsPreview(spec)
                                        if spec.font_candidate == prepared.candidate
                                            && spec.font_ready_epoch != prepared.snapshot.ready_epoch)
                                }))
                    })
                {
                    animation = crate::settings_preview::PreviewAnimation::Continuous;
                }
                if animation == crate::settings_preview::PreviewAnimation::None
                    && let Some(now) = state.theme_demo_frame(
                        phase_ms,
                        motion,
                        font_px,
                        self.windows[&wid].scale.max(f64::EPSILON) as f32,
                    )
                    && self
                        .windows
                        .get(&wid)
                        .and_then(|window| window.leaf_render_cache.get(&view))
                        .and_then(|cache| cache.native.as_ref())
                        .is_some_and(|raster| {
                            raster.compiled.paint.iter().any(|paint| {
                                matches!(&paint.content,
                                    crate::native_ui::UiContent::SettingsPreview(spec)
                                    if spec.demo_frame().is_some_and(|drawn| drawn != now))
                            })
                        })
                {
                    animation =
                        crate::settings_preview::PreviewAnimation::BlinkEdge { after_ms: 1 };
                }
                (animation != crate::settings_preview::PreviewAnimation::None)
                    .then_some((view, animation))
            }
            _ => None,
        }
    }

    /// Advance one active Settings preview and dirty only the logical preview band of the retained app-present artifact.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativePreviewFontConvergence",
            action = "Invalidate",
            project = "aterm_gui::app_render::native_damage_tests::project_native_preview_font_convergence"
        )
    )]
    pub(crate) fn invalidate_active_native_settings_preview(
        &mut self,
        wid: WindowId,
        phase_ms: u64,
    ) -> bool {
        let Ok(viewport) = self.native_ui_viewport(wid) else {
            return false;
        };
        self.invalidate_active_native_settings_preview_in(wid, phase_ms, viewport)
    }

    /// [`Self::invalidate_active_native_settings_preview`] in a `viewport` the caller already resolved ([`Self::active_native_settings_preview_in`]).
    pub(crate) fn invalidate_active_native_settings_preview_in(
        &mut self,
        wid: WindowId,
        phase_ms: u64,
        viewport: crate::native_ui::LogicalRect,
    ) -> bool {
        let Some((view, _animation)) =
            self.active_native_settings_preview_in(wid, phase_ms, viewport)
        else {
            return false;
        };
        let prepared = self.prepare_native_semantic_font(wid, view, phase_ms);
        let ticks = |spec: &crate::settings_preview::SettingsPreviewSpec| {
            spec.scene == crate::settings_preview::PreviewScene::ThemeDemo
                || spec.animation() != crate::settings_preview::PreviewAnimation::None
                || prepared.as_ref().is_some_and(|prepared| {
                    spec.font_candidate == prepared.candidate
                        && (prepared.snapshot.pending
                            || spec.font_ready_epoch != prepared.snapshot.ready_epoch)
                })
        };
        let damage = self
            .windows
            .get(&wid)
            .and_then(|window| window.leaf_render_cache.get(&view))
            .and_then(|cache| cache.native.as_ref())
            .and_then(|raster| {
                preview_damage_from_compiled(&raster.compiled, &ticks)
                    .or_else(|| preview_damage_from_compiled(&raster.compiled, &|_| true))
            })
            .unwrap_or(crate::native_app::DamageRegion::All);
        self.invalidate_native_view_cache(wid, view, damage);
        true
    }

    fn append_native_modal_prims(
        &self,
        id: WindowId,
        prims: &mut Vec<crate::widget::DrawPrim>,
        logical_x: f32,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> bool {
        let Some(window) = self.windows.get(&id) else {
            return false;
        };
        let Some(overlay) = window.overlay.as_ref() else {
            return false;
        };
        let transform = frame_plan.map_or_else(
            || self.overlay_coordinate_transform(id),
            |plan| self.overlay_coordinate_transform_from_plan(id, plan),
        );
        let Some(transform) = transform else {
            return false;
        };
        let mut modal = overlay.model().tray(
            &transform.geom,
            self.chrome_palette(ChromeSurface::NativePage),
        );
        crate::widget::translate_prims(&mut modal.prims, logical_x, 0.0);
        prims.extend(modal.prims);
        true
    }

    /// Fold the highest-priority global paint-only card into a native or heterogeneous route's retained tray.
    pub(crate) fn compose_native_route_card(&mut self, id: WindowId) -> bool {
        use std::hash::{Hash, Hasher};

        let Some(window) = self.windows.get_mut(&id) else {
            return false;
        };
        if window.overlay.is_some() {
            window.route_card = None;
            return window.settings_card.is_some();
        }
        let Some(base) = window.settings_card.as_ref() else {
            window.route_card = None;
            return false;
        };
        let Some(host) = window.badge_card.as_ref() else {
            window.route_card = None;
            return true;
        };

        let mut identity = std::collections::hash_map::DefaultHasher::new();
        for card in [base, host] {
            card.fp.hash(&mut identity);
            card.geom.hash(&mut identity);
            card.dx.hash(&mut identity);
            card.dy.hash(&mut identity);
            card.pw.hash(&mut identity);
            card.ph.hash(&mut identity);
        }
        let fp = identity.finish() | 1;
        let x0 = base.dx.min(host.dx);
        let y0 = base.dy.min(host.dy);
        let x1 = base
            .dx
            .saturating_add(base.pw)
            .max(host.dx.saturating_add(host.pw));
        let y1 = base
            .dy
            .saturating_add(base.ph)
            .max(host.dy.saturating_add(host.ph));
        let pw = x1.saturating_sub(x0);
        let ph = y1.saturating_sub(y0);
        let mut geometry = std::collections::hash_map::DefaultHasher::new();
        (x0, y0, pw, ph).hash(&mut geometry);
        let geom = geometry.finish();
        if window
            .route_card
            .as_ref()
            .is_some_and(|card| card.fp == fp && card.geom == geom)
        {
            return true;
        }
        let Some(byte_len) = usize::try_from(
            u64::from(pw)
                .saturating_mul(u64::from(ph))
                .saturating_mul(4),
        )
        .ok() else {
            window.route_card = None;
            return false;
        };
        let mut rgba = vec![0; byte_len];
        let destination_size = (pw, ph);
        let destination_origin = (x0, y0);
        let base_ok = crate::tray_raster::composite_rgba_surface(
            &mut rgba,
            destination_size,
            destination_origin,
            &base.rgba,
            (base.pw, base.ph),
            (base.dx, base.dy),
        );
        let host_ok = base_ok
            && crate::tray_raster::composite_rgba_surface(
                &mut rgba,
                destination_size,
                destination_origin,
                &host.rgba,
                (host.pw, host.ph),
                (host.dx, host.dy),
            );
        if !host_ok {
            window.route_card = None;
            return false;
        }
        window.route_card = Some(crate::SettingsCard {
            scrim: None,
            rgba,
            pw,
            ph,
            dx: x0,
            dy: y0,
            fp,
            geom,
            raster_fingerprint_once: std::sync::OnceLock::new(),
        });
        true
    }

    /// [`Self::prepare_native_input_scratch_from_plan`] on the layout the window shows now, for a standalone caller with no frame plan (a glassless window's native commit, `App::commit_glassless_native_frame`).
    pub(crate) fn prepare_native_input_scratch(&mut self, id: WindowId) -> bool {
        let Some((plan, route @ crate::VisibleContentRoute::Native { .. })) =
            self.active_visible_frame_layout(id)
        else {
            return false;
        };
        self.prepare_layout_coordinate_space_from_plan(id, route, &plan);
        self.prepare_native_input_scratch_from_plan(id, &plan)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeLeafCacheLifetime",
            action = "Paint",
            project = "native_damage_tests::leaf_cache_projection"
        )
    )]
    fn prepare_native_input_scratch_from_plan(
        &mut self,
        id: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> bool {
        let Some(crate::VisibleContentRoute::Native { instance, view }) =
            self.visible_content_route_from_plan(plan)
        else {
            return false;
        };
        let Some((rows, cols)) = self
            .windows
            .get(&id)
            .map(|ws| (usize::from(ws.rows), usize::from(ws.cols)))
        else {
            return false;
        };
        let (cw, ch) = self.win_cell_size(id);
        let pad = self.win_pad(id);
        let scale = self
            .windows
            .get(&id)
            .map_or(1.0, |ws| ws.scale.max(f64::EPSILON) as f32);
        let Ok(viewport) = self.native_ui_full_viewport(id) else {
            return false;
        };
        let Ok(stamp) = self.native_ui_compile_stamp_for(id, instance, view, viewport) else {
            return false;
        };
        let width = u32::try_from(
            cols.saturating_mul(cw)
                .saturating_add(pad.saturating_mul(2)),
        )
        .unwrap_or(u32::MAX);
        let height = u32::try_from(rows.saturating_mul(ch).saturating_add(pad)).unwrap_or(u32::MAX);
        #[cfg(a11y_tree)]
        let overlay_open = self
            .windows
            .get(&id)
            .is_some_and(|window| window.overlay.is_some());

        let page = self.chrome_palette(ChromeSurface::NativePage);
        let theme = page.theme;
        let terminal_theme = self.theme;
        let overlay_fp = self
            .windows
            .get(&id)
            .map_or(0, |window| window.overlay_fp());
        let geom = {
            use std::hash::{Hash, Hasher};
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            cw.hash(&mut hash);
            ch.hash(&mut hash);
            rows.hash(&mut hash);
            cols.hash(&mut hash);
            self.win_pad(id).hash(&mut hash);
            self.win_pad_top(id).hash(&mut hash);
            self.win_head(id).hash(&mut hash);
            scale.to_bits().hash(&mut hash);
            hash.finish()
        };
        let damage = self
            .windows
            .get(&id)
            .and_then(|window| window.leaf_render_cache.get(&view))
            .map_or(Some(crate::native_app::DamageRegion::All), |cache| {
                pending_native_leaf_damage(cache, stamp, width, height)
            });
        let compiled = if damage.is_some() {
            match self.compiled_native_ui_for(id, instance, view, viewport) {
                Ok(compiled) => Some(compiled),
                Err(_) => return false,
            }
        } else {
            None
        };
        let mut cache = self
            .windows
            .get_mut(&id)
            .and_then(|window| window.leaf_render_cache.remove(&view))
            .unwrap_or_default();
        let leaf_rasterized = if let (Some(damage), Some(compiled)) = (damage, compiled) {
            retain_native_leaf_raster(
                &mut cache,
                crate::app_native::NativeLeafScene {
                    stamp,
                    instance,
                    view,
                    viewport,
                    damage,
                    compiled,
                },
                width,
                height,
                scale,
                page,
                terminal_theme,
            );
            true
        } else {
            false
        };
        let Some(base_raster) = cache.native.as_ref() else {
            if let Some(window) = self.windows.get_mut(&id) {
                window.leaf_render_cache.insert(view, cache);
            }
            return false;
        };
        let compiled = &base_raster.compiled;
        let fp = {
            use std::hash::{Hash, Hasher};
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            compiled.fingerprint().hash(&mut hash);
            overlay_fp.hash(&mut hash);
            stamp.hash(&mut hash);
            hash.finish() | 1
        };
        let should_raster = leaf_rasterized
            || self.windows.get(&id).is_some_and(|ws| {
                ws.settings_card
                    .as_ref()
                    .is_none_or(|card| card.fp != fp || card.geom != geom)
            });
        let raster = should_raster.then(|| {
            let mut prims = Vec::new();
            if self.append_native_modal_prims(id, &mut prims, pad as f32 / scale, Some(plan)) {
                let mut tray = compiled.tray_with_chrome(page, terminal_theme, 13.0);
                tray.prims.extend(prims);
                (
                    crate::tray_raster::rasterize_tray_pixels(
                        &tray.prims,
                        width,
                        height,
                        scale,
                        [0, 0, 0, 0],
                    ),
                    width,
                    height,
                )
            } else {
                (base_raster.rgba.clone(), width, height)
            }
        });

        let tab_strip = self.redraw_tab_strip_state(id);
        let dy = self.native_content_origin_y(id) as u32;
        let blank = chrome_band::blank_cell(page);
        let Some(ws) = self.windows.get_mut(&id) else {
            return false;
        };
        fill_divider_grid_tiled(
            &mut ws.input_scratch,
            rows,
            cols,
            theme,
            None,
            std::iter::once(pane::PaneRect {
                session: 0, // unused by the geometry-only seam fill
                row_off: 0,
                col_off: 0,
                rows: ws.rows,
                cols: ws.cols,
            }),
        );
        ws.input_scratch.fx_clip = None;
        ws.input_scratch.wallpaper = None;
        for row in &mut ws.input_scratch.cells {
            row.fill(blank);
        }
        ws.input_scratch.default_bg = theme.bg;
        ws.input_scratch.cursor_color = theme.cursor;
        ws.input_scratch.cursor_visible = false;
        ws.input_scratch.selection = aterm_core::selection::TextSelection::new();
        ws.input_scratch.snapshot_seq = ws.input_scratch.snapshot_seq.wrapping_add(1);
        if !leaf_rasterized
            && let Some(frame) = ws.native_ui_compiled.as_mut()
            && frame.stamp == stamp
        {
            frame.phase = crate::app_native::NativeCompiledPhase::Staged;
        } else {
            ws.native_ui_compiled = Some(crate::app_native::NativeCompiledFrame {
                stamp,
                phase: crate::app_native::NativeCompiledPhase::Staged,
                compiled: compiled.clone(),
            });
        }
        if let Some(native) = cache.native.as_mut() {
            if native.presented_x != 0 || native.presented_y != 0 {
                native.presented = false;
            }
            native.presented_x = 0;
            native.presented_y = 0;
        }
        ws.leaf_render_cache.insert(view, cache);
        if let Some((rgba, pw, ph)) = raster {
            ws.settings_card = Some(crate::SettingsCard {
                scrim: None,
                rgba,
                pw,
                ph,
                dx: 0,
                dy,
                fp,
                geom,
                raster_fingerprint_once: std::sync::OnceLock::new(),
            });
        }
        self.splice_tab_strip_with(id, tab_strip);
        #[cfg(a11y_tree)]
        if !overlay_open {
            self.stage_native_accessibility(id, view, plan);
        }
        true
    }

    /// Prepare a native capture from geometry resolved in this synchronous control turn.
    pub(crate) fn prepare_native_input_scratch_from_capture_plan(
        &mut self,
        id: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> bool {
        let Some(route @ crate::VisibleContentRoute::Native { .. }) =
            self.visible_content_route_from_plan(plan)
        else {
            return false;
        };
        self.prepare_layout_coordinate_space_from_plan(id, route, plan);
        self.prepare_native_input_scratch_from_plan(id, plan)
    }

    #[cfg(test)]
    pub(crate) fn prepare_heterogeneous_input_scratch(&mut self, id: WindowId) -> Option<String> {
        self.prepare_heterogeneous_input_scratch_with_cursor_fx(id, None)
    }

    #[cfg(test)]
    pub(crate) fn prepare_heterogeneous_input_scratch_with_cursor_fx(
        &mut self,
        id: WindowId,
        cursor_fx: Option<ComposedCursorFxClock>,
    ) -> Option<String> {
        let plan = self.prepare_heterogeneous_layout_plan(id)?;
        self.prepare_heterogeneous_input_scratch_with_cursor_fx_after(
            id,
            cursor_fx,
            &plan,
            true,
            || {},
            None,
        )
    }

    #[cfg(test)]
    fn prepare_heterogeneous_layout_plan(
        &mut self,
        id: WindowId,
    ) -> Option<crate::tab_model::VisibleLeafPlan> {
        let (plan, route) = self.active_visible_frame_layout(id)?;
        if route != crate::VisibleContentRoute::Heterogeneous {
            return None;
        }
        self.prepare_layout_coordinate_space_from_plan(id, route, &plan);
        Some(plan)
    }

    fn prepare_heterogeneous_input_scratch_with_cursor_fx_from_plan(
        &mut self,
        id: WindowId,
        cursor_fx: Option<ComposedCursorFxClock>,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> Option<String> {
        if self.visible_content_route_from_plan(plan)
            != Some(crate::VisibleContentRoute::Heterogeneous)
        {
            return None;
        }
        self.prepare_heterogeneous_input_scratch_with_cursor_fx_after(
            id,
            cursor_fx,
            plan,
            true,
            || {},
            None,
        )
    }

    /// Exact-outcome heterogeneous capture from a plan owned by this synchronous paint/capture turn, keeping route, panes and cursor effects on one geometry.
    pub(crate) fn prepare_heterogeneous_input_scratch_with_cursor_fx_from_plan_outcome(
        &mut self,
        id: WindowId,
        cursor_fx: Option<ComposedCursorFxClock>,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> CapturePreparation<String> {
        let Some(route @ crate::VisibleContentRoute::Heterogeneous) =
            self.visible_content_route_from_plan(plan)
        else {
            return CapturePreparation::Unavailable;
        };
        self.prepare_layout_coordinate_space_from_plan(id, route, plan);
        let mut held_retry_at = None;
        match self.prepare_heterogeneous_input_scratch_with_cursor_fx_after(
            id,
            cursor_fx,
            plan,
            false,
            || {},
            Some(&mut held_retry_at),
        ) {
            Some(title) => CapturePreparation::Ready(title),
            None => held_retry_at.map_or(CapturePreparation::Unavailable, |retry_at| {
                CapturePreparation::Held { retry_at }
            }),
        }
    }

    #[cfg(test)]
    fn prepare_heterogeneous_input_scratch_with_cursor_fx_interleaved(
        &mut self,
        id: WindowId,
        cursor_fx: Option<ComposedCursorFxClock>,
        after_extract: impl FnOnce(),
    ) -> Option<String> {
        let plan = self.prepare_heterogeneous_layout_plan(id)?;
        self.prepare_heterogeneous_input_scratch_with_cursor_fx_after(
            id,
            cursor_fx,
            &plan,
            false,
            after_extract,
            None,
        )
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeLeafCacheLifetime",
            action = "Paint",
            project = "native_damage_tests::leaf_cache_projection"
        )
    )]
    fn prepare_heterogeneous_input_scratch_with_cursor_fx_after(
        &mut self,
        id: WindowId,
        cursor_fx: Option<ComposedCursorFxClock>,
        plan: &crate::tab_model::VisibleLeafPlan,
        present: bool,
        after_extract: impl FnOnce(),
        mut held_out: Option<&mut Option<Instant>>,
    ) -> Option<String> {
        use std::hash::{Hash, Hasher};
        let (rows, cols, scale) = self.windows.get(&id).map(|window| {
            (
                usize::from(window.rows),
                usize::from(window.cols),
                window.scale.max(f64::EPSILON) as f32,
            )
        })?;
        let (cw, ch) = self.win_cell_size(id);
        let (overlay_open, overlay_fp) = self
            .windows
            .get(&id)
            .map(|window| (window.overlay.is_some(), window.overlay_fp()))?;
        let preedit_on_grid =
            matches!(self.preedit_owner(id), crate::app_input::PreeditOwner::Grid);
        let width = u32::try_from(cols.saturating_mul(cw)).unwrap_or(u32::MAX);
        let height = u32::try_from(rows.saturating_mul(ch)).unwrap_or(u32::MAX);
        let byte_len = usize::try_from(width)
            .ok()?
            .checked_mul(usize::try_from(height).ok()?)?
            .checked_mul(4)?;
        let mut pending_blits: Vec<(crate::tab_model::ViewId, u32, u32)> = Vec::new();
        let mut any_leaf_rasterized = false;
        let mut modal_native_prims = Vec::new();
        let mut native_fp = std::collections::hash_map::DefaultHasher::new();
        let mut title = "aterm".to_string();
        let mut focused_compiled = None;
        let mut focused_cursor_fx_sample: Option<FocusedComposedCursorFxSample> = None;
        let mut focused_terminal_session = None;
        let native_blank = chrome_band::blank_cell(self.chrome_palette(ChromeSurface::NativePage));

        let sync_preflight = if let Some(clock) = cursor_fx {
            let now = clock.observed_at();
            let mut sync_observations = {
                let window = self.windows.get_mut(&id)?;
                std::mem::take(&mut window.composed_sync_observation_scratch)
            };
            sync_observations.clear();
            for leaf in &plan.leaves {
                let observation = (|| {
                    let crate::tab_model::View::Terminal(terminal_view) =
                        self.view_store.get(leaf.view).copied()?
                    else {
                        return None;
                    };
                    let session = self.pool.get(terminal_view.session)?;
                    let site = crate::metrics::TermWaitSite::Redraw;
                    let term = crate::term_lock_ui(&session.term, &session.ctx.ui_waiting, site);
                    Some(SyncObservation {
                        terminal_id: term.render_identity(),
                        active: term.modes().synchronized_output(),
                        end_seq: term.sync_end_seq(),
                        open_dirty: term.sync_open_dirty(),
                    })
                })();
                if let Some(observation) = observation {
                    sync_observations.push(observation);
                }
            }
            let timeout = Duration::from_millis(
                self.session_factory
                    .terminal_config
                    .as_ref()
                    .map_or(1000, |config| config.sync_timeout_ms),
            )
            .min(SYNC_HOLD_CAP);
            let (sync_hold, retry_at) = {
                let window = self.windows.get_mut(&id)?;
                let hold =
                    update_composed_sync_frame_hold(window, &sync_observations, now, timeout);
                window.composed_sync_observation_scratch = sync_observations;
                (hold, window.sync_hold_until)
            };
            if sync_hold {
                metrics::note_redraw_sync_hold();
                let retry_at = retry_at.expect("a true mixed sync hold owns a retry deadline");
                if let Some(out) = held_out.as_deref_mut() {
                    *out = Some(retry_at);
                }
                return None;
            }
            Some((now, timeout))
        } else {
            None
        };

        let window_focused = self.windows.get(&id).is_some_and(|window| window.focused);
        let active_pane_mark = plan
            .leaves
            .iter()
            .find(|leaf| leaf.focused)
            .and_then(|leaf| {
                ActivePaneMark::resolve(
                    self.config.split_focus_mark_or_default(),
                    window_focused,
                    (
                        leaf.rect.origin.y.round().max(0.0) as usize,
                        leaf.rect.origin.x.round().max(0.0) as usize,
                        (leaf.rect.size.height.round() as usize).max(1),
                        (leaf.rect.size.width.round() as usize).max(1),
                    ),
                )
            });
        let grid = self.chrome_palette(ChromeSurface::Grid);
        let fluent_seam = self.fluent_seam(plan);
        if let Some(window) = self.windows.get_mut(&id) {
            fill_divider_grid(
                &mut window.input_scratch,
                rows,
                cols,
                grid,
                active_pane_mark,
            );
            if let Some((inks, layout)) = &fluent_seam {
                crate::pane_chrome::stamp_seam_lines(
                    &mut window.input_scratch,
                    rows,
                    cols,
                    *inks,
                    layout,
                    &[],
                );
            }
            window.input_scratch.snapshot_seq = window.input_scratch.snapshot_seq.wrapping_add(1);
            window.input_scratch.default_bg = aterm_core::render::COLOR_UNSET;
            window.input_scratch.cursor_color = aterm_core::render::COLOR_UNSET;
            window.input_scratch.wallpaper = None;
            window.input_scratch.base_y = 0;
            window.input_scratch.absolute_row_revision = 0;
            window.input_scratch.history_renumber_epoch = 0;
            window.input_scratch.selection = aterm_core::selection::TextSelection::new();
        }

        let mut abandoned = false;
        let mut terminal_ordinal = 0usize;
        for leaf in &plan.leaves {
            let Some(view) = self.view_store.get(leaf.view).copied() else {
                abandoned = true;
                break;
            };
            match view {
                crate::tab_model::View::Terminal(terminal_view) => {
                    let Some((term, ui_waiting)) = self
                        .pool
                        .get(terminal_view.session)
                        .map(|s| (s.term.clone(), s.ctx.ui_waiting.clone()))
                    else {
                        abandoned = true;
                        break;
                    };
                    let site = crate::metrics::TermWaitSite::Redraw;
                    let mut terminal = crate::term_lock_ui(&term, &ui_waiting, site);
                    if let Some((now, timeout)) = sync_preflight.as_ref() {
                        let Some(preflight) = self.windows.get(&id).and_then(|window| {
                            window
                                .composed_sync_observation_scratch
                                .get(terminal_ordinal)
                                .copied()
                        }) else {
                            abandoned = true;
                            break;
                        };
                        let current = SyncObservation {
                            terminal_id: terminal.render_identity(),
                            active: terminal.modes().synchronized_output(),
                            end_seq: terminal.sync_end_seq(),
                            open_dirty: terminal.sync_open_dirty(),
                        };
                        let (sync_hold, retry_at) = {
                            let Some(window) = self.windows.get_mut(&id) else {
                                abandoned = true;
                                break;
                            };
                            let hold =
                                recheck_sync_frame_hold(window, preflight, current, *now, *timeout);
                            let retry_at = (hold && preflight.terminal_id == current.terminal_id)
                                .then_some(window.sync_hold_until)
                                .flatten();
                            (hold, retry_at)
                        };
                        if sync_hold {
                            metrics::note_redraw_sync_hold();
                            if let Some(retry_at) = retry_at
                                && let Some(out) = held_out.as_deref_mut()
                            {
                                *out = Some(retry_at);
                            }
                            abandoned = true;
                            break;
                        }
                    }
                    terminal_ordinal += 1;
                    let Some(mut cache) = self.windows.get_mut(&id).map(|window| {
                        window
                            .leaf_render_cache
                            .remove(&leaf.view)
                            .unwrap_or_default()
                    }) else {
                        abandoned = true;
                        break;
                    };
                    let sub_rows = (leaf.rect.size.height.round() as usize).max(1);
                    let sub_cols = (leaf.rect.size.width.round() as usize).max(1);
                    if leaf.focused {
                        focused_terminal_session = Some(terminal_view.session);
                    }
                    let capture_cursor_fx_sample = leaf.focused && cursor_fx.is_some();
                    let capture_row_probes = capture_cursor_fx_sample
                        && matches!(cursor_fx, Some(ComposedCursorFxClock::Advance(_)));
                    let v2_owns_frame = capture_row_probes
                        && self
                            .windows
                            .get(&id)
                            .is_some_and(|window| window.fx.cursor().glow.v2_owns_frame());
                    let reads_neighbor_rows = capture_row_probes
                        && self
                            .windows
                            .get(&id)
                            .is_some_and(|window| window.fx.cursor().glow.reads_neighbor_rows());
                    let mut requested_witness_rows =
                        [0u16; aterm_effects::cursor_glow::CURSOR_WITNESS_ROWS];
                    let witness_n = if v2_owns_frame {
                        self.windows.get(&id).map_or(0, |window| {
                            window
                                .fx
                                .cursor()
                                .glow
                                .ribbon_rows(&mut requested_witness_rows)
                        })
                    } else {
                        0
                    };
                    let (row_probe, row_above_probe, row_below_probe, witness_rows) =
                        if capture_row_probes {
                            match self.windows.get_mut(&id) {
                                Some(window) => (
                                    std::mem::take(&mut window.poof_row_buf),
                                    std::mem::take(&mut window.poof_row_above_buf),
                                    std::mem::take(&mut window.poof_row_below_buf),
                                    std::mem::take(&mut window.composed_witness_rows),
                                ),
                                None => {
                                    abandoned = true;
                                    break;
                                }
                            }
                        } else {
                            (
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                ComposedWitnessRows::default(),
                            )
                        };
                    let previous_scroll = self
                        .windows
                        .get(&id)
                        .and_then(|window| window.fx.scroll_baseline());
                    let (terminal_title, blank, cursor_color, cursor_fx_sample, ambiguous_cjk) = {
                        terminal.cell_frame_into(&mut cache.staged_input, sub_rows, sub_cols);
                        let blank = terminal_blank_cell(&terminal);
                        let cursor_color = terminal_cursor_color(&terminal);
                        let cursor_fx_sample = capture_cursor_fx_sample.then(|| {
                            focused_composed_cursor_fx_sample(
                                terminal_view.session,
                                &terminal,
                                capture_row_probes,
                                v2_owns_frame,
                                reads_neighbor_rows,
                                (
                                    leaf.rect.origin.y.round().max(0.0) as usize,
                                    leaf.rect.origin.x.round().max(0.0) as usize,
                                ),
                                &requested_witness_rows[..witness_n],
                                previous_scroll,
                                witness_rows,
                                row_probe,
                                row_above_probe,
                                row_below_probe,
                            )
                        });
                        terminal.take_damage();
                        (
                            terminal.title_arc(),
                            blank,
                            cursor_color,
                            cursor_fx_sample,
                            terminal.modes().ambiguous_width_double,
                        )
                    };
                    drop(terminal);
                    if cursor_fx_sample.is_some() {
                        focused_cursor_fx_sample = cursor_fx_sample;
                    }
                    cache.native = None;
                    cache.native_damage = None;
                    let row = leaf.rect.origin.y.round().max(0.0) as usize;
                    let col = leaf.rect.origin.x.round().max(0.0) as usize;
                    if let Some(window) = self.windows.get_mut(&id) {
                        // The focused terminal owns the transient IME overlay in a
                        // mixed split too; paint before the blit so glyphs and the
                        // candidate anchor use its width and caret. The next fresh
                        // extraction erases a cancelled composition; the engine is
                        // untouched.
                        if leaf.focused && preedit_on_grid && cache.staged_input.display_offset == 0
                        {
                            cache.staged_input.overlay_ime_preedit(
                                &window.preedit,
                                window.preedit_caret,
                                ambiguous_cjk,
                            );
                            cache.staged_input.cursor_visible = true;
                            cache.staged_input.snapshot_seq =
                                cache.staged_input.snapshot_seq.wrapping_add(1);
                        }
                        if leaf.focused {
                            window.input_scratch.cursor_color = cursor_color;
                            window.input_scratch.base_y = cache.staged_input.base_y;
                            window.input_scratch.absolute_row_revision =
                                cache.staged_input.absolute_row_revision;
                            window.input_scratch.history_renumber_epoch =
                                cache.staged_input.history_renumber_epoch;
                        }
                        if leaf.focused
                            && cache.staged_input.cursor_visible
                            && cache.staged_input.cursor_row < sub_rows
                            && cache.staged_input.cursor_col < sub_cols
                        {
                            window.input_scratch.cursor_row = row + cache.staged_input.cursor_row;
                            window.input_scratch.cursor_col = col + cache.staged_input.cursor_col;
                            window.input_scratch.cursor_style = cache.staged_input.cursor_style;
                            window.input_scratch.cursor_visible = true;
                        }
                        blit_pane_into(
                            &mut window.input_scratch,
                            &cache.staged_input,
                            row,
                            col,
                            blank,
                        );
                        push_pane_selection(
                            &mut window.input_scratch,
                            &cache.staged_input,
                            row,
                            col,
                            sub_rows,
                            sub_cols,
                            leaf.focused,
                        );
                        window.leaf_render_cache.insert(leaf.view, cache);
                    }
                    if leaf.focused && !terminal_title.is_empty() {
                        title = terminal_title.to_string();
                    }
                }
                crate::tab_model::View::Native(native) => {
                    let leaf_rows = (leaf.rect.size.height.round() as usize).max(1);
                    let leaf_cols = (leaf.rect.size.width.round() as usize).max(1);
                    let leaf_row = leaf.rect.origin.y.round().max(0.0) as usize;
                    let leaf_col = leaf.rect.origin.x.round().max(0.0) as usize;
                    if let Some(window) = self.windows.get_mut(&id) {
                        fill_blank_pane_into(
                            &mut window.input_scratch,
                            leaf_row,
                            leaf_col,
                            leaf_rows,
                            leaf_cols,
                            native_blank,
                        );
                    }
                    let leaf_width = (leaf.rect.size.width * cw as f32).round().max(1.0) as u32;
                    let leaf_height = (leaf.rect.size.height * ch as f32).round().max(1.0) as u32;
                    let viewport = crate::native_ui::LogicalRect::new(
                        0.0,
                        0.0,
                        leaf_width as f32 / scale,
                        leaf_height as f32 / scale,
                    );
                    let Ok(stamp) =
                        self.native_ui_compile_stamp_for(id, native.instance, leaf.view, viewport)
                    else {
                        abandoned = true;
                        break;
                    };
                    stamp.hash(&mut native_fp);
                    let Some(mut cache) = self.windows.get_mut(&id).map(|window| {
                        window
                            .leaf_render_cache
                            .remove(&leaf.view)
                            .unwrap_or_default()
                    }) else {
                        abandoned = true;
                        break;
                    };
                    if let Some(damage) =
                        pending_native_leaf_damage(&cache, stamp, leaf_width, leaf_height)
                    {
                        let Ok(scene) = self.build_native_leaf_scene(
                            id,
                            native.instance,
                            leaf.view,
                            viewport,
                            damage,
                        ) else {
                            abandoned = true;
                            break;
                        };
                        debug_assert_eq!(scene.stamp, stamp);
                        debug_assert_eq!(scene.instance, native.instance);
                        debug_assert_eq!(scene.view, leaf.view);
                        debug_assert_eq!(scene.viewport, viewport);
                        debug_assert_eq!(scene.damage, damage);
                        retain_native_leaf_raster(
                            &mut cache,
                            scene,
                            leaf_width,
                            leaf_height,
                            scale,
                            self.chrome_palette(ChromeSurface::NativePage),
                            self.theme,
                        );
                        any_leaf_rasterized = true;
                    }
                    let x = (leaf.rect.origin.x * cw as f32).round().max(0.0) as u32;
                    let y = (leaf.rect.origin.y * ch as f32).round().max(0.0) as u32;
                    if let Some(raster) = cache.native.as_mut() {
                        if raster.presented_x != x || raster.presented_y != y {
                            raster.presented = false;
                        }
                        raster.presented_x = x;
                        raster.presented_y = y;
                    }
                    let Some(raster) = cache.native.as_ref() else {
                        abandoned = true;
                        break;
                    };
                    raster.compiled.fingerprint().hash(&mut native_fp);
                    pending_blits.push((leaf.view, x, y));
                    if overlay_open {
                        let logical_x = x as f32 / scale;
                        let logical_y = y as f32 / scale;
                        modal_native_prims.push(crate::widget::DrawPrim::ClipPush {
                            x: logical_x,
                            y: logical_y,
                            w: leaf_width as f32 / scale,
                            h: leaf_height as f32 / scale,
                        });
                        let mut tray = raster.compiled.tray_with_chrome(
                            self.chrome_palette(ChromeSurface::NativePage),
                            self.theme,
                            13.0,
                        );
                        crate::widget::translate_prims(&mut tray.prims, logical_x, logical_y);
                        modal_native_prims.extend(tray.prims);
                        modal_native_prims.push(crate::widget::DrawPrim::ClipPop);
                    }
                    if leaf.focused {
                        focused_compiled = Some((stamp, raster.compiled.clone()));
                        if let Ok(presentation) =
                            self.native_runtime.presentation(native.instance, leaf.view)
                        {
                            title = presentation.title;
                        }
                    }
                    if let Some(window) = self.windows.get_mut(&id) {
                        window.leaf_render_cache.insert(leaf.view, cache);
                    }
                }
            }
        }
        if abandoned {
            // A focused terminal sampled before a later leaf failed owns the
            // neighbour-probe allocations; return them even though no tick commits,
            // or repeated sync holds would churn allocations.
            if matches!(cursor_fx, Some(ComposedCursorFxClock::Advance(_)))
                && let Some(sample) = focused_cursor_fx_sample
                && let Some(window) = self.windows.get_mut(&id)
            {
                window.poof_row_buf = sample.row_probe;
                window.poof_row_above_buf = sample.row_above_probe;
                window.poof_row_below_buf = sample.row_below_probe;
                window.composed_witness_rows = sample.witness_rows;
            }
            if any_leaf_rasterized && let Some(window) = self.windows.get_mut(&id) {
                window.settings_card = None;
            }
            return None;
        }

        if let Some(window) = self.windows.get_mut(&id) {
            for leaf in &plan.leaves {
                if matches!(
                    self.view_store.get(leaf.view),
                    Some(crate::tab_model::View::Terminal(_))
                ) && let Some(cache) = window.leaf_render_cache.get_mut(&leaf.view)
                {
                    std::mem::swap(&mut cache.input, &mut cache.staged_input);
                }
            }
        }

        overlay_fp.hash(&mut native_fp);

        let card_dx = u32::try_from(self.win_pad(id)).unwrap_or(u32::MAX);
        let card_dy = u32::try_from(self.native_content_origin_y(id)).unwrap_or(u32::MAX);

        let mut geometry = std::collections::hash_map::DefaultHasher::new();
        width.hash(&mut geometry);
        height.hash(&mut geometry);
        cw.hash(&mut geometry);
        ch.hash(&mut geometry);
        scale.to_bits().hash(&mut geometry);
        card_dx.hash(&mut geometry);
        card_dy.hash(&mut geometry);
        for leaf in &plan.leaves {
            leaf.view.hash(&mut geometry);
            leaf.rect.origin.x.to_bits().hash(&mut geometry);
            leaf.rect.origin.y.to_bits().hash(&mut geometry);
            leaf.rect.size.width.to_bits().hash(&mut geometry);
            leaf.rect.size.height.to_bits().hash(&mut geometry);
        }
        let geom = geometry.finish();
        let fp = native_fp.finish() | 1;
        let tab_strip = self
            .redraw_tab_strip_state(id)
            .wrapping_add(self.refresh_pane_headers(id, plan));
        let reuse = !any_leaf_rasterized
            && self.windows.get(&id).is_some_and(|window| {
                window.settings_card.as_ref().is_some_and(|card| {
                    card.fp == fp && card.geom == geom && card.pw == width && card.ph == height
                })
            });
        let native_layer = if reuse {
            None
        } else if overlay_open {
            let appended =
                self.append_native_modal_prims(id, &mut modal_native_prims, 0.0, Some(plan));
            debug_assert!(appended, "an open overlay must provide modal primitives");
            Some(crate::tray_raster::rasterize_tray_pixels(
                &modal_native_prims,
                width,
                height,
                scale,
                [0, 0, 0, 0],
            ))
        } else {
            let mut layer = self
                .windows
                .get_mut(&id)
                .and_then(|window| window.settings_card.take())
                .map_or_else(Vec::new, |card| card.rgba);
            layer.resize(byte_len, 0);
            layer.fill(0);
            if let Some(window) = self.windows.get(&id) {
                for &(view, x, y) in &pending_blits {
                    let Some(raster) = window
                        .leaf_render_cache
                        .get(&view)
                        .and_then(|cache| cache.native.as_ref())
                    else {
                        continue;
                    };
                    blit_rgba_over(
                        &mut layer,
                        (width, height),
                        &raster.rgba,
                        (raster.width, raster.height),
                        (x, y),
                    );
                }
            }
            Some(layer)
        };

        if let Some(window) = self.windows.get_mut(&id) {
            match native_layer {
                Some(rgba) => {
                    window.settings_card = Some(crate::SettingsCard {
                        scrim: None,
                        rgba,
                        pw: width,
                        ph: height,
                        dx: card_dx,
                        dy: card_dy,
                        fp,
                        geom,
                        raster_fingerprint_once: std::sync::OnceLock::new(),
                    });
                }
                None => {
                    if let Some(card) = window.settings_card.as_mut() {
                        card.dx = card_dx;
                        card.dy = card_dy;
                    }
                }
            }
            window.native_ui_compiled =
                focused_compiled.map(|(stamp, compiled)| crate::app_native::NativeCompiledFrame {
                    stamp,
                    phase: crate::app_native::NativeCompiledPhase::Staged,
                    compiled,
                });
            window.input_scratch.snapshot_seq = window.input_scratch.snapshot_seq.wrapping_add(1);
        }
        #[cfg(a11y_tree)]
        if !overlay_open {
            self.stage_visible_native_accessibility(id, plan);
        }
        after_extract();
        if let Some(clock) = cursor_fx {
            let projected = match clock {
                ComposedCursorFxClock::Advance(_) => match focused_terminal_session {
                    Some(session) => {
                        let sample = focused_cursor_fx_sample?;
                        debug_assert_eq!(sample.session, session);
                        self.composed_cursor_frame_sampled(id, clock, Some(sample), plan)
                    }
                    None => self.composed_cursor_frame_sampled(id, clock, None, plan),
                },
                ComposedCursorFxClock::Retain { .. } => match focused_cursor_fx_sample {
                    Some(sample) => {
                        self.composed_cursor_frame_sampled(id, clock, Some(sample), plan)
                    }
                    None => self.composed_cursor_frame_sampled(id, clock, None, plan),
                },
            };
            if !projected {
                return None;
            }
        }
        match cursor_fx {
            Some(ComposedCursorFxClock::Advance(now)) => {
                self.compose_heterogeneous_output_streak(id, plan, now, present);
            }
            Some(ComposedCursorFxClock::Retain { .. }) => {
                let window = self.windows.get_mut(&id)?;
                window
                    .heterogeneous_output_streak_staged
                    .clone_from(&window.heterogeneous_output_streak_presented);
                window
                    .input_scratch
                    .nova_add
                    .clone_from(&window.heterogeneous_output_streak_presented);
            }
            None => {}
        }
        self.paint_pane_headers(id);
        self.splice_tab_strip_with(id, tab_strip);
        Some(title)
    }

    /// A surface transaction did not commit.
    pub(crate) fn rearm_dropped_present(
        &mut self,
        id: WindowId,
        reason: metrics::PresentDropReason,
    ) {
        if let Some(state) = self.windows.get_mut(&id) {
            state.on_present_dropped();
            let now = Instant::now();
            let bootstrap_occluded = reason == metrics::PresentDropReason::GpuOccluded
                && state.bootstrap_present_retry_available
                && state.present_retry.bootstrap_drop_allowed();
            let deadline = if bootstrap_occluded {
                state.bootstrap_present_retry_available = false;
                state.present_retry.on_bootstrap_drop(reason, now)
            } else {
                state.present_retry.on_drop(reason, now)
            };
            let parked = deadline.is_none();
            metrics::note_present_drop(reason, parked);
        }
    }

    fn recover_latched_gpu_loss(
        &mut self,
        id: WindowId,
        failed_reason: Option<metrics::PresentDropReason>,
        frame_started: Instant,
    ) -> bool {
        let device_lost = self.backend.gpu_mut().is_some_and(|gpu| gpu.device_lost());
        if failed_present_route(self.use_gpu, device_lost) != FailedPresentRoute::RecoverGpu {
            return false;
        }

        let _ = self.complete_latched_gpu_loss_recovery(
            id,
            failed_reason,
            frame_started,
            |app, source_id, source_drop_counted| {
                app.recover_from_gpu_loss(source_id, source_drop_counted)
            },
        );
        true
    }

    /// Run the recovery transaction after a GPU loss has been observed.
    #[must_use]
    pub(crate) fn complete_latched_gpu_loss_recovery(
        &mut self,
        id: WindowId,
        failed_reason: Option<metrics::PresentDropReason>,
        frame_started: Instant,
        recover: impl FnOnce(&mut Self, WindowId, bool) -> GpuRecoveryOutcome,
    ) -> GpuRecoveryOutcome {
        let source_drop_counted = failed_reason.is_some();

        if let Some(reason) = failed_reason {
            if let Some(state) = self.windows.get_mut(&id) {
                state.on_present_dropped();
            }
            metrics::note_present_drop(reason, false);
        }
        let _ = self.video_abort_device_loss();
        let recovered = recover(self, id, source_drop_counted);
        if source_drop_counted {
            metrics::record_redraw_total(frame_started.elapsed().as_nanos() as u64);
        }
        if recovered == GpuRecoveryOutcome::BackendUnavailable {
            // A CPU-backend construction failure gets its own typed bounded retry,
            // even after a nominally successful GPU present. Reusing the original
            // surface reason would double-count the frame and could park forever on
            // a persistent GPU condition; one count per lost frame, with the final
            // diagnostic matching the CPU-recovery retry state.
            if let Some(state) = self.windows.get_mut(&id) {
                state.on_present_dropped();
                let (accounting, parked) = rearm_failed_gpu_recovery(
                    &mut state.present_retry,
                    source_drop_counted,
                    Instant::now(),
                );
                match accounting {
                    PresentDropAccounting::Update => metrics::update_present_drop_disposition(
                        metrics::PresentDropReason::CpuAcquire,
                        parked,
                    ),
                    PresentDropAccounting::Count => {
                        metrics::note_present_drop(metrics::PresentDropReason::CpuAcquire, parked);
                    }
                }
                return GpuRecoveryOutcome::SourceRetry(GpuRecoveryRetryObservation {
                    accounting,
                    reason: metrics::PresentDropReason::CpuAcquire,
                    parked,
                });
            }
        }
        recovered
    }

    fn handle_failed_present(
        &mut self,
        id: WindowId,
        reason: metrics::PresentDropReason,
        frame_started: Instant,
    ) {
        if !self.recover_latched_gpu_loss(id, Some(reason), frame_started) {
            self.rearm_dropped_present(id, reason);
            metrics::record_redraw_total(frame_started.elapsed().as_nanos() as u64);
        }
    }

    fn reveal_successfully_presented_window(&mut self, id: WindowId) {
        if let Some(state) = self.windows.get_mut(&id)
            && state.pending_reveal.take().is_some()
            && let Some(window) = state.os_window.clone()
        {
            window.set_visible(true);
            crate::metrics::mark_first_window_visible();
            self.maybe_signal_handoff_ready();
        } else if self.handoff_ready.is_some() {
            self.maybe_signal_handoff_ready();
        }
    }

    /// The bell invert and inset-border overlay window `id` paints at `now`.
    pub(crate) fn host_visual_state(&self, id: WindowId, now: Instant) -> HostVisualState {
        let Some(window) = self.windows.get(&id) else {
            return HostVisualState::default();
        };
        let overlay_open = window.overlay_open();
        let invert = window.bell_flash.is_active(now) && !overlay_open;
        let accent = self.grid_accent();
        let overlay = if window.drag_hover && !overlay_open {
            Some(OverlayGlow {
                accent,
                wash_a: DROP_WASH_ALPHA as u8,
                border_a: DROP_BORDER_ALPHA as u8,
                border_scale_q4: 0,
            })
        } else {
            self.level_up
                .as_ref()
                .map(|level| OverlayGlow {
                    accent: level.accent(accent, now),
                    wash_a: level.wash_alpha(now),
                    border_a: level.border_alpha(now),
                    border_scale_q4: level.border_scale_q4(now),
                })
                .or_else(|| {
                    if overlay_open {
                        None
                    } else {
                        self.presence_overlay(id)
                    }
                })
        };
        HostVisualState { invert, overlay }
    }

    #[cfg(test)]
    pub(crate) fn finalize_successful_terminal_present_for_test(
        &mut self,
        id: WindowId,
        visible_route: crate::VisibleContentRoute,
        visuals: HostVisualState,
    ) {
        let Some(plan) = self.active_visible_leaf_plan(id) else {
            return;
        };
        self.finalize_successful_present(
            id,
            metrics::StartupPresentTiming::collapsed(Instant::now()),
            0,
            None,
            SuccessfulPresentRoute::Terminal,
            visible_route,
            visuals,
            &plan,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn finalize_successful_present(
        &mut self,
        id: WindowId,
        startup_timing: metrics::StartupPresentTiming,
        render_ns: u64,
        window: Option<&Arc<Window>>,
        route: SuccessfulPresentRoute,
        visible_route: crate::VisibleContentRoute,
        visuals: HostVisualState,
        presented_plan: &crate::tab_model::VisibleLeafPlan,
    ) {
        let frame_started = startup_timing.frame_started();
        let acquire_wait_ns = self.last_acquire_wait_ns(id);
        self.reveal_successfully_presented_window(id);
        let app_frame_interval = self.frame_interval;
        let presented_at = Instant::now();
        let present = self.windows.get_mut(&id).map(|state| {
            let present = acknowledge_successful_present(
                state,
                app_frame_interval,
                frame_started,
                presented_at,
            );
            state.capture_present_invert = visuals.invert;
            state.capture_present_overlay = visuals.overlay;
            match route {
                SuccessfulPresentRoute::Terminal => {
                    state.note_acquire_wait(acquire_wait_ns);
                    state.heterogeneous_output_streak_staged.clear();
                    state.heterogeneous_output_streak_presented.clear();
                }
                SuccessfulPresentRoute::Heterogeneous => {
                    state.note_acquire_wait(acquire_wait_ns);
                    std::mem::swap(
                        &mut state.heterogeneous_output_streak_staged,
                        &mut state.heterogeneous_output_streak_presented,
                    );
                    state.redraw_pending = false;
                    if let Some(frame) = state.native_ui_compiled.as_mut() {
                        frame.phase = crate::app_native::NativeCompiledPhase::Presented;
                    }
                    for leaf in &presented_plan.leaves {
                        if let Some(raster) = state
                            .leaf_render_cache
                            .get_mut(&leaf.view)
                            .and_then(|cache| cache.native.as_mut())
                        {
                            let new_native_pixels = !raster.presented;
                            raster.presented = true;
                            if new_native_pixels {
                                metrics::record_native_content_present(
                                    &mut state.present_metrics.pending_native_input,
                                    raster.stamp,
                                );
                            }
                        }
                    }
                }
                SuccessfulPresentRoute::Native {
                    view,
                    presented_stamp,
                } => {
                    state.heterogeneous_output_streak_staged.clear();
                    state.heterogeneous_output_streak_presented.clear();
                    state.redraw_pending = false;
                    if let Some(frame) = state.native_ui_compiled.as_mut()
                        && Some(frame.stamp) == presented_stamp
                    {
                        frame.phase = crate::app_native::NativeCompiledPhase::Presented;
                    }
                    if let Some(raster) = state
                        .leaf_render_cache
                        .get_mut(&view)
                        .and_then(|cache| cache.native.as_mut())
                        && Some(raster.stamp) == presented_stamp
                    {
                        let new_native_pixels = !raster.presented;
                        raster.presented = true;
                        if new_native_pixels {
                            metrics::record_native_content_present(
                                &mut state.present_metrics.pending_native_input,
                                raster.stamp,
                            );
                        }
                    }
                }
            }
            present
        });

        self.report_native_ime_cursor_area(id, presented_plan);

        if !matches!(route, SuccessfulPresentRoute::Terminal) {
            self.refresh_native_hover_for_frame(id);
        }
        self.refresh_glass_cursor_for_frame(id);

        if matches!(route, SuccessfulPresentRoute::Heterogeneous)
            && matches!(
                self.view_store.get(presented_plan.focused),
                Some(crate::tab_model::View::Terminal(_))
            )
            && !matches!(
                self.preedit_owner(id),
                crate::app_input::PreeditOwner::Find | crate::app_input::PreeditOwner::Rename
            )
            && let Some(cell) = self.windows.get(&id).and_then(|state| {
                let input = &state.input_scratch;
                (state.overlay.is_none()
                    && input.cursor_visible
                    && input.cursor_row < input.rows
                    && input.cursor_col < input.cols)
                    .then_some((input.cursor_row, input.cursor_col))
            })
        {
            self.report_ime_cursor_area_frame(id, cell);
        }

        if self
            .video_rec
            .as_ref()
            .is_some_and(|recording| recording.window == id)
        {
            self.commit_recording_presented_meta(id, visible_route, presented_plan);
        }

        let _recovered = self.recover_latched_gpu_loss(id, None, frame_started);

        let present_latency_ns = self.present_latency_ns_with_plan(id, presented_plan);
        let capture_forced = self.capture_barrier == Some(id);
        metrics::record_present(
            present_latency_ns,
            render_ns,
            startup_timing,
            self.windows.get_mut(&id).map(|ws| &mut ws.present_metrics),
            &mut self.present_observer,
            capture_forced,
        );

        #[cfg(windows)]
        {
            static JUMP_LIST: std::sync::Once = std::sync::Once::new();
            JUMP_LIST.call_once(|| {
                if cfg!(test) {
                    return;
                }
                std::thread::Builder::new()
                    .name("aterm-jumplist".into())
                    .spawn(crate::jumplist_win::install)
                    .ok();
            });
        }
        if !self.first_present_done {
            self.sync_chrome_fonts();
        }
        if !self.first_present_done {
            crate::metrics::stamp_claim(crate::metrics::ClaimStamp::FirstPresent);
        }
        self.first_present_done = true;
        metrics::record_redraw_total(frame_started.elapsed().as_nanos() as u64);

        if self
            .video_rec
            .as_ref()
            .is_some_and(|recording| recording.window == id)
        {
            let t_us = crate::metrics::now_us();
            let mut capture_stopped = false;
            if let (Some(gpu), Some(state)) = (self.backend.gpu_mut(), self.windows.get_mut(&id))
                && let Some(
                    PresentTarget::Gpu { window_gpu, .. } | PresentTarget::Virtual { window_gpu },
                ) = &mut state.present
            {
                gpu.video_after_present(window_gpu, t_us);
                capture_stopped = gpu
                    .video_status(window_gpu)
                    .is_some_and(|(_, resized_or_format_stopped)| resized_or_format_stopped);
            }
            let deadline_reached = self.video_rec.as_ref().is_some_and(|recording| {
                recording.cancel.is_cancelled() || std::time::Instant::now() >= recording.deadline
            });
            if crate::video_post_present_should_finalize(deadline_reached, capture_stopped) {
                self.video_finalize();
            }
        }

        if let Some(present) = present
            && !present.offscreen
            && self.note_present_cost(render_ns, present.frame_interval, frame_started)
        {
            aterm_log::info!(
                "load-shed: perf_reduced -> {} (render EMA vs {:.1}ms frame budget)",
                self.perf_reduced,
                present.frame_interval.as_nanos() as f64 / 1e6,
            );
            metrics::note_shed_transition(self.perf_reduced);
            let shed = self.load_shed_active();
            let gpu_post_fx = self
                .serious_mode_policy()
                .allows(crate::motion::SeriousEffect::GpuPostFx);
            let bloom_on = !shed && gpu_post_fx && self.config.cursor_trail_bloom_or_default();
            let shimmer_on = !shed && gpu_post_fx && self.config.cursor_fire_shimmer_or_default();
            if let Some(gpu) = self.backend.gpu_mut() {
                gpu.set_bloom(bloom_on);
                gpu.set_shimmer(shimmer_on);
            }
            self.settle_motion_policy_transition();
        }

        if let Some(window) = window {
            self.update_accessibility(id, window, presented_plan);
        }
        let fallback_pending = self.backend.fallback_parse_pending();
        if let Some(state) = self.windows.get_mut(&id) {
            let fallback_was = state.cpu_cache.swap_fallback_pending(fallback_pending);
            let (rearm, invalidate) = fallback_convergence_action(fallback_pending, fallback_was);
            if rearm {
                if invalidate
                    && let Some(
                        PresentTarget::Gpu { window_gpu, .. }
                        | PresentTarget::Virtual { window_gpu },
                    ) = &mut state.present
                {
                    window_gpu.invalidate_present();
                }
                state.last_present = None;
                if let Some(window) = window {
                    window.request_redraw();
                }
            }
        }
        if self.gpu_lost_remedy_floor.is_some_and(|floor| id >= floor) {
            self.gpu_lost_remedy_floor = None;
            if let Some(live) = self
                .messages
                .live_by_key(crate::message_reporters::KEY_GPU_LOST)
            {
                let row = live.id;
                self.resolve_message(row, aterm_messages::Outcome::Ok);
            }
        }
        #[cfg(any(windows, target_os = "linux"))]
        self.rearm_for_strip_band_landing(id, || {
            if let Some(window) = window {
                window.request_redraw();
            }
        });
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn rearm_for_strip_band_landing(
        &mut self,
        id: WindowId,
        request_redraw: impl FnOnce(),
    ) -> bool {
        if self.tab_strip_rows == 0 {
            return false;
        }
        let Some(state) = self.windows.get_mut(&id) else {
            return false;
        };
        if crate::tray_raster::strip_band_font_epoch() == state.strip_band_key.4 {
            return false;
        }
        if let Some(PresentTarget::Gpu { window_gpu, .. } | PresentTarget::Virtual { window_gpu }) =
            &mut state.present
        {
            window_gpu.invalidate_present();
        }
        state.last_present = None;
        request_redraw();
        true
    }

    #[cfg(test)]
    fn prepare_layout_coordinate_space(
        &mut self,
        id: WindowId,
        route: crate::VisibleContentRoute,
    ) -> bool {
        let Some(plan) = self.active_visible_leaf_plan(id) else {
            return false;
        };
        self.prepare_layout_coordinate_space_from_plan(id, route, &plan)
    }

    fn prepare_layout_coordinate_space_from_plan(
        &mut self,
        id: WindowId,
        route: crate::VisibleContentRoute,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> bool {
        let Some(focused) = plan.leaf(plan.focused) else {
            return false;
        };
        let (cell_width, cell_height) = self.win_cell_size(id);
        let (surface_width, surface_height) = self
            .windows
            .get(&id)
            .and_then(|state| state.win_px)
            .map_or((0, 0), |size| (size.width, size.height));
        let coordinate = crate::LayoutCoordinateSpaceKey {
            route,
            focused: focused.view,
            zoomed: plan.zoomed,
            x: focused.rect.origin.x.to_bits(),
            y: focused.rect.origin.y.to_bits(),
            width: focused.rect.size.width.to_bits(),
            height: focused.rect.size.height.to_bits(),
            cell_width,
            cell_height,
            pad: self.win_pad(id),
            pad_top: self.win_pad_top(id),
            head: self.win_head(id),
            scale: self
                .windows
                .get(&id)
                .map_or(1.0, |state| state.scale)
                .to_bits(),
            surface_width,
            surface_height,
        };
        let Some(state) = self.windows.get_mut(&id) else {
            return false;
        };
        Self::retain_visible_leaf_render_caches(state, route, plan);
        let composed = route.is_composed();
        state.last_composed = Some(composed);
        let previous = state.last_layout_coordinate_space;
        let coordinate_changed = previous.is_some_and(|previous| previous != coordinate);
        state.last_layout_coordinate_space = Some(coordinate);
        if coordinate_changed {
            // Every coordinate-bearing cursor/companion plane crosses this boundary
            // together; durable kitty/pet identity survives, old pane origins, hit
            // targets and animation bodies do not. The resident pet asks which
            // boundary it is: the same focused view re-gridded keeps its body,
            // another view crossfades.
            if previous.is_some_and(|previous| previous.focused == coordinate.focused) {
                regrid_torn_cursor_fx(state);
            } else {
                retire_torn_cursor_fx(state);
            }
            state.predictor.reset();
            state.fx.regrid_rain();
            state.rain_hidden_band.clear();
        }
        if composed {
            // The scroll-pill latch belongs to single-pane viewport chrome; a
            // composed frame does not paint it, so do not leave its erase
            // cadence armed.
            state.pill_shown = false;
        } else {
            state.composed_cursor_effect_valid = false;
            state.composed_cursor_effect_session = None;
        }
        composed
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeLeafCacheLifetime",
            action = "BindTerminal",
            project = "native_damage_tests::leaf_cache_projection"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeLeafCacheLifetime",
            action = "BindFirst",
            project = "native_damage_tests::leaf_cache_projection"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeLeafCacheLifetime",
            action = "BindSecond",
            project = "native_damage_tests::leaf_cache_projection"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeLeafCacheLifetime",
            action = "BindSplit",
            project = "native_damage_tests::leaf_cache_projection"
        )
    )]
    fn retain_visible_leaf_render_caches(
        state: &mut crate::WindowState,
        route: crate::VisibleContentRoute,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) {
        match route {
            crate::VisibleContentRoute::Terminal { .. } => state.leaf_render_cache.clear(),
            crate::VisibleContentRoute::Native { .. }
            | crate::VisibleContentRoute::Heterogeneous => {
                state
                    .leaf_render_cache
                    .retain(|view, _| plan.leaf(*view).is_some());
            }
        }
    }

    fn redraw_heterogeneous_window(
        &mut self,
        id: WindowId,
        frame_started: Instant,
        window: &Option<Arc<aterm_window::window::Window>>,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) {
        let Some(title) = self.prepare_heterogeneous_input_scratch_with_cursor_fx_from_plan(
            id,
            Some(ComposedCursorFxClock::Advance(frame_started)),
            plan,
        ) else {
            return;
        };
        let Some(composed) =
            self.compose_route_chrome(id, crate::VisibleContentRoute::Heterogeneous, plan)
        else {
            return;
        };
        let visuals = self.host_visual_state(id, frame_started);
        if let Some(window) = window {
            self.apply_title(id, window, &title);
        }
        let compose_ns = u64::try_from(frame_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        metrics::note_pre_present(compose_ns);
        let startup_pre_present = (!self.first_present_done).then(Instant::now);
        let present_result =
            self.present_input_scratch(id, visuals.invert, visuals.overlay, composed.caption);
        let (raster_submit_ns, startup_timing) = match present_result {
            Ok(None) => return,
            Ok(Some(work_ns)) => (
                work_ns,
                metrics::StartupPresentTiming::finish(frame_started, startup_pre_present),
            ),
            Err(reason) => {
                self.handle_failed_present(id, reason, frame_started);
                return;
            }
        };
        let render_ns = causal_render_cost_ns(compose_ns, raster_submit_ns);
        self.finalize_successful_present(
            id,
            startup_timing,
            render_ns,
            window.as_ref(),
            SuccessfulPresentRoute::Heterogeneous,
            crate::VisibleContentRoute::Heterogeneous,
            visuals,
            plan,
        );
    }

    fn redraw_native_window(
        &mut self,
        id: WindowId,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        frame_started: Instant,
        window: &Option<Arc<aterm_window::window::Window>>,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) {
        if !self.prepare_native_input_scratch_from_plan(id, plan) {
            return;
        }
        let Some(composed) = self.compose_route_chrome(
            id,
            crate::VisibleContentRoute::Native { instance, view },
            plan,
        ) else {
            return;
        };
        let visuals = self.host_visual_state(id, frame_started);

        let title = self
            .native_runtime
            .presentation(instance, view)
            .map_or_else(|_| "aterm".to_string(), |presentation| presentation.title);
        if let Some(window) = window {
            self.apply_title(id, window, &title);
        }
        let compose_ns = u64::try_from(frame_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        metrics::note_pre_present(compose_ns);
        let startup_pre_present = (!self.first_present_done).then(Instant::now);
        let present_result =
            self.present_input_scratch(id, visuals.invert, visuals.overlay, composed.caption);
        let (raster_submit_ns, startup_timing) = match present_result {
            Ok(None) => return,
            Ok(Some(work_ns)) => (
                work_ns,
                metrics::StartupPresentTiming::finish(frame_started, startup_pre_present),
            ),
            Err(reason) => {
                self.handle_failed_present(id, reason, frame_started);
                return;
            }
        };
        let render_ns = causal_render_cost_ns(compose_ns, raster_submit_ns);
        let presented_stamp = self
            .windows
            .get(&id)
            .and_then(|state| state.native_ui_compiled.as_ref().map(|frame| frame.stamp));
        self.finalize_successful_present(
            id,
            startup_timing,
            render_ns,
            window.as_ref(),
            SuccessfulPresentRoute::Native {
                view,
                presented_stamp,
            },
            crate::VisibleContentRoute::Native { instance, view },
            visuals,
            plan,
        );
    }

    fn retire_unfocused_pane_snapshots(
        ws: &mut crate::WindowState,
        route: crate::VisibleContentRoute,
    ) {
        if !matches!(
            route,
            crate::VisibleContentRoute::Terminal { composed: true }
        ) {
            ws.unfocused_pane_scratch.clear();
        }
    }

    pub(crate) fn redraw_window(&mut self, id: WindowId) {
        self.redraw_window_with_layout(id, None, None);
    }

    /// A paint the window system asked for (`WindowEvent::RedrawRequested`) owes a present even when the frame is unchanged, when the CPU surface says the window holds pixels no present reached (on Windows, rows the window gained after its last present, still in its update region).
    pub(crate) fn note_owed_repaint(&mut self, id: WindowId) {
        if crate::platform::redraw_is_exposure()
            && let Some(ws) = self.windows.get_mut(&id)
        {
            ws.invalidate_raster_caches();
            let _ = ws.present_retry.on_external_stimulus();
            ws.present_retry.recovery_redraw_outstanding = true;
            return;
        }
        if let Some(ws) = self.windows.get_mut(&id)
            && let Some(PresentTarget::Cpu { surface }) = ws.present.as_ref()
            && crate::present::CpuPresenter::owes_repaint(surface)
        {
            let _ = ws.present_retry.on_external_stimulus();
            ws.present_retry.recovery_redraw_outstanding = true;
        }
    }

    #[cfg(test)]
    pub(crate) fn redraw_window_at(&mut self, id: WindowId, frame_started: Instant) {
        self.redraw_window_with_layout(id, None, Some(frame_started));
    }

    /// Redraw from a canonical visible-leaf layout the caller resolved in this same synchronous main-thread turn.
    pub(crate) fn redraw_window_from_layout(
        &mut self,
        id: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
        route: crate::VisibleContentRoute,
    ) {
        self.redraw_window_with_layout(id, Some((plan, route)), None);
    }

    fn redraw_window_with_layout(
        &mut self,
        id: WindowId,
        frame_layout: Option<(
            &crate::tab_model::VisibleLeafPlan,
            crate::VisibleContentRoute,
        )>,
        frame_clock: Option<Instant>,
    ) {
        metrics::note_redraw_attempt();
        match self.windows.get(&id) {
            Some(ws) if ws.gpu_acquire_pending() => return,
            Some(ws) if !ws.present_retry.present_attempt_allowed() => {
                metrics::note_redraw_retry_gated();
                return;
            }
            None => return,
            Some(_) => {}
        }
        #[cfg(windows)]
        if let Some(w) = self.windows.get(&id).and_then(|ws| ws.os_window.as_ref()) {
            crate::platform_win::verify_chrome_appearance(w);
        }
        let frame_started = frame_clock.unwrap_or_else(Instant::now);
        self.prepare_band_motion(id, frame_started);
        self.overview_prepaint(id);
        if let Err(reason) = self.ensure_cpu_present_target(id) {
            self.handle_failed_present(id, reason, frame_started);
            return;
        }
        self.refresh_edr_headroom(id, frame_started);
        let sync_hold_timeout = std::time::Duration::from_millis(
            self.session_factory
                .terminal_config
                .as_ref()
                .map_or(1000, |tc| tc.sync_timeout_ms),
        )
        .min(SYNC_HOLD_CAP);
        let Some(ws0) = self.windows.get(&id) else {
            return;
        };
        let window = ws0.os_window.clone();
        match ws0.present.as_ref() {
            Some(PresentTarget::Gpu { .. }) if self.backend.is_gpu() && window.is_some() => {}
            Some(PresentTarget::Cpu { .. }) if !self.backend.is_gpu() && window.is_some() => {}
            #[cfg(test)]
            Some(PresentTarget::Cpu { surface })
                if !self.backend.is_gpu() && surface.scripted().is_some() => {}
            Some(PresentTarget::Virtual { .. }) if self.backend.is_gpu() => {}
            _ => return,
        }
        self.bind_window_renderer_state(id);
        let owned_layout = frame_layout
            .is_none()
            .then(|| self.active_visible_frame_layout(id))
            .flatten();
        let (plan, route) = match frame_layout {
            Some(layout) => layout,
            None => {
                let Some((plan, route)) = owned_layout.as_ref() else {
                    return;
                };
                (plan, *route)
            }
        };
        self.prepare_layout_coordinate_space_from_plan(id, route, plan);
        if !matches!(route, crate::VisibleContentRoute::Terminal { .. })
            && let Some(ws) = self.windows.get_mut(&id)
        {
            ws.fx.companion_mut().withdraw_hit_rect();
        }
        if let Some(ws) = self.windows.get_mut(&id) {
            Self::retire_unfocused_pane_snapshots(ws, route);
            if !matches!(route, crate::VisibleContentRoute::Terminal { .. }) {
                ws.pane_overlay.clear();
            }
        }
        let multi_pane = match route {
            crate::VisibleContentRoute::Terminal { composed } => {
                if let Some(window) = self.windows.get_mut(&id) {
                    window.route_card = None;
                }
                composed
            }
            crate::VisibleContentRoute::Heterogeneous => {
                self.redraw_heterogeneous_window(id, frame_started, &window, plan);
                return;
            }
            crate::VisibleContentRoute::Native { instance, view } => {
                if let Some(window) = self.windows.get_mut(&id) {
                    window.composed_cursor_effect_valid = false;
                    window.composed_cursor_effect_session = None;
                }
                self.redraw_native_window(id, instance, view, frame_started, &window, plan);
                return;
            }
        };
        let Some(front_terminal) = self.front_terminal_mirror(id) else {
            return;
        };
        if self.windows.get(&id).is_some_and(|ws| ws.panes_stale) {
            let site = crate::resize_ledger::Site::caller(
                crate::resize_ledger::Cause::Redraw,
                std::panic::Location::caller(),
            );
            self.resize_panes_scoped_with_active_plan(id, true, Some(plan), site);
        }
        let Some(ws0) = self.windows.get(&id) else {
            return;
        };
        let (rows, cols) = (ws0.rows as usize, ws0.cols as usize);
        let grid_top = self.win_pad_top(id) + self.win_head(id);
        let strip_rows_n = usize::from(self.chrome_rows(id));
        let visuals = self.host_visual_state(id, frame_started);
        let cursor_override =
            (!ws0.focused && window.is_some()).then_some(CursorStyle::HollowBlock);
        let blink_phase = ws0.blink_phase;
        let drag_hover = ws0.drag_hover && !ws0.overlay_open();
        let last_present = ws0.last_present;

        let streak_knobs = (
            self.output_streak_shows(),
            self.config.output_streak_intensity_or_default(),
            self.config.output_streak_tail_or_default(),
            self.config.output_streak_max_streaks_or_default(),
            self.config.output_streak_idle_secs_or_default(),
            self.config.output_streak_sound_or_default(),
        );
        let streak_sound_allowed = self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::TerminalSound);

        let tab_strip = self
            .redraw_tab_strip_state(id)
            .wrapping_add(self.refresh_pane_headers(id, plan));
        let mut single_pane_ime_caret: Option<((u16, u16), bool)> = None;
        let mut single_pane_pred_sample = None;
        let single_pane_pred_mode = if !multi_pane
            && self
                .windows
                .get(&id)
                .is_some_and(|ws| !ws.predictor.idle() || ws.pred_shown)
        {
            Some((
                self.predict_mode(),
                self.config.predictive_echo_editing_or_default(),
            ))
        } else {
            None
        };
        let preedit_owner = self.preedit_owner(id);
        let preedit_on_grid = matches!(preedit_owner, crate::app_input::PreeditOwner::Grid);
        if self.sparkle_dirty {
            self.recompute_sparkle();
        }
        if self.rain_dirty {
            self.recompute_matrix_rain();
        }
        let kitty_log_on = self.kitty_log_enabled();
        let bonk_master = self.config.trail_sounds_or_default();
        let bonk_enabled = self.curse_bonk_enabled();
        let bonk_detonations = self.curse_bonk_detonation_enabled();
        let bonk_volume = self.config.trail_sound_volume();
        let frame_presentation = self.trail_presentation();
        let frame_glow = self.glow_config_for(frame_presentation);
        let bonk_style = frame_glow.style;
        let title = if multi_pane {
            match self.redraw_compose_from_plan(
                id,
                rows,
                cols,
                visuals.invert,
                drag_hover,
                cursor_override,
                tab_strip,
                frame_started,
                plan,
            ) {
                Some(title) => title,
                None => {
                    let site = crate::metrics::TermWaitSite::Redraw;
                    let title =
                        crate::term_lock_ui(&front_terminal.term, &front_terminal.ui_waiting, site)
                            .title_arc();
                    if let Some(w) = &window {
                        self.apply_title(id, w, &title);
                    }
                    return;
                }
            }
        } else {
            let load_shed = self.load_shed_active();
            let pet_glow = frame_glow;
            let cursor_fx_live = pet_glow.enabled;
            let pet_console_owned = resident_pet_owner_present(
                frame_presentation.pet_species.is_some(),
                pet_glow.enabled
                    && self
                        .serious_mode_policy()
                        .allows(crate::motion::SeriousEffect::CursorCat),
                pet_glow.style,
            );
            let rain_session_on = self.session_rain_enabled(front_terminal.session);
            let overlay_inputs = self.pane_overlay_inputs(id, false);
            let question = self.question_key(id);
            let Some(ws) = self.windows.get_mut(&id) else {
                return;
            };
            ws.fx
                .decos_mut()
                .set_scan_session(Some(front_terminal.session));
            crate::claude_footer::undo(&mut ws.claude_footer_undo, &mut ws.input_scratch);
            crate::claude_card::strip(&mut ws.claude_card, &mut ws.input_scratch);
            ws.input_scratch.fx_occluders.clear();
            let unspliced = strip_rows_n > 0
                && ws.input_scratch.undo_host_row_prepend(
                    rows,
                    &mut ws.strip_row_pool,
                    strip_rows_n,
                );
            if !unspliced && strip_rows_n > 0 && ws.input_scratch.cells.len() > rows {
                let pool = &mut ws.strip_row_pool;
                for buf in ws.input_scratch.cells.drain(rows..) {
                    if pool.len() >= strip_rows_n {
                        break;
                    }
                    pool.push(buf);
                }
            }
            let sparkle_on = self.sparkle.is_some();
            let rain_cfg = self
                .rain
                .filter(|_| rain_session_on || ws.fx.rain().is_some());
            let site = crate::metrics::TermWaitSite::Redraw;
            let (mut term, uncontended) = match term_try_lock(&front_terminal.term) {
                Some(term) => {
                    metrics::note_term_wait(site, 0);
                    (term, true)
                }
                None => (
                    crate::term_lock_ui(&front_terminal.term, &front_terminal.ui_waiting, site),
                    false,
                ),
            };
            let cpos = term.cursor();
            let wrap_serial = term.wrap_serial();
            let cursor_visible = term.cursor_visible();
            let hidden_withheld = aterm_effects::host::hidden_caret_withheld(&term);
            let frame_unfinished = term.sync_frame_unfinished();
            let sync_end_seq = term.sync_end_seq();
            let sync_end_caret = term.sync_end_caret();
            let cursor_style = term.cursor_style();
            let (print_anchor, print_anchor_glyph) =
                aterm_effects::host::motion_print_anchor(&term);
            let live_cursor_rgb = terminal_cursor_rgb(&term);
            let cursor_recoloured = term.cursor_color_recoloured();
            let epoch = term.damage_epoch();
            let content_seq = term.content_seq();
            let cmd_done = term
                .last_completed_command()
                .and_then(|m| Some((term.completed_command_seq(), m.exit_code?)));
            let cmd_dur_ms = term
                .last_completed_command()
                .and_then(|m| m.exec_duration_ms());
            let shell_executing = term.shell_state() == aterm_core::terminal::ShellState::Executing;
            let is_alt = term.is_alternate_screen();
            let terminal_id = term.render_identity();
            let sync_observation = SyncObservation {
                terminal_id,
                active: term.modes().synchronized_output(),
                end_seq: term.sync_end_seq(),
                open_dirty: term.sync_open_dirty(),
            };
            let sync_hold =
                update_sync_frame_hold(ws, sync_observation, frame_started, sync_hold_timeout);
            if sync_hold {
                let held_title = term.title_arc();
                drop(term);
                metrics::note_redraw_sync_hold();
                if let Some(window) = &window {
                    self.apply_title(id, window, &held_title);
                }
                return;
            }
            let display_offset = term.grid().display_offset();
            let scrollback_lines = term.grid().scrollback_lines();
            let content_scroll_state = term.content_scroll_state();
            sync_cursor_effect_coordinate_space(ws, terminal_id, is_alt);
            let scroll_change = sync_cursor_effect_scroll(ws, content_scroll_state);
            let blink_epoch = term.repaint_blink_epoch();
            if ws.blink_reseed {
                ws.blink_reseed = false;
                ws.blink_epoch_seen = blink_epoch;
            } else if blink_epoch != ws.blink_epoch_seen {
                ws.blink_epoch_seen = blink_epoch;
                ws.last_blink_at = Some(frame_started);
                ws.fx.cursor_mut().glow.note_repaint_blink(frame_started);
                ws.fx.cursor_mut().trail.note_repaint_blink(frame_started);
            }
            ws.fx.cursor_mut().glow.note_context(is_alt);
            ws.fx.cursor_mut().trail.note_context(is_alt);
            ws.fx.cursor_mut().glow.note_pane_columns(0, cols);
            ws.fx.cursor_mut().trail.note_pane_columns(0, cols);
            ws.fx.cursor_mut().glow.note_pane_rows(0, rows);
            let blink_recent = ws
                .last_blink_at
                .is_some_and(|t| frame_started.saturating_duration_since(t) <= BLINK_RECENT_MAX);
            let probe_trust = row_probe_trust(is_alt, blink_recent);
            if !cursor_fx_live {
                ws.fx.cursor_mut().glow.drop_row_probe();
            }
            let mut row_probe_neighbors = None;
            let row_probe = if cursor_fx_live && display_offset == 0 && !scroll_change.changed() {
                let reads_neighbor_rows = ws.fx.cursor_mut().glow.reads_neighbor_rows();
                let (_, neighbor_above, neighbor_below) = ws.single_pane_row_probe_cache.sample(
                    &term,
                    usize::from(cpos.row),
                    rows,
                    cols,
                    reads_neighbor_rows,
                    (
                        &mut ws.poof_row_buf,
                        &mut ws.poof_row_above_buf,
                        &mut ws.poof_row_below_buf,
                    ),
                );
                ws.fx
                    .cursor_mut()
                    .glow
                    .observe_ribbon_row(cpos.row, &ws.poof_row_buf);
                let mut ribbon_rows = [0u16; aterm_effects::cursor_glow::CURSOR_WITNESS_ROWS];
                let n = ws.fx.cursor_mut().glow.ribbon_rows(&mut ribbon_rows);
                for &r in &ribbon_rows[..n] {
                    if usize::from(r) < rows && r != cpos.row {
                        if let Some(captured) = captured_witness_neighbor(
                            r,
                            cpos.row,
                            neighbor_above.then_some(ws.poof_row_above_buf.as_slice()),
                            neighbor_below.then_some(ws.poof_row_below_buf.as_slice()),
                        ) {
                            ws.fx.cursor_mut().glow.observe_ribbon_row(r, captured);
                        } else {
                            ws.fx.cursor_mut().glow.capture_ribbon_row(r, |cols| {
                                term.row_cols_into(usize::from(r), cols);
                            });
                        }
                    }
                }
                row_probe_neighbors = Some((neighbor_above, neighbor_below));
                Some((cpos.row, cpos.col, probe_trust))
            } else {
                ws.single_pane_row_probe_cache.invalidate();
                if cursor_fx_live && display_offset == 0 && scroll_change.translated_only() {
                    let glow = &mut ws.fx.cursor_mut().glow;
                    glow.capture_ribbon_row(cpos.row, |cols| {
                        term.row_cols_into(usize::from(cpos.row), cols);
                    });
                    let mut ribbon_rows = [0u16; aterm_effects::cursor_glow::CURSOR_WITNESS_ROWS];
                    let n = glow.ribbon_rows(&mut ribbon_rows);
                    for &r in &ribbon_rows[..n] {
                        if usize::from(r) < rows && r != cpos.row {
                            glow.capture_ribbon_row(r, |cols| {
                                term.row_cols_into(usize::from(r), cols);
                            });
                        }
                    }
                }
                None
            };
            let selection = term.text_selection().clone();
            let (default_bg_u32, default_fg_u32, cursor_color_u32) = terminal_frame_colors(&term);
            let title = term.title_arc();
            let deco_suppress_alt = is_alt
                && self
                    .sparkle
                    .as_ref()
                    .is_some_and(|rs| rs.cfg.suppress_in_alt_screen);
            let deco_suspend = deco_suppress_alt || load_shed;
            let rain_suppress_alt = is_alt && rain_cfg.is_some_and(|c| c.suppress_in_alt_screen);
            let rain_suspend = rain_suppress_alt || load_shed || !rain_session_on;
            if rain_cfg.is_some() && !rain_suspend && !sync_hold {
                update_rain_hidden_band(&mut ws.rain_hidden_band, term.grid().damage(), rows);
            }
            let mut deco_rescan = false;
            if sparkle_on && !deco_suspend && !sync_hold && ws.fx.decos().needs_rescan(epoch) {
                deco_rescan = true;
            }
            let rain_refresh = rain_refresh_needed(
                rain_cfg.is_some(),
                rain_suspend,
                sync_hold,
                display_offset,
                ws.fx.rain(),
                epoch,
            );
            if !(deco_rescan || rain_refresh)
                && strip_rows_n == 0
                && effect_only_snapshot_reusable(
                    uncontended,
                    &ws.input_scratch,
                    &term,
                    epoch,
                    rows,
                    cols,
                )
            {
                metrics::note_frame_refill_skipped();
            } else {
                let refill = term.cell_frame_damage_scoped_into(&mut ws.input_scratch, rows, cols);
                metrics::note_frame_refill(refill);
            }
            let ambiguous_cjk = term.modes().ambiguous_width_double;
            let pet_world_facts = pet_console_owned.then(|| {
                aterm_effects::pet_world::PetWorldFacts::read(&term, front_terminal.session)
            });
            let app_kitty_raw = ws.fx.look_mut().claim(
                front_terminal.session,
                term.current_block().or_else(|| term.all_blocks().last()),
            );
            let pred_no_echo = is_alt || term.kitty_suppresses_predictive_echo();
            let pred_coords = content_scroll_state.coordinate_generation();
            let pred_blank = terminal_blank_cell(&term);
            self.refresh_hover_for_frame_locked(id, &term);
            drop(term);
            #[cfg(test)]
            redraw_hold_hook::after_terminal_hold();
            single_pane_ime_caret = Some(((cpos.row, cpos.col), cursor_visible));
            single_pane_pred_sample = Some((display_offset != 0, pred_blank));
            let cur = (display_offset == 0).then_some((cpos.row, cpos.col));
            let cur_drawn = cursor_visible;
            let Some(fx) = self.cursor_frame_for_present(
                id,
                sync_hold,
                CursorFxInputs {
                    now: frame_started,
                    rows,
                    cols,
                    cur,
                    live_viewport: display_offset == 0,
                    cursor_visible,
                    hidden_withheld,
                    cursor_style,
                    blink_phase,
                    live_cursor_rgb,
                    cursor_color_pinned: self.config.cursor_color.is_some() || cursor_recoloured,
                    default_bg: default_bg_u32,
                    default_fg: default_fg_u32,
                    row_probe,
                    row_probe_neighbors,
                    print_anchor: (display_offset == 0).then_some(print_anchor).flatten(),
                    print_anchor_glyph: (display_offset == 0)
                        .then_some(print_anchor_glyph)
                        .flatten(),
                },
            ) else {
                if sync_hold {
                    metrics::note_redraw_sync_hold();
                    if let Some(w) = &window {
                        self.apply_title(id, w, &title);
                    }
                }
                return;
            };
            let CursorFxTick {
                win_focused,
                motion,
                cursor_motion,
                scroll_motion,
                shed_envelope,
                glow_cfg,
                trail_color,
                glow_cw,
                glow_ch,
                glow_geom,
                glow_fp,
                trail_fp,
                block_fill,
                bolt_cursor,
                twinkle_cursor,
                momentum_steady,
                ..
            } = fx;
            let word_focus =
                self.motion_focus(id, self.windows.get(&id).is_some_and(|ws| ws.focused));
            let cursor_effect_style_override =
                compose_caret_style_override(bolt_cursor, twinkle_cursor, momentum_steady);
            let crate::cursor_glow::Geom {
                origin_x, origin_y, ..
            } = glow_geom;
            self.install_window_config_assets(id);
            let front_session = self.focused_session_id(id).unwrap_or(0);
            let companion_verdict =
                self.companion_verdict_with_claim(id, app_kitty_raw, frame_started);
            let trail_presentation = self.trail_presentation();
            let pet_mode = trail_presentation.pet_species.is_some();
            let pet_species = trail_presentation
                .pet_species
                .unwrap_or(aterm_effects::kitty_pet::PetSpecies::Cat);
            let cursor_companions_allowed = self
                .serious_mode_policy()
                .allows(crate::motion::SeriousEffect::CursorCat);
            let output_echo = self
                .pool
                .get(front_terminal.session)
                .map(|session| {
                    session
                        .ctx
                        .output_echo
                        .sample(&session.ctx.sink, frame_started)
                })
                .unwrap_or_default();
            let band_geom = self.band_geometry(id);
            let claude_footer_fp = self.claude_footer_fp(id, plan);
            let deck_fp = self.stats_deck_fp(id);
            let Some(ws) = self.windows.get_mut(&id) else {
                return;
            };
            ws.fx
                .companions_mut()
                .cursor_cat
                .set_look(companion_verdict);
            let words_presentable = sparkle_words_presentable(word_focus, deco_suspend);
            let cursor_companion_presentable = cursor_companion_presentable(
                shed_companion_presentable(cursor_companions_allowed && win_focused, shed_envelope),
                display_offset == 0,
            );
            ws.fx
                .companions_mut()
                .cursor_cat
                .set_collection_presentable(frame_started, cursor_companion_presentable);
            if shed_envelope_transitioning(load_shed, shed_envelope) && ws.os_window.is_some() {
                ws.note_deco_animating(frame_started);
            }
            ws.fx
                .decos_mut()
                .set_presentable(frame_started, words_presentable);
            if !cursor_companions_allowed {
                ws.fx.companions_mut().kitty_sing = aterm_effects::kitty_sing::KittySing::default();
                ws.fx.companions_mut().music_notes.clear();
                ws.fx.companions_mut().sing_riff_bar = None;
            }
            let sing_drive = if cursor_companions_allowed
                && matches!(glow_cfg.style, crate::cursor_glow::GlowStyle::RainbowKitty)
            {
                ws.fx.companions_mut().kitty_sing.drive(frame_started)
            } else {
                0.0
            };
            if sing_drive > 0.0 {
                ws.fx.cursor_mut().glow.celebrate(frame_started, sing_drive);
                if let Some(bar) = ws.fx.companions_mut().kitty_sing.bar(frame_started)
                    && ws.fx.companions_mut().sing_riff_bar != Some(bar)
                {
                    ws.fx.companions_mut().sing_riff_bar = Some(bar);
                    if let Some(gain) = sing_riff_gain(
                        ws.focused,
                        self.config.trail_sounds_or_default(),
                        self.config.trail_sound_riff_or_default(),
                        self.config.trail_sound_volume(),
                    ) {
                        self.trail_audio.push(sing_riff_event(
                            bar,
                            gain,
                            ws.fx.companions_mut().kitty_sing.signature(),
                        ));
                    }
                }
                if let Some(fan) = ws
                    .fx
                    .companions_mut()
                    .kitty_sing
                    .take_bar_fan(frame_started)
                {
                    ws.fx
                        .cursor_mut()
                        .glow
                        .party(frame_started, fan.n, fan.ring);
                }
                if let Some(outro) = ws.fx.companions_mut().kitty_sing.take_outro(frame_started) {
                    ws.fx
                        .cursor_mut()
                        .glow
                        .party(frame_started, outro.drop, false);
                    if let Some(gain) = sing_riff_gain(
                        ws.focused,
                        self.config.trail_sounds_or_default(),
                        self.config.trail_sound_riff_or_default(),
                        self.config.trail_sound_volume(),
                    ) {
                        self.trail_audio.push(sing_outro_event(gain, outro.sig));
                    }
                }
            } else {
                ws.fx.companions_mut().kitty_sing.settle(frame_started);
                ws.fx.companions_mut().sing_riff_bar = None;
            }
            let c = ws.fx.companions_mut();
            c.cursor_cat.set_singing(
                frame_started,
                crate::kitty_cursor::SingSync {
                    drive: sing_drive,
                    beat: c.kitty_sing.beat(frame_started).unwrap_or(0.0),
                },
            );
            retire_kitty_cursor_without_owner(
                glow_cfg.enabled && cursor_companions_allowed,
                pet_mode,
                glow_cfg.style,
                sing_drive,
                &mut ws.fx.companions_mut().cursor_cat,
            );
            let animate_cat = cursor_motion.animate(crate::motion::MotionEffect::CursorGlow)
                && shed_envelope > 0.0;
            let mut cat_frame = if animate_cat {
                ws.fx.companions_mut().cursor_cat.frame(frame_started)
            } else {
                ws.fx
                    .companions_mut()
                    .cursor_cat
                    .static_frame(frame_started)
            };
            pin_pet_mode_exit(pet_mode, &mut cat_frame);
            if !animate_cat
                && sing_drive > 0.0
                && !ws.fx.companions_mut().kitty_sing.external_live()
                && ws.os_window.is_some()
            {
                ws.note_deco_animating(frame_started);
            }
            let kitty_enabled = cursor_companion_presentable
                && cursor_cat_presentation_enabled(
                    animate_cat,
                    glow_cfg.enabled && cursor_companions_allowed,
                    glow_cfg.style,
                    cat_frame.collection_hello,
                )
                || (cursor_companion_presentable && cat_frame.sing > 0.0);
            let flying_alpha = if kitty_enabled { cat_frame.alpha } else { 0 };
            let kitty_alpha = if kitty_enabled && !pet_mode {
                shed_companion_alpha(cat_frame.alpha, shed_envelope)
            } else {
                0
            };
            cat_frame.alpha = kitty_alpha;
            if let Some(facts) = pet_world_facts.as_ref() {
                let exclusions = pet_console_exclusions(
                    &ws.input_scratch,
                    !ws.preedit.is_empty(),
                    ws.predictor.pending_bounds(),
                    ws.search.is_some(),
                    ws.pet_chrome.held(),
                    ws.pet_chrome.held_overlays(),
                );
                let observed = ws.fx.companion_mut().observe_console_reserved(
                    &ws.input_scratch,
                    facts,
                    aterm_effects::pet_world::PetPane::full(&ws.input_scratch),
                    exclusions.reserve(),
                );
                metrics::note_pet_world_observation(observed);
            }
            let pet_geom = pet_frame_geom(
                (glow_geom.cw, glow_geom.ch),
                (glow_geom.rows, glow_geom.cols),
                (i32::from(origin_x), i32::from(origin_y)),
            );
            ws.pet_pointer_sampled_px = Some(ws.last_cursor_px);
            ws.fx.companion_mut().set_species(pet_species);
            let pet_tick = ws.fx.pet_prepare(aterm_effects::companion::PetFacts {
                facts: &aterm_effects::host::TerminalFacts {
                    session: front_terminal.session,
                    caret: cur,
                    cursor_visible: cur_drawn,
                    frame_unfinished,
                    sync_end_seq,
                    sync_end_caret,
                    display_offset: i32::try_from(display_offset).unwrap_or(i32::MAX),
                    live_viewport: display_offset == 0,
                    content_seq,
                    wrap_serial,
                    scrolled: scroll_change.changed(),
                    shell_executing,
                    cmd_done: cmd_done.map(|(seq, code)| (seq, code, cmd_dur_ms)),
                    alt_screen: is_alt,
                },
                host: &aterm_effects::host::HostFrameInput {
                    now: frame_started,
                    visibility: pet_visibility(win_focused),
                    reduced_motion: !cursor_motion.animate(crate::motion::MotionEffect::CursorGlow),
                    serious: !cursor_companions_allowed,
                    shed_envelope,
                    shed_active: load_shed,
                    pointer_px: Some((ws.last_cursor_px.0 as f32, ws.last_cursor_px.1 as f32)),
                    capture: aterm_effects::host::CaptureMode::Present,
                    geometry: pet_geom,
                },
                glow: aterm_effects::companion::GlowOwnership {
                    enabled: glow_cfg.enabled,
                    style: glow_cfg.style,
                    style_raw_names_pet: pet_mode,
                },
                sing: aterm_effects::host::SingFacts {
                    drive: sing_drive,
                    flying_alpha,
                },
                focused: win_focused,
                obscured: ws.overlay_open() || ws.tab_menu.is_some(),
                pane: None,
                room: Some(room_facts(
                    self.pool.get(front_terminal.session),
                    self.session_status.status(front_terminal.session),
                    None,
                    self.harness_glass
                        .get(&front_terminal.session)
                        .and_then(crate::harness_glass::SessionGlass::landed),
                )),
            });
            mark_end_cheered(&mut self.harness_glass, pet_tick.landed_seen);
            let mut verdict_cue: Option<(bool, bool)> = None;
            if let Some((code, dur_ms)) = pet_tick.completion
                && keyed_celebration_completion(
                    &mut ws.fx.companions_mut().kitty_sing,
                    &mut ws.verdict_spent,
                    front_terminal.session,
                    output_echo.last_boundary_at,
                    frame_started,
                    code,
                )
            {
                verdict_cue = verdict_voice(code != 0, dur_ms);
                if let Some((false, true)) = verdict_cue {
                    ws.fx.cursor_mut().glow.note_command_verdict(frame_started);
                }
                ws.verdict_hush |= verdict_cue.is_some();
            }
            if ws.fx.companion_mut().grieving() {
                ws.fx.cursor_mut().glow.hush_fanfare(frame_started);
            }
            if ws.fx.cursor_mut().glow.take_flow_entry() {
                ws.fx.companion_mut().note_flow(frame_started);
            }
            let escort = pet_tick.escort_caret();
            let pet = ws.fx.companion_mut().tick(pet_tick);
            ws.fx
                .offer_pet_v2(&pet.pet, glow_geom, (0, 0), frame_started);
            let glow_fp = glow_fp ^ if kitty_alpha > 0 { cat_frame.fp() } else { 0 };
            let effect_geom = crate::word_decorations::EffectGeom {
                cell_w: glow_cw as u16,
                cell_h: glow_ch as u16,
                rows: rows as u16,
                cols: cols as u16,
            };
            ws.fx.open_words();
            let routed = route_single_cursor_companion(
                ws,
                effect_geom,
                escort,
                &pet,
                cat_frame,
                animate_cat,
                kitty_alpha,
                frame_started,
            );
            let mut deco_fp = if let Some(rs) = self.sparkle.as_ref() {
                if deco_suspend {
                    ws.fx.suspend_words(frame_started);
                    0
                } else {
                    let reduced_cfg = (!motion.animate(crate::motion::MotionEffect::WordSparkles)
                        && !rs.cfg.reduced_motion)
                        .then(|| {
                            let mut c = rs.cfg.clone();
                            c.reduced_motion = true;
                            c
                        });
                    let tick_cfg = reduced_cfg.as_ref().unwrap_or(&rs.cfg);
                    let companion_at = routed.on_glass(escort, pet.body_px);
                    let mut fp = ws
                        .fx
                        .word_grid(&aterm_effects::pipeline::WordGrid {
                            now: frame_started,
                            lexicon: &rs.lexicon,
                            cfg: tick_cfg,
                            geom: effect_geom,
                            cells: &ws.input_scratch.cells,
                            line_sizes: &ws.input_scratch.line_sizes,
                            base_y: ws.input_scratch.base_y,
                            default_bg: default_bg_u32,
                            epoch,
                            rescan: deco_rescan.then_some(aterm_effects::pipeline::WordRescan {
                                birth: frame_started,
                                cursor: (display_offset == 0).then_some((cpos.row, cpos.col)),
                            }),
                            companion_at,
                            sel: crate::word_decorations::SelView {
                                sel: &selection,
                                display_offset: display_offset as i32,
                            },
                            focused: ws.focused,
                        })
                        .fp;
                    if deco_rescan {
                        // This present supplied the birth tick; a later capture must
                        // not reuse its output stamp.
                        ws.pending_deco_birth = None;
                    }
                    self.kitty_log.observe_ambient(
                        front_terminal.session,
                        ws.fx.decos_mut().drain_kitty_sightings(),
                        &rs.lexicon,
                        frame_started,
                        kitty_log_on,
                    );
                    ws.fx
                        .forward_peek_cues(frame_started, (effect_geom.cell_w, effect_geom.cell_h));
                    let bonk_gain = bonk_sound_gain(
                        ws.focused,
                        bonk_master,
                        bonk_enabled && !ws.resize_sound_quiet(frame_started),
                        tick_cfg.reduced_motion,
                        bonk_volume,
                    );
                    let curse_drain = drain_curse_bonk_cues(
                        ws.fx.decos_mut(),
                        bonk_style,
                        self.config.trail_sound_voice(),
                        effect_geom.cols,
                        bonk_gain,
                        bonk_detonations,
                        |event| self.trail_audio.push(event),
                    );
                    if ws
                        .fx
                        .companions_mut()
                        .cursor_cat
                        .on_curse(frame_started, curse_drain.wince_hits)
                        && ws.os_window.is_some()
                    {
                        ws.note_deco_animating(frame_started);
                    }
                    let dog_alpha = if win_focused
                        && !deco_suspend
                        && sparkle_on
                        && ws.fx.companions_mut().dog_cameo_session == Some(front_session)
                    {
                        ws.fx.companions_mut().dog_cameo.alpha(frame_started)
                    } else {
                        0
                    };
                    if dog_alpha > 0
                        && let Some(cell) = escort
                        && let Some(dog_look) = ws.fx.companions_mut().dog_cameo.look()
                    {
                        let cw = i32::from(effect_geom.cell_w);
                        let ch = i32::from(effect_geom.cell_h);
                        let (crow, ccol) = cell;
                        let probe = aterm_effects::word_decorations::CatFootprint {
                            x: (i32::from(ccol) * cw - 4 * cw).max(0),
                            y: i32::from(crow) * ch - ch / 2,
                            w: (4 * cw).clamp(1, i32::from(u16::MAX)) as u16,
                            h: (2 * ch).clamp(1, i32::from(u16::MAX)) as u16,
                        };
                        let colors = cursor_cat_color_key(
                            &ws.input_scratch.cells,
                            effect_geom,
                            probe,
                            default_bg_u32,
                            cursor_color_u32,
                            glow_cfg.accent,
                        );
                        let bob = if tick_cfg.reduced_motion {
                            0.0
                        } else {
                            ws.fx.companions_mut().dog_cameo.bob(frame_started)
                        };
                        if let Some(dog_fp) = ws.fx.draw_dog(
                            aterm_effects::pipeline::CanvasInk::Window,
                            aterm_effects::word_decorations::DogCameoFrame {
                                geom: effect_geom,
                                cursor: cell,
                                look: dog_look,
                                colors,
                                bob,
                                alpha: dog_alpha,
                            },
                        ) {
                            fp ^= dog_fp.rotate_left(31);
                        }
                        if ws.os_window.is_some() {
                            ws.note_deco_animating(frame_started);
                        }
                    }
                    fp
                }
            } else {
                // The config-admission edge already retired word episodes; keep the
                // shared placement and atlas warm for the cursor companion below.
                0
            };
            deco_fp ^= emit_routed_cursor_companion(
                ws,
                effect_geom,
                &pet,
                true,
                routed,
                kitty_alpha,
                frame_started,
                !animate_cat,
                default_bg_u32,
                cursor_color_u32,
                glow_cfg.accent,
            );
            let active_id = front_terminal.session;
            let shell_execute_edge = ws.fx.note_rain_shell(active_id, shell_executing);
            let rain_fp = ws.fx.rain_frame(
                rain_cfg.map(|cfg| move || crate::rain_config_for_window(cfg, id)),
                &crate::matrix_rain::RainFrame {
                    clock: crate::matrix_rain::RainClock::At(frame_started),
                    suspended: rain_suspend,
                    animate: motion.animate(crate::motion::MotionEffect::MatrixRain),
                    visibility: if ws.focused {
                        crate::matrix_rain::RainVisibility::Focused
                    } else {
                        crate::matrix_rain::RainVisibility::VisibleUnfocused
                    },
                    session: active_id,
                    content_seq,
                    execute_edge: shell_execute_edge,
                    cmd_done,
                    scan: rain_refresh.then_some(crate::matrix_rain::RainScan {
                        cells: &ws.input_scratch.cells,
                        line_sizes: &ws.input_scratch.line_sizes,
                        images: &ws.input_scratch.images,
                        rows,
                        cols,
                        default_bg: default_bg_u32,
                        epoch,
                    }),
                    cursor: cur,
                    geom: crate::word_decorations::EffectGeom {
                        cell_w: glow_cw as u16,
                        cell_h: glow_ch as u16,
                        rows: rows as u16,
                        cols: cols as u16,
                    },
                    tick: crate::matrix_rain::RainTickInput {
                        cursor: cur,
                        hidden_band: &ws.rain_hidden_band,
                        sel: Some(crate::word_decorations::SelView {
                            sel: &selection,
                            display_offset: display_offset as i32,
                        }),
                        display_offset: display_offset as i32,
                        is_alt_screen: is_alt,
                    },
                },
            );

            let (s_enabled, s_intensity, s_tail, s_max, s_idle, s_sound) = streak_knobs;
            let streak_inert = output_streak_inert(
                s_enabled,
                motion.animate(crate::motion::MotionEffect::OutputStreak),
                s_intensity,
                is_alt || display_offset != 0,
            );
            let streak = ws.fx.streak_grid(
                (!streak_inert)
                    .then(|| aterm_effects::pipeline::StreakGrid {
                        now: frame_started,
                        cfg: crate::output_streak::StreakConfig {
                            enabled: true,
                            intensity: s_intensity,
                            tail: s_tail,
                            max_streaks: s_max,
                            idle_secs: s_idle,
                            dark_theme: aterm_render::theme_is_dark(default_bg_u32),
                            theme_bg: default_bg_u32,
                            theme_fg: default_fg_u32,
                            theme_cursor: cursor_color_u32,
                            sound: s_sound && streak_sound_allowed && ws.focused,
                        },
                        seed: aterm_effects::genome::mix(id.0 ^ 0x5052_4953_4D5F_574B),
                        echo: output_echo.streak(),
                        cells: &ws.input_scratch.cells,
                        cursor: (cpos.row as usize, cpos.col as usize),
                        viewport: (rows, cols),
                        content_seq,
                        geom: glow_geom,
                    })
                    .as_ref(),
            );
            if streak.is_some_and(|(_, active)| active) {
                ws.note_deco_animating(frame_started);
            }
            let streak_fp = streak.map_or(0, |(frame, _)| frame.fp);
            let streak_cue = streak.and_then(|(frame, _)| frame.cue);
            let streak_cue = streak_cue.filter(|cue| match cue.sound {
                crate::output_streak::StreakSound::Shimmer => {
                    ws.verdict_hush = false;
                    true
                }
                crate::output_streak::StreakSound::Settle => {
                    !std::mem::replace(&mut ws.verdict_hush, false)
                }
            });
            if let Some((failed, ice)) = verdict_cue
                && let Some(gain) = voice_gain(
                    Voice::Verdict,
                    &VoiceTerms {
                        master: self.config.trail_sounds_or_default(),
                        own: true,
                        volume: self.config.trail_sound_volume(),
                        raw_focused: ws.focused,
                        reduced_motion: false,
                    },
                )
            {
                self.trail_audio
                    .push(aterm_effects::trail_sound::SoundEvent {
                        style: glow_cfg.style,
                        voice: self.config.trail_sound_voice(),
                        kind: aterm_effects::trail_sound::SoundGesture::Output(
                            aterm_effects::trail_sound::OutputGesture::Verdict { failed, ice },
                        ),
                        pan: 0.0,
                        heat: 0.0,
                        hue: 0.0,
                        gain,
                        shifted: false,
                        tone: aterm_effects::tone::Tone::Technical,
                        bed: false,
                    });
            }
            if let Some(cue) = streak_cue
                && let Some(gain) = voice_gain(
                    Voice::StreakPip,
                    &VoiceTerms {
                        master: self.config.trail_sounds_or_default(),
                        own: self.config.output_streak_sound_or_default(),
                        volume: self.config.trail_sound_volume(),
                        raw_focused: ws.focused,
                        reduced_motion: !motion.animate(crate::motion::MotionEffect::OutputStreak),
                    },
                )
            {
                self.trail_audio
                    .push(aterm_effects::trail_sound::SoundEvent {
                        style: glow_cfg.style,
                        voice: self.config.trail_sound_voice(),
                        kind: aterm_effects::trail_sound::SoundGesture::Output(
                            cue.sound.output_gesture(),
                        ),
                        pan: cue.pan,
                        heat: 0.0,
                        hue: cue.hue,
                        gain,
                        shifted: false,
                        tone: aterm_effects::tone::Tone::Technical,
                        bed: false,
                    });
            }
            let pill_fp = if let Some(inputs) = overlay_inputs {
                ws.pane_overlay.begin();
                ws.pane_overlay.note(
                    crate::pane_chrome::PaneFacts {
                        session: front_terminal.session,
                        rect: crate::pane_chrome::CellBox {
                            row: 0,
                            col: 0,
                            rows,
                            cols,
                        },
                        focused: true,
                        offset: u32::try_from(display_offset).unwrap_or(u32::MAX),
                        history: u32::try_from(scrollback_lines).unwrap_or(u32::MAX),
                        alt: is_alt,
                        cursor: None,
                        cursor_rgb: [0; 3],
                        moved: None,
                    },
                    frame_started,
                );
                ws.pane_overlay.finish(
                    inputs,
                    &mut ws.scroll_pill,
                    &mut ws.pill_shown,
                    frame_started,
                )
            } else {
                ws.pane_overlay.clear();
                let animated = scroll_motion.animate(crate::motion::MotionEffect::ScrollPill);
                let alpha = ws.scroll_pill.alpha(frame_started, animated);
                let hist = scrollback_lines;
                let mut fp = 0u64;
                if alpha > 0 && hist > 0 && rows > 0 && !is_alt {
                    let (cw, ch) = (glow_cw, glow_ch);
                    let track = (rows * ch).saturating_sub(4) as u32;
                    let min_len = ch.max(8) as u32;
                    let off = display_offset as u32;
                    if let Some((y, len)) = crate::scroll_motion::pill_geometry(
                        track,
                        min_len,
                        rows as u32,
                        hist.min(u32::MAX as usize) as u32,
                        off,
                    ) {
                        let pw = (cw / 3).clamp(2, 6);
                        let (gox, goy) = (origin_x as usize, origin_y as usize);
                        let x = gox + (cols * cw).saturating_sub(pw + 2);
                        let l = (0xA8u32 * u32::from(alpha)) / 255;
                        let color = (l << 16) | (l << 8) | l;
                        let (top, bot) = (goy + 2 + y as usize, goy + 2 + (y + len) as usize);
                        for r in 0..rows {
                            let (b0, b1) = (goy + r * ch, goy + (r + 1) * ch);
                            let (s0, s1) = (top.max(b0), bot.min(b1));
                            if s0 < s1 {
                                ws.fx
                                    .cursor_mut()
                                    .glow_scratch
                                    .push(aterm_render::GlowQuad {
                                        row: r as u16,
                                        x: x as u16,
                                        y: s0 as u16,
                                        w: pw as u16,
                                        h: (s1 - s0) as u16,
                                        color,
                                        alpha: 0,
                                        color2: color,
                                        alpha2: 0,
                                    });
                            }
                        }
                        fp = (u64::from(alpha) << 48)
                            | (u64::from(y) << 24)
                            | u64::from(len)
                            | (1 << 63);
                    }
                }
                ws.pill_shown = fp != 0;
                fp
            };
            let settings_fp = ws.overlay_fp();
            let find_fp = ws.search.as_ref().map_or(0, |s| s.fingerprint());
            let relaunch_fp = self.relaunch.as_ref().map_or(0, |n| n.fingerprint());
            let badge_fp =
                crate::build_badge::fingerprint(self.config.show_build_badge_or_default());
            let level_up_fp = self
                .level_up
                .as_ref()
                .filter(|_| !(ws.drag_hover && !ws.overlay_open()))
                .map_or(0, |l| l.fingerprint(frame_started));
            let band_fp = ws.band.band_fp(
                &self.messages,
                usize::from(ws.cols),
                ws.band_hover,
                band_geom,
            );
            let presence_fp = ws.presence.fp();
            let key = RepaintKey {
                damage_epoch: epoch,
                grid_top,
                blink_phase,
                invert: visuals.invert,
                drag_hover,
                cursor_override,
                cursor_row: cpos.row as usize,
                cursor_col: cpos.col as usize,
                cursor_visible,
                cursor_style,
                glow_fp,
                trail_fp,
                deco_fp,
                rain_fp,
                streak_fp,
                selection: SelectionFingerprint::of(&selection),
                pane_selection_fp: crate::fold_pane_selection_fp(0, 0, &selection),
                active_pane_mark: 0,
                tab_strip,
                tab_menu_fp: crate::tab_menu::fingerprint(ws.tab_menu.as_ref(), ws.tab_menu_rect)
                    ^ crate::autopilot_menu::fingerprint(self.autopilot_menu.as_ref(), id),
                settings_fp,
                find_fp,
                pill_fp,
                preedit_fp: crate::preedit_fingerprint(&ws.preedit, ws.preedit_caret),
                scroll_frac_px: if scroll_motion.animate(crate::motion::MotionEffect::SmoothScroll)
                {
                    ws.scroll_frac_px
                } else {
                    0
                },
                relaunch_fp,
                badge_fp,
                deck_fp,
                level_up_fp,
                band_fp,
                presence_fp,
                question,
                system_dark: repaint_system_dark(self.os_appearance),
                link_caption: ws.link_hover,
                claude_footer_fp,
            };
            if !deco_rescan
                && !rain_refresh
                && !should_repaint_or_recover(
                    last_present,
                    key,
                    ws.present_retry.recovery_redraw_outstanding,
                )
                && !ws.predictor.is_displaying(frame_started)
                && !ws.pred_shown
                && ws.fx_prefetch_pending.is_empty()
            {
                metrics::note_redraw_early_out();
                ws.redraws_proved_unchanged = ws.redraws_proved_unchanged.wrapping_add(1);
                self.discard_unpresented_output_stamps_from_plan(plan, frame_started);
                if let Some(w) = &window {
                    self.apply_title(id, w, &title);
                }
                return;
            }
            if let Some((pmode, editing)) = single_pane_pred_mode {
                if pmode == crate::predict::PredictMode::Off {
                    // Switched off with guesses pending: flush them, or the stranded
                    // past deadline spins the event loop.
                    ws.predictor.reset();
                } else {
                    ws.predictor.set_mode(pmode);
                    ws.predictor.set_editing(editing);
                    if display_offset == 0 {
                        let input = &ws.input_scratch;
                        ws.predictor.reconcile_seen(
                            Some((cpos.row, cpos.col)),
                            pred_coords,
                            pred_no_echo,
                            frame_started,
                            |r, c| crate::predict_seen::seen_at(input, r, c),
                        );
                    }
                }
            }
            let preedit_drawn = preedit_on_grid && !ws.preedit.is_empty() && display_offset == 0;
            if preedit_drawn {
                ws.input_scratch
                    .overlay_ime_preedit(&ws.preedit, ws.preedit_caret, ambiguous_cjk);
                single_pane_ime_caret = Some((
                    (
                        u16::try_from(ws.input_scratch.cursor_row).unwrap_or(u16::MAX),
                        u16::try_from(ws.input_scratch.cursor_col).unwrap_or(u16::MAX),
                    ),
                    true,
                ));
            }
            if preedit_drawn || ws.preedit_shown {
                ws.input_scratch.snapshot_seq = ws.input_scratch.snapshot_seq.wrapping_add(1);
            }
            ws.preedit_shown = preedit_drawn;
            ws.fx.publish_cursor(&mut ws.input_scratch);
            ws.input_scratch.cursor_effect_style_override = cursor_effect_style_override;
            if kitty_alpha > 0
                && cat_frame.fade_out > 0.0
                && !matches!(cat_frame.exit, crate::kitty_cursor::CatExit::Plain)
                && let Some((crow, ccol)) = cur
            {
                let ax = i32::from(glow_geom.origin_x) + (i32::from(ccol) + 1) * glow_cw as i32;
                let ay = i32::from(glow_geom.origin_y) + crow as i32 * glow_ch as i32;
                crate::kitty_cursor::emit_exit_fx(
                    cat_frame.exit,
                    cat_frame.fade_out,
                    ax,
                    ay,
                    glow_cfg.dark_theme,
                    glow_geom,
                    &mut ws.input_scratch.cursor_glow_add,
                    &mut ws.input_scratch.glow_halo,
                );
            }
            ws.input_scratch.cursor_fill_override =
                window_cursor_fill(cursor_override, block_fill.map(|owned| owned.fill));
            ws.input_scratch.input_hot = ws.input_hot;
            ws.input_scratch.cursor_trail_color = trail_color;
            ws.block_fill = presented_block_fill(
                cursor_override,
                ws.input_scratch.cursor_fill_override,
                block_fill,
            );
            ws.fx.publish_words(&mut ws.input_scratch);
            ws.fx.publish_rain(&mut ws.input_scratch);
            ws.input_scratch.fx_clip = None;
            ws.input_scratch.default_bg = default_bg_u32;
            ws.input_scratch.cursor_color = cursor_color_u32;
            ws.stamp_present_decision(key);
            title
        };
        if let Some((pos, vis)) = single_pane_ime_caret
            && !matches!(
                preedit_owner,
                crate::app_input::PreeditOwner::Find | crate::app_input::PreeditOwner::Rename
            )
        {
            self.report_ime_cursor_area(id, pos, (0, 0), vis);
        }
        let mut painted_pred = false;
        if !multi_pane && let Some(ws) = self.windows.get_mut(&id) {
            metrics::note_prediction_shown(&mut ws.present_metrics, None, |_| false);
        }
        if !multi_pane
            && let Some((scrolled, pred_blank)) = single_pane_pred_sample
            && self
                .windows
                .get(&id)
                .is_some_and(|ws| !ws.predictor.idle() || ws.pred_shown)
            && let Some(ws) = self.windows.get_mut(&id)
        {
            let now = frame_started;
            if scrolled {
                // The expiry flush still runs: an in-flight guess must expire, or
                // `next_deadline()` stays in the past and the repaint spins.
                let _ = ws.predictor.overlay(now);
            } else {
                let painted = paint_prediction_overlay(
                    &mut ws.predictor,
                    &mut ws.input_scratch,
                    now,
                    cols,
                    pred_blank,
                );
                let predictor = &ws.predictor;
                metrics::note_prediction_shown(
                    &mut ws.present_metrics,
                    painted.closes_input_through(),
                    |op| predictor.op_is_pending(op),
                );
                if let Some((cursor_row, cursor_col)) = painted.cursor {
                    painted_pred = true;
                    ws.input_scratch.snapshot_seq = ws.input_scratch.snapshot_seq.wrapping_add(1);
                    ws.input_scratch.cursor_row = usize::from(cursor_row);
                    ws.input_scratch.cursor_col =
                        usize::from(cursor_col).min(cols.saturating_sub(1));
                    ws.input_scratch.apron_row.cursor_col = None;
                }
            }
        }
        if !multi_pane && let Some(ws) = self.windows.get_mut(&id) {
            #[cfg(any(test, debug_assertions))]
            if ws.pred_shown && !painted_pred {
                crate::predict_trace::clear(std::time::Instant::now());
            }
            ws.pred_shown = painted_pred;
        }
        if !multi_pane {
            self.paint_pane_headers(id);
        }
        self.splice_tab_strip_with(id, tab_strip);
        if !multi_pane {
            self.set_scroll_band(id);
        } else if let Some(ws) = self.windows.get_mut(&id) {
            ws.input_scratch.scroll_frac_px = 0;
            ws.input_scratch.grid_top_row = 0;
            ws.input_scratch.grid_bot_row = 0;
            ws.input_scratch.apron_row.clear();
            ws.input_scratch.apron_stamp = None;
        }
        let Some(composed) = self.compose_route_chrome(id, route, plan) else {
            return;
        };
        if !multi_pane && let Some(ws) = self.windows.get_mut(&id) {
            ws.capture_leaf_snapshot_seqs.clear();
            ws.capture_leaf_snapshot_seqs
                .push(ws.input_scratch.snapshot_seq);
            ws.capture_leaf_content_seqs.clear();
            ws.capture_leaf_content_seqs
                .push(ws.input_scratch.content_seq);
        }
        if let Some(w) = &window {
            self.apply_title(id, w, &title);
        }

        let compose_ns = u64::try_from(frame_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        metrics::note_pre_present(compose_ns);
        let startup_pre_present = (!self.first_present_done).then(Instant::now);
        let present_result =
            self.present_input_scratch(id, visuals.invert, visuals.overlay, composed.caption);
        let (raster_submit_ns, startup_timing) = match present_result {
            Ok(None) => return,
            Ok(Some(work_ns)) => (
                work_ns,
                metrics::StartupPresentTiming::finish(frame_started, startup_pre_present),
            ),
            Err(reason) => {
                self.handle_failed_present(id, reason, frame_started);
                return;
            }
        };
        let render_ns = causal_render_cost_ns(compose_ns, raster_submit_ns)
            .saturating_add(self.gpu_backpressure_ns(id));
        self.finalize_successful_present(
            id,
            startup_timing,
            render_ns,
            window.as_ref(),
            SuccessfulPresentRoute::Terminal,
            route,
            visuals,
            plan,
        );
    }

    /// Compute this window's tab-strip fingerprint for the frame (a RepaintKey term, so it runs before the early-out), refilling the window's resident `strip_titles_scratch` in place for the splice to paint from.
    pub(crate) fn redraw_tab_strip_state(&mut self, id: WindowId) -> u64 {
        let tab_count = self.windows.get(&id).map_or(0, |ws| ws.tab_set.len());
        if self.tab_strip_enabled() && tab_count > 0 {
            let Some(mut titles) = self
                .windows
                .get_mut(&id)
                .map(|ws| std::mem::take(&mut ws.strip_titles_scratch))
            else {
                return 0;
            };
            self.refill_strip_titles(id, &mut titles);
            let mut metadata = self
                .windows
                .get_mut(&id)
                .map(|ws| std::mem::take(&mut ws.strip_metadata_scratch))
                .unwrap_or_default();
            self.refill_strip_metadata(id, &mut metadata);
            let active = self
                .windows
                .get(&id)
                .and_then(|ws| ws.tab_set.active_index())
                .unwrap_or(0);
            let fp = self.tab_strip_fingerprint_from_parts(id, &titles, &metadata, active);
            if let Some(ws) = self.windows.get_mut(&id) {
                ws.strip_titles_scratch = titles;
                ws.strip_metadata_scratch = metadata;
            }
            fp
        } else {
            0
        }
    }

    fn refill_strip_titles(&self, id: WindowId, titles: &mut Vec<String>) {
        let Some(ws) = self.windows.get(&id) else {
            titles.clear();
            return;
        };
        titles.truncate(ws.tab_set.len());
        for (i, tab) in ws.tab_set.tabs().iter().enumerate() {
            if i >= titles.len() {
                titles.push(String::new());
            }
            let slot = &mut titles[i];
            let fallback = if tab.presentation.title.is_empty() {
                "aterm"
            } else {
                &tab.presentation.title
            };
            match self.view_store.get(tab.focus).copied() {
                Some(crate::tab_model::View::Terminal(view)) => {
                    let Some(s) = self.pool.get(view.session) else {
                        slot.clear();
                        slot.push_str(fallback);
                        continue;
                    };
                    let has_user_title = {
                        let meta = match s.ctx.meta.try_lock() {
                            Ok(meta) => meta,
                            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
                            Err(std::sync::TryLockError::WouldBlock) => {
                                if slot.is_empty() {
                                    slot.push_str(fallback);
                                }
                                continue;
                            }
                        };
                        meta.presentation_value_into("title", slot)
                    };
                    if !has_user_title {
                        let term = match s.term.try_lock() {
                            Ok(t) => t,
                            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
                            Err(std::sync::TryLockError::WouldBlock) => {
                                if slot.is_empty() {
                                    slot.push_str(fallback);
                                }
                                continue;
                            }
                        };
                        let t = crate::tab_label::informative(term.title());
                        slot.clear();
                        if !t.is_empty() {
                            t.push_to(slot);
                        } else if let Some(cwd) =
                            crate::cwd_native::ReportedCwd::native_working_directory(&*term)
                                .filter(|cwd| !cwd.is_empty())
                        {
                            let cwd = cwd.as_ref();
                            match crate::app_tabs::cached_home()
                                .and_then(|home| crate::app_tabs::home_relative_suffix(cwd, home))
                            {
                                Some(rest) => {
                                    slot.push('~');
                                    slot.push_str(rest);
                                }
                                None => slot.push_str(cwd),
                            }
                        } else {
                            slot.push_str(fallback);
                        }
                    }
                    self.chrome_labels.present_into(
                        Some(view.session),
                        crate::chrome_label::ChromeSurface::TabStrip,
                        slot,
                    );
                }
                Some(crate::tab_model::View::Native(_)) | None => {
                    slot.clear();
                    slot.push_str(fallback);
                }
            }
        }
    }

    fn refill_strip_metadata(&self, id: WindowId, metadata: &mut Vec<tab_bar::TabStripMetadata>) {
        let Some(ws) = self.windows.get(&id) else {
            metadata.clear();
            return;
        };
        metadata.clear();
        metadata.extend(
            ws.tab_set
                .tabs()
                .iter()
                .map(|tab| tab_bar::TabStripMetadata::from_presentation(&tab.presentation)),
        );
        self.stamp_presence_chips(id, metadata);
    }

    fn ensure_cpu_present_target(
        &mut self,
        id: WindowId,
    ) -> Result<(), metrics::PresentDropReason> {
        if self.backend.is_gpu() {
            return Ok(());
        }
        let Some(window) = self.windows.get(&id).and_then(|ws| ws.os_window.clone()) else {
            return Ok(());
        };
        if self
            .windows
            .get(&id)
            .is_some_and(|ws| matches!(ws.present, Some(PresentTarget::Cpu { .. })))
        {
            return Ok(());
        }
        if let Some(ws) = self.windows.get_mut(&id) {
            ws.present = None;
            ws.capture_acquire_armed = false;
            ws.gpu_acquire_wait = crate::GpuAcquireWait::default();
            ws.last_present = None;
        }
        let surface = crate::present::CpuSurface::new(window)
            .map_err(|_| metrics::PresentDropReason::CpuAcquire)?;
        let Some(ws) = self.windows.get_mut(&id) else {
            return Err(metrics::PresentDropReason::TargetMismatch);
        };
        ws.present = Some(PresentTarget::Cpu { surface });
        Ok(())
    }

    fn recover_from_gpu_loss(
        &mut self,
        source_id: WindowId,
        source_drop_counted: bool,
    ) -> GpuRecoveryOutcome {
        self.recover_from_gpu_loss_with(
            source_id,
            source_drop_counted,
            |app, wid| app.ensure_cpu_present_target(wid),
            |retry, _wid, window| {
                request_gpu_recovery_redraw(retry, window, |window| window.request_redraw())
            },
        )
    }

    /// GPU-loss fallback with the two OS-dependent target operations injected.
    pub(crate) fn recover_from_gpu_loss_with<EnsureTarget, RequestRedraw>(
        &mut self,
        source_id: WindowId,
        source_drop_counted: bool,
        mut ensure_target: EnsureTarget,
        mut request_redraw: RequestRedraw,
    ) -> GpuRecoveryOutcome
    where
        EnsureTarget: FnMut(&mut Self, WindowId) -> Result<(), metrics::PresentDropReason>,
        RequestRedraw: FnMut(&mut crate::PresentRetry, WindowId, Option<&Arc<Window>>) -> bool,
    {
        if !self.use_gpu {
            return GpuRecoveryOutcome::BackendUnavailable;
        }
        let loss_reason = self
            .backend
            .gpu_mut()
            .and_then(|gpu| gpu.device_loss_reason());
        crate::logging::stderr_line!(
            "aterm-gui: GPU device lost{} — downgrading to the CPU renderer so windows keep rendering",
            loss_reason
                .as_deref()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default()
        );
        #[cfg(windows)]
        if aterm_gpu::visual_swapchain_requested() {
            aterm_gpu::withdraw_visual_swapchain();
            crate::logging::stderr_line!(
                "aterm-gui: this session's windows were created for the background_material \
                 backdrop (no redirection bitmap); the CPU fallback cannot draw into them — \
                 open a new window or restart aterm"
            );
            self.gpu_lost_remedy_floor = Some(WindowId(self.next_window_id));
            crate::message_inbox::queue_message(crate::message_reporters::gpu_lost());
            crate::platform_win::note_client_backdrop_declined(
                crate::platform_win::ClientBackdropDecline::GpuLost,
            );
        }
        let cpu = match self
            .backend
            .cpu_renderer_from_admitted(self.font_px, self.theme)
        {
            Ok(cpu) => cpu,
            Err(error) => {
                crate::logging::stderr_line!(
                    "aterm-gui: could not rebuild the resident CPU fallback after GPU loss: {error}"
                );
                return GpuRecoveryOutcome::BackendUnavailable;
            }
        };
        self.backend.swap_renderer(crate::Backend::Cpu(cpu));
        self.use_gpu = false;
        metrics::set_backend_gpu(false);
        self.pin_backend_render_config_core();
        let mut source_outcome = GpuRecoveryOutcome::SourceWithoutTarget;
        let wids: Vec<WindowId> = self.windows.keys().copied().collect();
        for wid in wids {
            let window = self.windows.get(&wid).and_then(|ws| ws.os_window.clone());
            if let Some(window) = &window {
                self.apprt.window_set_vibrancy(
                    window,
                    crate::app_config::BackgroundMaterial::None,
                    false,
                    self.theme.bg,
                );
                self.apprt
                    .window_set_appearance(window, self.chrome_theme_for_apprt());
            }
            match ensure_target(self, wid) {
                Ok(()) => {
                    let requested = self.windows.get_mut(&wid).is_some_and(|ws| {
                        // Replacing the swapchain is a surface stimulus: keep an
                        // outstanding acknowledgement until a real CPU present/drop,
                        // so a suppressed winit request is re-delivered.
                        request_redraw(&mut ws.present_retry, wid, window.as_ref())
                    });
                    if wid == source_id {
                        source_outcome = if requested {
                            GpuRecoveryOutcome::SourceReadyRequested
                        } else {
                            GpuRecoveryOutcome::SourceWithoutTarget
                        };
                    }
                }
                Err(reason) => {
                    crate::logging::stderr_line!(
                        "aterm-gui: CPU surface creation failed during GPU recovery; retrying with bounded backoff"
                    );
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        ws.on_present_dropped();
                        let _ = ws.present_retry.on_external_stimulus();
                        let (accounting, parked) = rearm_failed_gpu_recovery(
                            &mut ws.present_retry,
                            source_drop_counted,
                            Instant::now(),
                        );
                        if wid == source_id {
                            match accounting {
                                PresentDropAccounting::Update => {
                                    metrics::update_present_drop_disposition(reason, parked);
                                }
                                PresentDropAccounting::Count => {
                                    metrics::note_present_drop(reason, parked);
                                }
                            }
                            source_outcome =
                                GpuRecoveryOutcome::SourceRetry(GpuRecoveryRetryObservation {
                                    accounting,
                                    reason,
                                    parked,
                                });
                        }
                    }
                }
            }
        }
        self.warn_font_feature_issues();
        self.sync_chrome_fonts();
        source_outcome
    }

    /// Prepare and install this frame's wallpaper base layer for the terminal present.
    pub(crate) fn splice_wallpaper(&mut self, id: WindowId) {
        let fp = self.config_assets.wallpaper.fingerprint();
        let source = match &self.config_assets.wallpaper {
            crate::app_config::WallpaperAsset::Ready { w, h, rgba, .. } => {
                Some((*w as usize, *h as usize, std::sync::Arc::clone(rgba)))
            }
            _ => None,
        };
        let dim = self.config.wallpaper_dim_or_default();
        let tint_on = self.config.wallpaper_text_tint_or_default();
        let bg = self.theme.bg;
        let theme_fg = self.theme.fg;
        let Some((rows, cols)) = self
            .windows
            .get(&id)
            .map(|ws| (ws.input_scratch.rows, ws.input_scratch.cols))
        else {
            return;
        };
        let (w, h) = self.backend.raw_frame_size(rows, cols);
        let Some(ws) = self.windows.get_mut(&id) else {
            return;
        };
        let Some((sw, sh, rgba)) = source else {
            ws.wallpaper_scaled = None;
            ws.input_scratch.wallpaper = None;
            return;
        };
        if w == 0
            || h == 0
            || sw == 0
            || sh == 0
            || rgba.len() < sw.saturating_mul(sh).saturating_mul(4)
        {
            ws.wallpaper_scaled = None;
            ws.input_scratch.wallpaper = None;
            return;
        }
        let dim_bits = dim.to_bits();
        let default_fg = if ws.input_scratch.default_fg == aterm_core::render::COLOR_UNSET {
            theme_fg
        } else {
            ws.input_scratch.default_fg
        };
        let fresh = ws.wallpaper_scaled.as_ref().is_some_and(|cached| {
            cached.fp == fp
                && cached.dim_bits == dim_bits
                && cached.bg == bg
                && cached.fg == default_fg
                && cached.w == w as u32
                && cached.h == h as u32
                && cached.rows == rows
                && cached.cols == cols
        });
        if !fresh {
            let prepared = crate::wallpaper_present::prepare(
                crate::wallpaper_present::WallpaperSource {
                    rgba: &rgba,
                    w: sw,
                    h: sh,
                    fp,
                },
                (w, h),
                (rows, cols),
                dim,
                (bg, default_fg),
            );
            ws.wallpaper_scaled = Some(WallpaperScaled {
                fp,
                dim_bits,
                bg,
                fg: default_fg,
                w: w as u32,
                h: h as u32,
                rows,
                cols,
                atlas: prepared.atlas,
                tint: prepared.tint,
            });
        }
        ws.input_scratch.wallpaper = ws
            .wallpaper_scaled
            .as_ref()
            .map(|scaled| std::sync::Arc::clone(&scaled.atlas));
        if tint_on && let Some(scaled) = ws.wallpaper_scaled.as_ref() {
            let dfg = [
                ((default_fg >> 16) & 0xff) as u8,
                ((default_fg >> 8) & 0xff) as u8,
                (default_fg & 0xff) as u8,
            ];
            crate::wallpaper_present::merge_wallpaper_tint_ink(
                &ws.input_scratch.cells,
                rows,
                cols,
                dfg,
                &scaled.tint,
                &mut ws.input_scratch.ink,
                &mut ws.wallpaper_ink_scratch,
            );
        }
    }

    /// Queue `arts` for GPU residency ahead of the frame that draws them.
    pub(crate) fn queue_fx_prefetch(
        &mut self,
        wid: WindowId,
        arts: &[Arc<aterm_core::render::FxArt>],
    ) {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let before = ws.fx_prefetch_pending.len();
        for art in arts {
            if !ws.fx_prefetch_pending.iter().any(|q| Arc::ptr_eq(q, art)) {
                ws.fx_prefetch_pending.push(Arc::clone(art));
            }
        }
        if ws.fx_prefetch_pending.len() > before
            && let Some(window) = ws.os_window.as_ref()
        {
            window.request_redraw();
        }
    }

    fn present_input_scratch(
        &mut self,
        id: WindowId,
        invert: bool,
        overlay: Option<OverlayGlow>,
        caption: Option<crate::app_mouse::ShownCaption>,
    ) -> Result<Option<u64>, metrics::PresentDropReason> {
        let probe_before = startup_raster_probe_armed().then(|| self.backend.glyph_cache_len());
        if let Some(ws) = self.windows.get_mut(&id) {
            hand_fx_prefetch(&mut ws.fx_prefetch_pending, &mut ws.input_scratch);
        }
        let result = self.present_input_scratch_inner(id, invert, overlay);
        if matches!(result, Ok(Some(_)))
            && let Some(window) = self.windows.get(&id).and_then(|ws| ws.os_window.as_ref())
        {
            self.apprt.window_frame_presented(window);
        }
        if let Some(ws) = self.windows.get_mut(&id) {
            settle_fx_prefetch(
                &mut ws.fx_prefetch_pending,
                &mut ws.input_scratch,
                matches!(result, Ok(Some(_))),
            );
            crate::app_mouse::commit_presented_caption(&mut ws.shown_caption, caption, &result);
        }
        if let (Some(before), Ok(Some(_))) = (probe_before, &result) {
            note_startup_present_rasters(before, self.backend.glyph_cache_len());
        }
        result
    }

    fn present_input_scratch_inner(
        &mut self,
        id: WindowId,
        invert: bool,
        overlay: Option<OverlayGlow>,
    ) -> Result<Option<u64>, metrics::PresentDropReason> {
        let tray_floor_y = self.paste_banner_tray_floor_y(id);
        let app_frame_interval = self.frame_interval;
        let App {
            backend,
            windows,
            theme,
            ..
        } = self;
        let ws = windows
            .get_mut(&id)
            .ok_or(metrics::PresentDropReason::TargetMismatch)?;
        ws.input_scratch.refresh_cursor_effect_damage();
        metrics::note_fx_streams(&ws.input_scratch);
        if backend.is_gpu() {
            let (input_rows, input_cols) = (ws.input_scratch.rows, ws.input_scratch.cols);
            let (visible_width, visible_height) = backend.frame_size(input_rows, input_cols);
            let destination_height = ws
                .win_px
                .map_or_else(|| visible_height, |size| size.height.max(1) as usize);
            let source_crop_top =
                backend.configure_gpu_visible_y(input_rows, input_cols, destination_height);
            let present_crop = aterm_gpu::PresentCrop {
                source_y: u32::try_from(source_crop_top).unwrap_or(u32::MAX),
                height: u32::try_from(visible_height).unwrap_or(u32::MAX),
            };
            let tray_arg = ws
                .deck_fold
                .as_ref()
                .or(ws.route_card.as_ref())
                .or(ws.settings_card.as_ref())
                .or(ws.badge_card.as_ref())
                .or(ws.deck_card.as_ref())
                .and_then(|card| tray_quad_below_y(card, tray_floor_y))
                .map(|mut quad| {
                    let crop = u32::try_from(source_crop_top).unwrap_or(u32::MAX);
                    quad.dy = quad.dy.saturating_add(crop);
                    if let Some(scrim) = quad.scrim.as_mut() {
                        scrim.top = scrim.top.saturating_add(crop);
                    }
                    quad
                });
            let gpu_overlay = overlay.map(|g| aterm_gpu::DropOverlay {
                accent: g.accent,
                wash_a: g.wash_a,
                border_a: g.border_a,
                border_scale_q4: g.border_scale_q4,
            });
            let source_shift = i32::try_from(source_crop_top).ok();
            let effects_shifted = source_shift.is_some_and(|shift| {
                crate::try_shift_window_absolute_effects_y(&mut ws.input_scratch, shift)
            });
            let effects_transport_shift_y = if effects_shifted {
                source_shift.expect("a successful effect transport shift has a delta")
            } else {
                0
            };
            let present_interval_ns =
                u64::try_from(ws.frame_interval.unwrap_or(app_frame_interval).as_nanos())
                    .unwrap_or(u64::MAX);
            let present_hot = ws.input_hot;
            let prev_registered_ns = ws.last_registration_ns;
            let input = &ws.input_scratch;
            let mut pending_surface = None;
            let presented = match (backend.gpu_mut(), ws.present.as_mut()) {
                (
                    Some(gpu),
                    Some(PresentTarget::Gpu {
                        gpu_surface,
                        window_gpu,
                    }),
                ) => {
                    aterm_gpu::present_glass::arm_present(aterm_gpu::present_glass::ArmedPresent {
                        input_hot: present_hot,
                        occluded: window_gpu.occluded_hint(),
                        interval_ns: present_interval_ns,
                        prev_registered_ns,
                    });
                    let result = gpu.present_input_cropped_with_effects_transport(
                        window_gpu,
                        gpu_surface,
                        input,
                        invert,
                        gpu_overlay,
                        tray_arg,
                        present_crop,
                        effects_transport_shift_y,
                    );
                    if let Some(parked_ns) = window_gpu.take_gpu_park_sample_ns() {
                        metrics::note_gpu_park(parked_ns);
                    }
                    if let Some(queued_ns) = window_gpu.take_acquire_queue_sample_ns() {
                        metrics::note_acquire_queue(queued_ns);
                    }
                    let reported = report_gpu_surface_present(
                        result,
                        window_gpu.take_acquire_wait_sample_ns(),
                        metrics::note_acquire_wait,
                    );
                    if matches!(reported, Ok(None)) {
                        pending_surface = Some(gpu_surface.acquire_id());
                    }
                    reported.map(|presented| {
                        presented.map(|()| {
                            metrics::note_gpu_frame(
                                gpu.frame_shape(),
                                window_gpu.last_present_encode_ns(),
                            );
                            window_gpu.last_present_work_ns()
                        })
                    })
                }
                (Some(gpu), Some(PresentTarget::Virtual { window_gpu })) => {
                    if gpu.present_virtual_cropped_with_effects_transport(
                        window_gpu,
                        input,
                        invert,
                        gpu_overlay,
                        tray_arg,
                        present_crop,
                        effects_transport_shift_y,
                        (
                            u32::try_from(visible_width).unwrap_or(u32::MAX),
                            u32::try_from(visible_height).unwrap_or(u32::MAX),
                        ),
                    ) {
                        metrics::note_gpu_frame(
                            gpu.frame_shape(),
                            window_gpu.last_present_encode_ns(),
                        );
                        Ok(Some(window_gpu.last_present_work_ns()))
                    } else {
                        Err(metrics::PresentDropReason::Virtual)
                    }
                }
                _ => Err(metrics::PresentDropReason::TargetMismatch),
            };
            if matches!(presented, Ok(Some(_))) {
                ws.capture_present_frame = Some((
                    u32::try_from(visible_width).unwrap_or(u32::MAX),
                    u32::try_from(visible_height).unwrap_or(u32::MAX),
                ));
            }
            if effects_shifted {
                let restored = crate::try_shift_window_absolute_effects_y(
                    &mut ws.input_scratch,
                    -source_shift.expect("successful shift has a delta"),
                );
                debug_assert!(restored, "valid effect shift must be reversible");
            }
            if let Some(surface) = pending_surface {
                ws.on_gpu_acquire_pending(surface);
            }
            if let Some(gpu) = backend.gpu_mut() {
                metrics::note_gpu_repaint_totals(
                    gpu.scissor_taken(),
                    gpu.full_repaints(),
                    gpu.scroll_rescues(),
                );
            }
            presented
        } else {
            let Some(PresentTarget::Cpu { surface, .. }) = ws.present.as_mut() else {
                return Err(metrics::PresentDropReason::TargetMismatch);
            };
            let Backend::Cpu(r) = backend.ready_mut() else {
                return Err(metrics::PresentDropReason::TargetMismatch);
            };
            let chrome = invert
                || overlay.is_some()
                || ws.route_card.is_some()
                || ws.settings_card.is_some()
                || ws.badge_card.is_some()
                || ws.deck_card.is_some()
                || ws.lens_card.is_some();
            let chrome_prev = ws.cpu_cache.presented_chrome();
            let raster_started = Instant::now();
            let (fw, raw_fh) = {
                let view = r
                    .try_render_input_cached(&mut ws.cpu_cache, &ws.input_scratch)
                    .map_err(|error| {
                        aterm_log::warn!("CPU render refused: {error}");
                        metrics::PresentDropReason::CpuRaster
                    })?;
                (view.width().max(1), view.height().max(1))
            };
            let fh = crate::visible_frame_height(raw_fh, ws.metrics.pad, ws.metrics.pad_top);
            let pixels = ws.cpu_cache.presented_pixels();
            let translated = ws.cpu_cache.last_translated();
            let translated_prev = ws.cpu_cache.presented_translated();
            let (dw, dh) = ws.win_px.map_or((fw, fh), |s| {
                (s.width.max(1) as usize, s.height.max(1) as usize)
            });
            let place = aterm_render::frame_placement(dw, dh, fw, fh);
            let band_bg = present_band_bg(ws.input_scratch.default_bg, theme.bg);
            let chrome_edge_rows = r.chrome_extent_px(fh);
            let chrome_bands_stale = dw > fw
                && (chrome_edge_rows != ws.cpu_cache.presented_edge_rows()
                    || ws
                        .cpu_cache
                        .dirty_rows()
                        .iter()
                        .enumerate()
                        .any(|(row, &dirty)| {
                            dirty
                                && chrome_edge_rows
                                    .contains(&r.row_pixel_band(row, ws.input_scratch.rows, fh).0)
                        }));
            if surface
                .resize(
                    NonZeroU32::new(dw as u32).unwrap(),
                    NonZeroU32::new(dh as u32).unwrap(),
                )
                .is_err()
            {
                ws.cpu_cache.invalidate();
                return Err(metrics::PresentDropReason::CpuResize);
            }
            let mut causal_work_ns =
                u64::try_from(raster_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            let capture_client_requested = ws.capture_client_requested;
            let mut captured_client = None;
            let presented_work_ns = cpu_surface_transaction(surface.buffer_mut(), |mut buf| {
                let copy_started = Instant::now();
                let n = buf.len().min(dw * dh);
                let damage = ws.cpu_cache.last_damage();
                let full = cpu_present_needs_full_copy(
                    chrome || chrome_prev || chrome_bands_stale,
                    translated,
                    translated_prev,
                    damage,
                    buf.age() == 1 && buf.len() == dw * dh,
                );
                let commit = if full {
                    aterm_render::place_frame_bands(
                        &mut buf[..n],
                        dw,
                        dh,
                        pixels,
                        fw,
                        fh,
                        false,
                        band_bg,
                        chrome_edge_rows.clone(),
                    );
                    for px in buf.iter_mut().skip(n) {
                        *px = 0;
                    }
                    let tray = ws
                        .deck_fold
                        .as_ref()
                        .or(ws.route_card.as_ref())
                        .or(ws.settings_card.as_ref())
                        .or(ws.badge_card.as_ref())
                        .or(ws.deck_card.as_ref())
                        .and_then(|card| tray_quad_below_y(card, tray_floor_y));
                    apply_host_chrome_at(
                        &mut buf[..n],
                        dw,
                        dh,
                        place.x.origin,
                        place.y.origin,
                        fw,
                        fh,
                        chrome_edge_rows.clone(),
                        tray,
                        invert,
                        overlay,
                    );
                    if capture_client_requested {
                        captured_client = cpu_client_capture(&buf, dw, dh);
                    }
                    causal_work_ns = causal_work_ns.saturating_add(
                        u64::try_from(copy_started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    );
                    buf.present()
                } else if damage == DamageOutcome::GateHit {
                    let one = NonZeroU32::new(1).unwrap();
                    if capture_client_requested {
                        captured_client = cpu_client_capture(&buf, dw, dh);
                    }
                    causal_work_ns = causal_work_ns.saturating_add(
                        u64::try_from(copy_started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    );
                    buf.present_with_damage(&[crate::present::DamageRect {
                        x: 0,
                        y: 0,
                        width: one,
                        height: one,
                    }])
                } else {
                    let off_y = place.y.origin;
                    let x0 = place.x.band_before;
                    let span = place.x.visible;
                    let sx0 = place.x.crop_before;
                    let copied = cpu_damage_rects_into(
                        &mut ws.cpu_damage_rect_scratch,
                        ws.cpu_cache.dirty_rows(),
                        x0,
                        span,
                        off_y,
                        dh,
                        |row| r.row_pixel_band(row, ws.input_scratch.rows, fh),
                    );
                    if copied {
                        for rect in &ws.cpu_damage_rect_scratch {
                            let dy0 = rect.y as usize;
                            let height = rect.height.get() as usize;
                            let fy0 = usize::try_from((dy0 as i64).saturating_sub(off_y))
                                .expect("a visible damage row maps into the source frame");
                            for offset in 0..height {
                                let (dy, fy) = (dy0 + offset, fy0 + offset);
                                buf[dy * dw + x0..dy * dw + x0 + span]
                                    .copy_from_slice(&pixels[fy * fw + sx0..fy * fw + sx0 + span]);
                            }
                        }
                    }
                    if capture_client_requested {
                        captured_client = cpu_client_capture(&buf, dw, dh);
                    }
                    causal_work_ns = causal_work_ns.saturating_add(
                        u64::try_from(copy_started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    );
                    buf.present_with_damage(&ws.cpu_damage_rect_scratch)
                };
                commit.map(|()| causal_work_ns)
            });
            if presented_work_ns.is_ok() {
                ws.capture_present_frame = Some((
                    u32::try_from(fw).unwrap_or(u32::MAX),
                    u32::try_from(fh).unwrap_or(u32::MAX),
                ));
                ws.cpu_cache.set_presented_chrome(chrome);
                ws.cpu_cache.set_presented_edge_rows(chrome_edge_rows);
                ws.cpu_cache.set_presented_translated(translated);
                if capture_client_requested && let Some(client) = captured_client {
                    ws.capture_present_client = Some(client);
                    ws.capture_client_requested = false;
                }
            } else {
                ws.cpu_cache.invalidate();
            }
            presented_work_ns.map(Some)
        }
    }

    #[cfg(test)]
    pub(crate) fn present_input_scratch_for_test(
        &mut self,
        id: WindowId,
        invert: bool,
        overlay: Option<OverlayGlow>,
    ) -> Result<Option<u64>, metrics::PresentDropReason> {
        self.present_input_scratch(id, invert, overlay, None)
    }

    #[cfg(test)]
    pub(crate) fn present_latency_ns(&mut self, wid: WindowId) -> u64 {
        let Some(plan) = self.active_visible_leaf_plan(wid) else {
            return 0;
        };
        self.present_latency_ns_with_plan(wid, &plan)
    }

    fn discard_unpresented_output_stamps_from_plan(
        &self,
        plan: &crate::tab_model::VisibleLeafPlan,
        composed_at: Instant,
    ) {
        let composed_ns = u64::try_from(
            composed_at
                .saturating_duration_since(self.lat_epoch)
                .as_nanos(),
        )
        .unwrap_or(u64::MAX);
        for leaf in &plan.leaves {
            let Some(sid) = self
                .view_store
                .get(leaf.view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
            else {
                continue;
            };
            let Some(sess) = self.pool.get(sid) else {
                continue;
            };
            let mut stamp = sess.last_output_ns.load(Ordering::Relaxed);
            while stamp != 0 && stamp <= composed_ns {
                match sess.last_output_ns.compare_exchange_weak(
                    stamp,
                    0,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(current) => stamp = current,
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn discard_unpresented_output_stamps(
        &mut self,
        wid: WindowId,
        composed_at: Instant,
    ) {
        if let Some(plan) = self.active_visible_leaf_plan(wid) {
            self.discard_unpresented_output_stamps_from_plan(&plan, composed_at);
        }
    }

    fn present_latency_ns_with_plan(
        &mut self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> u64 {
        const PRESENT_LATENCY_CAP_NS: u64 = 5_000_000_000;
        let now = self.lat_epoch.elapsed().as_nanos() as u64;
        let revealed = self.windows.get(&wid).is_some_and(|ws| {
            ws.last_visible_views.len() != plan.leaves.len()
                || ws
                    .last_visible_views
                    .iter()
                    .zip(plan.leaves.iter())
                    .any(|(was, leaf)| *was != leaf.view)
        });
        if revealed {
            for leaf in &plan.leaves {
                if self
                    .windows
                    .get(&wid)
                    .is_some_and(|ws| ws.last_visible_views.contains(&leaf.view))
                {
                    // Distinct branch.
                    continue; // already on this glass: its stamp is honest
                }
                if let Some(sid) = self
                    .view_store
                    .get(leaf.view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                    && let Some(sess) = self.pool.get(sid)
                {
                    #[cfg(test)]
                    crate::work_counts::latency_stamp_touched();
                    sess.last_output_ns.store(0, Ordering::Relaxed);
                }
            }
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.last_visible_views.clear();
                ws.last_visible_views
                    .extend(plan.leaves.iter().map(|leaf| leaf.view));
            }
        }
        let mut dt_max = 0u64;
        for leaf in &plan.leaves {
            let Some(sid) = self
                .view_store
                .get(leaf.view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
            else {
                continue;
            };
            let Some(sess) = self.pool.get(sid) else {
                continue;
            };
            #[cfg(test)]
            crate::work_counts::latency_stamp_touched();
            let stamp = sess.last_output_ns.swap(0, Ordering::Relaxed);
            if stamp != 0 {
                let dt = now.saturating_sub(stamp);
                if dt < PRESENT_LATENCY_CAP_NS {
                    dt_max = dt_max.max(dt);
                }
            }
        }
        if dt_max != 0 && self.trace_latency {
            crate::logging::stderr_line!(
                "aterm-latency output->present: {:.2} ms",
                dt_max as f64 / 1e6
            );
        }
        dt_max
    }

    #[cfg(test)]
    pub(crate) fn prepare_terminal_capture_grid(
        &mut self,
        wid: WindowId,
    ) -> Option<TerminalCaptureGrid> {
        self.prepare_terminal_capture_grid_outcome_after(wid, None, || {})
            .map(|(grid, _)| grid)
            .into_option()
    }

    /// Headless composed-capture variant of [`Self::prepare_terminal_capture_grid`].
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn prepare_terminal_capture_grid_with_cursor_fx(
        &mut self,
        wid: WindowId,
        clock: ComposedCursorFxClock,
    ) -> Option<TerminalCaptureGrid> {
        self.prepare_terminal_capture_grid_with_cursor_fx_outcome(wid, clock)
            .into_option()
    }

    /// Exact-outcome arm for control callers that must distinguish a retryable synchronized-output hold from a route/capability disappearance.
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn prepare_terminal_capture_grid_with_cursor_fx_outcome(
        &mut self,
        wid: WindowId,
        clock: ComposedCursorFxClock,
    ) -> CapturePreparation<TerminalCaptureGrid> {
        self.prepare_terminal_capture_grid_with_cursor_fx_and_plan_outcome(wid, clock)
            .map(|(grid, _)| grid)
    }

    /// Capture result plus the exact canonical layout used to build it.
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn prepare_terminal_capture_grid_with_cursor_fx_and_plan_outcome(
        &mut self,
        wid: WindowId,
        clock: ComposedCursorFxClock,
    ) -> CapturePreparation<(TerminalCaptureGrid, crate::tab_model::VisibleLeafPlan)> {
        self.prepare_terminal_capture_grid_outcome_after(wid, Some(clock), || {})
    }

    /// Capture from a plan resolved by the same synchronous control turn.
    pub(crate) fn prepare_terminal_capture_grid_with_cursor_fx_for_plan_outcome(
        &mut self,
        wid: WindowId,
        plan: crate::tab_model::VisibleLeafPlan,
        clock: ComposedCursorFxClock,
    ) -> CapturePreparation<(TerminalCaptureGrid, crate::tab_model::VisibleLeafPlan)> {
        self.prepare_terminal_capture_grid_for_plan_outcome_after(wid, plan, Some(clock), || {})
    }

    #[cfg(test)]
    pub(crate) fn prepare_terminal_capture_grid_with_cursor_fx_interleaved(
        &mut self,
        wid: WindowId,
        clock: ComposedCursorFxClock,
        after_extract: impl FnOnce(),
    ) -> Option<TerminalCaptureGrid> {
        self.prepare_terminal_capture_grid_outcome_after(wid, Some(clock), after_extract)
            .map(|(grid, _)| grid)
            .into_option()
    }

    #[cfg(any(test, feature = "bench-support"))]
    fn prepare_terminal_capture_grid_outcome_after(
        &mut self,
        wid: WindowId,
        cursor_fx: Option<ComposedCursorFxClock>,
        after_extract: impl FnOnce(),
    ) -> CapturePreparation<(TerminalCaptureGrid, crate::tab_model::VisibleLeafPlan)> {
        let Some(plan) = self.active_visible_leaf_plan(wid) else {
            return CapturePreparation::Unavailable;
        };
        self.prepare_terminal_capture_grid_for_plan_outcome_after(
            wid,
            plan,
            cursor_fx,
            after_extract,
        )
    }

    fn prepare_terminal_capture_grid_for_plan_outcome_after(
        &mut self,
        wid: WindowId,
        plan: crate::tab_model::VisibleLeafPlan,
        cursor_fx: Option<ComposedCursorFxClock>,
        after_extract: impl FnOnce(),
    ) -> CapturePreparation<(TerminalCaptureGrid, crate::tab_model::VisibleLeafPlan)> {
        let mut held_retry_at = None;
        match self.prepare_terminal_capture_grid_sampled_for_plan_after(
            wid,
            plan,
            cursor_fx,
            after_extract,
            Some(&mut held_retry_at),
        ) {
            Some(prepared) => CapturePreparation::Ready(prepared),
            None => held_retry_at.map_or(CapturePreparation::Unavailable, |retry_at| {
                CapturePreparation::Held { retry_at }
            }),
        }
    }

    fn prepare_terminal_capture_grid_sampled_for_plan_after(
        &mut self,
        wid: WindowId,
        plan: crate::tab_model::VisibleLeafPlan,
        cursor_fx: Option<ComposedCursorFxClock>,
        after_extract: impl FnOnce(),
        mut held_out: Option<&mut Option<Instant>>,
    ) -> Option<(TerminalCaptureGrid, crate::tab_model::VisibleLeafPlan)> {
        let route = self.visible_content_route_from_plan(&plan)?;
        let crate::VisibleContentRoute::Terminal { composed } = route else {
            return None;
        };
        self.prepare_layout_coordinate_space_from_plan(wid, route, &plan);
        let (rows, cols, composed) = {
            let ws = self.windows.get(&wid)?;
            (usize::from(ws.rows), usize::from(ws.cols), composed)
        };

        let panes: Vec<_> = plan
            .leaves
            .iter()
            .map(|leaf| {
                let crate::tab_model::View::Terminal(terminal) =
                    self.view_store.get(leaf.view).copied()?
                else {
                    return None;
                };
                let term = self.pool.get(terminal.session)?.term.clone();
                let row = leaf.rect.origin.y.round().max(0.0) as usize;
                let col = leaf.rect.origin.x.round().max(0.0) as usize;
                let pane_rows = (leaf.rect.size.height.round() as usize).max(1);
                let pane_cols = (leaf.rect.size.width.round() as usize).max(1);
                Some((
                    leaf.view,
                    terminal.session,
                    leaf.focused,
                    row,
                    col,
                    pane_rows,
                    pane_cols,
                    term,
                ))
            })
            .collect::<Option<Vec<_>>>()?;
        if panes.is_empty() {
            return None;
        }

        let sync_preflight = if let Some(clock) = cursor_fx {
            let now = clock.observed_at();
            let timeout = Duration::from_millis(
                self.session_factory
                    .terminal_config
                    .as_ref()
                    .map_or(1000, |config| config.sync_timeout_ms),
            )
            .min(SYNC_HOLD_CAP);
            let mut observations = {
                let window = self.windows.get_mut(&wid)?;
                std::mem::take(&mut window.composed_sync_observation_scratch)
            };
            observations.clear();
            for (_, _, _, _, _, _, _, term) in &panes {
                let term = term_lock(term);
                observations.push(SyncObservation {
                    terminal_id: term.render_identity(),
                    active: term.modes().synchronized_output(),
                    end_seq: term.sync_end_seq(),
                    open_dirty: term.sync_open_dirty(),
                });
            }
            let (hold, retry_at) = {
                let window = self.windows.get_mut(&wid)?;
                let hold = update_composed_sync_frame_hold(window, &observations, now, timeout);
                window.composed_sync_observation_scratch = observations;
                (hold, window.sync_hold_until)
            };
            if hold {
                metrics::note_redraw_sync_hold();
                let retry_at = retry_at.expect("a true composed sync hold owns a retry deadline");
                if let Some(out) = held_out.as_deref_mut() {
                    *out = Some(retry_at);
                }
                return None;
            }
            Some((now, timeout))
        } else {
            None
        };

        let mut leaves = Vec::with_capacity(panes.len());

        if !composed {
            let (view, session, focused, _, _, pane_rows, pane_cols, term) = &panes[0];
            let capture_cursor_fx = cursor_fx.is_some();
            let advance_cursor_fx = matches!(cursor_fx, Some(ComposedCursorFxClock::Advance(_)));
            let mut term = term_lock(term);
            if let Some((now, timeout)) = sync_preflight.as_ref() {
                let preflight = self.windows.get(&wid)?.composed_sync_observation_scratch[0];
                let current = SyncObservation {
                    terminal_id: term.render_identity(),
                    active: term.modes().synchronized_output(),
                    end_seq: term.sync_end_seq(),
                    open_dirty: term.sync_open_dirty(),
                };
                let window = self.windows.get_mut(&wid)?;
                if recheck_sync_frame_hold(window, preflight, current, *now, *timeout) {
                    metrics::note_redraw_sync_hold();
                    if preflight.terminal_id == current.terminal_id {
                        let retry_at = window
                            .sync_hold_until
                            .expect("a true terminal sync hold owns a retry deadline");
                        if let Some(out) = held_out.as_deref_mut() {
                            *out = Some(retry_at);
                        }
                    }
                    return None;
                }
            }
            let (focus, cursor_fx_sample) = {
                let ws = self.windows.get_mut(&wid)?;
                term.cell_frame_into(&mut ws.input_scratch, rows, cols);
                ws.input_scratch.fx_occluders.clear();
                crate::claude_card::strip(&mut ws.claude_card, &mut ws.input_scratch);
                let content_seq = ws.input_scratch.content_seq;
                let dbg = if term.modes().reverse_video() {
                    term.default_foreground()
                } else {
                    term.default_background()
                };
                ws.input_scratch.default_bg = aterm_render::rgb_to_u32([dbg.r, dbg.g, dbg.b]);
                ws.input_scratch.cursor_color = terminal_cursor_color(&term);
                leaves.push(TerminalCaptureLeaf {
                    view: *view,
                    session: *session,
                    focused: *focused,
                    rows: *pane_rows,
                    cols: *pane_cols,
                    snapshot_seq: ws.input_scratch.snapshot_seq,
                    content_seq,
                });
                let display_offset = term.grid().display_offset();
                let live_viewport = display_offset == 0;
                let cursor = term.cursor();
                let focus = TerminalCaptureFocus {
                    view: *view,
                    session: *session,
                    terminal_id: term.render_identity(),
                    content_seq,
                    alternate_screen: term.is_alternate_screen(),
                    live_viewport,
                    display_offset,
                    cursor: live_viewport.then_some((cursor.row, cursor.col)),
                    cursor_visible: term.cursor_visible(),
                    frame_unfinished: term.sync_frame_unfinished(),
                    sync_end_seq: term.sync_end_seq(),
                    sync_end_caret: term.sync_end_caret(),
                    pet_world: aterm_effects::pet_world::PetWorldFacts::read(&term, *session),
                    damage_consumed: advance_cursor_fx,
                };
                let mut requested_witness_rows =
                    [0u16; aterm_effects::cursor_glow::CURSOR_WITNESS_ROWS];
                let witness_n = if advance_cursor_fx {
                    ws.fx
                        .cursor_mut()
                        .glow
                        .ribbon_rows(&mut requested_witness_rows)
                } else {
                    0
                };
                let (row_probe, row_above_probe, row_below_probe, witness_rows) =
                    if advance_cursor_fx {
                        (
                            std::mem::take(&mut ws.poof_row_buf),
                            std::mem::take(&mut ws.poof_row_above_buf),
                            std::mem::take(&mut ws.poof_row_below_buf),
                            std::mem::take(&mut ws.composed_witness_rows),
                        )
                    } else {
                        (
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                            ComposedWitnessRows::default(),
                        )
                    };
                let cursor_fx_sample = capture_cursor_fx.then(|| {
                    focused_composed_cursor_fx_sample(
                        *session,
                        &term,
                        advance_cursor_fx,
                        ws.fx.cursor_mut().glow.v2_owns_frame(),
                        ws.fx.cursor_mut().glow.reads_neighbor_rows(),
                        (0, 0),
                        &requested_witness_rows[..witness_n],
                        ws.fx.scroll_baseline(),
                        witness_rows,
                        row_probe,
                        row_above_probe,
                        row_below_probe,
                    )
                });
                if advance_cursor_fx {
                    // This headless capture is the presentation boundary: consume
                    // only the damage the snapshot represents; later writes stay
                    // pending.
                    term.take_damage();
                }
                (focus, cursor_fx_sample)
            };
            self.refresh_hover_for_frame_locked(wid, &term);
            drop(term);
            after_extract();
            match cursor_fx {
                Some(ComposedCursorFxClock::Advance(now)) => {
                    if !self.composed_cursor_frame_sampled(
                        wid,
                        ComposedCursorFxClock::Advance(now),
                        Some(cursor_fx_sample?),
                        &plan,
                    ) {
                        return None;
                    }
                }
                Some(clock @ ComposedCursorFxClock::Retain { .. }) => {
                    let _ = self.composed_cursor_frame_sampled(wid, clock, cursor_fx_sample, &plan);
                }
                None => {}
            }
            return Some((
                TerminalCaptureGrid {
                    composed: false,
                    focus: Some(focus),
                    leaves,
                },
                plan,
            ));
        }

        let grid = self.chrome_palette(ChromeSurface::Grid);
        let preedit_on_grid = matches!(
            self.preedit_owner(wid),
            crate::app_input::PreeditOwner::Grid
        );
        let window_focused = self.windows.get(&wid).is_some_and(|window| window.focused);
        let active_pane_mark = panes.iter().find(|pane| pane.2).and_then(
            |(_, _, _, row, col, pane_rows, pane_cols, _)| {
                ActivePaneMark::resolve(
                    self.config.split_focus_mark_or_default(),
                    window_focused,
                    (*row, *col, *pane_rows, *pane_cols),
                )
            },
        );
        let fluent_seam = self.fluent_seam(&plan);
        let ws = self.windows.get_mut(&wid)?;
        fill_divider_grid(&mut ws.input_scratch, rows, cols, grid, active_pane_mark);
        if let Some((inks, layout)) = &fluent_seam {
            crate::pane_chrome::stamp_seam_lines(
                &mut ws.input_scratch,
                rows,
                cols,
                *inks,
                layout,
                &[],
            );
        }
        let sole_pane = panes.len() == 1;
        let mut sole_default_bg = None;
        let mut focused_cursor_rgb = None;
        let capture_cursor_fx = cursor_fx.is_some();
        let advance_cursor_fx = matches!(cursor_fx, Some(ComposedCursorFxClock::Advance(_)));
        let mut focused_cursor_fx_sample: Option<FocusedComposedCursorFxSample> = None;
        let mut capture_focus = None;
        for (pane_index, (view, session, focused, row, col, pane_rows, pane_cols, term)) in
            panes.into_iter().enumerate()
        {
            let ws = self.windows.get_mut(&wid)?;
            let mut term = term_lock(&term);
            if let Some((now, timeout)) = sync_preflight.as_ref() {
                let preflight = ws.composed_sync_observation_scratch[pane_index];
                let current = SyncObservation {
                    terminal_id: term.render_identity(),
                    active: term.modes().synchronized_output(),
                    end_seq: term.sync_end_seq(),
                    open_dirty: term.sync_open_dirty(),
                };
                if recheck_sync_frame_hold(ws, preflight, current, *now, *timeout) {
                    metrics::note_redraw_sync_hold();
                    if preflight.terminal_id == current.terminal_id {
                        let retry_at = ws
                            .sync_hold_until
                            .expect("a true pane sync hold owns a retry deadline");
                        if let Some(out) = held_out.as_deref_mut() {
                            *out = Some(retry_at);
                        }
                    }
                    if advance_cursor_fx && let Some(sample) = focused_cursor_fx_sample.take() {
                        ws.poof_row_buf = sample.row_probe;
                        ws.poof_row_above_buf = sample.row_above_probe;
                        ws.poof_row_below_buf = sample.row_below_probe;
                        ws.composed_witness_rows = sample.witness_rows;
                    }
                    return None;
                }
            }
            let mut requested_witness_rows =
                [0u16; aterm_effects::cursor_glow::CURSOR_WITNESS_ROWS];
            let witness_n = if focused && advance_cursor_fx {
                ws.fx
                    .cursor_mut()
                    .glow
                    .ribbon_rows(&mut requested_witness_rows)
            } else {
                0
            };
            let (row_probe, row_above_probe, row_below_probe, witness_rows) =
                if focused && advance_cursor_fx {
                    (
                        std::mem::take(&mut ws.poof_row_buf),
                        std::mem::take(&mut ws.poof_row_above_buf),
                        std::mem::take(&mut ws.poof_row_below_buf),
                        std::mem::take(&mut ws.composed_witness_rows),
                    )
                } else {
                    (
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                        ComposedWitnessRows::default(),
                    )
                };
            term.cell_frame_into(&mut ws.pane_scratch, pane_rows, pane_cols);
            let content_seq = ws.pane_scratch.content_seq;
            if focused {
                ws.composed_focus_scratch.clone_from(&ws.pane_scratch);
            } else {
                ws.unfocused_pane_scratch
                    .entry(pane_index)
                    .or_default()
                    .clone_from(&ws.pane_scratch);
            }
            if focused {
                // Re-probe the hover under a still pointer against this pane's own
                // hold, as the composed route does for its focused pane.
                self.refresh_hover_for_frame_locked(wid, &term);
            }
            let ws = self.windows.get_mut(&wid)?;
            let blank = terminal_blank_cell(&term);
            if sole_pane {
                sole_default_bg = Some(aterm_render::rgb_to_u32(blank.bg));
            }
            let snapshot_seq = ws.pane_scratch.snapshot_seq;
            if focused {
                ws.input_scratch.base_y = ws.pane_scratch.base_y;
                ws.input_scratch.absolute_row_revision = ws.pane_scratch.absolute_row_revision;
                ws.input_scratch.history_renumber_epoch = ws.pane_scratch.history_renumber_epoch;
                focused_cursor_rgb = Some(terminal_cursor_rgb(&term));
                let display_offset = term.grid().display_offset();
                let live_viewport = display_offset == 0;
                capture_focus = Some(TerminalCaptureFocus {
                    view,
                    session,
                    terminal_id: term.render_identity(),
                    content_seq,
                    alternate_screen: term.is_alternate_screen(),
                    live_viewport,
                    display_offset,
                    cursor: live_viewport.then(|| {
                        let cursor = term.cursor();
                        (cursor.row, cursor.col)
                    }),
                    cursor_visible: term.cursor_visible(),
                    frame_unfinished: term.sync_frame_unfinished(),
                    sync_end_seq: term.sync_end_seq(),
                    sync_end_caret: term.sync_end_caret(),
                    pet_world: aterm_effects::pet_world::PetWorldFacts::read(&term, session),
                    damage_consumed: advance_cursor_fx,
                });
                if capture_cursor_fx {
                    focused_cursor_fx_sample = Some(focused_composed_cursor_fx_sample(
                        session,
                        &term,
                        advance_cursor_fx,
                        ws.fx.cursor_mut().glow.v2_owns_frame(),
                        ws.fx.cursor_mut().glow.reads_neighbor_rows(),
                        (row, col),
                        &requested_witness_rows[..witness_n],
                        ws.fx.scroll_baseline(),
                        witness_rows,
                        row_probe,
                        row_above_probe,
                        row_below_probe,
                    ));
                }
            }
            if preedit_on_grid
                && focused
                && !ws.preedit.is_empty()
                && ws.pane_scratch.display_offset == 0
            {
                let ambiguous_cjk = term.modes().ambiguous_width_double;
                ws.pane_scratch
                    .overlay_ime_preedit(&ws.preedit, ws.preedit_caret, ambiguous_cjk);
            }
            let cursor = (focused
                && ws.pane_scratch.cursor_visible
                && ws.pane_scratch.cursor_row < pane_rows
                && ws.pane_scratch.cursor_col < pane_cols)
                .then_some((
                    ws.pane_scratch.cursor_row,
                    ws.pane_scratch.cursor_col,
                    ws.pane_scratch.cursor_style,
                ));
            blit_pane_into(&mut ws.input_scratch, &ws.pane_scratch, row, col, blank);
            push_pane_selection(
                &mut ws.input_scratch,
                &ws.pane_scratch,
                row,
                col,
                pane_rows,
                pane_cols,
                focused,
            );
            if focused {
                match cursor {
                    Some((cursor_row, cursor_col, cursor_style)) => {
                        ws.input_scratch.cursor_row = row + cursor_row;
                        ws.input_scratch.cursor_col = col + cursor_col;
                        ws.input_scratch.cursor_style = cursor_style;
                        ws.input_scratch.cursor_visible = true;
                    }
                    None => ws.input_scratch.cursor_visible = false,
                }
            }
            leaves.push(TerminalCaptureLeaf {
                view,
                session,
                focused,
                rows: pane_rows,
                cols: pane_cols,
                snapshot_seq,
                content_seq,
            });
            if advance_cursor_fx {
                term.take_damage();
            }
        }
        let ws = self.windows.get_mut(&wid)?;
        ws.unfocused_pane_scratch
            .retain(|pane_index, _| leaves.get(*pane_index).is_some_and(|leaf| !leaf.focused));
        ws.input_scratch.default_bg = sole_default_bg.unwrap_or(aterm_core::render::COLOR_UNSET);
        ws.input_scratch.cursor_color =
            focused_cursor_rgb.map_or(aterm_core::render::COLOR_UNSET, aterm_render::rgb_to_u32);
        ws.input_scratch.snapshot_seq = ws.input_scratch.snapshot_seq.wrapping_add(1);
        let grid = TerminalCaptureGrid {
            composed: true,
            focus: capture_focus,
            leaves,
        };

        after_extract();
        match cursor_fx {
            Some(ComposedCursorFxClock::Advance(now)) => {
                let sample = focused_cursor_fx_sample?;
                if !self.composed_cursor_frame_sampled(
                    wid,
                    ComposedCursorFxClock::Advance(now),
                    Some(sample),
                    &plan,
                ) {
                    return None;
                }
            }
            Some(clock @ ComposedCursorFxClock::Retain { .. }) => {
                let sample = focused_cursor_fx_sample?;
                let _ = self.composed_cursor_frame_sampled(wid, clock, Some(sample), &plan);
            }
            None => {}
        }
        Some((grid, plan))
    }

    fn composed_output_streak_config(
        &self,
        now: Instant,
        win_focused: bool,
        animate: bool,
    ) -> ComposedOutputStreakConfig {
        let sound_gain = voice_gain(
            Voice::StreakPip,
            &VoiceTerms {
                master: self.config.trail_sounds_or_default()
                    && self
                        .serious_mode_policy()
                        .allows(crate::motion::SeriousEffect::TerminalSound),
                own: self.config.output_streak_sound_or_default(),
                volume: self.config.trail_sound_volume(),
                raw_focused: win_focused,
                reduced_motion: !animate,
            },
        );
        ComposedOutputStreakConfig {
            enabled: self.output_streak_shows(),
            intensity: self.config.output_streak_intensity_or_default(),
            tail: self.config.output_streak_tail_or_default(),
            max_streaks: self.config.output_streak_max_streaks_or_default(),
            idle_secs: self.config.output_streak_idle_secs_or_default(),
            sound_gain,
            style: self.glow_config().style,
            voice: self.config.trail_sound_voice(),
            animate,
            now,
        }
    }

    /// Prism wake on the composed path.
    pub(crate) fn compose_output_streak(&mut self, wid: WindowId, ctx: &ComposeDecoCtx<'_>) -> u64 {
        let resolved =
            self.composed_output_streak_config(ctx.now, ctx.win_focused, ctx.animate_streak);
        let trail_audio = &mut self.trail_audio;
        let pool = &self.pool;
        let Some(ws) = self.windows.get_mut(&wid) else {
            return 0;
        };
        ws.fx.retain_streak_panes(|session, pane_index| {
            ctx.panes
                .get(pane_index)
                .is_some_and(|(pane, _)| pane.session == session)
        });
        if resolved.inert() {
            ws.fx.clear_streak_panes();
            return 0;
        }

        let mut fp: u64 = 0;
        let (fx, focus_snapshot, background_snapshots) = (
            &mut ws.fx,
            &ws.composed_focus_scratch,
            &ws.unfocused_pane_scratch,
        );
        for (index, (r, _)) in ctx.panes.iter().enumerate() {
            let place = PanePlace {
                row_off: r.row_off,
                col_off: r.col_off,
                rows: r.rows,
                cols: r.cols,
                win_rows: ctx.win_rows,
                win_cols: ctx.win_cols,
                cell_w: ctx.cell_w,
                cell_h: ctx.cell_h,
            };
            let focused_pane = index == ctx.focus_index;
            let snapshot = if focused_pane {
                focus_snapshot
            } else {
                let Some(snapshot) = background_snapshots.get(&index) else {
                    continue;
                };
                snapshot
            };
            let pane = ComposedOutputStreakPane {
                session: r.session,
                slot: index,
                place,
                may_speak: ctx.present && focused_pane,
                echo: pool
                    .get(r.session)
                    .map(|session| session.ctx.output_echo.sample(&session.ctx.sink, ctx.now))
                    .unwrap_or_default(),
            };
            let (pane_fp, sound) = tick_composed_output_streak_pane(
                fx,
                aterm_effects::pipeline::NovaInto::Window,
                snapshot,
                snapshot.content_seq,
                pane,
                resolved,
            );
            if let Some(sound) = sound {
                trail_audio.push(sound);
            }
            fp ^= placed_output_streak_fp(pane_fp, pane, index);
        }
        let active = ws.fx.streak_panes().any(|engine| engine.is_active());
        if active {
            // Composed panes share the single-pane engine's phase-locked
            // decoration lane; refresh it while any pane has moving light (the
            // idle/Settle interval is handled by `output_streak_next_change_deadline`).
            ws.note_deco_animating(ctx.now);
        }
        fp
    }

    /// Prism wake for a mixed terminal/native frame.
    pub(crate) fn compose_heterogeneous_output_streak(
        &mut self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
        now: Instant,
        present: bool,
    ) -> u64 {
        let raw_focused = self.windows.get(&wid).is_some_and(|window| window.focused);
        let motion_focused = self.cursor_fx_focus(wid, raw_focused, now);
        let animate = self
            .motion_policy(motion_focused)
            .animate(crate::motion::MotionEffect::OutputStreak);
        let resolved = self.composed_output_streak_config(now, raw_focused, animate);
        let (cell_w, cell_h) = self.win_cell_size(wid);
        let view_store = &self.view_store;
        let trail_audio = &mut self.trail_audio;
        let pool = &self.pool;
        let Some(ws) = self.windows.get_mut(&wid) else {
            return 0;
        };
        ws.fx.retain_streak_panes(|session, pane_index| {
            plan.leaves.get(pane_index).is_some_and(|leaf| {
                matches!(
                    view_store.get(leaf.view),
                    Some(crate::tab_model::View::Terminal(terminal))
                        if terminal.session == session
                )
            })
        });
        ws.heterogeneous_output_streak_staged.clear();
        if resolved.inert() {
            ws.fx.clear_streak_panes();
            ws.input_scratch.nova_add.clear();
            return 0;
        }

        let (win_rows, win_cols) = (ws.rows, ws.cols);
        let (fx, nova, caches) = (
            &mut ws.fx,
            &mut ws.heterogeneous_output_streak_staged,
            &ws.leaf_render_cache,
        );
        let mut fp = 0u64;
        for (index, leaf) in plan.leaves.iter().enumerate() {
            let Some(crate::tab_model::View::Terminal(terminal)) =
                view_store.get(leaf.view).copied()
            else {
                continue;
            };
            let Some(snapshot) = caches.get(&leaf.view).map(|cache| &cache.input) else {
                fx.drop_streak_pane(terminal.session, index);
                continue;
            };
            let place = PanePlace {
                row_off: leaf.rect.origin.y.round().max(0.0) as u16,
                col_off: leaf.rect.origin.x.round().max(0.0) as u16,
                rows: (leaf.rect.size.height.round() as u16).max(1),
                cols: (leaf.rect.size.width.round() as u16).max(1),
                win_rows,
                win_cols,
                cell_w: cell_w as u32,
                cell_h: cell_h as u32,
            };
            let pane = ComposedOutputStreakPane {
                session: terminal.session,
                slot: index,
                place,
                may_speak: present && leaf.focused,
                echo: pool
                    .get(terminal.session)
                    .map(|session| session.ctx.output_echo.sample(&session.ctx.sink, now))
                    .unwrap_or_default(),
            };
            let (pane_fp, sound) = tick_composed_output_streak_pane(
                fx,
                aterm_effects::pipeline::NovaInto::Staged(nova),
                snapshot,
                snapshot.content_seq,
                pane,
                resolved,
            );
            if let Some(sound) = sound {
                trail_audio.push(sound);
            }
            fp ^= placed_output_streak_fp(pane_fp, pane, index);
        }
        ws.input_scratch.nova_add.clone_from(nova);
        let active = ws.fx.streak_panes().any(|engine| engine.is_active());
        if active {
            ws.note_deco_animating(now);
        }
        fp
    }

    /// Sparkle words, animated ink, peeking cats, novas and the cursor companion on a composed frame.
    pub(crate) fn compose_words(&mut self, wid: WindowId, ctx: &ComposeDecoCtx<'_>) -> u64 {
        let kitty_log_on = self.kitty_log_enabled();
        let bonk_master = self.config.trail_sounds_or_default();
        let bonk_enabled = self.curse_bonk_enabled();
        let bonk_detonations = self.curse_bonk_detonation_enabled();
        let bonk_volume = self.config.trail_sound_volume();
        let bonk_style = self.glow_config().style;
        let bonk_voice = self.config.trail_sound_voice();
        let load_shed = self.load_shed_active();
        let place_of = |r: &pane::PaneRect| PanePlace {
            row_off: r.row_off,
            col_off: r.col_off,
            rows: r.rows,
            cols: r.cols,
            win_rows: ctx.win_rows,
            win_cols: ctx.win_cols,
            cell_w: ctx.cell_w,
            cell_h: ctx.cell_h,
        };
        let companion_focus_place = ctx.panes.get(ctx.focus_index).map(|(r, _)| place_of(r));
        let routed = self.route_composed_companion(wid, ctx, companion_focus_place);
        let Some(rs) = self.sparkle.as_ref() else {
            {
                let Some(ws) = self.windows.get_mut(&wid) else {
                    return 0;
                };
                ws.fx.open_words();
                ws.fx
                    .retain_word_panes(|s| ctx.panes.iter().any(|(r, _)| r.session == s));
            }
            return self.compose_cursor_companion(wid, ctx, companion_focus_place, routed);
        };
        let (deco_cfg, lexicon) = (&rs.cfg, rs.lexicon.as_ref());
        let reduced_cfg = (!ctx.animate_sparkles && !deco_cfg.reduced_motion).then(|| {
            let mut c = deco_cfg.clone();
            c.reduced_motion = true;
            c
        });
        let tick_cfg = reduced_cfg.as_ref().unwrap_or(deco_cfg);
        let kitty_log = &mut self.kitty_log;
        let trail_audio = &mut self.trail_audio;
        let Some(ws) = self.windows.get_mut(&wid) else {
            return 0;
        };
        ws.fx.open_words();
        ws.fx
            .retain_word_panes(|s| ctx.panes.iter().any(|(r, _)| r.session == s));
        let companion_at =
            companion_focus_place.and_then(|_| routed.on_glass(ctx.escort_caret, ctx.pet.body_px));
        let resize_quiet = ws.resize_sound_quiet(ctx.now);
        let (fx, focus_snapshot, background_snapshots) = (
            &mut ws.fx,
            &ws.composed_focus_scratch,
            &ws.unfocused_pane_scratch,
        );
        let mut wince_hits = 0u8;
        let panes = ctx.panes.iter().enumerate().map(|(index, (r, _))| {
            let focused = index == ctx.focus_index;
            let snapshot = if focused {
                Some(focus_snapshot)
            } else {
                background_snapshots.get(&index)
            };
            aterm_effects::pipeline::WordPane {
                session: r.session,
                focused,
                place: place_of(r),
                grid: snapshot.map(|snapshot| aterm_effects::pipeline::PaneGrid {
                    cells: &snapshot.cells,
                    line_sizes: &snapshot.line_sizes,
                    base_y: snapshot.base_y,
                    default_bg: snapshot.default_bg,
                    epoch: snapshot.snapshot_seq,
                    alt: snapshot.engine_alt,
                    display_offset: snapshot.display_offset,
                    cursor: (
                        u16::try_from(snapshot.cursor_row).unwrap_or(u16::MAX),
                        u16::try_from(snapshot.cursor_col).unwrap_or(u16::MAX),
                    ),
                    sel: &snapshot.selection,
                }),
            }
        });
        let out = fx.word_panes(
            &aterm_effects::pipeline::WordPanes {
                now: ctx.now,
                lexicon,
                cfg: deco_cfg,
                tick_cfg,
                load_shed,
                word_focus: ctx.word_focus,
                window_focused: ws.focused,
                companion_at,
            },
            panes,
            |pane, decos| {
                kitty_log.observe_ambient(
                    pane.session,
                    decos.drain_kitty_sightings(),
                    lexicon,
                    ctx.now,
                    kitty_log_on,
                );
                let bonk_gain = bonk_sound_gain(
                    ws.focused,
                    bonk_master,
                    bonk_enabled && !resize_quiet,
                    tick_cfg.reduced_motion,
                    bonk_volume,
                );
                let curse_drain = drain_curse_bonk_cues(
                    decos,
                    bonk_style,
                    bonk_voice,
                    pane.cols,
                    bonk_gain,
                    bonk_detonations,
                    |event| trail_audio.push(event),
                );
                wince_hits = wince_hits.saturating_add(curse_drain.wince_hits);
            },
        );
        ws.fx
            .companions_mut()
            .cursor_cat
            .on_curse(ctx.now, wince_hits);
        if out.rescanned {
            ws.pending_deco_birth = None;
        }
        let mut fp = out.fp;
        fp ^= self.compose_cursor_companion(wid, ctx, companion_focus_place, routed);
        fp ^= self.compose_typed_dog(wid, ctx, out.focus_place);
        fp
    }

    fn compose_typed_dog(
        &mut self,
        wid: WindowId,
        ctx: &ComposeDecoCtx<'_>,
        focus_place: Option<PanePlace>,
    ) -> u64 {
        let reduced = match self.sparkle.as_ref() {
            Some(rs) if rs.cfg.canine => !ctx.animate_sparkles || rs.cfg.reduced_motion,
            _ => return 0,
        };
        let (Some(place), Some(cell)) = (focus_place, ctx.escort_caret) else {
            return 0;
        };
        let default_bg = self.theme.bg;
        let (accent, cursor_color) = (ctx.accent, ctx.cursor_color);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return 0;
        };
        if ws.fx.companions_mut().dog_cameo_session != Some(ctx.focus) {
            return 0;
        }
        let alpha = ws.fx.companions_mut().dog_cameo.alpha(ctx.now);
        if alpha == 0 {
            return 0;
        }
        let Some(look) = ws.fx.companions_mut().dog_cameo.look() else {
            return 0;
        };
        let geom = crate::word_decorations::EffectGeom {
            cell_w: ctx.cell_w as u16,
            cell_h: ctx.cell_h as u16,
            rows: place.rows,
            cols: place.cols,
        };
        let cw = i32::from(geom.cell_w);
        let ch = i32::from(geom.cell_h);
        let (crow, ccol) = cell;
        let probe = aterm_effects::word_decorations::CatFootprint {
            x: (i32::from(ccol) * cw - 4 * cw).max(0),
            y: i32::from(crow) * ch - ch / 2,
            w: (4 * cw).clamp(1, i32::from(u16::MAX)) as u16,
            h: (2 * ch).clamp(1, i32::from(u16::MAX)) as u16,
        };
        let colors = cursor_cat_color_key(
            &ws.composed_focus_scratch.cells,
            geom,
            probe,
            default_bg,
            cursor_color,
            accent,
        );
        let bob = if reduced {
            0.0
        } else {
            ws.fx.companions_mut().dog_cameo.bob(ctx.now)
        };
        ws.fx.clear_pane_sprites();
        let fp = ws.fx.draw_dog(
            aterm_effects::pipeline::CanvasInk::Pane,
            aterm_effects::word_decorations::DogCameoFrame {
                geom,
                cursor: cell,
                look,
                colors,
                bob,
                alpha,
            },
        );
        ws.fx.place_pane_sprites(place);
        if ws.os_window.is_some() {
            ws.note_deco_animating(ctx.now);
        }
        fp.map_or(0, |f| f.rotate_left(31))
    }

    fn compose_pet_companion(
        &mut self,
        wid: WindowId,
        ctx: &ComposeDecoCtx<'_>,
        focus_place: Option<PanePlace>,
    ) -> u64 {
        if !ctx.pet.on_glass {
            return 0;
        }
        let Some(place) = focus_place else {
            return 0;
        };
        let fallback = aterm_effects::companion::ContrastFallback {
            bg: self.theme.bg,
            cursor: ctx.cursor_color,
            accent: ctx.accent,
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return 0;
        };
        ws.fx.decos_mut().bind_pane(
            ctx.focus,
            (
                i32::from(place.col_off) * ctx.cell_w as i32,
                i32::from(place.row_off) * ctx.cell_h as i32,
            ),
        );
        ws.fx.clear_pane_sprites();
        let fp = emit_resident_pet(
            ws,
            &ctx.pet,
            ctx.cat_frame.look.normalized(),
            CompanionInk::Composed,
            ctx.present,
            ctx.now,
            fallback,
        );
        ws.fx.place_pane_sprites(place);
        fp
    }

    fn route_composed_companion(
        &mut self,
        wid: WindowId,
        ctx: &ComposeDecoCtx<'_>,
        focus_place: Option<PanePlace>,
    ) -> RoutedCompanion {
        let duty = cursor_companion_duty(ctx.pet.on_glass, ctx.kitty_alpha, ctx.escort_caret);
        let mut cat_frame = ctx.cat_frame;
        let router_geom = composed_companion_geom(ctx, focus_place);
        let mut placement = None;
        let seat = self.windows.get_mut(&wid).map_or_else(
            aterm_effects::pipeline::FlyingSeat::default,
            |ws| {
                if let Some(place) = focus_place {
                    ws.fx.decos_mut().bind_pane(
                        ctx.focus,
                        (
                            i32::from(place.col_off) * ctx.cell_w as i32,
                            i32::from(place.row_off) * ctx.cell_h as i32,
                        ),
                    );
                }
                let seat = route_v2_companion(
                    ws,
                    CompanionInk::Composed,
                    duty,
                    ctx.now,
                    router_geom,
                    &mut cat_frame,
                    ctx.animate_cat,
                );
                placement = prepare_routed_head(ws, router_geom, duty, cat_frame, seat, ctx.now);
                seat
            },
        );
        RoutedCompanion {
            duty,
            cat_frame,
            seat,
            placement,
        }
    }

    fn compose_cursor_companion(
        &mut self,
        wid: WindowId,
        ctx: &ComposeDecoCtx<'_>,
        focus_place: Option<PanePlace>,
        routed: RoutedCompanion,
    ) -> u64 {
        let RoutedCompanion {
            duty,
            cat_frame,
            seat,
            placement,
        } = routed;
        let seat_lift = seat.lift;
        let cell = match duty {
            CompanionDuty::Pet => return self.compose_pet_companion(wid, ctx, focus_place),
            CompanionDuty::Idle => return 0,
            CompanionDuty::FlyingHead { cell } => cell,
        };
        let Some(place) = focus_place else {
            return 0;
        };
        let reduced = !ctx.animate_cat;
        let default_bg = self.theme.bg;
        let accent = ctx.accent;
        let cursor_color = ctx.cursor_color;
        let Some(ws) = self.windows.get_mut(&wid) else {
            return 0;
        };
        ws.fx.decos_mut().bind_pane(
            ctx.focus,
            (
                i32::from(place.col_off) * ctx.cell_w as i32,
                i32::from(place.row_off) * ctx.cell_h as i32,
            ),
        );
        let geom = crate::word_decorations::EffectGeom {
            cell_w: ctx.cell_w as u16,
            cell_h: ctx.cell_h as u16,
            rows: place.rows,
            cols: place.cols,
        };
        let layout = aterm_effects::word_decorations::KittyCursorLayout {
            geom,
            cursor: cell,
            look: cat_frame.render_look(),
            bob: cat_frame.bob,
            lift: seat_lift,
        };
        let Some(placement) = placement else {
            return 0;
        };
        let colors = match ws.fx.companions_mut().cursor_cat.episode_colors() {
            Some(colors) => colors,
            None => {
                let sampled = cursor_cat_color_key(
                    &ws.composed_focus_scratch.cells,
                    geom,
                    placement.sample,
                    default_bg,
                    cursor_color,
                    accent,
                );
                ws.fx
                    .companions_mut()
                    .cursor_cat
                    .colors_for_episode(sampled)
            }
        };
        let c = ws.fx.companions_mut();
        c.music_notes.update(
            ctx.now,
            c.kitty_sing.is_armed(ctx.now),
            c.kitty_sing.beat(ctx.now),
        );
        let notes = c.music_notes.frame_array(ctx.now, reduced);
        ws.fx.clear_pane_sprites();
        let fp = ws.fx.draw_flying_head(
            aterm_effects::pipeline::CanvasInk::Pane,
            aterm_effects::word_decorations::KittyCursorFrame {
                geom,
                cursor: cell,
                look: layout.look,
                colors,
                bob: cat_frame.bob,
                lift: seat_lift,
                alpha: ctx.kitty_alpha,
                pose: cat_frame.pose,
                sing: cat_frame.sing,
                notes,
            },
            placement,
        );
        ws.fx.place_pane_sprites(place);
        fp.map_or(0, |f| f.rotate_left(29))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "a window's full compose inputs (id/dims/invert/drag-hover/cursor-override/tab-strip); bundling them into a struct only relocates the argument list"
    )]
    #[cfg(test)]
    pub(crate) fn redraw_compose(
        &mut self,
        wid: WindowId,
        rows: usize,
        cols: usize,
        invert: bool,
        drag_hover: bool,
        cursor_override: Option<CursorStyle>,
        tab_strip: u64,
        now: Instant,
    ) -> Option<Arc<str>> {
        let plan = self.active_visible_leaf_plan(wid)?;
        self.redraw_compose_from_plan(
            wid,
            rows,
            cols,
            invert,
            drag_hover,
            cursor_override,
            tab_strip,
            now,
            &plan,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the frame-owned plan joins the existing composed-redraw inputs"
    )]
    fn redraw_compose_from_plan(
        &mut self,
        wid: WindowId,
        rows: usize,
        cols: usize,
        invert: bool,
        drag_hover: bool,
        cursor_override: Option<CursorStyle>,
        tab_strip: u64,
        now: Instant,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> Option<Arc<str>> {
        self.redraw_compose_after_focused_extract(
            wid,
            rows,
            cols,
            invert,
            drag_hover,
            cursor_override,
            tab_strip,
            now,
            plan,
            || {},
            || {},
        )
    }

    #[cfg(test)]
    #[allow(
        clippy::too_many_arguments,
        reason = "test seam mirrors the shipping composed-redraw inputs and adds only a deterministic interleave hook"
    )]
    fn redraw_compose_interleaved(
        &mut self,
        wid: WindowId,
        rows: usize,
        cols: usize,
        invert: bool,
        drag_hover: bool,
        cursor_override: Option<CursorStyle>,
        tab_strip: u64,
        now: Instant,
        after_focused_extract: impl FnOnce(),
    ) -> Option<Arc<str>> {
        let plan = self.active_visible_leaf_plan(wid)?;
        self.redraw_compose_after_focused_extract(
            wid,
            rows,
            cols,
            invert,
            drag_hover,
            cursor_override,
            tab_strip,
            now,
            &plan,
            || {},
            after_focused_extract,
        )
    }

    #[cfg(test)]
    #[allow(
        clippy::too_many_arguments,
        reason = "test seam mirrors the shipping composed-redraw inputs and adds only a deterministic interleave hook"
    )]
    fn redraw_compose_before_focused_extract(
        &mut self,
        wid: WindowId,
        rows: usize,
        cols: usize,
        invert: bool,
        drag_hover: bool,
        cursor_override: Option<CursorStyle>,
        tab_strip: u64,
        now: Instant,
        before_focused_extract: impl FnOnce(),
    ) -> Option<Arc<str>> {
        let plan = self.active_visible_leaf_plan(wid)?;
        self.redraw_compose_after_focused_extract(
            wid,
            rows,
            cols,
            invert,
            drag_hover,
            cursor_override,
            tab_strip,
            now,
            &plan,
            before_focused_extract,
            || {},
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "internal exact-sample compositor mirrors the public redraw inputs and carries one deterministic test hook"
    )]
    fn redraw_compose_after_focused_extract(
        &mut self,
        wid: WindowId,
        rows: usize,
        cols: usize,
        invert: bool,
        drag_hover: bool,
        cursor_override: Option<CursorStyle>,
        tab_strip: u64,
        now: Instant,
        plan: &crate::tab_model::VisibleLeafPlan,
        before_focused_extract: impl FnOnce(),
        after_focused_extract: impl FnOnce(),
    ) -> Option<Arc<str>> {
        let theme = self.theme;
        let grid = self.chrome_palette(ChromeSurface::Grid);
        let raw_focused = self.windows.get(&wid)?.focused;
        let focused = self.cursor_fx_focus(wid, raw_focused, now);
        let mode = self.config.motion_mode();
        let policy = crate::motion::MotionPolicy::resolve(mode, self.system_reduce_motion, focused);
        let (glow_cw, glow_ch) = self.win_cell_size(wid);
        let grid_top = self.win_pad_top(wid) + self.win_head(wid);
        let pmode = self.predict_mode();
        let pediting = self.config.predictive_echo_editing_or_default();
        let trail_presentation = self.trail_presentation();
        let pet_glow = self.glow_config_for(trail_presentation);
        let pet_console_owned = resident_pet_owner_present(
            trail_presentation.pet_species.is_some(),
            pet_glow.enabled
                && self
                    .serious_mode_policy()
                    .allows(crate::motion::SeriousEffect::CursorCat),
            pet_glow.style,
        );
        let sync_hold_timeout = Duration::from_millis(
            self.session_factory
                .terminal_config
                .as_ref()
                .map_or(1000, |config| config.sync_timeout_ms),
        )
        .min(SYNC_HOLD_CAP);
        let focus_index = plan.leaves.iter().position(|leaf| leaf.focused)?;
        let focus = self
            .view_store
            .get(plan.leaves.get(focus_index)?.view)
            .copied()?
            .terminal_session()?;
        let ws = self.windows.get(&wid)?;
        let tree = &ws.layouts[ws.tabs.active];
        let blink_phase = ws.blink_phase;
        let last_present = ws.last_present;
        let rects = tree.compute_layout(ws.rows, ws.cols);
        if rects.len() != plan.leaves.len()
            || rects.iter().zip(&plan.leaves).any(|(rect, leaf)| {
                self.view_store
                    .get(leaf.view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                    != Some(rect.session)
            })
        {
            return None;
        }
        let mut damage_epoch: u64 = 0;
        let mut focus_selection =
            SelectionFingerprint::of(&aterm_core::selection::TextSelection::new());
        let mut pane_selection_fp = 0u64;
        let mut focus_cur_pos = (0u16, 0u16);
        let mut focus_vis = false;
        let mut focus_frame_unfinished = false;
        let mut focus_sync_end_seq = 0u64;
        let mut focus_sync_end_caret = None;
        let mut focus_style = CursorStyle::default();
        let mut focus_off = (0u16, 0u16);
        let mut focus_dims = (0u16, 0u16);
        let mut focus_scrolled = false;
        let mut focus_no_echo = false;
        let mut focus_coords = 0u64;
        let mut focus_cursor_rgb: Option<[u8; 3]> = None;
        let mut focus_recoloured = false;
        let mut focus_default_bg = theme.bg;
        let mut focus_blank = divider_cell(grid);
        let mut focus_title_sample: Arc<str> = Arc::default();
        let mut focus_snapshot_valid = false;
        let mut focus_world_facts = None;
        let mut focus_ambiguous_cjk = false;
        let mut focus_probe: Option<(u16, u16, aterm_effects::cursor_glow::ProbeTrust)> = None;
        let mut focus_probe_above = false;
        let mut focus_probe_below = false;
        let mut focus_print_anchor: Option<(u16, u16, u64)> = None;
        let mut focus_print_anchor_glyph: Option<char> = None;
        let mut focus_epoch = 0u64;
        let mut focus_content_seq = 0u64;
        let mut focus_wrap_serial = 0u64;
        let mut focus_cmd_done: Option<(u64, i32)> = None;
        let mut focus_cmd_dur_ms: Option<u64> = None;
        let mut focus_shell_edge = false;
        let mut focus_shell_exec = false;
        let mut focus_scrolled_rows = 0usize;
        let mut sibling_talking: Option<(u16, u16)> = None;
        let mut focus_alt = false;
        let mut focus_d_off = 0usize;
        let mut focus_sel_clone = aterm_core::selection::TextSelection::new();
        let mut pane_default_bg_u32 = 0u32;
        let mut rain_refresh = false;
        let mut rain_suspend = true;
        let panes: Vec<(pane::PaneRect, Arc<Mutex<Terminal>>)> = rects
            .iter()
            .map(|r| self.pool.get(r.session).map(|s| (*r, s.term.clone())))
            .collect::<Option<_>>()?;
        {
            let ws = self.windows.get_mut(&wid)?;
            ws.composed_pane_stage_meta.clear();
            ws.composed_pane_stage_meta
                .resize_with(panes.len(), || None);
        }
        let mut sync_observations = {
            let ws = self.windows.get_mut(&wid)?;
            std::mem::take(&mut ws.composed_sync_observation_scratch)
        };
        sync_observations.clear();
        for (r, term) in &panes {
            let observation = {
                let site = crate::metrics::TermWaitSite::Redraw;
                let term = match self.pool.get(r.session) {
                    Some(s) => crate::term_lock_ui(term, &s.ctx.ui_waiting, site),
                    None => term_lock(term),
                };
                SyncObservation {
                    terminal_id: term.render_identity(),
                    active: term.modes().synchronized_output(),
                    end_seq: term.sync_end_seq(),
                    open_dirty: term.sync_open_dirty(),
                }
            };
            sync_observations.push(observation);
        }
        let sync_hold = {
            let ws = self.windows.get_mut(&wid)?;
            let hold =
                update_composed_sync_frame_hold(ws, &sync_observations, now, sync_hold_timeout);
            ws.composed_sync_observation_scratch = sync_observations;
            hold
        };
        if sync_hold {
            metrics::note_redraw_sync_hold();
            return None;
        }
        let rain_session_on = self.session_rain_enabled(focus);
        let load_shed = self.load_shed_active();
        let rain_cfg = self.rain.filter(|_| {
            rain_session_on
                || self
                    .windows
                    .get(&wid)
                    .is_some_and(|w| w.fx.rain().is_some())
        });
        let overlay_inputs = self.pane_overlay_inputs(wid, panes.len() > 1);
        if let Some(ws) = self.windows.get_mut(&wid) {
            if overlay_inputs.is_some() {
                ws.pane_overlay.begin();
            } else {
                ws.pane_overlay.clear();
            }
        }
        let pane_indices = (0..panes.len())
            .filter(|index| *index != focus_index)
            .chain(std::iter::once(focus_index));
        let mut before_focused_extract = Some(before_focused_extract);
        for pane_index in pane_indices {
            if pane_index == focus_index
                && let Some(hook) = before_focused_extract.take()
            {
                hook();
            }
            let (r, term) = &panes[pane_index];
            let site = crate::metrics::TermWaitSite::Redraw;
            let mut term = match self.pool.get(r.session) {
                Some(s) => crate::term_lock_ui(term, &s.ctx.ui_waiting, site),
                None => term_lock(term),
            };
            let preflight = self
                .windows
                .get(&wid)?
                .composed_sync_observation_scratch
                .get(pane_index)
                .copied()?;
            let sync_hold = {
                let ws = self.windows.get_mut(&wid)?;
                recheck_sync_frame_hold(
                    ws,
                    preflight,
                    SyncObservation {
                        terminal_id: term.render_identity(),
                        active: term.modes().synchronized_output(),
                        end_seq: term.sync_end_seq(),
                        open_dirty: term.sync_open_dirty(),
                    },
                    now,
                    sync_hold_timeout,
                )
            };
            if sync_hold {
                metrics::note_redraw_sync_hold();
                return None;
            }
            damage_epoch = damage_epoch.wrapping_add(term.damage_epoch());
            pane_selection_fp =
                crate::fold_pane_selection_fp(pane_selection_fp, pane_index, term.text_selection());
            if overlay_inputs.is_some()
                && let Some(ws) = self.windows.get_mut(&wid)
            {
                ws.pane_overlay.note(
                    crate::pane_chrome::PaneFacts::read(
                        r.session,
                        crate::pane_chrome::CellBox {
                            row: usize::from(r.row_off),
                            col: usize::from(r.col_off),
                            rows: usize::from(r.rows),
                            cols: usize::from(r.cols),
                        },
                        pane_index == focus_index,
                        &term,
                    ),
                    now,
                );
            }
            if pane_index == focus_index {
                focus_selection = SelectionFingerprint::of(term.text_selection());
                let cp = term.cursor();
                focus_cur_pos = (cp.row, cp.col);
                focus_wrap_serial = term.wrap_serial();
                focus_vis = term.cursor_visible();
                focus_frame_unfinished = term.sync_frame_unfinished();
                focus_sync_end_seq = term.sync_end_seq();
                focus_sync_end_caret = term.sync_end_caret();
                focus_style = term.cursor_style();
                focus_off = (r.row_off, r.col_off);
                focus_dims = (r.rows, r.cols);
                focus_cursor_rgb = Some(terminal_cursor_rgb(&term));
                focus_recoloured = term.cursor_color_recoloured();
                focus_blank = terminal_blank_cell(&term);
                focus_default_bg = aterm_render::rgb_to_u32(focus_blank.bg);
                focus_title_sample = term.title_arc();
                focus_ambiguous_cjk = term.modes().ambiguous_width_double;
                if let Some(ws) = self.windows.get_mut(&wid) {
                    let refill = term.cell_frame_damage_scoped_into(
                        &mut ws.composed_focus_scratch,
                        usize::from(r.rows),
                        usize::from(r.cols),
                    );
                    metrics::note_frame_refill(refill);
                    focus_snapshot_valid = true;
                    focus_world_facts = pet_console_owned
                        .then(|| aterm_effects::pet_world::PetWorldFacts::read(&term, focus));
                    let d_off = term.grid().display_offset();
                    focus_scrolled = d_off != 0;
                    focus_no_echo =
                        term.is_alternate_screen() || term.kitty_suppresses_predictive_echo();
                    let content_scroll_state = term.content_scroll_state();
                    focus_coords = content_scroll_state.coordinate_generation();
                    let pane_alt = term.is_alternate_screen();
                    sync_cursor_effect_coordinate_space(ws, term.render_identity(), pane_alt);
                    let scroll_change = sync_cursor_effect_scroll(ws, content_scroll_state);
                    let blink_epoch = term.repaint_blink_epoch();
                    if ws.blink_reseed {
                        ws.blink_reseed = false;
                        ws.blink_epoch_seen = blink_epoch;
                    } else if blink_epoch != ws.blink_epoch_seen {
                        ws.blink_epoch_seen = blink_epoch;
                        ws.last_blink_at = Some(now);
                        ws.fx.cursor_mut().glow.note_repaint_blink(now);
                        ws.fx.cursor_mut().trail.note_repaint_blink(now);
                    }
                    ws.fx.cursor_mut().glow.note_context(pane_alt);
                    ws.fx.cursor_mut().trail.note_context(pane_alt);
                    ws.fx
                        .cursor_mut()
                        .glow
                        .note_pane_columns(r.col_off, usize::from(r.cols));
                    ws.fx
                        .cursor_mut()
                        .trail
                        .note_pane_columns(r.col_off, usize::from(r.cols));
                    let blink_recent = ws
                        .last_blink_at
                        .is_some_and(|t| now.saturating_duration_since(t) <= BLINK_RECENT_MAX);
                    let pane_probe_trust = row_probe_trust(pane_alt, blink_recent);
                    focus_probe = if d_off == 0 && !scroll_change.changed() {
                        let _fill = term.row_cols_into(cp.row as usize, &mut ws.poof_row_buf);
                        (focus_probe_above, focus_probe_below) = capture_cursor_neighbor_rows(
                            &term,
                            usize::from(cp.row),
                            usize::from(r.rows),
                            ws.fx.cursor_mut().glow.reads_neighbor_rows(),
                            &mut ws.poof_row_above_buf,
                            &mut ws.poof_row_below_buf,
                        );
                        Some((cp.row + r.row_off, cp.col + r.col_off, pane_probe_trust))
                    } else {
                        None
                    };
                    let (anchor, anchor_glyph) = aterm_effects::host::motion_print_anchor(&term);
                    focus_print_anchor =
                        (d_off == 0)
                            .then_some(anchor)
                            .flatten()
                            .map(|(ar, ac, seq)| {
                                (
                                    ar.saturating_add(r.row_off),
                                    ac.saturating_add(r.col_off),
                                    seq,
                                )
                            });
                    focus_print_anchor_glyph = (d_off == 0).then_some(anchor_glyph).flatten();
                    focus_scrolled_rows = usize::from(scroll_change.translated_rows)
                        + usize::from(scroll_change.invalidated)
                        + usize::from(scroll_change.band_moves);
                    focus_epoch = term.damage_epoch();
                    focus_content_seq = term.content_seq();
                    focus_cmd_done = term
                        .last_completed_command()
                        .and_then(|m| Some((term.completed_command_seq(), m.exit_code?)));
                    focus_cmd_dur_ms = term
                        .last_completed_command()
                        .and_then(|m| m.exec_duration_ms());
                    let shell_exec =
                        term.shell_state() == aterm_core::terminal::ShellState::Executing;
                    focus_shell_edge = ws.fx.note_rain_shell(r.session, shell_exec);
                    focus_shell_exec = shell_exec;
                    focus_alt = pane_alt;
                    focus_d_off = d_off;
                    focus_sel_clone = term.text_selection().clone();
                    let dbg = if term.modes().reverse_video() {
                        term.default_foreground()
                    } else {
                        term.default_background()
                    };
                    pane_default_bg_u32 = aterm_render::rgb_to_u32([dbg.r, dbg.g, dbg.b]);
                    let rain_suppress_alt =
                        pane_alt && rain_cfg.is_some_and(|c| c.suppress_in_alt_screen);
                    rain_suspend = rain_suppress_alt || load_shed || !rain_session_on;
                    if rain_cfg.is_some() && !rain_suspend {
                        update_rain_hidden_band(
                            &mut ws.rain_hidden_band,
                            term.grid().damage(),
                            usize::from(r.rows),
                        );
                    }
                    rain_refresh = rain_refresh_needed(
                        rain_cfg.is_some(),
                        rain_suspend,
                        false,
                        d_off,
                        ws.fx.rain(),
                        focus_epoch,
                    );
                }
                self.refresh_hover_for_frame_locked(wid, &term);
            } else if let Some(ws) = self.windows.get_mut(&wid) {
                let staged = ws.unfocused_pane_scratch.entry(pane_index).or_default();
                let staged_seq = staged.content_seq;
                let refill = term.cell_frame_damage_scoped_into(
                    staged,
                    usize::from(r.rows),
                    usize::from(r.cols),
                );
                metrics::note_frame_refill(refill);
                if staged_seq != 0
                    && aterm_effects::companion::pet_output_burst(
                        false,
                        staged.content_seq > staged_seq,
                        term.shell_state() == aterm_core::terminal::ShellState::Executing,
                        term.grid().display_offset() == 0,
                        term.is_alternate_screen(),
                    )
                {
                    let cp = term.cursor();
                    sibling_talking = Some((
                        cp.row.saturating_add(r.row_off),
                        cp.col.saturating_add(r.col_off),
                    ));
                }
                ws.composed_pane_stage_meta[pane_index] =
                    Some((term.title_arc(), terminal_blank_cell(&term)));
            }
        }
        after_focused_extract();
        if !focus_snapshot_valid {
            return None;
        }
        let rain_fp = match self.windows.get_mut(&wid) {
            Some(ws) => {
                let pane_cursor = (focus_vis && !focus_scrolled).then_some(focus_cur_pos);
                let fp = ws.fx.rain_frame(
                    rain_cfg.map(|cfg| move || crate::rain_config_for_window(cfg, wid)),
                    &crate::matrix_rain::RainFrame {
                        clock: crate::matrix_rain::RainClock::At(now),
                        suspended: rain_suspend,
                        animate: policy.animate(crate::motion::MotionEffect::MatrixRain),
                        visibility: if raw_focused {
                            crate::matrix_rain::RainVisibility::Focused
                        } else {
                            crate::matrix_rain::RainVisibility::VisibleUnfocused
                        },
                        session: focus,
                        content_seq: focus_content_seq,
                        execute_edge: focus_shell_edge,
                        cmd_done: focus_cmd_done,
                        scan: rain_refresh.then_some(crate::matrix_rain::RainScan {
                            cells: &ws.composed_focus_scratch.cells,
                            line_sizes: &ws.composed_focus_scratch.line_sizes,
                            images: &ws.composed_focus_scratch.images,
                            rows: usize::from(focus_dims.0),
                            cols: usize::from(focus_dims.1),
                            default_bg: pane_default_bg_u32,
                            epoch: focus_epoch,
                        }),
                        cursor: pane_cursor,
                        geom: crate::word_decorations::EffectGeom {
                            cell_w: glow_cw as u16,
                            cell_h: glow_ch as u16,
                            rows: focus_dims.0,
                            cols: focus_dims.1,
                        },
                        tick: crate::matrix_rain::RainTickInput {
                            cursor: pane_cursor,
                            hidden_band: &ws.rain_hidden_band,
                            sel: Some(crate::word_decorations::SelView {
                                sel: &focus_sel_clone,
                                display_offset: focus_d_off as i32,
                            }),
                            display_offset: focus_d_off as i32,
                            is_alt_screen: focus_alt,
                        },
                    },
                );
                if rain_cfg.is_some() {
                    ws.fx.place_rain(PanePlace {
                        row_off: focus_off.0,
                        col_off: focus_off.1,
                        rows: focus_dims.0,
                        cols: focus_dims.1,
                        win_rows: ws.rows,
                        win_cols: ws.cols,
                        cell_w: glow_cw as u32,
                        cell_h: glow_ch as u32,
                    });
                }
                if fp == 0 {
                    0
                } else {
                    (fp.rotate_left(17) ^ (u64::from(focus_off.0) << 16 | u64::from(focus_off.1)))
                        | 1
                }
            }
            None if rain_cfg.is_some() => return None,
            None => 0,
        };
        let pet_mode = trail_presentation.pet_species.is_some();
        let cursor_companions_allowed = self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::CursorCat);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.pill_shown = false;
            if focus_probe.is_some() && focus_off.1 > 0 {
                let off = usize::from(focus_off.1);
                for probe in [
                    &mut ws.poof_row_buf,
                    &mut ws.poof_row_above_buf,
                    &mut ws.poof_row_below_buf,
                ] {
                    let len = probe.len();
                    probe.resize(len.saturating_add(off), ' ');
                    probe.rotate_right(off);
                }
            }
        }
        let win_cur = (focus_vis && !focus_scrolled)
            .then_some((focus_cur_pos.0 + focus_off.0, focus_cur_pos.1 + focus_off.1));
        let live_cursor_rgb = focus_cursor_rgb?;
        let fx = self.cursor_frame(
            wid,
            CursorFxInputs {
                now,
                rows,
                cols,
                cur: win_cur,
                live_viewport: !focus_scrolled,
                cursor_visible: focus_vis,
                hidden_withheld: false,
                cursor_style: focus_style,
                blink_phase,
                live_cursor_rgb,
                cursor_color_pinned: self.config.cursor_color.is_some() || focus_recoloured,
                default_bg: focus_default_bg,
                default_fg: aterm_render::rgb_to_u32(focus_blank.fg),
                row_probe: focus_probe,
                row_probe_neighbors: Some((focus_probe_above, focus_probe_below)),
                print_anchor: focus_print_anchor,
                print_anchor_glyph: focus_print_anchor_glyph,
            },
        )?;
        let glow_fp = fx.glow_fp;
        let trail_fp = fx.trail_fp;
        let block_fill = fx.block_fill;
        let active_cursor_fill = block_fill.map(|owned| owned.fill);
        let body_style_override =
            compose_caret_style_override(fx.bolt_cursor, fx.twinkle_cursor, fx.momentum_steady);
        let focused = fx.win_focused;
        let policy = fx.motion;
        let cursor_motion = fx.cursor_motion;
        let shed_envelope = fx.shed_envelope;
        let glow_cfg = fx.glow_cfg;
        let trail_color = fx.trail_color;
        let animate_cat =
            cursor_motion.animate(crate::motion::MotionEffect::CursorGlow) && shed_envelope > 0.0;
        let animate_sparkles = policy.animate(crate::motion::MotionEffect::WordSparkles);
        let decoration_presentable = shed_companion_presentable(focused, shed_envelope);
        let cursor_companion_presentable = cursor_companion_presentable(
            cursor_companions_allowed && decoration_presentable,
            !focus_scrolled,
        );
        let companion_verdict = self.companion_verdict(wid, focus, now);
        let sing_style = cursor_companions_allowed
            && matches!(glow_cfg.style, crate::cursor_glow::GlowStyle::RainbowKitty);
        let sound_on = self.config.trail_sounds_or_default();
        let sound_volume = self.config.trail_sound_volume();
        let riff_key = self.config.trail_sound_riff_or_default();
        let pet_species = trail_presentation
            .pet_species
            .unwrap_or(aterm_effects::kitty_pet::PetSpecies::Cat);
        if pet_mode
            && let Some(facts) = focus_world_facts.as_ref()
            && let Some(ws) = self.windows.get_mut(&wid)
        {
            let exclusions = pet_console_exclusions(
                &ws.composed_focus_scratch,
                !ws.preedit.is_empty(),
                ws.predictor.pending_bounds(),
                ws.search.is_some(),
                ws.pet_chrome.held(),
                ws.pet_chrome.held_overlays(),
            );
            let observed = ws.fx.companion_mut().observe_console_reserved(
                &ws.composed_focus_scratch,
                facts,
                aterm_effects::pet_world::PetPane::full(&ws.composed_focus_scratch),
                exclusions.reserve(),
            );
            metrics::note_pet_world_observation(observed);
        }
        let focus_pane_dims = panes.get(focus_index).map(|(r, _)| (r.rows, r.cols));
        let pane_geom = fx.glow_geom;
        let (fx_ox, fx_oy) = (pane_geom.origin_x, pane_geom.origin_y);
        let pet_origin = (
            i32::from(fx_ox) + i32::from(focus_off.1) * glow_cw as i32,
            i32::from(fx_oy) + i32::from(focus_off.0) * glow_ch as i32,
        );
        let (cat_frame, kitty_alpha, riff, outro, pet, escort) = {
            let ws = self.windows.get_mut(&wid)?;
            ws.fx
                .companions_mut()
                .cursor_cat
                .set_look(companion_verdict);
            ws.fx
                .companions_mut()
                .cursor_cat
                .set_collection_presentable(now, cursor_companion_presentable);
            if shed_envelope_transitioning(load_shed, shed_envelope) && ws.os_window.is_some() {
                ws.note_deco_animating(now);
            }
            if !cursor_companions_allowed {
                ws.fx.companions_mut().kitty_sing = aterm_effects::kitty_sing::KittySing::default();
                ws.fx.companions_mut().music_notes.clear();
                ws.fx.companions_mut().sing_riff_bar = None;
            }
            let sing_drive = if sing_style {
                ws.fx.companions_mut().kitty_sing.drive(now)
            } else {
                0.0
            };
            let mut riff: Option<(u64, f32, u32)> = None;
            let mut outro: Option<(f32, u32)> = None;
            if sing_drive > 0.0 {
                ws.fx.cursor_mut().glow.celebrate(now, sing_drive);
                if let Some(bar) = ws.fx.companions_mut().kitty_sing.bar(now)
                    && ws.fx.companions_mut().sing_riff_bar != Some(bar)
                {
                    ws.fx.companions_mut().sing_riff_bar = Some(bar);
                    riff = sing_riff_gain(ws.focused, sound_on, riff_key, sound_volume)
                        .map(|gain| (bar, gain, ws.fx.companions_mut().kitty_sing.signature()));
                }
                if let Some(fan) = ws.fx.companions_mut().kitty_sing.take_bar_fan(now) {
                    ws.fx.cursor_mut().glow.party(now, fan.n, fan.ring);
                }
                if let Some(o) = ws.fx.companions_mut().kitty_sing.take_outro(now) {
                    ws.fx.cursor_mut().glow.party(now, o.drop, false);
                    outro = sing_riff_gain(ws.focused, sound_on, riff_key, sound_volume)
                        .map(|gain| (gain, o.sig));
                }
            } else {
                ws.fx.companions_mut().kitty_sing.settle(now);
                ws.fx.companions_mut().sing_riff_bar = None;
            }
            let c = ws.fx.companions_mut();
            c.cursor_cat.set_singing(
                now,
                crate::kitty_cursor::SingSync {
                    drive: sing_drive,
                    beat: c.kitty_sing.beat(now).unwrap_or(0.0),
                },
            );
            retire_kitty_cursor_without_owner(
                glow_cfg.enabled && cursor_companions_allowed,
                pet_mode,
                glow_cfg.style,
                sing_drive,
                &mut ws.fx.companions_mut().cursor_cat,
            );
            let mut cat_frame = if animate_cat {
                ws.fx.companions_mut().cursor_cat.frame(now)
            } else {
                ws.fx.companions_mut().cursor_cat.static_frame(now)
            };
            if !animate_cat
                && sing_drive > 0.0
                && !ws.fx.companions_mut().kitty_sing.external_live()
                && ws.os_window.is_some()
            {
                ws.note_deco_animating(now);
            }
            pin_pet_mode_exit(pet_mode, &mut cat_frame);
            let kitty_enabled = cursor_companion_presentable
                && (cursor_cat_presentation_enabled(
                    animate_cat,
                    glow_cfg.enabled && cursor_companions_allowed,
                    glow_cfg.style,
                    cat_frame.collection_hello,
                ) || cat_frame.sing > 0.0);
            let flying_alpha = if kitty_enabled { cat_frame.alpha } else { 0 };
            let alpha = if kitty_enabled && !pet_mode {
                shed_companion_alpha(cat_frame.alpha, shed_envelope)
            } else {
                0
            };
            cat_frame.alpha = alpha;
            let (pane_rows, pane_cols) = focus_pane_dims.unwrap_or((0, 0));
            ws.pet_pointer_sampled_px = Some(ws.last_cursor_px);
            ws.fx.companion_mut().set_species(pet_species);
            ws.fx
                .companion_mut()
                .offer_glass(
                    panes
                        .iter()
                        .map(|(r, _)| aterm_effects::companion::PetGlass {
                            session: r.session,
                            origin: (
                                i32::from(r.col_off) * glow_cw as i32,
                                i32::from(r.row_off) * glow_ch as i32,
                            ),
                            cols: r.cols,
                            rows: r.rows,
                        }),
                );
            let pet_tick = ws.fx.pet_prepare(aterm_effects::companion::PetFacts {
                facts: &aterm_effects::host::TerminalFacts {
                    session: focus,
                    caret: (focus_pane_dims.is_some() && !focus_scrolled).then_some(focus_cur_pos),
                    cursor_visible: focus_vis,
                    frame_unfinished: focus_frame_unfinished,
                    sync_end_seq: focus_sync_end_seq,
                    sync_end_caret: focus_sync_end_caret,
                    display_offset: i32::try_from(focus_d_off).unwrap_or(i32::MAX),
                    live_viewport: !focus_scrolled,
                    content_seq: focus_content_seq,
                    wrap_serial: focus_wrap_serial,
                    scrolled: focus_scrolled_rows > 0,
                    shell_executing: focus_shell_exec,
                    cmd_done: focus_cmd_done.map(|(seq, code)| (seq, code, focus_cmd_dur_ms)),
                    alt_screen: focus_alt,
                },
                host: &aterm_effects::host::HostFrameInput {
                    now,
                    visibility: pet_visibility(focused),
                    reduced_motion: !cursor_motion.animate(crate::motion::MotionEffect::CursorGlow),
                    serious: !cursor_companions_allowed,
                    shed_envelope,
                    shed_active: load_shed,
                    pointer_px: Some((ws.last_cursor_px.0 as f32, ws.last_cursor_px.1 as f32)),
                    capture: aterm_effects::host::CaptureMode::Present,
                    geometry: pet_frame_geom(
                        (glow_cw, glow_ch),
                        (usize::from(pane_rows), usize::from(pane_cols)),
                        pet_origin,
                    ),
                },
                glow: aterm_effects::companion::GlowOwnership {
                    enabled: glow_cfg.enabled,
                    style: glow_cfg.style,
                    style_raw_names_pet: pet_mode,
                },
                sing: aterm_effects::host::SingFacts {
                    drive: sing_drive,
                    flying_alpha,
                },
                focused,
                obscured: ws.overlay_open() || ws.tab_menu.is_some(),
                pane: Some((
                    focus,
                    (
                        i32::from(focus_off.1) * glow_cw as i32,
                        i32::from(focus_off.0) * glow_ch as i32,
                    ),
                )),
                room: Some(room_facts(
                    self.pool.get(focus),
                    self.session_status.status(focus),
                    sibling_talking.map(|(row, col)| {
                        (
                            f32::from(col) - f32::from(focus_off.1),
                            f32::from(row) - f32::from(focus_off.0),
                        )
                    }),
                    self.harness_glass
                        .get(&focus)
                        .and_then(crate::harness_glass::SessionGlass::landed),
                )),
            });
            mark_end_cheered(&mut self.harness_glass, pet_tick.landed_seen);
            if let Some((code, _)) = pet_tick.completion {
                let focus_boundary = self.pool.get(focus).and_then(|session| {
                    session
                        .ctx
                        .output_echo
                        .sample(&session.ctx.sink, now)
                        .last_boundary_at
                });
                keyed_celebration_completion(
                    &mut ws.fx.companions_mut().kitty_sing,
                    &mut ws.verdict_spent,
                    focus,
                    focus_boundary,
                    now,
                    code,
                );
            }
            if ws.fx.companion_mut().grieving() {
                ws.fx.cursor_mut().glow.hush_fanfare(now);
            }
            if ws.fx.cursor_mut().glow.take_flow_entry() {
                ws.fx.companion_mut().note_flow(now);
            }
            let escort = pet_tick.escort_caret();
            let pet = ws.fx.companion_mut().tick(pet_tick);
            ws.fx.offer_pet_v2(&pet.pet, pane_geom, focus_off, now);
            (cat_frame, alpha, riff, outro, pet, escort)
        };
        if let Some((bar, gain, sig)) = riff {
            self.trail_audio.push(sing_riff_event(bar, gain, sig));
        }
        if let Some((gain, sig)) = outro {
            self.trail_audio.push(sing_outro_event(gain, sig));
        }
        let word_focus =
            self.motion_focus(wid, self.windows.get(&wid).is_some_and(|ws| ws.focused));
        let deco_fp = self.compose_words(
            wid,
            &ComposeDecoCtx {
                panes: &panes,
                focus,
                focus_index,
                win_rows: rows.min(u16::MAX as usize) as u16,
                win_cols: cols.min(u16::MAX as usize) as u16,
                cell_w: glow_cw as u32,
                cell_h: glow_ch as u32,
                escort_caret: escort,
                win_focused: focused,
                word_focus,
                animate_sparkles,
                animate_streak: policy.animate(crate::motion::MotionEffect::OutputStreak),
                animate_cat,
                kitty_alpha,
                cat_frame,
                pet,
                present: true,
                accent: glow_cfg.accent,
                cursor_color: focus_cursor_rgb
                    .map_or(aterm_core::render::COLOR_UNSET, aterm_render::rgb_to_u32),
                now,
            },
        ) ^ if kitty_alpha > 0 { cat_frame.fp() } else { 0 };
        let streak_fp = self.compose_output_streak(
            wid,
            &ComposeDecoCtx {
                panes: &panes,
                focus,
                focus_index,
                win_rows: rows.min(u16::MAX as usize) as u16,
                win_cols: cols.min(u16::MAX as usize) as u16,
                cell_w: glow_cw as u32,
                cell_h: glow_ch as u32,
                escort_caret: escort,
                win_focused: focused,
                word_focus,
                animate_sparkles,
                animate_streak: policy.animate(crate::motion::MotionEffect::OutputStreak),
                animate_cat,
                kitty_alpha,
                cat_frame,
                pet,
                present: true,
                accent: glow_cfg.accent,
                cursor_color: focus_cursor_rgb
                    .map_or(aterm_core::render::COLOR_UNSET, aterm_render::rgb_to_u32),
                now,
            },
        );
        let preedit_owner = self.preedit_owner(wid);
        let preedit_on_grid = matches!(preedit_owner, crate::app_input::PreeditOwner::Grid);
        if !matches!(
            preedit_owner,
            crate::app_input::PreeditOwner::Find | crate::app_input::PreeditOwner::Rename
        ) {
            let composed_caret = (preedit_on_grid && !focus_scrolled)
                .then(|| self.windows.get(&wid))
                .flatten()
                .filter(|ws| !ws.preedit.is_empty())
                .map(|ws| {
                    aterm_core::render::ime_preedit_caret_col(
                        &ws.preedit,
                        ws.preedit_caret,
                        focus_ambiguous_cjk,
                        usize::from(focus_cur_pos.1),
                        usize::from(focus_dims.1),
                    )
                });
            let (pos, vis) = match composed_caret {
                Some(col) => (
                    (focus_cur_pos.0, u16::try_from(col).unwrap_or(u16::MAX)),
                    true,
                ),
                None => (focus_cur_pos, focus_vis),
            };
            self.report_ime_cursor_area(wid, pos, focus_off, vis);
        }
        let settings_fp = self.windows.get(&wid).map_or(0, |ws| ws.overlay_fp());
        let find_fp = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.search.as_ref())
            .map_or(0, |s| s.fingerprint());
        let relaunch_fp = self.relaunch.as_ref().map_or(0, |n| n.fingerprint());
        let badge_fp = crate::build_badge::fingerprint(self.config.show_build_badge_or_default());
        let deck_fp = self.stats_deck_fp(wid);
        let drag_covers = self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.drag_hover && !ws.overlay_open());
        let level_up_fp = self
            .level_up
            .as_ref()
            .filter(|_| !drag_covers)
            .map_or(0, |l| l.fingerprint(now));
        let band_fp = self.windows.get(&wid).map_or(0, |ws| {
            ws.band.band_fp(
                &self.messages,
                usize::from(ws.cols),
                ws.band_hover,
                self.band_geometry(wid),
            )
        });
        let presence_fp = self.presence_fp(wid);
        let overlay_fp = match overlay_inputs {
            Some(inputs) => self.windows.get_mut(&wid).map_or(0, |ws| {
                ws.pane_overlay
                    .finish(inputs, &mut ws.scroll_pill, &mut ws.pill_shown, now)
            }),
            None => 0,
        };
        let active_pane_mark = ActivePaneMark::resolve(
            self.config.split_focus_mark_or_default(),
            raw_focused,
            (
                usize::from(focus_off.0),
                usize::from(focus_off.1),
                usize::from(focus_dims.0),
                usize::from(focus_dims.1),
            ),
        );
        let key = RepaintKey {
            damage_epoch,
            grid_top,
            blink_phase,
            invert,
            drag_hover,
            cursor_override,
            cursor_row: focus_cur_pos.0 as usize,
            cursor_col: focus_cur_pos.1 as usize,
            cursor_visible: focus_vis,
            cursor_style: focus_style,
            glow_fp,
            trail_fp,
            deco_fp,
            rain_fp,
            streak_fp,
            selection: focus_selection,
            pane_selection_fp,
            active_pane_mark: ActivePaneMark::key_term(active_pane_mark),
            tab_strip,
            tab_menu_fp: self.windows.get(&wid).map_or(0, |ws| {
                crate::tab_menu::fingerprint(ws.tab_menu.as_ref(), ws.tab_menu_rect)
            }) ^ crate::autopilot_menu::fingerprint(self.autopilot_menu.as_ref(), wid),
            settings_fp,
            find_fp,
            pill_fp: overlay_fp,
            preedit_fp: self.windows.get(&wid).map_or(0, |ws| {
                crate::preedit_fingerprint(&ws.preedit, ws.preedit_caret)
            }),
            scroll_frac_px: 0,
            relaunch_fp,
            badge_fp,
            deck_fp,
            level_up_fp,
            band_fp,
            presence_fp,
            question: self.question_key(wid),
            system_dark: repaint_system_dark(self.os_appearance),
            link_caption: self.windows.get(&wid).and_then(|ws| ws.link_hover),
            claude_footer_fp: self.claude_footer_fp(wid, plan),
        };
        let recovery_redraw_outstanding = self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.present_retry.recovery_redraw_outstanding);
        if !should_repaint_or_recover(last_present, key, recovery_redraw_outstanding)
            && self.windows.get(&wid).is_none_or(|ws| {
                !ws.predictor.is_displaying(now)
                    && !ws.pred_shown
                    && ws.fx_prefetch_pending.is_empty()
            })
        {
            // The screen is proved unchanged: the `video` recorder's sampling
            // ledger counts this as a sample (see `redraws_proved_unchanged`).
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.redraws_proved_unchanged = ws.redraws_proved_unchanged.wrapping_add(1);
            }
            return None;
        }
        let pane_clip = self.composed_cursor_effect_clip(
            wid,
            ComposedCursorEffectPane {
                row: usize::from(focus_off.0),
                col: usize::from(focus_off.1),
                pane_rows: usize::from(focus_dims.0),
                pane_cols: usize::from(focus_dims.1),
                cell_w: glow_cw,
                cell_h: glow_ch,
            },
        );
        let fluent_seam = self.fluent_seam(plan);
        let ws = self.windows.get_mut(&wid)?;
        let tiles_ready = panes.iter().enumerate().all(|(i, _)| {
            i == focus_index
                || ws
                    .composed_pane_stage_meta
                    .get(i)
                    .is_some_and(Option::is_some)
        });
        #[cfg(test)]
        let tiles_ready = tiles_ready && !COMPOSE_SEAM_UNTILED.with(std::cell::Cell::get);
        let focus_quiet = ws.preedit.is_empty() && ws.predictor.idle() && !ws.pred_shown;
        crate::claude_footer::undo(&mut ws.claude_footer_undo, &mut ws.input_scratch);
        crate::claude_card::strip(&mut ws.claude_card, &mut ws.input_scratch);
        plan_composed_retention(
            ws,
            rows,
            cols,
            divider_cell(grid),
            active_pane_mark,
            &panes,
            focus_index,
            focus_blank,
            focus_quiet,
            tiles_ready,
        );
        fill_divider_grid_retained(
            &mut ws.input_scratch,
            rows,
            cols,
            grid,
            active_pane_mark,
            panes
                .iter()
                .map(|(r, _)| *r)
                .take(if tiles_ready { panes.len() } else { 0 }),
            &ws.composed_retain.row_keep,
        );
        if let Some((inks, layout)) = &fluent_seam {
            crate::pane_chrome::stamp_seam_lines(
                &mut ws.input_scratch,
                rows,
                cols,
                *inks,
                layout,
                &ws.composed_retain.row_keep,
            );
        }
        ws.input_scratch.fx_clip = pane_clip.map(|clip| (clip.x0, clip.y0, clip.x1, clip.y1));
        let sole_pane = panes.len() == 1;
        ws.input_scratch.default_bg = aterm_core::render::COLOR_UNSET;
        ws.input_scratch.cursor_color = aterm_core::render::COLOR_UNSET;
        ws.input_scratch.base_y = 0;
        ws.input_scratch.absolute_row_revision = 0;
        ws.input_scratch.history_renumber_epoch = 0;
        let mut focus_title: Arc<str> = Arc::default();
        let mut painted_pred = false;
        metrics::note_prediction_shown(&mut ws.present_metrics, None, |_| false);
        let mut focus_pred_cursor: Option<(u16, u16)> = None;
        let mut focus_sub_cols = cols;
        let mut preedit_drawn = false;
        ws.capture_leaf_snapshot_seqs.clear();
        ws.capture_leaf_content_seqs.clear();
        for (pane_index, (r, _)) in panes.iter().enumerate() {
            let (sub_rows, sub_cols) = (r.rows as usize, r.cols as usize);
            let focused_pane = pane_index == focus_index;
            if focused_pane {
                // Move the focused extraction into the ordinary pane slot for
                // host-only prediction/blit work (allocation-free; restored after
                // this pane). The return leg must be told whether the region
                // below wrote cells; see [`take_focused_pane_scratch`].
                take_focused_pane_scratch(ws);
            } else {
                let buf = ws.unfocused_pane_scratch.entry(pane_index).or_default();
                std::mem::swap(&mut ws.pane_scratch, buf);
            }
            let (title, pred_paint, blank) = if focused_pane {
                ws.capture_leaf_snapshot_seqs
                    .push(ws.pane_scratch.snapshot_seq);
                ws.capture_leaf_content_seqs
                    .push(ws.pane_scratch.content_seq);
                if sole_pane {
                    ws.input_scratch.default_bg = aterm_render::rgb_to_u32(focus_blank.bg);
                }
                ws.input_scratch.base_y = ws.pane_scratch.base_y;
                ws.input_scratch.absolute_row_revision = ws.pane_scratch.absolute_row_revision;
                ws.input_scratch.history_renumber_epoch = ws.pane_scratch.history_renumber_epoch;
                ws.input_scratch.cursor_color = focus_cursor_rgb
                    .map_or(aterm_core::render::COLOR_UNSET, aterm_render::rgb_to_u32);
                let pred_paint = if pmode == crate::predict::PredictMode::Off {
                    ws.predictor.reset();
                    None
                } else if !ws.predictor.idle() || ws.pred_shown {
                    ws.predictor.set_mode(pmode);
                    ws.predictor.set_editing(pediting);
                    if focus_scrolled {
                        // Scrolled into history: never paint over scrollback,
                        // but still run the expiry flush after this branch.
                        Some(false)
                    } else {
                        let input = &ws.pane_scratch;
                        ws.predictor.reconcile_seen(
                            Some(focus_cur_pos),
                            focus_coords,
                            focus_no_echo,
                            now,
                            |rr, cc| crate::predict_seen::seen_at(input, rr, cc),
                        );
                        Some(true)
                    }
                } else {
                    None
                };
                preedit_drawn = preedit_on_grid
                    && !ws.preedit.is_empty()
                    && ws.pane_scratch.display_offset == 0;
                if preedit_drawn {
                    ws.pane_scratch.overlay_ime_preedit(
                        &ws.preedit,
                        ws.preedit_caret,
                        focus_ambiguous_cjk,
                    );
                }
                (focus_title_sample.clone(), pred_paint, focus_blank)
            } else {
                let (title, blank) = ws
                    .composed_pane_stage_meta
                    .get(pane_index)
                    .and_then(Option::as_ref)
                    .cloned()?;
                ws.capture_leaf_snapshot_seqs
                    .push(ws.pane_scratch.snapshot_seq);
                ws.capture_leaf_content_seqs
                    .push(ws.pane_scratch.content_seq);
                (title, None, blank)
            };
            match pred_paint {
                Some(false) => {
                    let _ = ws.predictor.overlay(now);
                }
                Some(true) => {
                    let painted = paint_prediction_overlay(
                        &mut ws.predictor,
                        &mut ws.pane_scratch,
                        now,
                        sub_cols,
                        blank,
                    );
                    let predictor = &ws.predictor;
                    metrics::note_prediction_shown(
                        &mut ws.present_metrics,
                        painted.closes_input_through(),
                        |op| predictor.op_is_pending(op),
                    );
                    if painted.cursor.is_some() {
                        painted_pred = true;
                        focus_pred_cursor = painted.cursor;
                        focus_sub_cols = sub_cols;
                    }
                }
                None => {}
            }
            let cursor = (focused_pane
                && ws.pane_scratch.cursor_visible
                && ws.pane_scratch.cursor_row < sub_rows
                && ws.pane_scratch.cursor_col < sub_cols)
                .then_some((
                    ws.pane_scratch.cursor_row,
                    ws.pane_scratch.cursor_col,
                    ws.pane_scratch.cursor_style,
                ));
            blit_pane_into_retained(
                &mut ws.input_scratch,
                &ws.pane_scratch,
                r.row_off as usize,
                r.col_off as usize,
                blank,
                &ws.composed_retain.row_keep,
            );
            if tiles_ready {
                seam_declared_shortfall(
                    &mut ws.input_scratch,
                    grid,
                    *r,
                    ws.pane_scratch.rows,
                    ws.pane_scratch.cols,
                    &ws.composed_retain.row_keep,
                );
            }
            record_retain_pane(
                &mut ws.composed_retain,
                pane_index,
                &ws.pane_scratch,
                *r,
                blank,
                focused_pane && (preedit_drawn || painted_pred),
            );
            push_pane_selection(
                &mut ws.input_scratch,
                &ws.pane_scratch,
                r.row_off as usize,
                r.col_off as usize,
                sub_rows,
                sub_cols,
                focused_pane,
            );
            if focused_pane {
                focus_title = title;
                match cursor {
                    Some((cr, cc, style)) => {
                        ws.input_scratch.cursor_row = r.row_off as usize + cr;
                        ws.input_scratch.cursor_col = r.col_off as usize + cc;
                        ws.input_scratch.cursor_visible = true;
                        ws.input_scratch.cursor_style = style;
                    }
                    None => ws.input_scratch.cursor_visible = false,
                }
                #[cfg(test)]
                note_retain_unsound_if_kept(
                    &ws.composed_retain.row_keep,
                    *r,
                    preedit_drawn || painted_pred,
                );
                return_focused_pane_scratch(ws, preedit_drawn || painted_pred);
            } else {
                // Return this pane's buffer to the map with its fresh extraction,
                // restoring `pane_scratch`'s shared identity (the entry was minted
                // by the swap-in).
                if let Some(buf) = ws.unfocused_pane_scratch.get_mut(&pane_index) {
                    std::mem::swap(&mut ws.pane_scratch, buf);
                }
            }
        }
        ws.unfocused_pane_scratch.retain(|pane_index, _| {
            panes
                .get(*pane_index)
                .is_some_and(|_| *pane_index != focus_index)
        });
        let cursor_fx = ws.fx.cursor();
        project_focused_composed_cursor_effects(
            &mut ws.input_scratch,
            FocusedComposedCursorEffects {
                glow_add: &cursor_fx.glow_scratch,
                glow_halo: cursor_fx.glow.halos(),
                fire_patch: cursor_fx.glow.patches(),
                glow_under: cursor_fx.glow.under_quads(),
                char_fg: cursor_fx.glow.charred(),
                fire_halo: cursor_fx.glow.halo_cells(),
                trail: &cursor_fx.trail_scratch,
                trail_color,
                active_cursor_fill,
                fx_layers: &[],
            },
            pane_clip,
            cursor_override,
            body_style_override,
        );
        ws.block_fill = presented_block_fill(
            cursor_override,
            ws.input_scratch.cursor_fill_override,
            block_fill,
        );
        ws.composed_cursor_effect_valid = true;
        ws.composed_cursor_effect_session = Some(focus);
        ws.composed_cursor_fill = active_cursor_fill;
        ws.composed_cursor_style_override = body_style_override;
        ws.composed_cursor_trail_color = trail_color;
        ws.fx.publish_rain(&mut ws.input_scratch);
        ws.fx.publish_words(&mut ws.input_scratch);
        if let Some((pr, pc)) = focus_pred_cursor {
            ws.input_scratch.cursor_row = focus_off.0 as usize + usize::from(pr);
            ws.input_scratch.cursor_col =
                focus_off.1 as usize + usize::from(pc).min(focus_sub_cols.saturating_sub(1));
        }
        #[cfg(any(test, debug_assertions))]
        if ws.pred_shown && !painted_pred {
            crate::predict_trace::clear(Instant::now());
        }
        ws.pred_shown = painted_pred;
        ws.preedit_shown = preedit_drawn;
        ws.input_scratch.snapshot_seq = ws.input_scratch.snapshot_seq.wrapping_add(1);
        ws.pane_headers.paint(&mut ws.input_scratch);
        ws.input_scratch.bless_composed_fill();
        ws.composed_retain.panes.truncate(panes.len());
        ws.composed_retain.seq = if ws.composed_retain.panes.len() == panes.len() {
            ws.input_scratch.composed_fill_seq
        } else {
            0
        };
        ws.composed_retain.rows = rows;
        ws.composed_retain.cols = cols;
        ws.composed_retain.seam = Some(divider_cell(grid));
        ws.composed_retain.mark = active_pane_mark;
        ws.composed_retain.focus = focus_index;
        ws.stamp_present_decision(key);
        ws.present_retry.on_recovery_redraw_serviced();
        Some(focus_title)
    }

    /// Splice the visible tab strip into the top `tab_strip_rows` rows of the just-composed `input_scratch` (filled at terminal size by either the single-pane or composed path), shifting content and cursor down to form the full-window frame the renderer presents.
    pub(crate) fn splice_tab_strip(&mut self, wid: WindowId) {
        // Chrome rows, not strip rows: with the strip off (the macOS default) a
        // live status bar is still a row the capture must carry.
        if self.chrome_rows(wid) == 0 {
            self.splice_tab_strip_with(wid, 0);
            return;
        }
        let Some(mut titles) = self
            .windows
            .get_mut(&wid)
            .map(|ws| std::mem::take(&mut ws.strip_titles_scratch))
        else {
            return;
        };
        self.refill_strip_titles(wid, &mut titles);
        let mut metadata = self
            .windows
            .get_mut(&wid)
            .map(|ws| std::mem::take(&mut ws.strip_metadata_scratch))
            .unwrap_or_default();
        self.refill_strip_metadata(wid, &mut metadata);
        let active = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active_index())
            .unwrap_or(0);
        let tab_strip = self.tab_strip_fingerprint_from_parts(wid, &titles, &metadata, active);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.strip_titles_scratch = titles;
            ws.strip_metadata_scratch = metadata;
        }
        self.splice_tab_strip_with(wid, tab_strip);
    }

    /// Does the strip's version label carry the update mark (` ↑`)? The macOS bar arrow's rule (`App::refresh_version_menu`).
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn strip_version_attention(&self) -> bool {
        crate::menu::staged_for_version_menu(
            self.relaunch
                .as_ref()
                .map(|notice| (notice.build, notice.version.as_str())),
            crate::running_build_number(),
        )
        .is_some()
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn strip_version_slot(
        &mut self,
        wid: WindowId,
        band_key: (usize, usize, usize, u32, u64),
        strip: usize,
        attention: bool,
    ) -> tab_bar::VersionSlotCols {
        let key = (band_key, strip, attention);
        if let Some((memo_key, slot)) = self.windows.get(&wid).and_then(|ws| ws.version_slot_memo)
            && memo_key == key
        {
            return slot;
        }
        let band_h = band_key.2 + strip * band_key.1;
        let slot = tab_bar::pixel_band::version_slot_cols(
            band_key.0,
            band_h,
            f32::from_bits(band_key.3),
            attention,
        );
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.version_slot_memo = Some((key, slot));
        }
        slot
    }

    /// What lies under window `wid`'s strip seam this frame ([`tab_bar::StripFloor`]), first match wins:.
    pub(crate) fn strip_floor(
        &self,
        wid: WindowId,
        bars: usize,
        header: crate::chrome_band::ChromePalette,
    ) -> tab_bar::StripFloor {
        if !tab_bar::SELECTED_TAB_IS_GROUND {
            return tab_bar::StripFloor::Ground;
        }
        if bars > 0 {
            return tab_bar::StripFloor::Surface(chrome_band::band_colors(header).bar_bg);
        }
        if self.find_panel_meets_strip(wid) {
            let panel = self.chrome_palette(ChromeSurface::OverGrid(wid));
            return tab_bar::StripFloor::Surface(chrome_band::band_colors(panel).bar_bg);
        }
        match self.active_lone_view(wid) {
            None if self.active_tab_is_split(wid) => {
                tab_bar::StripFloor::Surface(crate::pane_header::header_colors(header).bar_bg)
            }
            Some(view)
                if matches!(
                    self.view_store.get(view),
                    Some(crate::tab_model::View::Native(_))
                ) =>
            {
                let page = self.chrome_palette(ChromeSurface::NativePage);
                tab_bar::StripFloor::Surface(crate::settings::Roles::from_theme(page).surface)
            }
            _ => tab_bar::StripFloor::Ground,
        }
    }

    /// The colour window `wid` paints its selected tab with while the `active_tab_color` key is unset ([`tab_bar::automatic_active_tab_color`]).
    pub(crate) fn automatic_tab_color(&self, wid: WindowId) -> [u8; 3] {
        let mut header = self.chrome_palette(ChromeSurface::Header);
        header.authored.active_tab = self.installed_colors.theme_authored_chrome().active_tab;
        let floor = self.strip_floor(wid, usize::from(self.message_band_rows), header);
        tab_bar::automatic_active_tab_color(
            header,
            floor,
            self._toolbars
                .get(&wid)
                .and_then(crate::toolbar::native_selected_pill_rgb),
        )
    }

    fn active_tab_is_split(&self, wid: WindowId) -> bool {
        self.windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active())
            .is_some_and(|tab| {
                !tab.zoomed && matches!(tab.root, crate::tab_model::SplitTree::Split { .. })
            })
    }

    fn active_lone_view(&self, wid: WindowId) -> Option<crate::tab_model::ViewId> {
        let tab = self.windows.get(&wid)?.tab_set.active()?;
        match &tab.root {
            crate::tab_model::SplitTree::Leaf(view) => Some(*view),
            crate::tab_model::SplitTree::Split { .. } => tab.zoomed.then_some(tab.focus),
        }
    }

    /// Does this frame's find panel sit on the window's first row, right under the strip? The panel rides the focused pane's top rows, floated to its bottom while the current match sits under them.
    pub(crate) fn find_panel_meets_strip(&self, wid: WindowId) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let Some(search) = ws.search.as_ref() else {
            return false;
        };
        if self.active_tab_is_split(wid) {
            return false;
        }
        let rows = usize::from(ws.rows);
        if rows == 0 || ws.cols == 0 {
            return false;
        }
        let stale = ws.input_scratch.absolute_row_revision != search.match_absolute_row_revision
            || ws.input_scratch.history_renumber_epoch != search.match_history_renumber_epoch;
        let delta = ws.input_scratch.base_y.saturating_sub(search.match_base_y);
        let offset = i64::from(ws.input_scratch.display_offset).saturating_sub(delta);
        let current = (!stale)
            .then(|| search.matches.get(search.current).map(|&(r, _, _)| r))
            .flatten()
            .map(|r| i64::from(r).saturating_add(offset));
        let band_rows = crate::find_bar::FIND_BAR_ROWS.min(rows);
        let floats =
            rows > band_rows && current.is_some_and(|r| r >= 0 && (r as usize) < band_rows);
        !floats
    }

    /// Splice with the strip fingerprint `tab_strip` the caller already computed (the redraw reuses the RepaintKey's, so each tab's terminal is locked once per present).
    pub(crate) fn splice_tab_strip_with(&mut self, wid: WindowId, tab_strip: u64) {
        // Composed panes reuse scratch without extracting a whole frame, so retire
        // its previous chrome even when no band remains; current band rasters are
        // installed below at their new rows.
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.input_scratch.chrome_rasters.clear();
        }
        let strip = self.tab_strip_rows as usize;
        let extend_top = false;
        let bars = usize::from(self.message_band_rows);
        let header = self.chrome_palette(ChromeSurface::Header);
        let chrome_theme = header.theme;
        #[cfg(windows)]
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            crate::platform_win::note_caption_surface(w, self.caption_surface_tone(wid));
        }
        let strip_bleed = (strip > 0)
            .then(|| tab_bar::strip_bleed_tones(header))
            .flatten();
        let bleed = match strip_bleed {
            Some((color, seam)) => Some(aterm_render::ChromeBleed {
                rows: strip + bars,
                first: 0,
                color,
                seam,
                top_extends_cells: extend_top,
                row_edges: [None; aterm_render::CHROME_ROW_EDGES],
            }),
            None if bars > 0 => Some(aterm_render::band::band_bleed(
                &chrome_band::band_colors(header),
                strip,
                bars,
                [None; aterm_render::CHROME_ROW_EDGES],
            )),
            None => None,
        };
        if bars > 0 && self.windows.contains_key(&wid) {
            self.splice_message_band(wid, chrome_theme);
        }
        let bleed = bleed.map(|mut b| {
            if let Some(ws) = self.windows.get(&wid) {
                b.row_edges = band_row_edges(
                    strip,
                    usize::from(self.message_band_rows),
                    &ws.cached_band_edges,
                );
            }
            b
        });
        self.backend.set_chrome_bleed(bleed);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.chrome_rows.clear();
        }
        if !self.windows.contains_key(&wid) {
            return;
        }
        if strip == 0 {
            return;
        }
        let (cols, tab_count, active) = match self.windows.get(&wid) {
            Some(ws) => (
                ws.cols as usize,
                ws.tab_set.len(),
                ws.tab_set.active_index().unwrap_or(0),
            ),
            None => return,
        };
        let floor = self.strip_floor(wid, bars, header);
        let tab_strip = {
            use std::hash::{Hash, Hasher};
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            (tab_strip, floor).hash(&mut hash);
            hash.finish()
        };
        let show_update = self.relaunch.is_some();
        let hovered = self.windows.get(&wid).and_then(|ws| ws.strip_hover);
        let plus_hovered = self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.strip_hover_new_tab);
        let subtitle = (tab_count == 1)
            .then(|| {
                let ws = self.windows.get(&wid)?;
                let tab = ws.tab_set.tabs().first()?;
                tab_bar::solo_subtitle(&tab.presentation.title, tab.presentation.tooltip.as_deref())
            })
            .flatten();
        let rename = self
            .inline_rename_edit(wid)
            .map(|edit| (edit.tab, edit.text.clone(), edit.cursor))
            .and_then(|(tab, text, cursor)| Some((self.tab_index_for_id(wid, tab)?, text, cursor)))
            .map(|(tab, text, cursor)| {
                match self
                    .windows
                    .get(&wid)
                    .filter(|ws| !ws.preedit.is_empty())
                    .map(|ws| field_preedit_display(&text, cursor, &ws.preedit, ws.preedit_caret))
                {
                    Some((display, caret, _)) => (tab, display, caret),
                    None => (tab, text, cursor),
                }
            });
        #[cfg(any(windows, target_os = "linux"))]
        let band_key = {
            let (cell_w, cell_h) = self.win_cell_size(wid);
            let band_top = self.strip_band_top_px(wid);
            let scale = self.windows.get(&wid).map_or(1.0, |ws| ws.scale) as f32;
            (
                cell_w,
                cell_h,
                band_top,
                scale.to_bits(),
                crate::tray_raster::strip_band_font_epoch(),
            )
        };
        #[cfg(any(windows, target_os = "linux"))]
        let version_slot = if tab_bar::STRIP_VERSION_SLOT {
            let attention = self.strip_version_attention();
            self.strip_version_slot(wid, band_key, strip, attention)
        } else {
            tab_bar::VersionSlotCols::NONE
        };
        #[cfg(not(any(windows, target_os = "linux")))]
        let version_slot = tab_bar::VersionSlotCols::NONE;
        let version_hovered = self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.strip_hover_version);
        let pill_key = self
            .strip_carries_pill(wid)
            .then(|| self.presence_pill_key(wid))
            .flatten();
        #[cfg(any(windows, target_os = "linux"))]
        let caption = tab_bar::CaptionSlot::measure(
            self.windows.get(&wid).and_then(|ws| ws.client_frame),
            band_key.0,
            f32::from_bits(band_key.3),
        );
        #[cfg(not(any(windows, target_os = "linux")))]
        let caption = tab_bar::CaptionSlot::NONE;
        let caption_hovered = self.windows.get(&wid).and_then(|ws| ws.strip_hover_caption);
        let hit = self.windows.get(&wid).is_some_and(|ws| {
            #[cfg(any(windows, target_os = "linux"))]
            let band_hit = ws.strip_band_key == band_key;
            #[cfg(not(any(windows, target_os = "linux")))]
            let band_hit = true;
            band_hit
                && ws.cached_strip_rows.len() == strip
                && ws.last_strip_fp.as_ref().is_some_and(|key| {
                    key.0 == tab_strip
                        && key.1 == cols
                        && key.2 == show_update
                        && key.3 == hovered
                        && key.4 == plus_hovered
                        && key.5 == subtitle
                        && key.6 == version_slot
                        && key.7 == version_hovered
                        && key.8 == caption
                        && key.9 == caption_hovered
                        && key.10 == pill_key
                })
        });
        if !hit {
            let palette = self.chrome_palette(ChromeSurface::Header);
            let titles = self
                .windows
                .get_mut(&wid)
                .map(|ws| std::mem::take(&mut ws.strip_titles_scratch))
                .unwrap_or_default();
            let metadata = self
                .windows
                .get_mut(&wid)
                .map(|ws| std::mem::take(&mut ws.strip_metadata_scratch))
                .unwrap_or_default();
            let pill = pill_key.and_then(|_| self.presence_pill(wid));
            let shown = tab_bar::team_titles(&titles, &metadata);
            #[cfg(any(windows, target_os = "linux"))]
            let wants = tab_bar::pixel_band::tab_wants(
                &shown,
                &metadata,
                band_key.0,
                band_key.2 + strip * band_key.1,
                f32::from_bits(band_key.3),
            );
            #[cfg(not(any(windows, target_os = "linux")))]
            let wants: Vec<u16> = Vec::new();
            let sizing = tab_bar::TabSizing::at(
                self.win_cell_size(wid).0,
                self.windows.get(&wid).map_or(1.0, |ws| ws.scale) as f32,
                &wants,
            );
            let segments = if tab_count > 0 || show_update || caption.cols() > 0 {
                tab_bar::layout_segments_with_caption_sized(
                    cols as u16,
                    tab_count,
                    &metadata,
                    active,
                    show_update,
                    version_slot,
                    pill.as_ref().map(|pill| &pill.words),
                    caption,
                    sizing,
                )
            } else {
                Vec::new()
            };
            let mut strip_images = Vec::new();
            let mut strip_rename_caret = None;
            let mut rows: Vec<Vec<RenderCell>> = Vec::with_capacity(strip);
            for r in 0..strip {
                let mut row = vec![tab_bar::blank_cell(palette); cols];
                if r + 1 == strip && !segments.is_empty() {
                    let paint = tab_bar::StripPaint {
                        hovered,
                        new_tab_hovered: plus_hovered,
                        version_hovered,
                        caption_hovered,
                        subtitle: subtitle.as_deref(),
                        rename: rename.as_ref().map(|(tab, text, cursor)| {
                            tab_bar::StripRenameField {
                                tab: *tab,
                                text,
                                cursor: *cursor,
                            }
                        }),
                    };
                    (strip_images, strip_rename_caret) =
                        tab_bar::paint_strip_with_metadata_and_rename_caret(
                            &mut row, &segments, &shown, &metadata, paint, active, palette, floor,
                        );
                    if let Some(pill) = pill.as_ref() {
                        tab_bar::paint_strip_pill(
                            &mut row,
                            &segments,
                            tab_bar::StripPill {
                                kind: pill.kind,
                                words: &pill.words,
                            },
                            palette,
                        );
                    }
                }
                if r + 1 == strip {
                    // The band's bottom edge on Linux, stamped after the paint on
                    // whatever the last row is (segments or bare chrome). On
                    // Windows it is a rail on the last row's last pixel rows
                    // (`tab_bar::strip_seam_rail`, installed after the prepend). A
                    // no-op on macOS.
                    tab_bar::seal_strip_bottom(&mut row, palette);
                }
                rows.push(row);
            }
            #[cfg(any(windows, target_os = "linux"))]
            let strip_band = {
                let paint = tab_bar::StripPaint {
                    hovered,
                    new_tab_hovered: plus_hovered,
                    version_hovered,
                    caption_hovered,
                    subtitle: subtitle.as_deref(),
                    rename: rename
                        .as_ref()
                        .map(|(tab, text, cursor)| tab_bar::StripRenameField {
                            tab: *tab,
                            text,
                            cursor: *cursor,
                        }),
                };
                let foot_px = self.strip_foot_px(wid, strip);
                let seam_top_px = if tab_bar::STRIP_WINDOWS_PAINT {
                    Some(
                        (band_key.2 + strip * band_key.1)
                            .saturating_sub(tab_bar::strip_seam_px(f32::from_bits(band_key.3)))
                            .saturating_sub(foot_px),
                    )
                } else {
                    self.windows.get(&wid).map(|ws| {
                        band_key.2
                            + strip.saturating_sub(1) * band_key.1
                            + self
                                .backend
                                .deco_metrics_for(ws.metrics.font_px)
                                .underline_y
                    })
                };
                tab_bar::pixel_band::raster_band(
                    &tab_bar::pixel_band::BandInput {
                        segments: &segments,
                        titles: &shown,
                        metadata: &metadata,
                        paint,
                        active,
                        palette,
                        geometry: tab_bar::pixel_band::BandGeometry {
                            cols,
                            cell_w: band_key.0,
                            cell_h: band_key.1,
                            strip_rows: strip,
                            band_top_px: band_key.2,
                            scale: f32::from_bits(band_key.3),
                            seam_top_px,
                            foot_px,
                        },
                        floor,
                    },
                    &strip_images,
                )
                .unwrap_or_default()
            };
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.tab_segments = segments;
                ws.cached_strip_rows = rows;
                ws.cached_strip_images = strip_images;
                ws.cached_strip_rename_caret = strip_rename_caret;
                ws.strip_pill_session = pill.as_ref().map(|pill| pill.session);
                ws.last_strip_fp = Some((
                    tab_strip,
                    cols,
                    show_update,
                    hovered,
                    plus_hovered,
                    subtitle,
                    version_slot,
                    version_hovered,
                    caption,
                    caption_hovered,
                    pill_key,
                ));
                #[cfg(any(windows, target_os = "linux"))]
                {
                    ws.cached_strip_band = strip_band;
                    ws.strip_band_key = band_key;
                }
                ws.strip_titles_scratch = titles;
                ws.strip_metadata_scratch = metadata;
            }
        }
        if matches!(
            self.preedit_owner(wid),
            crate::app_input::PreeditOwner::Rename
        ) && let Some(cell) = tab_bar::rename_ime_cursor_cell(
            strip,
            self.windows
                .get(&wid)
                .and_then(|ws| ws.cached_strip_rename_caret),
        ) {
            self.report_ime_cursor_area_frame(wid, cell);
        }
        let (cell_w, cell_h) = self.win_cell_size(wid);
        let grid_top = self.win_pad_top(wid) + self.win_head(wid);
        let seam_rail = tab_bar::strip_seam_rail(
            u16::try_from(strip - 1).unwrap_or(u16::MAX),
            cols.saturating_mul(cell_w)
                .saturating_add(2 * self.win_pad(wid)),
            tab_bar::strip_seam_px(self.windows.get(&wid).map_or(1.0, |ws| ws.scale) as f32),
            self.strip_foot_px(wid, strip),
            header,
            floor,
        );
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        prepend_strip_rows(
            &mut ws.input_scratch,
            &ws.cached_strip_rows,
            cell_h,
            grid_top,
            &mut ws.strip_row_pool,
        );
        if let Some(rail) = seam_rail {
            let rasters = &mut ws.input_scratch.chrome_rasters;
            rasters.retain(|m| m.row != rail.row);
            rasters.push(rail);
        }
        #[cfg(any(windows, target_os = "linux"))]
        let band_applied = {
            let applied = !ws.cached_strip_band.is_empty();
            for (r, refs) in ws.cached_strip_band.iter().enumerate().take(strip) {
                if let Some(image_row) = ws.input_scratch.images.get_mut(r) {
                    image_row.clone_from(refs);
                }
            }
            applied
        };
        #[cfg(not(any(windows, target_os = "linux")))]
        let band_applied = false;
        if !band_applied && let Some(image_row) = ws.input_scratch.images.get_mut(strip - 1) {
            image_row.clone_from(&ws.cached_strip_images);
        }
    }

    /// The chrome accent on the terminal grid, packed `0x00RRGGBB`.
    pub(crate) fn grid_accent(&self) -> u32 {
        aterm_render::rgb_to_u32(self.chrome_palette(ChromeSurface::Grid).tokens().accent)
    }

    /// The palette a chrome surface paints from ([`ChromeSurface`]).
    pub(crate) fn chrome_palette(
        &self,
        surface: ChromeSurface,
    ) -> crate::chrome_band::ChromePalette {
        let theme = match surface {
            ChromeSurface::Header => self.chrome_palette_theme(),
            ChromeSurface::OverGrid(wid) => self.chrome_card_theme(wid),
            ChromeSurface::Content(wid) => self.overlay_card_theme(wid),
            ChromeSurface::Grid => self.theme,
            ChromeSurface::NativePage => self.native_page_palette_theme(),
        };
        self.chrome_palette_on(theme)
    }

    /// [`Self::chrome_palette`] over an explicit `theme`.
    pub(crate) fn chrome_palette_on(
        &self,
        theme: aterm_render::Theme,
    ) -> crate::chrome_band::ChromePalette {
        let ansi = self.session_factory.terminal_config.as_ref().map(|tc| {
            tc.custom_palette.as_ref().map_or_else(
                || crate::chrome_band::ansi_of_palette(&aterm_types::ColorPalette::new()),
                crate::chrome_band::ansi_of_palette,
            )
        });
        crate::chrome_band::ChromePalette {
            theme,
            ansi,
            authored: self.installed_colors.authored_chrome(),
        }
    }

    /// Paint (cached) and prepend the message band rows onto the just-composed frame.
    pub(crate) fn splice_message_band(&mut self, wid: WindowId, theme: aterm_render::Theme) {
        let Some((cols, hover)) = self
            .windows
            .get(&wid)
            .map(|ws| (ws.cols as usize, ws.band_hover))
        else {
            return;
        };
        let palette = self.chrome_palette_on(theme);
        let prepared = self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.band.is_prepared(&self.messages, cols));
        if !prepared {
            self.prepare_band_motion(wid, Instant::now());
        }
        let geom = self.band_geometry(wid);
        let forced = crate::chrome_band::forced_chrome().is_some();
        let inks_once = std::cell::OnceCell::new();
        let inks = || *inks_once.get_or_init(|| palette.colors());
        if let Some(ws) = self.windows.get_mut(&wid)
            && let Some(resolved) = ws
                .band
                .paint(&self.messages, cols, hover, geom, forced, &inks)
        {
            (
                ws.cached_band_rows,
                ws.cached_band_edges,
                ws.cached_band_rasters,
            ) = aterm_render::band::rows(resolved);
        }
        let cell_h = self.win_cell_size(wid).1;
        let room = aterm_render::band::native_host_room(|| self.backend.band_room());
        let pad = self.win_pad(wid);
        let grid_top = self.win_pad_top(wid) + self.win_head(wid);
        let committed = usize::from(self.message_band_rows);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if committed == 0 {
            return;
        }
        let inks = inks();
        let at = aterm_render::band::BandFrame {
            cols,
            cell_w: geom.cell_w,
            cell_h,
            pad,
            lo: geom.cells_x.saturating_sub(pad),
            frame_w: cols.saturating_mul(geom.cell_w).saturating_add(2 * pad),
            grid_top,
            room,
        };
        let _ = aterm_render::band::compose_band(
            &mut ws.input_scratch,
            &ws.cached_band_rows,
            &ws.cached_band_rasters,
            committed,
            &inks,
            forced,
            at,
            &mut ws.strip_row_pool,
            aterm_core::render::HostRowPixels::Placed,
        );
    }

    fn set_scroll_band(&mut self, wid: WindowId) {
        let strip = usize::from(self.chrome_rows(wid));
        let cell_h = self.win_cell_size(wid).1.max(1) as i32;
        let animate = self.smooth_scroll_animates(wid);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        ws.input_scratch.grid_top_row = strip;
        ws.input_scratch.grid_bot_row = ws.input_scratch.rows;
        ws.input_scratch.scroll_frac_px = if animate {
            ws.scroll_frac_px.clamp(-(cell_h - 1), cell_h - 1)
        } else {
            0
        };
    }

    fn find_region(&self, wid: WindowId) -> FindRegion {
        let Some(ws) = self.windows.get(&wid) else {
            return FindRegion::default();
        };
        let whole = FindRegion {
            row0: 0,
            col0: 0,
            rows: usize::from(ws.rows),
            cols: usize::from(ws.cols),
            display_offset: ws.input_scratch.display_offset,
        };
        let Some(plan) = self
            .active_visible_leaf_plan(wid)
            .filter(|plan| plan.leaves.len() > 1)
        else {
            return whole;
        };
        let Some(leaf) = plan.leaves.iter().find(|leaf| leaf.focused) else {
            return whole;
        };
        let source = match self.visible_content_route_from_plan(&plan) {
            Some(crate::VisibleContentRoute::Terminal { composed: true }) => {
                Some(&ws.composed_focus_scratch)
            }
            Some(crate::VisibleContentRoute::Heterogeneous) => ws
                .leaf_render_cache
                .get(&leaf.view)
                .map(|cache| &cache.input),
            _ => None,
        };
        let Some(source) = source else {
            return whole;
        };
        let cells = |v: f32| v.round().max(0.0) as usize;
        let row0 = cells(leaf.rect.origin.y).min(whole.rows);
        let col0 = cells(leaf.rect.origin.x).min(whole.cols);
        FindRegion {
            row0,
            col0,
            rows: cells(leaf.rect.size.height).min(whole.rows - row0),
            cols: cells(leaf.rect.size.width).min(whole.cols - col0),
            display_offset: source.display_offset,
        }
    }

    /// Paint the Cmd-f find bar over the top terminal rows (below the tab strip; floated to the bottom when the current match sits under it) while find is active (`ws.search`).
    pub(crate) fn splice_find_bar(&mut self, wid: WindowId) {
        let Some((base_y_now, frame_absolute_row_revision, frame_history_renumber_epoch)) =
            self.windows.get(&wid).map(|ws| {
                (
                    ws.input_scratch.base_y,
                    ws.input_scratch.absolute_row_revision,
                    ws.input_scratch.history_renumber_epoch,
                )
            })
        else {
            return;
        };
        let (match_base_y, match_absolute_row_revision, match_history_renumber_epoch) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.search.as_ref())
            .map_or((0, 0, 0), |s| {
                (
                    s.match_base_y,
                    s.match_absolute_row_revision,
                    s.match_history_renumber_epoch,
                )
            });
        let stale_absolute_rows = frame_absolute_row_revision != match_absolute_row_revision
            || frame_history_renumber_epoch != match_history_renumber_epoch;
        let delta = base_y_now.saturating_sub(match_base_y);
        let owner_is_find = matches!(
            self.preedit_owner(wid),
            crate::app_input::PreeditOwner::Find
        );
        let field_preedit = owner_is_find
            .then(|| {
                self.windows
                    .get(&wid)
                    .map(|ws| (ws.preedit.clone(), ws.preedit_caret))
            })
            .flatten();
        let Some((view, preedit_span, cur_sel_row, cur_idx, nrows, term_rows)) =
            self.windows.get(&wid).and_then(|ws| {
                ws.search.as_ref().map(|s| {
                    let cur_sel_row = if stale_absolute_rows {
                        None
                    } else {
                        s.matches.get(s.current).map(|&(r, _, _)| r)
                    };
                    let (query, cursor, preedit_span) = match &field_preedit {
                        Some((preedit, pc)) => {
                            let (q, c, r) = field_preedit_display(&s.query, s.cursor, preedit, *pc);
                            (q, c, Some(r))
                        }
                        None => (s.query.clone(), s.cursor, None),
                    };
                    (
                        crate::find_bar::FindBarView {
                            query,
                            cursor,
                            idx: s.current + 1,
                            total: s.matches.len(),
                            case_sensitive: s.case_sensitive,
                            is_regex: s.is_regex,
                            regex_error: s.regex_error,
                            truncated: s.truncated,
                            searched_lines: s.searched_lines,
                            match_capped: s.match_capped,
                            cut_short: s.cut_short,
                            stale: s.results_dirty,
                            history_away: s.history_away,
                        },
                        preedit_span,
                        cur_sel_row,
                        s.current,
                        ws.input_scratch.cells.len(),
                        ws.rows as usize,
                    )
                })
            })
        else {
            // Not searching: drop any indicator geometry a prior frame left, so a
            // click on those cells is not swallowed by a no-op toggle.
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.find_bar_hit = None;
            }
            return;
        };
        let region = self.find_region(wid);
        let cols = self.windows.get(&wid).map_or(0, |ws| ws.cols as usize);
        if cols == 0 || region.rows == 0 || region.cols == 0 {
            return;
        }
        let offset = i64::from(region.display_offset).saturating_sub(delta);
        let cur_term_row = cur_sel_row.map(|r| i64::from(r).saturating_add(offset));
        let strip = nrows.saturating_sub(term_rows);
        if term_rows == 0 {
            // Distinct branch.
            return; // zero-row terminal
        }
        let band_rows = crate::find_bar::FIND_BAR_ROWS.min(region.rows);
        let match_under_top_band = cur_term_row.is_some_and(|r| r >= 0 && (r as usize) < band_rows);
        let bar_row = if region.rows > band_rows && match_under_top_band {
            region.rows - band_rows
        } else {
            0
        };
        let pane_band = bar_row..bar_row + band_rows;
        let band = region.row0 + bar_row..region.row0 + bar_row + band_rows;
        let frame_bar_row = strip + band.start;
        let (cell_w, cell_h) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let grid_top = self.win_pad_top(wid) + self.win_head(wid);
        let rails = aterm_render::band::seam_can_rail(
            cell_h,
            aterm_render::band::native_host_room(|| self.backend.band_room()),
        );
        let whole_rows = region.col0 == 0 && region.cols == cols;
        let room = self.backend.band_room();
        let rail_h = |want: u16| {
            if whole_rows {
                Some(want)
            } else {
                unlifted_rail_h(want, room)
            }
        };
        let seam = if pane_band.end >= region.rows {
            crate::find_bar::FindSeam::Top
        } else if rails && rail_h(1).is_some() {
            crate::find_bar::FindSeam::BottomRail
        } else {
            crate::find_bar::FindSeam::BottomUnderline
        };
        let panel = self.chrome_palette(ChromeSurface::OverGrid(wid));
        let content = self.chrome_palette(ChromeSurface::Content(wid));
        let tokens = content.tokens();
        let hi = tokens.search_match(crate::settings::u32_rgb(content.theme.selection));
        let current_rgb = crate::find_bar::current_match_fill(&tokens, hi);
        let current_fill = aterm_render::rgb_to_u32(current_rgb);
        let current_ink =
            aterm_render::rgb_to_u32(crate::find_bar::current_match_ink(&tokens, current_rgb));
        let painted = crate::find_bar::find_bar_paint(&view, region.cols, band_rows, panel, seam);
        let scale = self.windows.get(&wid).map_or(1.0, |ws| ws.scale as f32);
        let fluent = crate::find_bar::fluent_bar(&crate::find_bar::FluentInput {
            view: &view,
            paint: &painted,
            colors: crate::chrome_band::band_colors(panel),
            geom: crate::find_bar::FluentGeom {
                cell_w,
                cell_h,
                scale,
            },
            seam,
            preedit: preedit_span.as_ref().and_then(|span| {
                let start = view.query.get(..span.start)?.chars().count();
                Some(start..start + view.query.get(span.clone())?.chars().count())
            }),
        });
        let hi_u32 = aterm_render::rgb_to_u32(hi);
        let (visible_matches, _match_search_comparisons) = if stale_absolute_rows {
            (0..0, 0)
        } else {
            self.windows
                .get(&wid)
                .and_then(|ws| ws.search.as_ref())
                .map_or((0..0, 0), |search| {
                    visible_match_range(&search.matches, offset, region.rows, search.max_match_rows)
                })
        };
        let laid = self.glass_grid(wid);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        #[cfg(test)]
        {
            ws.find_bar_match_work = _match_search_comparisons + visible_matches.len();
        }
        let matches: &[(i32, u16, u32)] = match ws.search.as_ref() {
            Some(s) => &s.matches,
            None => return,
        };
        let cols_wide = region.cols.max(1);
        for (relative, &(sel_row, c0, c1)) in matches[visible_matches.clone()].iter().enumerate() {
            let i = visible_matches.start + relative;
            let is_current = i == cur_idx;
            let last_col = c1 as usize;
            let last_offset = last_col / cols_wide;
            for row_offset in 0..=last_offset {
                let term_row = i64::from(sel_row)
                    .saturating_add(i64::try_from(row_offset).unwrap_or(i64::MAX))
                    .saturating_add(offset);
                if term_row < 0 || term_row as usize >= region.rows {
                    continue;
                }
                if pane_band.contains(&(term_row as usize)) {
                    continue;
                }
                let from = if row_offset == 0 { usize::from(c0) } else { 0 };
                let through = if row_offset == last_offset {
                    last_col - row_offset * cols_wide
                } else {
                    cols_wide - 1
                };
                let frame_row = strip + region.row0 + term_row as usize;
                let Some(row) = ws.input_scratch.cells.get_mut(frame_row) else {
                    continue;
                };
                let map = ws
                    .input_scratch
                    .visual_to_logical
                    .get(frame_row)
                    .map_or(&[][..], Vec::as_slice);
                let (scan_from, scan_end) = if map.is_empty() {
                    (region.col0 + from, region.col0 + through.saturating_add(1))
                } else {
                    (region.col0, region.col0 + region.cols)
                };
                for (v, cell) in row.iter_mut().enumerate().take(scan_end).skip(scan_from) {
                    let logical = map.get(v).map_or(v, |&l| usize::from(l));
                    let Some(pane_col) = logical.checked_sub(region.col0) else {
                        continue;
                    };
                    if !(from..=through).contains(&pane_col) {
                        continue;
                    }
                    cell.bg = hi;
                    if !is_current {
                        cell.fg = crate::settings::u32_rgb(aterm_render::floor_selection_fg(
                            aterm_render::rgb_to_u32(cell.fg),
                            hi_u32,
                        ));
                    }
                }
            }
        }
        if cur_term_row.is_some() {
            ws.input_scratch.selection_bg = current_fill;
            ws.input_scratch.selection_fg = current_ink;
            for entry in &mut ws.input_scratch.selections {
                if !entry.inactive {
                    entry.bg = current_fill;
                    entry.fg = current_ink;
                }
            }
        }
        debug_assert_eq!(painted.rows.len(), band_rows);
        let frame_band = frame_bar_row..frame_bar_row + band_rows;
        for (offset_row, mut built) in painted.rows.into_iter().enumerate() {
            debug_assert_eq!(built.len(), region.cols);
            if fluent.is_some() {
                for cell in &mut built {
                    cell.underline = aterm_core::terminal::UnderlineStyle::None;
                    cell.underline_color = None;
                    cell.overline = false;
                    cell.overline_color = None;
                }
            }
            let frame_row = frame_bar_row + offset_row;
            if whole_rows {
                ws.input_scratch.cells[frame_row] = built;
                clear_find_row_lists(&mut ws.input_scratch, frame_row);
            } else {
                splice_find_row_span(&mut ws.input_scratch, frame_row, region.col0, built);
            }
        }
        if whole_rows {
            scrub_overlay_row_band(
                &mut ws.input_scratch,
                frame_band.clone(),
                aterm_render::RowBands::new(grid_top, cell_h),
            );
        } else {
            scrub_overlay_card(
                &mut ws.input_scratch,
                crate::tab_menu::MenuRect {
                    row: frame_bar_row,
                    col: region.col0,
                    w: region.cols,
                    h: band_rows,
                },
                (cell_w, cell_h),
                (pad, grid_top),
            );
        }
        if let Some(bar) = fluent.as_ref() {
            let input = &mut ws.input_scratch;
            if input.images.len() < input.cells.len() {
                input.images.resize_with(input.cells.len(), Vec::new);
            }
            let span = region.col0..region.col0 + region.cols;
            for (offset_row, image) in bar.rows.iter().enumerate() {
                let Some(images) = input.images.get_mut(frame_bar_row + offset_row) else {
                    continue;
                };
                images.retain(|(col, _)| !span.contains(col));
                images.extend((0..region.cols).map(|i| {
                    (
                        region.col0 + i,
                        aterm_core::grid::extra::ImageRef {
                            image: Arc::clone(image),
                            cell_row: 0,
                            cell_col: u16::try_from(i).unwrap_or(u16::MAX),
                            kitty: None,
                        },
                    )
                }));
                images.sort_by_key(|(col, _)| *col);
            }
        }
        let px = |col: usize| pad + (region.col0 + col) * cell_w;
        let seam_rail = seam == crate::find_bar::FindSeam::BottomRail;
        if seam_rail
            && fluent.is_none()
            && let Ok(row) = u16::try_from(frame_band.end - 1)
        {
            let rail = find_rail(row, px(0)..px(region.cols), painted.seam, 1);
            ws.input_scratch.chrome_rasters.push(rail);
        }
        if let Some(ink) = painted.well_line
            && fluent.is_none()
            && rails
            && !(seam_rail && painted.field_row + 1 == band_rows)
            && let Some(well_h) = rail_h(2)
            && let Ok(row) = u16::try_from(frame_bar_row + painted.field_row)
        {
            let well = &painted.field_cols;
            let rail = find_rail(row, px(well.start)..px(well.end), ink, well_h);
            ws.input_scratch.chrome_rasters.push(rail);
        }
        if let Some(close) = painted.close_cols.as_ref()
            && fluent.is_none()
            && let Ok(row) = u16::try_from(frame_bar_row + painted.field_row)
            && let Ok(col) = u16::try_from(region.col0 + close.start + 1)
        {
            let icon = aterm_render::ChromeIcon {
                col,
                icon: aterm_render::BandIcon::Error,
            };
            let rasters = &mut ws.input_scratch.chrome_rasters;
            match rasters.iter_mut().find(|raster| raster.row == row) {
                Some(raster) => raster.icons.push(icon),
                None => rasters.push(aterm_render::ChromeRaster {
                    row,
                    ground: Arc::from(Vec::new()),
                    rail: Arc::from(Vec::new()),
                    rail_h: 0,
                    clear_rail: false,
                    own: Vec::new(),
                    split: None,
                    rings: Vec::new(),
                    icons: vec![icon],
                }),
            }
        }
        claim_chrome_rows(ws, frame_band.clone());
        if frame_band.end == nrows {
            if ws.input_scratch.grid_bot_row > frame_band.start {
                let top = ws.input_scratch.grid_top_row;
                ws.input_scratch.grid_bot_row = frame_band.start.max(top);
            }
        } else if ws.input_scratch.grid_top_row < frame_band.end {
            let bot = ws.input_scratch.grid_bot_row;
            ws.input_scratch.grid_top_row = frame_band.end.min(bot);
        }
        let at = |cols: Range<usize>| cols.start + region.col0..cols.end + region.col0;
        let field_cols = at(painted.field_cols.clone());
        let mut ime_caret: Option<(u16, u16)> = None;
        if let Some(bar) = fluent.as_ref()
            && owner_is_find
        {
            let col =
                (bar.caret_x.max(0.0) as usize / cell_w.max(1)).min(region.cols.saturating_sub(1));
            ime_caret = Some((
                u16::try_from(band.start + painted.field_row).unwrap_or(u16::MAX),
                u16::try_from(region.col0 + col).unwrap_or(u16::MAX),
            ));
        }
        if let Some(span) = preedit_span.as_ref().filter(|r| !r.is_empty())
            && fluent.is_none()
            && !field_cols.is_empty()
        {
            let pre_chars = view.query[..span.start].chars().count();
            let run_chars = view.query[span.clone()].chars().count();
            let scroll = painted.field_scroll;
            let field = field_cols.clone();
            let frame_field_row = frame_bar_row + painted.field_row;
            if let Some(rowv) = ws.input_scratch.cells.get_mut(frame_field_row) {
                for i in pre_chars..pre_chars + run_chars {
                    let Some(rel) = i.checked_sub(scroll) else {
                        // Distinct branch.
                        continue; // scrolled out past the well's left edge
                    };
                    let col = field.start + rel;
                    if col >= field.end {
                        break;
                    }
                    if let Some(cell) = rowv.get_mut(col) {
                        cell.underline = aterm_core::terminal::UnderlineStyle::Single;
                    }
                }
            }
            let mut cb = view.cursor.min(view.query.len());
            while cb > 0 && !view.query.is_char_boundary(cb) {
                cb -= 1;
            }
            let caret_chars = view.query[..cb].chars().count();
            let caret_col = field.start
                + caret_chars
                    .saturating_sub(scroll)
                    .min(field.len().saturating_sub(1));
            ime_caret = Some((
                u16::try_from(band.start + painted.field_row).unwrap_or(u16::MAX),
                u16::try_from(caret_col).unwrap_or(u16::MAX),
            ));
        }
        let fallback_cell = (owner_is_find && ime_caret.is_none()).then(|| {
            (
                u16::try_from(band.start + painted.field_row).unwrap_or(u16::MAX),
                u16::try_from(field_cols.start.min(cols.saturating_sub(1))).unwrap_or(u16::MAX),
            )
        });
        ws.find_bar_hit = Some(crate::FindBarHit {
            row: band.start + painted.field_row,
            case_cols: painted.case_cols.map(at),
            regex_cols: painted.regex_cols.map(at),
            prev_cols: painted.prev_cols.map(at),
            next_cols: painted.next_cols.map(at),
            close_cols: painted.close_cols.map(at),
            field_cols,
            field_scroll: fluent
                .as_ref()
                .map_or(painted.field_scroll, |bar| bar.scroll),
            band,
            cols: region.col0..region.col0 + region.cols,
            laid,
        });
        ws.input_scratch.snapshot_seq = ws.input_scratch.snapshot_seq.wrapping_add(1);
        if let Some(cell) = ime_caret.or(fallback_cell) {
            self.report_ime_cursor_area(wid, cell, (0, 0), true);
        }
    }

    /// The theme an overlay card on window `wid` tints with.
    pub(crate) fn overlay_card_theme(&self, wid: WindowId) -> Theme {
        if self.card_over_native_page(wid) {
            return self.native_page_palette_theme();
        }
        self.terminal_card_theme(wid)
    }

    fn terminal_card_theme(&self, wid: WindowId) -> Theme {
        let mut theme = self.theme;
        if let Some(ws) = self.windows.get(&wid) {
            theme.bg = present_band_bg(ws.input_scratch.default_bg, theme.bg);
        }
        theme
    }

    fn card_over_native_page(&self, wid: WindowId) -> bool {
        if !self.active_tab_contains_native(wid) {
            return false;
        }
        let Some((plan, route)) = self.active_visible_frame_layout(wid) else {
            return false;
        };
        match route {
            crate::VisibleContentRoute::Native { .. } => true,
            crate::VisibleContentRoute::Terminal { .. } => false,
            crate::VisibleContentRoute::Heterogeneous => {
                let bounds = plan.bounds;
                let right = bounds.origin.x + bounds.size.width;
                let bottom = bounds.origin.y + bounds.size.height;
                let (x, y) = (right - 0.5, bounds.origin.y + 0.5);
                let point = crate::tab_model::LogicalPoint {
                    x: x.clamp(bounds.origin.x, (right - 0.5).max(bounds.origin.x)),
                    y: y.clamp(bounds.origin.y, (bottom - 0.5).max(bounds.origin.y)),
                };
                plan.leaves
                    .iter()
                    .find(|leaf| leaf.outer_rect().contains(point))
                    .is_some_and(|leaf| {
                        matches!(
                            self.view_store.get(leaf.view),
                            Some(crate::tab_model::View::Native(_))
                        )
                    })
            }
        }
    }

    /// Overlay the modal Settings panel over the top `panel_rows` rows of the composed frame (a byte-identical no-op when no overlay is open).
    pub(crate) fn splice_settings_panel(&mut self, wid: WindowId) {
        let panel_rows = self.windows.get(&wid).map_or(0, |ws| ws.overlay_rows());
        if panel_rows == 0 {
            // Closed (or too short to paint): drop any stale card so it stops compositing.
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.settings_card = None;
            }
            return;
        }
        let cols = match self.windows.get(&wid) {
            Some(ws) => ws.cols as usize,
            None => return,
        };
        let palette = self.chrome_palette(ChromeSurface::OverGrid(wid));
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid) as u32;
        let pad_top = self.win_pad_top(wid) as u32;
        let head = self.win_head(wid) as u32;
        let font_px = self.win_font_px(wid);
        let system_dark = repaint_system_dark(self.os_appearance);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let fp = ws.overlay_fp();
        let geom_key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            palette.hash(&mut h);
            cw.hash(&mut h);
            ch.hash(&mut h);
            font_px.to_bits().hash(&mut h);
            cols.hash(&mut h);
            panel_rows.hash(&mut h);
            pad.hash(&mut h);
            pad_top.hash(&mut h);
            head.hash(&mut h); // the dy anchor moves with the chrome headroom
            system_dark.hash(&mut h);
            h.finish()
        };
        if ws
            .settings_card
            .as_ref()
            .is_none_or(|c| c.fp != fp || c.geom != geom_key)
        {
            let geom = crate::settings::SettingsGeom {
                cw: cw as f32,
                ch: ch as f32,
                font_px,
                cols,
                panel_rows,
            };
            let Some(overlay) = ws.overlay.as_ref() else {
                return;
            };
            let mut tray = overlay.model().tray(&geom, palette);
            const PAINT_MARGIN: f32 = 12.0; // covers the 2-step drop shadow (≤ 8 px)
            let tray_w = (cols * cw) as f32;
            let tray_h = (panel_rows * ch) as f32;
            let (card_x, card_y, card_w, card_h) = tray.card;
            let x0 = (card_x - PAINT_MARGIN).max(0.0).floor();
            let y0 = (card_y - PAINT_MARGIN).max(0.0).floor();
            let x1 = (card_x + card_w + PAINT_MARGIN).min(tray_w).ceil();
            let y1 = (card_y + card_h + PAINT_MARGIN).min(tray_h).ceil();
            if x1 <= x0 || y1 <= y0 {
                ws.settings_card = None;
                return;
            }
            crate::widget::translate_prims(&mut tray.prims, -x0, -y0);
            let (rgba, pw, ph) = crate::tray_raster::rasterize_tray(
                &tray.prims,
                (x1 - x0) as u32,
                (y1 - y0) as u32,
                1.0,
                [0, 0, 0, 0],
            );
            ws.settings_card = Some(crate::SettingsCard {
                scrim: overlay.model().scrim(),
                rgba,
                pw,
                ph,
                dx: (pad as f32 + x0) as u32,
                dy: ((pad_top + head) as f32 + y0) as u32,
                fp,
                geom: geom_key,
                raster_fingerprint_once: std::sync::OnceLock::new(),
            });
        }
        ws.input_scratch.cursor_visible = false;
        ws.input_scratch.apron_row.cursor_col = None;
        ws.input_scratch.snapshot_seq = ws.input_scratch.snapshot_seq.wrapping_add(1);
    }

    /// The palette a floating chrome card (build badge, package-progress card, and every card over the grid: [`ChromeSurface::OverGrid`]) rasterizes from, by the surface under it ([`Self::card_over_native_page`]).
    pub(crate) fn chrome_card_theme(&self, wid: WindowId) -> aterm_render::Theme {
        if self.card_over_native_page(wid) {
            return self.native_page_palette_theme();
        }
        let chrome = self.chrome_palette_theme();
        if (chrome.bg, chrome.fg) != (self.theme.bg, self.theme.fg) {
            return chrome;
        }
        self.terminal_card_theme(wid)
    }

    /// Rasterize the top-right build/version badge into its paint-only `badge_card` slot (never gates the mouse).
    pub(crate) fn splice_build_badge(&mut self, wid: WindowId) {
        if !self.config.show_build_badge_or_default() {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.badge_card = None;
            }
            return;
        }
        let cols = self.windows.get(&wid).map_or(0, |ws| ws.cols as usize);
        if cols == 0 {
            // No window / zero-width frame: drop any stale badge so it stops compositing.
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.badge_card = None;
            }
            return;
        }
        let palette = self.chrome_palette(ChromeSurface::OverGrid(wid));
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid) as u32;
        let pad_top = self.win_pad_top(wid) as u32;
        let head = self.win_head(wid) as u32;
        let font_px = self.win_font_px(wid);
        let strip_dy = if tab_bar::STRIP_VERSION_SLOT {
            u32::from(self.tab_strip_rows) * ch as u32
        } else {
            0
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let fp = crate::build_badge::fingerprint(true);
        let geom_key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            cw.hash(&mut h);
            ch.hash(&mut h);
            font_px.to_bits().hash(&mut h);
            cols.hash(&mut h);
            pad.hash(&mut h);
            pad_top.hash(&mut h);
            head.hash(&mut h); // the dy anchor moves with the chrome headroom
            strip_dy.hash(&mut h); // …and with the strip's rows where it clears them
            palette.hash(&mut h);
            h.finish()
        };
        if ws
            .badge_card
            .as_ref()
            .is_none_or(|c| c.fp != fp || c.geom != geom_key)
        {
            let geom = crate::settings::SettingsGeom {
                cw: cw as f32,
                ch: ch as f32,
                font_px,
                cols,
                panel_rows: 0, // unused: the badge self-positions at the top-right
            };
            let mut tray = crate::build_badge::badge_tray(&geom, palette);
            let tray_w = (cols * cw) as f32;
            let (card_x, card_y, card_w, card_h) = tray.card;
            const PAINT_MARGIN: f32 = 2.0;
            let x0 = (card_x - PAINT_MARGIN).max(0.0).floor();
            let y0 = (card_y - PAINT_MARGIN).max(0.0).floor();
            let x1 = (card_x + card_w + PAINT_MARGIN).min(tray_w).ceil();
            let y1 = (card_y + card_h + PAINT_MARGIN).ceil();
            if x1 <= x0 || y1 <= y0 {
                ws.badge_card = None;
                return;
            }
            crate::widget::translate_prims(&mut tray.prims, -x0, -y0);
            let (rgba, pw, ph) = crate::tray_raster::rasterize_tray(
                &tray.prims,
                (x1 - x0) as u32,
                (y1 - y0) as u32,
                1.0,
                [0, 0, 0, 0],
            );
            ws.badge_card = Some(crate::SettingsCard {
                scrim: None,
                rgba,
                pw,
                ph,
                dx: pad + x0 as u32,
                dy: pad_top + head + strip_dy + y0 as u32,
                fp,
                geom: geom_key,
                raster_fingerprint_once: std::sync::OnceLock::new(),
            });
        }
    }

    /// The questions standing over `wid`, for the repaint key.
    pub(crate) fn question_key(&self, wid: WindowId) -> (u64, u64) {
        (
            self.paste_banner
                .as_ref()
                .filter(|p| p.wid == wid)
                .map_or(0, crate::paste_banner::PendingPaste::serial),
            self.close_banner
                .as_ref()
                .filter(|p| p.wid == wid)
                .map_or(0, crate::close_confirm::PendingClose::serial),
        )
    }

    /// Device-pixel floor below the top-row banner band (this window's multi-line-paste confirmation, or the close/quit confirmation that shares its slot on Windows, [`crate::close_confirm`]).
    pub(crate) fn paste_banner_tray_floor_y(&self, wid: WindowId) -> u32 {
        let Some(window) = self.windows.get(&wid) else {
            return 0;
        };
        let cols = window.cols as usize;
        let paste_rows = self
            .paste_banner
            .as_ref()
            .filter(|p| p.wid == wid)
            .map_or(0, |p| p.wanted_rows(cols));
        let close_rows = self
            .close_banner
            .as_ref()
            .filter(|p| p.wid == wid)
            .map_or(0, |p| p.wanted_rows(cols));
        let rows = paste_rows
            .max(close_rows)
            .min(window.input_scratch.cells.len());
        if rows == 0 {
            return 0;
        }
        let (_, cell_h) = self.win_cell_size(wid);
        u32::try_from(
            self.win_pad_top(wid)
                .saturating_add(self.win_head(wid))
                .saturating_add(rows.saturating_mul(cell_h)),
        )
        .unwrap_or(u32::MAX)
    }

    #[cfg(test)]
    pub(crate) fn splice_link_target_from_plan(
        &mut self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) {
        let _ = self.compose_link_caption(wid, Some(plan));
    }

    #[cfg(test)]
    pub(crate) fn splice_link_target(&mut self, wid: WindowId) {
        let _ = self.compose_link_caption(wid, None);
    }

    /// Compose the band and return its caption to the owning redraw.
    pub(crate) fn compose_link_caption(
        &mut self,
        wid: WindowId,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> Option<crate::app_mouse::ShownCaption> {
        let Some(hover) = self.windows.get(&wid).and_then(|ws| {
            ws.link_hover
                .filter(|_| ws.overlay.is_none() && ws.tab_menu.is_none())
        }) else {
            // Distinct branch.
            return None; // the ordinary frame: no hover, no band, no cost
        };
        if self.chrome_owns_terminal_row(wid, usize::from(hover.window_row)) {
            return None;
        }
        if !self
            .front_terminal(wid)
            .is_some_and(|front| front.session == hover.session)
        {
            return None;
        }
        let Some((url, path_link, names)) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.hover_memo.as_ref())
            .and_then(|memo| {
                let names = memo.caption_names(&hover)?;
                memo.url_for(&hover)
                    .cloned()
                    .map(|url| (url, memo.path_link_for(&hover), names))
            })
        else {
            // Distinct branch.
            return None; // whatever was there, it is not a link any more
        };
        let owned_plan = frame_plan
            .is_none()
            .then(|| self.active_visible_leaf_plan(wid))
            .flatten();
        let plan = frame_plan.or(owned_plan.as_ref())?;
        let (frame_row, seam) = self.link_caption_row(wid, hover, plan)?;
        let cols = self.windows.get(&wid).map(|ws| usize::from(ws.cols))?;
        let palette = self.chrome_palette(ChromeSurface::OverGrid(wid));
        let tracking = self
            .pool
            .get(hover.session)
            .is_some_and(|owner| owner.ctx.modes.mouse_tracking_enabled());
        let built = if path_link {
            crate::link_target::path_caption_row(&url, cols, palette, seam, tracking)
        } else {
            crate::link_target::caption_row(&url, cols, palette, seam, tracking)
        };
        let Some(built) = built else {
            // Distinct branch.
            return None; // too narrow to name a site; the row stays the person's
        };
        let cell_h = self.win_cell_size(wid).1;
        self.splice_band_rows_at(wid, frame_row, vec![built], cols, cell_h);
        self.windows
            .get(&wid)
            .filter(|ws| frame_row < ws.input_scratch.cells.len())
            .map(|_| crate::app_mouse::ShownCaption {
                session: hover.session,
                cell: hover.cell,
                names,
            })
    }

    fn link_caption_row(
        &self,
        wid: WindowId,
        hover: crate::link_target::LinkHover,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> Option<(usize, bool)> {
        let carets = self.visible_pane_caret_rows(plan);
        let ws = self.windows.get(&wid)?;
        let term_rows = usize::from(ws.rows);
        let nrows = ws.input_scratch.cells.len();
        let bottom = term_rows.checked_sub(1)?;
        let strip = nrows.checked_sub(term_rows)?;
        if ws.cols == 0 || nrows == 0 {
            return None;
        }
        let caret = ws
            .input_scratch
            .cursor_visible
            .then_some(ws.input_scratch.cursor_row);
        let free = |row: &usize| {
            usize::from(hover.window_row) != *row
                && caret != Some(strip + row)
                && !carets.contains(row)
                && !chrome_owns_terminal_row_of(ws, *row)
        };
        let term_row = [bottom, 0].into_iter().find(free)?;
        Some((strip + term_row, term_row == bottom))
    }

    fn visible_pane_caret_rows(&self, plan: &crate::tab_model::VisibleLeafPlan) -> Vec<usize> {
        let mut rows = Vec::with_capacity(plan.leaves.len());
        for leaf in &plan.leaves {
            // Distinct branch.
            if leaf.focused {
                // Distinct branch.
                continue; // the frame's own `cursor_row`/`cursor_visible` answer for it
            }
            let Some(session) = self
                .view_store
                .get(leaf.view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
            else {
                // Distinct branch.
                continue; // a native leaf owns its own caret and its own rows
            };
            let Some((term, ui_waiting)) = self
                .pool
                .get(session)
                .map(|entry| (entry.term.clone(), entry.ctx.ui_waiting.clone()))
            else {
                continue;
            };
            let pane_rows = (leaf.rect.size.height.round().max(1.0)) as usize;
            let row_off = leaf.rect.origin.y.round().max(0.0) as usize;
            if let Some(caret) =
                crate::term_lock_ui(&term, &ui_waiting, crate::metrics::TermWaitSite::Redraw)
                    .cursor_row_on_screen(pane_rows)
            {
                rows.push(row_off + caret);
            }
        }
        rows
    }

    /// Whether window chrome took terminal row `row` in the frame being composed (i.e. the pointer on that row is over a band, not the grid).
    pub(crate) fn chrome_owns_terminal_row(&self, wid: WindowId, row: usize) -> bool {
        self.windows
            .get(&wid)
            .is_some_and(|ws| chrome_owns_terminal_row_of(ws, row))
    }

    fn splice_band_rows(
        &mut self,
        wid: WindowId,
        built: Vec<Vec<aterm_core::terminal::RenderCell>>,
        cols: usize,
        panel_rows: usize,
        cell_h: usize,
    ) {
        debug_assert_eq!(built.len(), panel_rows);
        self.splice_band_rows_at(wid, 0, built, cols, cell_h);
    }

    fn splice_band_rows_at(
        &mut self,
        wid: WindowId,
        start: usize,
        built: Vec<Vec<aterm_core::terminal::RenderCell>>,
        cols: usize,
        cell_h: usize,
    ) {
        let panel_rows = built.len();
        let bands = aterm_render::RowBands::new(self.win_pad_top(wid) + self.win_head(wid), cell_h);
        if let Some(ws) = self.windows.get_mut(&wid) {
            for (offset, row) in built.into_iter().enumerate() {
                let r = start + offset;
                if r >= ws.input_scratch.cells.len() {
                    break;
                }
                debug_assert_eq!(row.len(), cols);
                ws.input_scratch.cells[r] = row;
                if let Some(c) = ws.input_scratch.clusters.get_mut(r) {
                    c.clear();
                }
                if let Some(c) = ws.input_scratch.combining.get_mut(r) {
                    c.clear();
                }
                if let Some(m) = ws.input_scratch.visual_to_logical.get_mut(r) {
                    m.clear();
                }
                if let Some(im) = ws.input_scratch.images.get_mut(r) {
                    im.clear();
                }
                if let Some(ls) = ws.input_scratch.line_sizes.get_mut(r) {
                    *ls = aterm_core::grid::LineSize::SingleWidth;
                }
                if let Some(spans) = ws.input_scratch.line_size_spans.get_mut(r) {
                    spans.clear();
                }
                if let Some(spans) = ws.input_scratch.default_bg_spans.get_mut(r) {
                    spans.clear();
                }
            }
            let band = start..(start + panel_rows).min(ws.input_scratch.cells.len());
            claim_chrome_rows(ws, band.clone());
            scrub_overlay_row_band(&mut ws.input_scratch, band.clone(), bands);
            if ws.input_scratch.grid_top_row < ws.input_scratch.grid_bot_row {
                if band.start > ws.input_scratch.grid_top_row
                    && band.end >= ws.input_scratch.grid_bot_row
                {
                    ws.input_scratch.grid_bot_row = band.start;
                } else {
                    ws.input_scratch.grid_top_row = ws
                        .input_scratch
                        .grid_top_row
                        .max(band.end)
                        .min(ws.input_scratch.grid_bot_row);
                }
            }
            ws.input_scratch.snapshot_seq = ws.input_scratch.snapshot_seq.wrapping_add(1);
        }
        let covered = start..start + panel_rows;
        if let Some(mut bleed) = self.backend.chrome_bleed() {
            let before = bleed.row_edges;
            for slot in &mut bleed.row_edges {
                if slot.is_some_and(|e| covered.contains(&e.row)) {
                    *slot = None;
                }
            }
            if bleed.row_edges != before {
                self.backend.set_chrome_bleed(Some(bleed));
            }
        }
    }

    /// Paint the multi-line-paste confirmation banner (`self.paste_banner`) by overwriting the top rows in place, over the message band's rows for the duration of the question.
    pub(crate) fn splice_paste_banner(&mut self, wid: WindowId) {
        let (panel_rows, cols) = {
            let Some(pending) = self.paste_banner.as_ref().filter(|p| p.wid == wid) else {
                self.splice_close_banner(wid);
                return;
            };
            let (avail, cols) = match self.windows.get(&wid) {
                Some(ws) => (ws.input_scratch.cells.len(), ws.cols as usize),
                None => return,
            };
            (pending.wanted_rows(cols).min(avail), cols)
        };
        if panel_rows == 0 {
            return;
        }
        let cell_h = self.win_cell_size(wid).1;
        let palette = self.chrome_palette(ChromeSurface::OverGrid(wid));
        let built = {
            let Some(pending) = self.paste_banner.as_ref().filter(|p| p.wid == wid) else {
                return;
            };
            crate::paste_banner::banner_rows(pending.text(), cols, panel_rows, palette)
        };
        self.splice_band_rows(wid, built, cols, panel_rows, cell_h);
    }

    fn splice_close_banner(&mut self, wid: WindowId) {
        let (panel_rows, cols) = {
            let Some(pending) = self.close_banner.as_ref().filter(|p| p.wid == wid) else {
                // Distinct branch.
                return; // no confirmation here -> byte-identical frame
            };
            let (avail, cols) = match self.windows.get(&wid) {
                Some(ws) => (ws.input_scratch.cells.len(), ws.cols as usize),
                None => return,
            };
            (pending.wanted_rows(cols).min(avail), cols)
        };
        if panel_rows == 0 {
            return;
        }
        let cell_h = self.win_cell_size(wid).1;
        let palette = self.chrome_palette(ChromeSurface::OverGrid(wid));
        let built = {
            let Some(pending) = self.close_banner.as_ref().filter(|p| p.wid == wid) else {
                return;
            };
            crate::close_confirm::banner_rows(&pending.prompt, cols, panel_rows, palette)
        };
        self.splice_band_rows(wid, built, cols, panel_rows, cell_h);
    }

    /// Paint the open tab context menu over the finished frame.
    pub(crate) fn splice_tab_menu(&mut self, wid: WindowId) {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if ws.tab_menu.is_none() {
            ws.tab_menu_rect = None;
            return;
        }
        let strip = usize::from(self.chrome_rows(wid));
        let palette = self.chrome_palette(ChromeSurface::OverGrid(wid));
        let theme = palette.theme;
        let ((cw, ch), pad) = (self.win_cell_size(wid), self.win_pad(wid));
        let grid_top = self.win_pad_top(wid) + self.win_head(wid);
        let laid = self.glass_grid(wid);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let Some(menu) = ws.tab_menu.as_ref() else {
            return;
        };
        let frame_rows = ws.input_scratch.cells.len();
        let cols = ws.input_scratch.cols;
        let Some(rect) =
            crate::tab_menu::place(&menu.entries, menu.anchor_col, cols, frame_rows, strip)
        else {
            self.close_tab_menu(wid);
            return;
        };
        let rows = crate::tab_menu::paint(rect, &menu.entries, menu.highlight, palette);
        let ground = crate::tab_menu::ground_cell(theme, ws.input_scratch.default_bg);
        let right_edge = rect.col + rect.w;
        for (r, painted) in rows.into_iter().enumerate() {
            let frame_row = rect.row + r;
            let Some(cells) = ws.input_scratch.cells.get_mut(frame_row) else {
                break;
            };
            if cells.len() < right_edge {
                cells.resize(right_edge, ground);
            }
            for (c, cell) in painted.into_iter().enumerate() {
                let Some(slot) = cells.get_mut(rect.col + c) else {
                    break;
                };
                *slot = cell;
            }
            let span = rect.col..(rect.col + rect.w);
            if let Some(cl) = ws.input_scratch.clusters.get_mut(frame_row) {
                cl.retain(|(col, _)| !span.contains(col));
            }
            if let Some(cb) = ws.input_scratch.combining.get_mut(frame_row) {
                cb.retain(|(col, _)| !span.contains(col));
            }
            if let Some(im) = ws.input_scratch.images.get_mut(frame_row) {
                im.retain(|(col, _)| !span.contains(col));
            }
        }
        crate::claude_card::cut_out(&mut ws.claude_card, &mut ws.input_scratch, rect, cw, pad);
        scrub_overlay_card(&mut ws.input_scratch, rect, (cw, ch), (pad, grid_top));
        ws.tab_menu_rect = Some(rect);
        ws.tab_menu_laid = laid;
        if ws.input_scratch.grid_top_row < ws.input_scratch.grid_bot_row {
            ws.input_scratch.grid_top_row = ws
                .input_scratch
                .grid_top_row
                .max(rect.row + rect.h)
                .min(ws.input_scratch.grid_bot_row);
        }
        ws.input_scratch.snapshot_seq = ws.input_scratch.snapshot_seq.wrapping_add(1);
    }

    /// Apply a `(rows, cols)` grid resize to the engine, PTY and GPU swapchain (the CPU softbuffer resizes itself in `redraw`).
    #[track_caller]
    pub(crate) fn apply_term_resize(&mut self, wid: WindowId, rows: u16, cols: u16) -> bool {
        let _site = crate::resize_ledger::SiteScope::enter(crate::resize_ledger::Cause::Term);
        self.apply_term_resize_inner(wid, rows, cols)
    }

    fn apply_term_resize_inner(&mut self, wid: WindowId, rows: u16, cols: u16) -> bool {
        self.sync_cell_pixel_size(wid);
        self.sync_gpu_surface_size(wid);
        if Some((rows, cols)) == self.windows.get(&wid).map(|ws| (ws.rows, ws.cols)) {
            return false;
        }
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.fx.cursor_mut().glow.curtain(std::time::Instant::now());
            ws.fx.cursor_mut().trail.reset();
            ws.rows = rows;
            ws.cols = cols;
            ws.note_normal_grid();
            ws.predictor.reset();
            ws.pending_resize = None;
            ws.next_resize_settle = None;
        }
        crate::metrics::note_grid_committed();
        self.resize_panes(wid);
        if let Some(scroll_state) = self
            .focused_session_id(wid)
            .and_then(|id| self.session_by_id(id))
            .map(|s| term_lock(&s.term).content_scroll_state())
            && let Some(ws) = self.windows.get_mut(&wid)
        {
            ws.fx.set_scroll_baseline(Some(scroll_state));
        }
        true
    }

    fn sync_gpu_surface_size(&mut self, wid: WindowId) {
        let strip = if self.windows.contains_key(&wid) {
            usize::from(self.chrome_rows(wid))
        } else {
            0
        };
        let App {
            backend, windows, ..
        } = self;
        if let Some(ws) = windows.get_mut(&wid)
            && let (
                Some(gpu),
                Some(PresentTarget::Gpu {
                    gpu_surface,
                    window_gpu,
                }),
            ) = (backend.gpu_mut(), ws.present.as_mut())
        {
            let (w_px, h_px) = match ws.win_px {
                Some(size) => (size.width.max(1), size.height.max(1)),
                None => {
                    let win_rows = ws.rows as usize + strip;
                    let (w, h) = gpu.frame_size(win_rows, ws.cols as usize);
                    (w as u32, h as u32)
                }
            };
            gpu.resize_surface(window_gpu, gpu_surface, w_px, h_px);
        }
    }

    /// Device-pixel size of window `wid`'s full composed frame with no glass, using the same fallback math as [`Self::sync_gpu_surface_size`]'s `win_px == None` arm (the offscreen `encode_frame` produces).
    pub(crate) fn headless_frame_px(&mut self, wid: WindowId) -> Option<(u32, u32)> {
        let strip = if self.windows.contains_key(&wid) {
            usize::from(self.chrome_rows(wid))
        } else {
            0
        };
        let (win_rows, cols) = {
            let ws = self.windows.get(&wid)?;
            (ws.rows as usize + strip, ws.cols as usize)
        };
        self.backend.gpu_mut()?;
        let (w, h) = self.backend.frame_size(win_rows, cols);
        Some((w as u32, h as u32))
    }

    /// Reconfigure this window's GPU swapchain to the raw window pixels (`win_px`), called from `WindowEvent::Resized` before the throttled grid reflow so the surface never lags the window.
    pub(crate) fn sync_surface_to_window(&mut self, wid: WindowId) {
        let App {
            backend, windows, ..
        } = self;
        if let Some(ws) = windows.get_mut(&wid)
            && let Some(size) = ws.win_px
            && let (
                Some(gpu),
                Some(PresentTarget::Gpu {
                    gpu_surface,
                    window_gpu,
                }),
            ) = (backend.gpu_mut(), ws.present.as_mut())
        {
            gpu.resize_surface(
                window_gpu,
                gpu_surface,
                size.width.max(1),
                size.height.max(1),
            );
        }
    }

    /// Apply a control-socket `resize` (an `InputEvent::Resize { echo_to_window: true }`).
    pub(crate) fn apply_grid_resize(&mut self, rows: u16, cols: u16) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        let changed = self.apply_term_resize(wid, rows, cols);
        if !changed {
            return;
        }
        let size = self.window_frame_px_for(wid, rows, cols);
        #[cfg(target_os = "linux")]
        if let Some(ws) = self.front_mut() {
            ws.initial_frame_settle = None;
        }
        if let Some(w) = self.front().and_then(|ws| ws.os_window.clone()) {
            // Best effort; the WM may clamp. `Some` means the backend applied
            // the size synchronously and no `Resized` follows (Wayland).
            if let Some(applied) = w.request_inner_size(size) {
                if let Some(ws) = self.front_mut() {
                    ws.win_px = Some(applied);
                }
                self.sync_surface_to_window(wid);
            }
            w.request_redraw();
        }
    }
}

fn preview_damage_from_compiled(
    compiled: &crate::native_ui::CompiledUi,
    ticks: &dyn Fn(&crate::settings_preview::SettingsPreviewSpec) -> bool,
) -> Option<crate::native_app::DamageRegion> {
    let mut union: Option<crate::native_ui::LogicalRect> = None;
    for node in &compiled.paint {
        let crate::native_ui::UiContent::SettingsPreview(spec) = &node.content else {
            continue;
        };
        if !ticks(spec) {
            continue;
        }
        let rect = spec
            .tick_rect(node.rect)
            .intersect(node.clip)?
            .intersect(compiled.bounds)?;
        union = Some(match union {
            Some(existing) => {
                let x = existing.x.min(rect.x);
                let y = existing.y.min(rect.y);
                let right = existing.right().max(rect.right());
                let bottom = existing.bottom().max(rect.bottom());
                crate::native_ui::LogicalRect::new(x, y, right - x, bottom - y)
            }
            None => rect,
        });
    }
    let rect = union?;
    let x = rect.x.floor().max(0.0) as u32;
    let y = rect.y.floor().max(0.0) as u32;
    let right = rect.right().ceil().max(0.0) as u32;
    let bottom = rect.bottom().ceil().max(0.0) as u32;
    (right > x && bottom > y).then_some(crate::native_app::DamageRegion::Rect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    })
}

/// Per-step input-line budget for the native `aterm-reflow` worker's chunked rewrap ([`drive_reflow_job`]).
pub(crate) const REFLOW_WORKER_STEP_LINES: usize = 50_000;

/// Live gauge of the off-thread scrollback rewrap.
pub(crate) mod reflow_gauge {
    use std::sync::atomic::{AtomicU64, Ordering};

    static RUNNING: AtomicU64 = AtomicU64::new(0);
    static PEAK: AtomicU64 = AtomicU64::new(0);
    static SUBMITTED: AtomicU64 = AtomicU64::new(0);
    static FINISHED: AtomicU64 = AtomicU64::new(0);
    static THREADS: AtomicU64 = AtomicU64::new(0);

    #[cfg(test)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct Snapshot {
        /// Workers driving a job at this instant.
        pub(crate) running: u64,
        /// High-water mark of `running` since process start.
        pub(crate) peak: u64,
        /// Jobs handed off (main thread), since process start.
        pub(crate) submitted: u64,
        /// Jobs completed — re-attached, aborted, or cancelled.
        pub(crate) finished: u64,
        /// OS threads the hand-off site has created since process start, capped by [`reflow_worker_ceiling`] however many panes a settle touches.
        pub(crate) threads: u64,
    }

    /// Record a hand-off.
    pub(crate) fn submit() {
        SUBMITTED.fetch_add(1, Ordering::Relaxed);
    }

    /// Record that an OS thread was successfully created to run reflow work.
    pub(crate) fn note_thread_spawned() {
        THREADS.fetch_add(1, Ordering::Relaxed);
    }

    /// A worker is about to drive a job.
    #[must_use = "the returned guard's Drop is what lowers the live count"]
    pub(crate) fn enter() -> Running {
        let live = RUNNING.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        PEAK.fetch_max(live, Ordering::Relaxed);
        Running { _seal: () }
    }

    /// The live-count guard minted by [`enter`].
    pub(crate) struct Running {
        _seal: (),
    }

    impl Drop for Running {
        fn drop(&mut self) {
            RUNNING.fetch_sub(1, Ordering::Relaxed);
            FINISHED.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The spawn-failure fallback rewrapped a job inline on the main thread.
    pub(crate) fn note_inline() {
        FINISHED.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn snapshot() -> Snapshot {
        Snapshot {
            running: RUNNING.load(Ordering::Relaxed),
            peak: PEAK.load(Ordering::Relaxed),
            submitted: SUBMITTED.load(Ordering::Relaxed),
            finished: FINISHED.load(Ordering::Relaxed),
            threads: THREADS.load(Ordering::Relaxed),
        }
    }
}

/// The bound on concurrent off-thread scrollback rewraps, process-wide.
pub(crate) fn reflow_worker_ceiling() -> usize {
    static CEILING: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CEILING.get_or_init(|| {
        reflow_worker_ceiling_for(
            physical_cpu_count(),
            std::thread::available_parallelism()
                .ok()
                .map(std::num::NonZeroUsize::get),
        )
    })
}

const REFLOW_WORKER_MAX: usize = 8;

fn reflow_worker_ceiling_for(physical: Option<usize>, logical: Option<usize>) -> usize {
    let logical = logical.unwrap_or(REFLOW_WORKER_MAX);
    physical
        .filter(|&cores| cores > 0)
        .map_or(logical, |cores| cores.min(logical))
        .clamp(1, REFLOW_WORKER_MAX)
}

#[cfg(target_os = "macos")]
fn physical_cpu_count() -> Option<usize> {
    let mut cores: libc::c_int = 0;
    let mut len: libc::size_t = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is a NUL-terminated literal.
    let rc = unsafe {
        libc::sysctlbyname(
            c"hw.physicalcpu".as_ptr(),
            (&mut cores as *mut libc::c_int).cast::<libc::c_void>(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len != std::mem::size_of::<libc::c_int>() {
        return None;
    }
    usize::try_from(cores).ok().filter(|&cores| cores > 0)
}

#[cfg(not(target_os = "macos"))]
fn physical_cpu_count() -> Option<usize> {
    None
}

#[cfg(test)]
mod reflow_ceiling_tests {
    use super::{REFLOW_WORKER_MAX, reflow_worker_ceiling_for};

    #[test]
    fn the_ceiling_is_bounded_by_physical_cores_not_hyperthreads() {
        for (physical, logical, want) in [
            (Some(4), Some(8), 4),
            (Some(12), Some(12), REFLOW_WORKER_MAX),
            (Some(8), Some(2), 2),
            (None, Some(6), 6),
            (None, None, REFLOW_WORKER_MAX),
            (Some(0), Some(8), 8),
        ] {
            assert_eq!(
                reflow_worker_ceiling_for(physical, logical),
                want,
                "physical {physical:?}, logical {logical:?}"
            );
        }
    }
}

struct ReflowHandoff {
    run: Box<dyn FnOnce() + Send + 'static>,
    inline: Box<dyn FnOnce() + Send + 'static>,
    priority: bool,
}

struct ReflowPoolState {
    queue: std::collections::VecDeque<ReflowHandoff>,
    front: usize,
    workers: usize,
}

static REFLOW_POOL: std::sync::Mutex<ReflowPoolState> = std::sync::Mutex::new(ReflowPoolState {
    queue: std::collections::VecDeque::new(),
    front: 0,
    workers: 0,
});

static REFLOW_WORK: std::sync::Condvar = std::sync::Condvar::new();

const REFLOW_WORKER_LINGER: Duration = Duration::from_millis(50);

fn reflow_pool_submit(handoff: ReflowHandoff) -> Vec<ReflowHandoff> {
    let ceiling = reflow_worker_ceiling();
    let mut state = REFLOW_POOL.lock().unwrap_or_else(|p| p.into_inner());
    if handoff.priority {
        let at = state.front;
        state.queue.insert(at, handoff);
        state.front += 1;
    } else {
        state.queue.push_back(handoff);
    }
    REFLOW_WORK.notify_one();
    if state.workers >= ceiling || state.queue.len() <= state.workers {
        return Vec::new();
    }
    state.workers += 1;
    drop(state); // never spawn under the pool lock
    if crate::qos::spawn_lane_thread(
        "aterm-reflow",
        crate::qos::Role::Responsive,
        reflow_worker_main,
    )
    .is_ok()
    {
        reflow_gauge::note_thread_spawned();
        return Vec::new();
    }
    let mut state = REFLOW_POOL.lock().unwrap_or_else(|p| p.into_inner());
    state.workers = state.workers.saturating_sub(1);
    if state.workers == 0 {
        aterm_log::error!(
            "reflow worker spawn failed with no live worker; rewrapping {} queued \
             job(s) inline (bounded)",
            state.queue.len()
        );
        state.front = 0;
        state.queue.drain(..).collect()
    } else {
        Vec::new()
    }
}

struct ReflowWorkerSeat {
    seated: bool,
}

impl Drop for ReflowWorkerSeat {
    fn drop(&mut self) {
        if !self.seated {
            return;
        }
        let _ = std::panic::catch_unwind(release_reflow_seat_and_replace);
    }
}

fn release_reflow_seat_and_replace() {
    let mut state = REFLOW_POOL.lock().unwrap_or_else(|p| p.into_inner());
    state.workers = state.workers.saturating_sub(1);
    if state.queue.is_empty() || state.workers > 0 {
        return;
    }
    state.workers += 1;
    drop(state);
    if crate::qos::spawn_lane_thread(
        "aterm-reflow",
        crate::qos::Role::Responsive,
        reflow_worker_main,
    )
    .is_ok()
    {
        reflow_gauge::note_thread_spawned();
        return;
    }
    let mut state = REFLOW_POOL.lock().unwrap_or_else(|p| p.into_inner());
    state.workers = state.workers.saturating_sub(1);
    aterm_log::error!("reflow worker died and no replacement could be started");
}

fn reflow_worker_main() {
    let mut seat = ReflowWorkerSeat { seated: true };
    loop {
        let handoff = {
            let mut state = REFLOW_POOL.lock().unwrap_or_else(|p| p.into_inner());
            let deadline = Instant::now() + REFLOW_WORKER_LINGER;
            loop {
                if let Some(handoff) = state.queue.pop_front() {
                    state.front = state.front.saturating_sub(1);
                    break Some(handoff);
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    state.workers = state.workers.saturating_sub(1);
                    seat.seated = false;
                    break None;
                }
                let (next, _) = REFLOW_WORK
                    .wait_timeout(state, left)
                    .unwrap_or_else(|p| p.into_inner());
                state = next;
            }
        };
        let Some(handoff) = handoff else {
            return;
        };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(handoff.run));
    }
}
#[cfg(test)]
mod reflow_pool_tests {
    use super::{ReflowHandoff, reflow_pool_submit, reflow_worker_ceiling};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    #[cfg(target_os = "macos")]
    #[test]
    fn a_pooled_rewrap_runs_at_user_initiated_not_the_inherited_band() {
        // SAFETY: `qos_class_self` is libpthread's, part of `libSystem`, linked into every process on this platform. It reads only the calling thread's own requested class and takes no arguments.
        unsafe extern "C" {
            fn qos_class_self() -> u32;
        }
        const USER_INITIATED_RAW: u32 = 0x19;

        let seen = Arc::new((Mutex::new(None::<u32>), Condvar::new()));
        let posted = Arc::clone(&seen);
        let stranded = reflow_pool_submit(ReflowHandoff {
            run: Box::new(move || {
                // SAFETY: see the extern above.
                let class = unsafe { qos_class_self() };
                let (lock, cv) = &*posted;
                *lock.lock().unwrap_or_else(|p| p.into_inner()) = Some(class);
                cv.notify_all();
            }),
            inline: Box::new(|| {}),
            priority: true,
        });
        if !stranded.is_empty() {
            aterm_spec::host::could_not_run("no rewrap worker could start here (threads or fds)");
        }

        let (lock, cv) = &*seen;
        let mut class = lock.lock().unwrap_or_else(|p| p.into_inner());
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while class.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "no pooled worker drove the job");
            let (next, _) = cv
                .wait_timeout(class, left)
                .unwrap_or_else(|p| p.into_inner());
            class = next;
        }
        let class = class.expect("the job posted its class");
        assert_eq!(
            class, USER_INITIATED_RAW,
            "an aterm-reflow worker ran at QoS class {class:#x}, not USER_INITIATED \
             ({USER_INITIATED_RAW:#x}) — a rewrap the human is waiting on, holding \
             the pool lock, below the class qos.rs gives it"
        );
    }

    #[test]
    fn the_pool_runs_every_job_once_and_never_exceeds_its_ceiling() {
        const JOBS: usize = 64;
        let ceiling = reflow_worker_ceiling();
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let ran = Arc::new(AtomicUsize::new(0));
        let stranded_inline = Arc::new(AtomicUsize::new(0));
        let done = Arc::new((Mutex::new(0usize), Condvar::new()));

        for _ in 0..JOBS {
            let (live, peak, ran, done) =
                (live.clone(), peak.clone(), ran.clone(), Arc::clone(&done));
            let inline_seen = stranded_inline.clone();
            let run = move || {
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(2));
                live.fetch_sub(1, Ordering::SeqCst);
                ran.fetch_add(1, Ordering::SeqCst);
                let (lock, cv) = &*done;
                *lock.lock().unwrap_or_else(|p| p.into_inner()) += 1;
                cv.notify_all();
            };
            let inline = move || {
                inline_seen.fetch_add(1, Ordering::SeqCst);
            };
            let stranded = reflow_pool_submit(ReflowHandoff {
                run: Box::new(run),
                inline: Box::new(inline),
                priority: false,
            });
            for job in stranded {
                (job.inline)();
            }
        }

        let (lock, cv) = &*done;
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let mut seen = lock.lock().unwrap_or_else(|p| p.into_inner());
        while *seen + stranded_inline.load(Ordering::SeqCst) < JOBS {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "the pool left {} of {JOBS} jobs undriven — a dropped job wedges \
                 that session's tiered history forever",
                JOBS - *seen
            );
            let (next, _) = cv
                .wait_timeout(seen, Duration::from_millis(50))
                .unwrap_or_else(|p| p.into_inner());
            seen = next;
        }
        drop(seen);

        assert_eq!(
            ran.load(Ordering::SeqCst) + stranded_inline.load(Ordering::SeqCst),
            JOBS,
            "every submitted job must be driven exactly once"
        );
        let observed = peak.load(Ordering::SeqCst);
        assert!(
            observed <= ceiling,
            "{observed} jobs were in flight at once, above the pool ceiling \
             {ceiling} — the bound MPT-1 exists to impose is not being imposed"
        );
        assert!(
            observed >= 1 || stranded_inline.load(Ordering::SeqCst) == JOBS,
            "no job was ever observed in flight and nothing fell back to the \
             inline arm, so the ceiling assertion above proved nothing"
        );
    }
}

#[cfg(test)]
pub(crate) fn drive_reflow_job(
    job: aterm_core::grid::PendingScrollbackReflow,
    cancel: &std::sync::atomic::AtomicBool,
    step_budget: usize,
) -> Option<aterm_core::grid::ReflowedScrollback> {
    drive_reflow_job_gauged(job, cancel, step_budget, None)
}

/// [`drive_reflow_job`], storing the job's progress into `gauge` after each step (one relaxed store per step, read only while a person waits).
pub(crate) fn drive_reflow_job_gauged(
    mut job: aterm_core::grid::PendingScrollbackReflow,
    cancel: &std::sync::atomic::AtomicBool,
    step_budget: usize,
    gauge: Option<&crate::session_waits::RewrapGauge>,
) -> Option<aterm_core::grid::ReflowedScrollback> {
    loop {
        // Distinct branch.
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            // Distinct branch.
            return None; // drops `job`: bounded loss, caller aborts the offload
        }
        match job.reflow_step(step_budget) {
            aterm_core::grid::ReflowStep::InProgress(next) => {
                if let Some(gauge) = gauge {
                    gauge.step(next.lines_done());
                }
                job = next;
            }
            aterm_core::grid::ReflowStep::Done(reflowed) => return Some(reflowed),
        }
    }
}

#[cfg(test)]
mod reflow_worker_tests {
    use super::drive_reflow_job;
    use aterm_core::grid::ReflowStep;
    use aterm_core::terminal::Terminal;
    use std::sync::atomic::AtomicBool;

    fn term_with_history(rows: u16, cols: u16, lines: usize) -> Terminal {
        let sb = aterm_scrollback::Scrollback::new(64, 512, 8_000_000);
        let mut t = Terminal::with_scrollback(rows, cols, 8, sb);
        let fill = "x".repeat((cols as usize).saturating_sub(8));
        let mut buf = Vec::new();
        for i in 0..lines {
            buf.extend_from_slice(format!("L{i}-{fill}\r\n").as_bytes());
        }
        t.process(&buf);
        t
    }

    #[test]
    fn cancelled_mid_flight_job_is_dropped_at_the_next_step() {
        let mut t = term_with_history(24, 80, 800);
        let before = t.grid().scrollback_lines();
        assert!(before > 100, "precondition: deep tiered history ({before})");

        let job = t
            .resize_offloading_scrollback(24, 40)
            .expect("width change with tiered history detaches a job");
        let ReflowStep::InProgress(half) = job.reflow_step(5) else {
            panic!("an 800-line history cannot complete in one 5-line step");
        };
        let cancel = AtomicBool::new(true); // Session::drop raced in
        assert!(
            drive_reflow_job(half, &cancel, 5).is_none(),
            "the worker loop must observe the cancel at the next step boundary"
        );
    }
}

#[cfg(test)]
#[path = "app_render_strip_title_lock_tests.rs"]
mod strip_title_lock_tests;

#[cfg(test)]
#[path = "app_render_overlay_band_tests.rs"]
mod overlay_band_tests;

#[cfg(test)]
#[path = "app_render_chrome_tokens_tests.rs"]
mod chrome_tokens_tests;

#[cfg(test)]
#[path = "app_render_tab_menu_splice_tests.rs"]
mod tab_menu_splice_tests;

#[cfg(test)]
#[path = "app_render_tab_strip_bleed_tests.rs"]
mod tab_strip_bleed_tests;

#[cfg(test)]
#[path = "app_render_strip_tabs_tests.rs"]
mod strip_tabs_tests;

#[cfg(test)]
#[path = "app_render_find_bar_splice_tests.rs"]
mod find_bar_splice_tests;

#[cfg(test)]
#[path = "app_render_ime_preedit_splice_tests.rs"]
mod ime_preedit_splice_tests;

#[cfg(test)]
#[path = "app_render_split_preedit_compose_tests.rs"]
mod split_preedit_compose_tests;

#[cfg(test)]
#[path = "app_render_split_sparkle_tests.rs"]
mod split_sparkle_tests;

#[cfg(test)]
#[path = "app_render_rain_host_tests.rs"]
mod rain_host_tests;

#[cfg(test)]
#[path = "app_render_message_band_visual_tests.rs"]
mod message_band_visual_tests;

#[cfg(test)]
#[path = "app_render_message_band_raster_tests.rs"]
mod message_band_raster_tests;

#[cfg(test)]
#[path = "app_render_tone_melody_seam_tests.rs"]
mod tone_melody_seam_tests;

#[cfg(test)]
#[path = "app_render_key_time_click_tests.rs"]
mod key_time_click_tests;

#[cfg(test)]
mod settled_compose_early_out_tests {

    use crate::{App, WindowId, term_lock};
    use std::time::{Duration, Instant};

    fn compose(app: &mut App, wid: WindowId, now: Instant) -> bool {
        app.redraw_compose(wid, 24, 80, false, false, None, 0, now)
            .is_some()
    }

    fn settle(app: &mut App, wid: WindowId, now: &mut Instant) {
        let mut settled = false;
        for _ in 0..40 {
            *now += Duration::from_millis(16);
            if !compose(app, wid, *now) {
                settled = true;
                break;
            }
        }
        assert!(settled, "the fixture must reach a settled (skipping) state");
        *now += Duration::from_millis(16);
        assert!(!compose(app, wid, *now), "settled stays settled");
    }

    #[test]
    fn recovery_edge_buys_one_present_not_a_train() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.windows.get_mut(&wid).expect("window").focused = true;
        let mut now = Instant::now();
        settle(&mut app, wid, &mut now);
        app.windows
            .get_mut(&wid)
            .expect("window")
            .present_retry
            .recovery_redraw_outstanding = true;
        now += Duration::from_millis(16);
        assert!(
            compose(&mut app, wid, now),
            "the delivered edge services one recomposed present"
        );
        assert!(
            !app.windows[&wid].present_retry.recovery_redraw_outstanding,
            "the composed commit acknowledged the delivered edge"
        );
        now += Duration::from_millis(16);
        assert!(
            !compose(&mut app, wid, now),
            "and the train ends: the settled compose takes the early-out"
        );
    }

    #[test]
    fn split_settles_and_every_real_change_still_repaints() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.config.cursor_trail = Some(false);
        app.config.cursor_trail_style = Some("off".into());
        app.config.output_streak = Some(crate::app_config::OutputStreakConfig {
            enabled: Some(false),
            ..Default::default()
        });
        app.kitty_cursor_enabled_cache = None;
        app.recompute_sparkle();
        let sid = app.split_active_stub_tab(wid);
        app.windows.get_mut(&wid).expect("window").focused = true;
        let mut now = Instant::now();
        settle(&mut app, wid, &mut now);

        let bg = app.pool.get(0).expect("background session").term.clone();
        term_lock(&bg).process(b"background write");
        now += Duration::from_millis(16);
        assert!(
            compose(&mut app, wid, now),
            "background PTY damage repaints"
        );
        settle(&mut app, wid, &mut now);

        let fg = app.pool.get(sid).expect("focused session").term.clone();
        term_lock(&fg).process(b"\x1b[C");
        now += Duration::from_millis(16);
        assert!(
            compose(&mut app, wid, now),
            "a damage-free cursor move repaints"
        );
        settle(&mut app, wid, &mut now);

        {
            let ws = app.windows.get_mut(&wid).expect("window");
            ws.blink_phase = !ws.blink_phase;
        }
        now += Duration::from_millis(16);
        assert!(compose(&mut app, wid, now), "a blink phase flip repaints");
        settle(&mut app, wid, &mut now);

        app.config.show_build_badge = Some(true);
        now += Duration::from_millis(16);
        assert!(compose(&mut app, wid, now), "a config change repaints");
        settle(&mut app, wid, &mut now);
    }
}

#[cfg(test)]
#[path = "app_render_strip_lane_oracle_tests.rs"]
mod strip_lane_oracle;

#[cfg(test)]
#[path = "app_render_cell_pixel_size_tests.rs"]
mod cell_pixel_size_tests;

#[cfg(test)]
#[path = "app_render_compose_focused_carrier_tests.rs"]
mod compose_focused_carrier;

#[cfg(test)]
#[path = "app_render_dmg1_full_arm_attribution_tests.rs"]
mod dmg1_full_arm_attribution;

#[cfg(test)]
#[path = "app_render_link_target_caption_tests.rs"]
mod link_target_caption_tests;

#[cfg(test)]
#[path = "app_render_fx_occluder_tests.rs"]
pub(crate) mod fx_occluder_tests;

#[cfg(test)]
#[path = "app_render_fx_pane_tests.rs"]
mod fx_pane_tests;

#[cfg(test)]
#[path = "app_render_trail_verdict_tests.rs"]
mod trail_verdict;

fn report_gpu_surface_present(
    result: Result<(), aterm_gpu::SurfacePresentFailure>,
    acquire_wait_ns: Option<u64>,
    record: impl FnOnce(u64),
) -> Result<Option<()>, metrics::PresentDropReason> {
    if let Some(wait_ns) = acquire_wait_ns {
        record(wait_ns);
    }
    match result {
        Ok(()) => Ok(Some(())),
        Err(aterm_gpu::SurfacePresentFailure::AcquirePending) => Ok(None),
        Err(aterm_gpu::SurfacePresentFailure::Reconfigured) => {
            Err(metrics::PresentDropReason::GpuReconfigured)
        }
        Err(aterm_gpu::SurfacePresentFailure::Timeout) => {
            Err(metrics::PresentDropReason::GpuTimeout)
        }
        Err(aterm_gpu::SurfacePresentFailure::Unavailable) => {
            Err(metrics::PresentDropReason::GpuUnavailable)
        }
        Err(aterm_gpu::SurfacePresentFailure::Occluded) => {
            Err(metrics::PresentDropReason::GpuOccluded)
        }
        Err(aterm_gpu::SurfacePresentFailure::Validation) => {
            Err(metrics::PresentDropReason::GpuValidation)
        }
    }
}

#[cfg(test)]
mod acquire_wait_publication_tests {
    use super::{metrics, report_gpu_surface_present};
    use aterm_gpu::SurfacePresentFailure;

    #[test]
    fn surface_results_route_and_publish_only_completed_waits() {
        use metrics::PresentDropReason as Reason;
        for (result, wait, want) in [
            (Err(SurfacePresentFailure::AcquirePending), None, Ok(None)),
            (Ok(()), Some(80_000), Ok(Some(()))),
            (
                Err(SurfacePresentFailure::Timeout),
                Some(542_000_000),
                Err(Reason::GpuTimeout),
            ),
            (
                Err(SurfacePresentFailure::Occluded),
                None,
                Err(Reason::GpuOccluded),
            ),
            (
                Err(SurfacePresentFailure::Unavailable),
                None,
                Err(Reason::GpuUnavailable),
            ),
            (
                Err(SurfacePresentFailure::Validation),
                None,
                Err(Reason::GpuValidation),
            ),
        ] {
            let mut samples = Vec::new();
            assert_eq!(
                report_gpu_surface_present(result, wait, |ns| samples.push(ns)),
                want,
                "{result:?}"
            );
            assert_eq!(samples, wait.into_iter().collect::<Vec<_>>(), "{result:?}");
        }

        let mut app = crate::App::headless_for_test();
        let id = crate::WindowId(0);
        for _ in 0..crate::PRESENT_RETRY_CAP {
            let before = crate::Instant::now();
            let reason =
                report_gpu_surface_present(Err(SurfacePresentFailure::Unavailable), None, |_| {
                    panic!("no completed acquire")
                })
                .expect_err("a temporarily unavailable surface");
            app.handle_failed_present(id, reason, before);
            let retry = &mut app.windows.get_mut(&id).expect("the window").present_retry;
            let due = retry.deadline.expect("another attempt without input");
            assert!(due > before, "the retry is strictly future");
            assert!(!retry.present_attempt_allowed());
            assert!(retry.take_due(due));
            assert!(retry.present_attempt_allowed());
        }
        app.handle_failed_present(id, Reason::GpuUnavailable, crate::Instant::now());
        let retry = &mut app.windows.get_mut(&id).expect("the window").present_retry;
        assert!(retry.parked, "exhaustion cannot spin");
        assert!(retry.deadline.is_none());
        assert!(retry.on_external_stimulus());
        app.handle_failed_present(id, Reason::GpuValidation, crate::Instant::now());
        let retry = &app.windows[&id].present_retry;
        assert!(retry.parked, "validation still waits for external input");
        assert!(retry.deadline.is_none());
    }
}

static STARTUP_RASTER_PROBE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

const STARTUP_RASTER_PROBE_PRESENTS: u32 = 600;

fn startup_raster_probe_armed() -> bool {
    STARTUP_RASTER_PROBE.load(std::sync::atomic::Ordering::Relaxed) < STARTUP_RASTER_PROBE_PRESENTS
}

fn note_startup_present_rasters(before: usize, after: usize) {
    use std::sync::atomic::Ordering::Relaxed;
    let nth = STARTUP_RASTER_PROBE.fetch_add(1, Relaxed).saturating_add(1);
    let grew = after.saturating_sub(before);
    if nth == 1 {
        aterm_log::debug!(
            "startup: first present rasterized {grew} glyphs on the UI thread (glyph cache \
             {before} -> {after}; the cache's size before it is the post-seal warm that \
             survived the join)"
        );
    }
    if grew > 0 {
        aterm_log::debug!(
            "startup: present #{nth} is the first to rasterize on the UI thread: {grew} glyphs \
             (glyph cache {before} -> {after})"
        );
        STARTUP_RASTER_PROBE.store(u32::MAX, Relaxed);
    }
}

#[cfg(test)]
#[path = "app_render_mixed_ime_tests.rs"]
mod mixed_ime_tests;

#[cfg(test)]
mod native_ime_anchor_tests {
    use super::*;
    use crate::native_app::{ActionInvocation, AppEvent};

    #[test]
    fn a_presented_native_field_replaces_the_terminal_ime_anchor_at_its_painted_caret() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
        app.dispatch_native_event(
            wid,
            AppEvent::Action(ActionInvocation {
                id: crate::native_ui::ActionId::new("settings/search"),
                value: None,
            }),
        )
        .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let stale = (3, 5, 7, 11);
        app.windows.get_mut(&wid).unwrap().last_ime_rect = Some(stale);
        let mut previous_x = None;
        for caret in [0, 3, 6] {
            app.on_ime_preedit(wid, "日本語".to_string(), Some((caret, caret)));
            let (plan, route) = app.active_visible_frame_layout(wid).unwrap();
            app.prepare_layout_coordinate_space_from_plan(wid, route, &plan);
            assert!(app.prepare_native_input_scratch_from_plan(wid, &plan));
            let before = app.windows[&wid].last_ime_rect;
            app.report_native_ime_cursor_area(wid, &plan);
            assert_eq!(app.windows[&wid].last_ime_rect, before);
            let stamp = app.windows[&wid]
                .native_ui_compiled
                .as_ref()
                .map(|frame| frame.stamp);
            app.first_present_done = true;
            app.finalize_successful_present(
                wid,
                crate::metrics::StartupPresentTiming::collapsed(Instant::now()),
                0,
                None,
                SuccessfulPresentRoute::Native {
                    view,
                    presented_stamp: stamp,
                },
                crate::VisibleContentRoute::Native { instance, view },
                HostVisualState::default(),
                &plan,
            );
            let (x, y, w, h) = app.windows[&wid].last_ime_rect.expect("native IME anchor");
            assert_ne!(
                (x, y, w, h),
                stale,
                "the parked terminal cannot own this field"
            );
            if let Some(previous_x) = previous_x {
                assert!(
                    x > previous_x,
                    "a middle IME caret moves the candidate anchor"
                );
            }
            previous_x = Some(x);

            let (artifact, lx, ly) = app
                .retained_native_leaf_at_pointer(wid, f64::from(x), f64::from(y))
                .expect("anchor lies on the retained native leaf");
            assert_eq!(artifact.view, view);
            let caret = artifact.compiled.ime_caret_rect().unwrap();
            assert!((lx - caret.x).abs() <= 1.0 / artifact.scale as f32);
            assert!((ly - caret.y).abs() <= 1.0 / artifact.scale as f32);
        }

        let plan = app.active_visible_leaf_plan(wid).unwrap();
        app.windows.get_mut(&wid).unwrap().win_px =
            Some(aterm_window::dpi::PhysicalSize::new(100, 100));
        let (caret, scale) = {
            let artifact = app
                .retained_native_leaf_artifact_from_plan(wid, view, true, &plan)
                .unwrap();
            (artifact.ime_caret.unwrap(), artifact.scale)
        };
        let (frame_x, frame_y) = app.frame_origin(wid);
        if cfg!(target_os = "linux") {
            assert_eq!(frame_y, 0, "Linux pins the frame to the window's top");
            return;
        }
        for (top, visible) in [
            (-1, true),
            (-(f64::from(caret.height) * scale).ceil() as i32 - 2, false),
        ] {
            let window = app.windows.get_mut(&wid).unwrap();
            let card = window.settings_card.as_mut().unwrap();
            let caret_x = frame_x + (f64::from(caret.x) * scale).floor() as i64;
            card.dx = u32::try_from(caret_x.max(20) - caret_x).unwrap();
            card.dy = u32::try_from(
                i64::from(top) - frame_y - (f64::from(caret.y) * scale).floor() as i64,
            )
            .unwrap();
            window.last_ime_rect = Some(stale);
            assert!(
                app.retained_native_leaf_artifact_from_plan(wid, view, true, &plan)
                    .is_some(),
                "the retained leaf still intersects the window"
            );
            app.report_native_ime_cursor_area(wid, &plan);
            if visible {
                let (x, y, w, h) = app.windows[&wid].last_ime_rect.unwrap();
                assert_eq!(y, 0);
                assert!(x >= 0 && x as u32 + w <= 100 && h <= 100);
                assert!((h as f64) < f64::from(caret.height) * scale + 1.0);
            } else {
                assert_eq!(
                    app.windows[&wid].last_ime_rect,
                    Some(stale),
                    "fully cropped caret cannot publish an offscreen anchor"
                );
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod redraw_hold_hook {
    use std::cell::RefCell;

    type Hook = Box<dyn FnMut()>;

    thread_local! {
        static HOOK: RefCell<Option<Hook>> = RefCell::new(None);
    }

    /// Run `hook` after every later terminal hold on this thread.
    pub(crate) fn install(hook: impl FnMut() + 'static) {
        HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    }

    pub(crate) fn clear() {
        HOOK.with(|h| *h.borrow_mut() = None);
    }

    pub(crate) fn after_terminal_hold() {
        HOOK.with(|h| {
            if let Some(hook) = h.borrow_mut().as_mut() {
                hook();
            }
        });
    }
}

#[cfg(test)]
mod torn_present_motion_caret_tests {
    use crate::scripted_redraw_fixture::{feed, scripted_app};
    use crate::{App, WindowId};
    use aterm_core::terminal::ClockReading;
    use std::time::{Duration, Instant};

    const CARET: (u16, u16) = (10, 3);
    const WRITE_CURSOR: (u16, u16) = (2, 47);

    fn lit_app() -> App {
        let mut app = scripted_app();
        app.config.serious_mode = Some(false);
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty".into());
        app.config.motion = Some("full".into());
        app.config.trail_sounds = Some(false);
        app.kitty_cursor_enabled_cache = None;
        app.recompute_sparkle();
        app.windows.get_mut(&WindowId(0)).expect("window").focused = true;
        app
    }

    fn feed_at(app: &App, bytes: &[u8], monotonic: Instant) {
        let term = app
            .front_terminal(WindowId(0))
            .expect("front terminal")
            .term
            .clone();
        crate::term_lock(&term).process_at(
            bytes,
            ClockReading {
                monotonic,
                wall_ms: None,
            },
        );
    }

    #[derive(Clone, Copy, Debug)]
    enum Tear {
        PastTheHoldCap,
        PastTheTerminalTimeout,
        Finished,
    }

    type Facts = (Option<(u16, u16)>, u64, u64);

    fn run(tear: Tear) -> (Facts, Facts) {
        let wid = WindowId(0);
        let mut app = lit_app();
        let t0 = Instant::now();
        feed_at(
            &app,
            format!("\x1b[2J\x1b[{};{}H", CARET.0 + 1, CARET.1 + 1).as_bytes(),
            t0,
        );
        app.redraw_window(wid);
        {
            let ws = app.windows.get_mut(&wid).expect("window");
            let now = Instant::now();
            ws.fx.cursor_mut().glow.note_typed_cells(now, 1);
            ws.fx.cursor_mut().trail.note_typed(now);
        }
        let facts = |app: &App| -> Facts {
            let glow = &app.windows[&wid].fx.cursor().glow;
            (
                glow.cursor_anchor(),
                glow.spawns(),
                glow.admission_tally().licensed,
            )
        };
        let before = facts(&app);
        let repaint = format!(
            "\x1b[?2026h\x1b[?25l\x1b[H\x1b[{};1Htranscript row\x1b[{};{}H",
            WRITE_CURSOR.0 + 1,
            WRITE_CURSOR.0 + 1,
            WRITE_CURSOR.1 + 1
        );
        match tear {
            Tear::PastTheHoldCap => {
                feed(&app, repaint.as_bytes());
                app.redraw_window(wid); // the rising edge arms the hold
                assert_eq!(facts(&app), before, "{tear:?}: the held frame runs nothing");
                std::thread::sleep(super::SYNC_HOLD_CAP + Duration::from_millis(20));
            }
            Tear::PastTheTerminalTimeout => {
                feed_at(&app, repaint.as_bytes(), t0);
                feed_at(&app, b"x\x08", t0 + Duration::from_millis(1_200));
            }
            Tear::Finished => feed(&app, format!("{repaint}\x1b[?2026l").as_bytes()),
        }
        {
            let term = app
                .front_terminal(wid)
                .expect("front terminal")
                .term
                .clone();
            let t = crate::term_lock(&term);
            let c = t.cursor();
            assert_eq!((c.row, c.col), WRITE_CURSOR, "{tear:?}: the write cursor");
            assert!(!t.cursor_visible(), "{tear:?}: hidden");
            match tear {
                Tear::PastTheHoldCap => assert!(t.modes().synchronized_output()),
                Tear::PastTheTerminalTimeout => assert!(
                    !t.modes().synchronized_output(),
                    "the terminal's timeout cleared the mode"
                ),
                Tear::Finished => assert!(!t.sync_frame_unfinished()),
            }
        }
        app.redraw_window(wid);
        assert!(
            app.windows[&wid].last_present.is_some(),
            "{tear:?}: the frame presented"
        );
        (before, facts(&app))
    }

    #[test]
    fn an_unfinished_frames_hidden_write_cursor_moves_no_engine_through_the_present() {
        for tear in [
            Tear::PastTheHoldCap,
            Tear::PastTheTerminalTimeout,
            Tear::Finished,
        ] {
            let (before, after) = run(tear);
            assert_eq!(before.0, Some(CARET), "{tear:?}: seeded at the caret");
            if matches!(tear, Tear::Finished) {
                assert_eq!(
                    after.0,
                    Some(WRITE_CURSOR),
                    "the hidden caret is the anchor"
                );
            } else {
                assert_eq!(
                    after, before,
                    "{tear:?}: the write cursor is no caret (anchor, spawns, licensed)"
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "app_render_single_hold_redraw_tests.rs"]
mod single_hold_redraw_tests;

#[cfg(test)]
#[path = "app_render_prediction_input_tests.rs"]
mod prediction_input_tests;

#[cfg(test)]
#[path = "app_render_licence_fence_tests.rs"]
mod licence_fence_tests;

#[cfg(test)]
mod effects_rise_allowance_tests {
    #[test]
    fn no_rise_allowance_over_the_message_band() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.backend.set_head(40);
        let (rows, cols) = (
            usize::from(app.windows[&wid].rows),
            usize::from(app.windows[&wid].cols),
        );
        let ch = app.win_cell_size(wid).1;
        let head = app.win_head(wid);
        assert!(head > 0, "the fixture has a head band");
        assert_eq!(app.chrome_rows(wid), 0);
        let (_, y0, _, _, rise0) = app.effects_origin_win(wid, rows, cols, ch);
        assert_eq!(usize::from(rise0), head, "no chrome: the head is rise room");
        app.post_message(aterm_messages::Message::new(
            aterm_messages::tags::CONFIG,
            aterm_messages::Severity::Warn,
            "Misspelled setting",
        ));
        assert_eq!(app.message_band_rows, 1);
        let (_, y1, _, _, rise1) = app.effects_origin_win(wid, rows, cols, ch);
        assert_eq!(usize::from(y1), usize::from(y0) + ch, "the band's row");
        assert_eq!(rise1, 0, "no rise over the band");
    }
}

#[cfg(test)]
mod rain_latch_without_engine_tests {
    use crate::WindowId;
    use crate::scripted_redraw_fixture::{feed, scripted_app};

    #[test]
    fn a_single_pane_with_no_rain_engine_keeps_the_completion_latch_observing() {
        let wid = WindowId(0);
        let mut app = scripted_app();
        app.config.serious_mode = Some(false);
        app.config.matrix_rain = Some(crate::app_config::MatrixRainConfig {
            enabled: Some(true),
            ..Default::default()
        });
        app.recompute_matrix_rain();
        app.redraw_window(wid);
        let front = app.front_terminal(wid).expect("front terminal");
        let (session, term) = (front.session, front.term.clone());
        assert!(
            app.windows[&wid].fx.rain().is_some(),
            "REACH: the rain engine was built"
        );
        assert_eq!(
            app.windows[&wid].fx.rain_latches().last_cmd,
            Some((session, 0))
        );
        assert!(app.set_serious_mode(true));
        feed(
            &app,
            b"\x1b]133;A\x07$ \x1b]133;B\x07false\r\n\x1b]133;C\x07\x1b]133;D;1\x07",
        );
        app.redraw_window(wid);
        assert!(
            app.windows[&wid].fx.rain().is_none(),
            "REACH: no rain engine runs under Serious Mode"
        );
        let seq = crate::term_lock(&term).completed_command_seq();
        assert!(seq > 0, "fixture: the shell reported a completion");
        assert_eq!(
            app.windows[&wid].fx.rain_latches().last_cmd,
            Some((session, seq)),
            "the rain-less frame baselined the completion; before the fix the \
             engine the lift rebuilds read it as news"
        );
    }
}
