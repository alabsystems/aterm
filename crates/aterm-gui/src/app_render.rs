// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Frame composition / redraw: the per-window present path (`redraw_window`), the
//! split-pane composition (`redraw_compose`), the in-grid tab-strip splice, blink
//! sync, and the resize plumbing — plus the pure render helpers they use
//! (`should_repaint`, divider/blit/prepend, the pixel→cell geometry). A verbatim
//! inherent-impl split of `App`; no logic change to the hot present path.

use std::num::NonZeroU32;
use std::ops::Range;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_core::terminal::{CursorStyle, RenderCell, Terminal};
use aterm_render::{DamageOutcome, Frame, RenderInput, Theme};
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::{
    App, BLINK_INTERVAL, Backend, PresentDropAccounting, PresentTarget, RepaintKey,
    SelectionFingerprint, WindowId, chrome_band, metrics, pane, platform::AppRt,
    rearm_failed_gpu_recovery, request_recovery_redraw, tab_bar, term_lock,
};

/// LOAD-ADAPTIVE EFFECT SHEDDING (Change #1) tunables. The EMA smoothing weight for
/// each new real-present cost sample: 0.5 is a light "recent frame vs history"
/// smoother, with the real debounce carried by the hysteresis-frame counter below.
const PERF_EMA_ALPHA: f64 = 0.5;
/// Shed decorative effects once the smoothed present cost exceeds this multiple of
/// the frame budget (a session that can't keep up with its own refresh).
const PERF_SHED_FACTOR: f64 = 1.5;
/// Re-engage effects once the smoothed present cost falls back below this multiple of
/// the frame budget. The gap to [`PERF_SHED_FACTOR`] is the hysteresis dead-band that
/// stops the latch flapping at the boundary.
const PERF_CLEAR_FACTOR: f64 = 0.8;
/// Consecutive qualifying real presents required to flip the shed latch in either
/// direction — a lone slow/fast frame never trips it.
const PERF_HYSTERESIS_FRAMES: u32 = 3;
/// ANTI-FLAP DWELL. The shed threshold is measured WITH the effects running and the
/// clear threshold WITHOUT them (shedding removes the very cost being measured), so
/// when the effects' marginal cost alone spans the dead-band the raw thresholds form
/// a relaxation oscillator: shed → cost collapses → clear → cost returns → shed, on
/// the order of 2×[`PERF_HYSTERESIS_FRAMES`] presents. Each flip used to wipe the
/// in-flight trail — the reported "gapping". The latch therefore must STAY shed at
/// least this long before a restore is considered.
pub(crate) const PERF_SHED_DWELL_MIN: std::time::Duration = std::time::Duration::from_millis(1500);
/// Ceiling for the flap-backoff dwell below.
const PERF_SHED_DWELL_MAX: std::time::Duration = std::time::Duration::from_secs(30);
/// A re-shed this soon after a restore is a FLAP (the restored effects immediately
/// re-overloaded the budget): the shed dwell doubles, up to [`PERF_SHED_DWELL_MAX`],
/// so a genuinely overloaded session converges to "shed, with a brief restore probe
/// every dwell" instead of oscillating. A restore that survives longer than this
/// window resets the dwell to [`PERF_SHED_DWELL_MIN`].
const PERF_RESHED_QUICK_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);
/// SOFT-SHED ENVELOPE fade times (seconds). On a shed edge the cursor glow/trail
/// amplitude ramps 1→0 over the fade-out instead of stepping — the engines keep
/// their spark buffers and decay them visibly, where the old hard step cleared them
/// the same frame (an abrupt whole-trail dropout). Restore ramps 0→1 a little
/// faster. Accessibility cuts (OS Reduce Motion / `motion = "reduced"`) remain
/// HARD zeros — only the load-shed contribution is enveloped.
const SHED_FADE_OUT_SECS: f32 = 0.45;
/// See [`SHED_FADE_OUT_SECS`].
const SHED_FADE_IN_SECS: f32 = 0.25;
/// SYNC-1: hard cap on how long a DEC-2026 present-hold may withhold glass,
/// independent of the (1 s default) protocol timeout that clears the MODE for a
/// crashed app. A present withheld for a second IS the frozen screen mode 2026
/// exists to prevent; past this cap a possible tear beats the guaranteed freeze.
/// ~9 frames at 60 Hz — generous for any legitimate multi-write update batch.
const SYNC_HOLD_CAP: std::time::Duration = std::time::Duration::from_millis(150);

/// Combine the two causal CPU-wall-time slices of one frame. `compose_ns` ends
/// immediately before surface acquisition; `raster_submit_ns` is measured only
/// while doing CPU raster/copy or while the CPU encodes GPU commands and calls
/// `queue.submit`. It is NOT a GPU timestamp and does not claim to measure
/// completed shader execution. Swapchain/buffer acquisition and the final
/// compositor present are intentionally absent, so healthy FIFO pacing cannot
/// trip the adaptive-effect latch.
fn causal_render_cost_ns(compose_ns: u64, raster_submit_ns: u64) -> u64 {
    compose_ns.saturating_add(raster_submit_ns)
}

/// The portion of a swapchain-acquire wait that CANNOT be healthy display pacing,
/// and therefore is genuine GPU/compositor back-pressure.
///
/// [`causal_render_cost_ns`] deliberately excludes acquisition so that ordinary
/// vsync pacing never sheds effects — correct, but it left the load-shed latch
/// blind in the one direction that matters most: the effects the latch sheds
/// (`set_bloom` / `set_shimmer`) are pure GPU passes whose CPU encode cost is a
/// few microseconds whether the GPU is idle or saturated. So on a GPU-bound
/// machine the bloom could eat milliseconds of GPU per frame, the drawable pool
/// stay exhausted, and the EMA still see ~0.3 ms and never shed — the safety
/// valve was inert for its two primary levers.
///
/// The discriminator: with a drawable pool of `latency + 1`, waiting up to ONE
/// refresh is normal pacing; waiting LONGER than a full frame interval means the
/// pool is exhausted because the GPU has not finished prior frames. Only that
/// excess is charged, so a healthy FIFO/Immediate present contributes exactly 0
/// and the existing hysteresis/dwell machinery is reused unchanged.
fn gpu_backpressure_excess_ns(acquire_wait_ns: u64, frame_interval: std::time::Duration) -> u64 {
    let fi = u64::try_from(frame_interval.as_nanos()).unwrap_or(u64::MAX);
    acquire_wait_ns.saturating_sub(fi)
}

/// Shipping decision at the failed-present seam. A latched device loss is not
/// a transient surface condition: retrying the dead GPU can only exhaust the
/// finite train and park forever, so it must route directly to CPU recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailedPresentRoute {
    RetrySurface,
    RecoverGpu,
}

/// Testable projection of the failed CPU-builder branch of one already-routed
/// GPU-loss transaction. Production publishes the same reason/accounting tuple
/// to the process metrics ledger before returning it here for Tier-1 binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GpuRecoveryRetryObservation {
    pub(crate) accounting: PresentDropAccounting,
    pub(crate) reason: metrics::PresentDropReason,
    pub(crate) parked: bool,
}

/// Exact source-window outcome of one routed GPU-loss transaction.  Keeping
/// success distinct from "the process-wide CPU renderer was installed" is
/// load-bearing: the source softbuffer may still have failed to build, in
/// which case it owns a bounded [`GpuRecoveryRetryObservation`] rather than a
/// redraw that was never requested.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GpuRecoveryOutcome {
    /// The CPU renderer itself could not be installed (or recovery raced an
    /// already-completed fallback).  The completion layer converts this to a
    /// typed source retry whenever the source window still exists.
    BackendUnavailable,
    /// The source CPU target exists and its first redraw edge was delivered.
    SourceReadyRequested,
    /// The source CPU target is not ready and owns this diagnosed bounded
    /// retry.  This includes target construction failing after the global CPU
    /// backend was installed.
    SourceRetry(GpuRecoveryRetryObservation),
    /// No windowed presentation target exists for the source (headless or the
    /// logical window disappeared).  Do not classify this as ready/requested.
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

#[inline]
fn present_band_bg(default_bg: u32, theme_bg: u32) -> u32 {
    if default_bg == aterm_core::render::COLOR_UNSET {
        theme_bg
    } else {
        default_bg
    }
}

/// Select the Fire style's decorative cursor-body fill through the same gates
/// in both the single-terminal and split-pane render paths. Keeping the fill
/// lazy matters: a disabled effect must not even sample retained forge state.
fn forge_cursor_fill(
    cursor_body_allowed: bool,
    glow_cfg: &crate::cursor_glow::GlowConfig,
    fill: impl FnOnce() -> Option<u32>,
) -> Option<u32> {
    (cursor_body_allowed
        && glow_cfg.enabled
        && glow_cfg.intensity > 0.0
        && matches!(glow_cfg.style, crate::cursor_glow::GlowStyle::Fire))
    .then(fill)
    .flatten()
}

const INACTIVE_CURSOR_FILL: u32 = 0x00FF_FFFF;

/// Resolve the host-owned cursor fill after the window-focus style override.
///
/// `HollowBlock` is not a terminal DECSCUSR style: the window host installs it
/// exclusively for a real unfocused window. Give that inactive outline a quiet
/// neutral white instead of inheriting the active theme/OSC-12 colour (normally
/// green). Every other style keeps its active/effect fill unchanged, including
/// glass-less virtual captures where no focus override is installed.
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

#[cfg(test)]
mod window_cursor_fill_tests {
    use super::{INACTIVE_CURSOR_FILL, window_cursor_fill};
    use aterm_core::terminal::CursorStyle;

    #[test]
    fn inactive_hollow_cursor_is_white_even_with_an_active_effect_fill() {
        assert_eq!(
            window_cursor_fill(Some(CursorStyle::HollowBlock), None),
            Some(INACTIVE_CURSOR_FILL)
        );
        assert_eq!(
            window_cursor_fill(Some(CursorStyle::HollowBlock), Some(0x0050_FA7B)),
            Some(INACTIVE_CURSOR_FILL),
            "an inactive window must not retain the active green/effect fill"
        );
    }

    #[test]
    fn focused_and_effect_shape_overrides_keep_their_fill() {
        assert_eq!(window_cursor_fill(None, None), None);
        assert_eq!(
            window_cursor_fill(None, Some(0x0050_FA7B)),
            Some(0x0050_FA7B),
            "a focused cursor keeps its active fill"
        );
        assert_eq!(
            window_cursor_fill(Some(CursorStyle::Bolt), Some(0x00AA_44FF)),
            Some(0x00AA_44FF),
            "non-focus shape overrides keep their effect fill"
        );
    }
}

/// Run the two fallible stages of one CPU-surface present as a transaction.
/// A buffer that cannot be acquired, or pixels that cannot be committed, did
/// not reach glass: both failures return a typed error so the caller takes the
/// shared dropped-present path and clears its optimistic repaint stamp. `work_ns` is
/// published only after the commit succeeds, preventing phantom frame metrics.
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
        FailedPresentRoute, cpu_surface_transaction, failed_present_route, present_band_bg,
    };

    #[derive(Debug)]
    struct FakeBuffer;

    #[derive(Debug)]
    struct FakeSurfaceError;

    /// Negative controls for both formerly ignored softbuffer errors. The
    /// closure-not-called assertion also proves an acquire failure cannot paint
    /// or manufacture a successful work sample.
    #[test]
    fn acquire_and_commit_failures_are_dropped_not_counted_as_presents() {
        let mut paint_called = false;
        let acquire_failed = cpu_surface_transaction(
            Err::<FakeBuffer, FakeSurfaceError>(FakeSurfaceError),
            |_| {
                paint_called = true;
                Ok(11)
            },
        );
        assert_eq!(
            acquire_failed,
            Err(crate::metrics::PresentDropReason::CpuAcquire)
        );
        assert!(!paint_called, "a failed acquire must not enter the painter");

        let commit_failed = cpu_surface_transaction(Ok(FakeBuffer), |_| {
            Err::<u64, FakeSurfaceError>(FakeSurfaceError)
        });
        assert_eq!(
            commit_failed,
            Err(crate::metrics::PresentDropReason::CpuCommit),
            "a failed final commit must not publish a phantom frame"
        );

        assert_eq!(
            cpu_surface_transaction(Ok::<_, FakeSurfaceError>(FakeBuffer), |_| Ok(17)),
            Ok(17),
            "the causal work sample is returned only after a successful commit"
        );
    }

    #[test]
    fn surface_bands_track_live_terminal_background_with_theme_fallback() {
        assert_eq!(present_band_bg(0x0012_3456, 0x00aa_bbcc), 0x0012_3456);
        assert_eq!(
            present_band_bg(aterm_core::render::COLOR_UNSET, 0x00aa_bbcc),
            0x00aa_bbcc
        );
    }

    #[test]
    fn latched_gpu_loss_routes_to_fallback_before_surface_retry() {
        assert_eq!(
            failed_present_route(true, true),
            FailedPresentRoute::RecoverGpu
        );
        for (use_gpu, device_lost) in [(true, false), (false, true), (false, false)] {
            assert_eq!(
                failed_present_route(use_gpu, device_lost),
                FailedPresentRoute::RetrySurface
            );
        }
    }
}

/// Resolve trail-audio policy from REAL window focus, never the synthetic
/// `motion_focus` bit that recordings use to keep visual effects moving. A
/// background recording may animate; it must never make the Mac speak.
fn trail_sound_gain(raw_focused: bool, configured: bool, volume: f32) -> Option<f32> {
    (raw_focused && configured && volume > 0.0).then_some(volume)
}

/// The per-event trail-audio POLICY the host resolves once per drain and
/// stamps on every emitted [`SoundEvent`] — the knobs ride together so the
/// synth stays policy-free.
struct TrailSoundPolicy {
    /// The `trail_sound_style` override (default `Style` = follow the visual
    /// trail style).
    voice: aterm_effects::trail_sound::SoundVoice,
    /// Resolved gain (`None` = muted: focus/knob/volume law — see
    /// [`trail_sound_gain`]).
    gain: Option<f32>,
    /// The window's cached tone-of-typing verdict (`tone_infer`). The host
    /// resolves it (knob off ⇒ the neutral `Technical` identity) exactly
    /// like it resolves gain.
    tone: aterm_effects::tone::Tone,
    /// The `trail_sound_bed` knob (default OFF — the owner dislikes the
    /// drone): with it off no event ever feeds the synth's bed layer, so
    /// the ambient texture contributes exactly zero samples while the notes
    /// keep playing.
    bed: bool,
}

/// Drain every visual spawn cue and optionally emit its allocation-free sound
/// twin. Both single-pane and split-pane composition route through this seam,
/// so muting never leaves a backlog and layouts cannot silently lose audio.
#[allow(clippy::too_many_arguments)] // Explicit policy axes keep cue emission allocation-free.
fn drain_trail_sound_cues(
    glow: &mut crate::cursor_glow::CursorGlow,
    style: crate::cursor_glow::GlowStyle,
    cols: u16,
    policy: TrailSoundPolicy,
    mut emit: impl FnMut(aterm_effects::trail_sound::SoundEvent),
) -> usize {
    let mut emitted = 0;
    for cue in glow.drain_sound_cues() {
        let Some(gain) = policy.gain else {
            continue;
        };
        emit(aterm_effects::trail_sound::SoundEvent {
            style,
            voice: policy.voice,
            kind: aterm_effects::trail_sound::SoundGesture::Trail(cue.kind),
            pan: if cols > 1 {
                let last = (cols - 1) as f32;
                ((cue.col as f32).min(last) / last) * 2.0 - 1.0
            } else {
                0.0
            },
            heat: cue.heat,
            hue: cue.hue,
            gain,
            tone: policy.tone,
            bed: policy.bed,
        });
        emitted += 1;
    }
    emitted
}

/// Resolve curse-BONK policy — the profanity `bonk` knob, RAW window focus
/// (the trail-sound law verbatim: a background recording may animate, it must
/// never make the Mac speak), the motion policy (a Reduced window pushes no
/// events, matching the glow engine's intensity-0 silence), and the shared
/// `trail_sound_volume`. Per-class/master sparkle gates need no re-check: the
/// engine records cues only for words it is actually decorating.
fn bonk_sound_gain(
    raw_focused: bool,
    enabled: bool,
    reduced_motion: bool,
    volume: f32,
) -> Option<f32> {
    (raw_focused && enabled && !reduced_motion && volume > 0.0).then_some(volume)
}

/// Drain every curse-BONK cue the word-decoration tick recorded and emit the
/// enabled ones as namespaced [`WordGesture::Bonk`] gestures — the
/// sparkle-words twin of [`drain_trail_sound_cues`], one seam for glass and
/// capture alike so a disabled knob never leaves a backlog. `detonations`
/// separately gates the on-screen [`CurseCueKind::Detonated`] kind (typed
/// provenance stays typed-only unless the user opted the blast edge in).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CurseDrain {
    emitted: usize,
    /// Distinct profanity locations this tick. A typed cue and its supernova
    /// detonation at the same word are one visual wince, while `fuck fuck`
    /// produces two beats. This remains independent of sound policy.
    pub(crate) wince_hits: u8,
}

pub(crate) fn drain_curse_bonk_cues(
    decos: &mut aterm_effects::word_decorations::WordDecorations,
    style: crate::cursor_glow::GlowStyle,
    // The `trail_sound_style` override — the bonk's clash SHAPE is
    // style-agnostic, but its register anchor follows the speaking palette.
    voice: aterm_effects::trail_sound::SoundVoice,
    cols: u16,
    gain: Option<f32>,
    detonations: bool,
    mut emit: impl FnMut(aterm_effects::trail_sound::SoundEvent),
) -> CurseDrain {
    use aterm_effects::word_decorations::CurseCueKind;
    let mut result = CurseDrain::default();
    let mut seen = [u32::MAX; 16];
    let mut seen_len = 0usize;
    for cue in decos.drain_curse_cues() {
        let location = u32::from(cue.row) << 16 | u32::from(cue.col);
        if !seen[..seen_len].contains(&location) {
            if seen_len < seen.len() {
                seen[seen_len] = location;
                seen_len += 1;
            }
            result.wince_hits = result.wince_hits.saturating_add(1);
        }
        let Some(gain) = gain else {
            continue;
        };
        if cue.kind == CurseCueKind::Detonated && !detonations {
            continue;
        }
        emit(aterm_effects::trail_sound::SoundEvent {
            // The active trail style keys the bonk's clash REGISTER (its
            // palette anchor) so the wrong note is wrong against the melody
            // actually playing; the gesture itself is style-agnostic.
            style,
            voice,
            kind: aterm_effects::trail_sound::SoundGesture::Words(
                aterm_effects::trail_sound::WordGesture::Bonk,
            ),
            pan: if cols > 1 {
                let last = (cols - 1) as f32;
                ((cue.col as f32).min(last) / last) * 2.0 - 1.0
            } else {
                0.0
            },
            heat: 0.0,
            hue: 0.0,
            gain,
            // The bonk is tone-BLIND by contract (the wrong note is wrong in
            // every mood, and its path is byte-pinned): always the neutral
            // identity, never the window's inferred tone.
            tone: aterm_effects::tone::Tone::Technical,
            // Words gestures never feed the bed (punctuation must not swell
            // the ambience) — carried OFF so the event states the policy it
            // actually gets, independent of the `trail_sound_bed` knob.
            bed: false,
        });
        result.emitted += 1;
    }
    result
}

#[cfg(test)]
mod curse_bonk_drain_tests {
    use std::time::Instant;

    use aterm_core::terminal::Terminal;
    use aterm_effects::trail_sound::{SoundGesture, SoundVoice, WordGesture};
    use aterm_effects::word_decorations::{EffectGeom, WordDecorations};
    use aterm_lexicon::Lexicon;

    use super::{bonk_sound_gain, drain_curse_bonk_cues};

    /// The policy resolver: raw focus, the bonk knob, the reduced-motion
    /// demotion, and the shared volume each silence independently; the
    /// survivor passes the volume through as the event gain.
    #[test]
    fn bonk_gain_policy_gates_focus_knob_motion_and_volume() {
        assert_eq!(bonk_sound_gain(true, true, false, 0.4), Some(0.4));
        assert_eq!(bonk_sound_gain(false, true, false, 0.4), None, "raw focus");
        assert_eq!(bonk_sound_gain(true, false, false, 0.4), None, "bonk knob");
        assert_eq!(
            bonk_sound_gain(true, true, true, 0.4),
            None,
            "reduced motion"
        );
        assert_eq!(bonk_sound_gain(true, true, false, 0.0), None, "zero volume");
    }

    /// The RESIZE QUIET law: a window inside [`crate::RESIZE_SOUND_QUIET`] of
    /// its last applied reflow mutes the sound seams (a TUI's resize repaint
    /// storm must not read as typing), a window that never resized is never
    /// quiet, and the mute expires once the window elapses.
    #[test]
    fn resize_quiets_the_sound_seams_and_expires() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let now = std::time::Instant::now();
        assert!(
            !app.windows.get(&wid).unwrap().resize_sound_quiet(now),
            "a never-resized window must not be quiet"
        );
        app.windows.get_mut(&wid).unwrap().last_resize_at = Some(now);
        assert!(
            app.windows.get(&wid).unwrap().resize_sound_quiet(now),
            "a just-reflowed window must be quiet"
        );
        assert!(
            !app.windows
                .get(&wid)
                .unwrap()
                .resize_sound_quiet(now + crate::RESIZE_SOUND_QUIET),
            "the quiet window must expire"
        );
    }

    /// The drain seam end to end over a REAL typed-curse tick: an enabled
    /// drain emits exactly one namespaced `Words(Bonk)` gesture at the user
    /// volume; a MUTED drain drops the identical cue while still emptying the
    /// engine vec — no backlog may cross a later unmute (the trail-sound
    /// discipline verbatim).
    #[test]
    fn drain_maps_typed_cues_to_namespaced_bonk_and_never_backlogs() {
        let cfg = crate::app_config::Config::default()
            .sparkle_deco_config()
            .expect("defaults enable sparkle words");
        let lex = Lexicon::with_languages(&["en"]);
        let geom = EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 4,
            cols: 32,
        };
        let t0 = Instant::now();
        let style = crate::cursor_glow::GlowStyle::Nyan;
        let tick = |wd: &mut WordDecorations, now: Instant| {
            let (mut o, mut i, mut f, mut n) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            wd.tick(
                now, &cfg, geom, None, None, true, &mut o, &mut i, &mut f, &mut n,
            );
        };
        let typed_curse = |wd: &mut WordDecorations, now: Instant| {
            let mut term = Terminal::new(4, 32);
            term.process(b"fuck");
            wd.rescan(&term, 4, 32, &lex, &cfg, 1, now);
            tick(wd, now);
        };

        // Enabled: one cue ⇒ one namespaced bonk at the user volume.
        let mut wd = WordDecorations::default();
        typed_curse(&mut wd, t0);
        let mut events = Vec::new();
        let drained = drain_curse_bonk_cues(
            &mut wd,
            style,
            SoundVoice::Style,
            geom.cols,
            Some(0.4),
            false,
            |ev| events.push(ev),
        );
        assert_eq!(drained.emitted, 1);
        assert_eq!(drained.wince_hits, 1);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].kind,
            SoundGesture::Words(WordGesture::Bonk),
            "the drain must speak the namespaced sparkle-words gesture"
        );
        assert_eq!(events[0].gain, 0.4);
        assert_eq!(
            events[0].style, style,
            "the clash register keys off the trail style"
        );

        // Muted (knob off / unfocused / reduced ⇒ gain None): the same cue is
        // dropped AND the vec is emptied — nothing replays on unmute.
        let mut wd2 = WordDecorations::default();
        typed_curse(&mut wd2, t0);
        let drained = drain_curse_bonk_cues(
            &mut wd2,
            style,
            SoundVoice::Style,
            geom.cols,
            None,
            false,
            |_| panic!("a muted drain must not emit"),
        );
        assert_eq!(drained.emitted, 0);
        assert_eq!(
            drained.wince_hits, 1,
            "visual reaction is sound-independent"
        );
        let drained = drain_curse_bonk_cues(
            &mut wd2,
            style,
            SoundVoice::Style,
            geom.cols,
            Some(0.4),
            false,
            |_| panic!("the muted frame's cue must not backlog into the unmute"),
        );
        assert_eq!(drained, super::CurseDrain::default());
    }
}

/// Consume one due cursor-effect deadline at the event-loop boundary. The
/// fired deadline—not the later wake timestamp—becomes the cadence anchor, and
/// the pending slot is cleared before the redraw is requested. Returning
/// `false` leaves both fields byte-for-byte unchanged.
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

/// A successful content/input present already advanced every cursor-effect
/// engine during composition. If an older animation deadline is still armed,
/// consume it and make this frame the cadence anchor; otherwise the stale timer
/// fires immediately after the useful present (a visible frame doublet followed
/// by a longer hole). A timer-driven present arrives with `next == None`, so its
/// phase-locked fired-deadline anchor is deliberately left untouched.
fn rebase_pending_trail_tick_after_present(
    next: &mut Option<Instant>,
    anchor: &mut Option<Instant>,
    frame_started: Instant,
) {
    if next.take().is_some() {
        *anchor = Some(frame_started);
    }
}

/// Choose the next effect deadline without accumulating render/present cost.
/// Brisk geometry continues from the fired cadence anchor when that next slot
/// is still in the future; an overloaded frame starts a fresh full interval.
/// Coarse-only effects (forge ember, vapor, kill-hint fallback) keep the exact
/// engine deadline they computed.
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
        nyan_sprite: crate::app_config::NyanSpriteAsset,
        themes: Arc<crate::app_config::ThemeCatalog>,
    ) -> Arc<crate::app_config::ConfigAssetCatalog> {
        Arc::new(crate::app_config::ConfigAssetCatalog {
            trail_packs: crate::app_config::TrailPackCatalog::empty(),
            nyan_sprite,
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
        let second = app.insert_logical_window(crate::stub_session(sid), 24, 80);
        assert_eq!(app.windows.len(), 2);

        let rgba: Arc<[u8]> = Arc::from([0x12, 0x34, 0x56, 0xff]);
        let ready = catalog(
            crate::app_config::NyanSpriteAsset::Ready {
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
        for window in app.windows.values() {
            assert_eq!(window.word_decos.nyan_sprite_source_fingerprint(), Some(11));
            assert!(Arc::ptr_eq(
                window.word_decos.nyan_sprite_rgba().expect("custom RGBA"),
                &rgba
            ));
        }

        // Both presentation preparation paths observe that publication already
        // installed the generation; neither needs a late worker/poll repair.
        assert!(!app.install_window_config_assets(WindowId(0)));
        app.splice_word_decorations(second, std::time::Instant::now());
        assert_exact_catalog_on_every_window(&app, &ready);

        let invalid = catalog(
            crate::app_config::NyanSpriteAsset::Invalid {
                source_id: Arc::from("same.png"),
                bounded_reason: Arc::from("replacement is invalid"),
            },
            crate::app_config::ThemeCatalog::empty(),
        );
        assert_eq!(app.publish_config_assets(Arc::clone(&invalid)), 2);
        assert_exact_catalog_on_every_window(&app, &invalid);
        assert!(
            app.windows
                .values()
                .all(|window| window.word_decos.nyan_sprite_source_fingerprint().is_none())
        );

        let builtin = catalog(
            crate::app_config::NyanSpriteAsset::BuiltIn,
            crate::app_config::ThemeCatalog::empty(),
        );
        assert_eq!(app.publish_config_assets(Arc::clone(&builtin)), 2);
        assert_exact_catalog_on_every_window(&app, &builtin);
        assert!(
            app.windows
                .values()
                .all(|window| { window.word_decos.nyan_sprite_source_fingerprint() == Some(0) })
        );
        assert_eq!(
            app.publish_config_assets(Arc::clone(&builtin)),
            0,
            "re-publishing the identical outer Arc is a complete no-op"
        );
    }

    #[test]
    fn same_path_content_and_theme_only_generations_fan_out_exact_arcs() {
        let mut app = App::headless_for_test();
        let sid = app.next_session_id;
        let second = app.insert_logical_window(crate::stub_session(sid), 24, 80);

        let first_rgba: Arc<[u8]> = Arc::from([0x10, 0x20, 0x30, 0xff]);
        let first = catalog(
            crate::app_config::NyanSpriteAsset::Ready {
                source_id: Arc::from("unchanged/path.png"),
                w: 1,
                h: 1,
                rgba: Arc::clone(&first_rgba),
                fp: 21,
            },
            crate::app_config::ThemeCatalog::empty(),
        );
        assert_eq!(app.publish_config_assets(first), 2);

        // The authored path and TOML can be byte-identical while the file's
        // content changes. A new catalog identity must replace the exact RGBA
        // Arc in every existing window before capture or effects preparation.
        let second_rgba: Arc<[u8]> = Arc::from([0xaa, 0xbb, 0xcc, 0xff]);
        let replacement = catalog(
            crate::app_config::NyanSpriteAsset::Ready {
                source_id: Arc::from("unchanged/path.png"),
                w: 1,
                h: 1,
                rgba: Arc::clone(&second_rgba),
                fp: 22,
            },
            crate::app_config::ThemeCatalog::empty(),
        );
        assert_eq!(app.publish_config_assets(Arc::clone(&replacement)), 2);
        assert_exact_catalog_on_every_window(&app, &replacement);
        assert!(app.windows.values().all(|window| {
            window.word_decos.nyan_sprite_source_fingerprint() == Some(22)
                && Arc::ptr_eq(
                    window
                        .word_decos
                        .nyan_sprite_rgba()
                        .expect("replacement RGBA"),
                    &second_rgba,
                )
        }));
        app.splice_word_decorations(second, std::time::Instant::now());
        assert_exact_catalog_on_every_window(&app, &replacement);

        // A theme-directory-only generation still changes the outer catalog.
        // Nyan pixels remain the same allocation, while every window adopts the
        // new exact outer Arc immediately.
        let themes = crate::app_config::ThemeCatalog::from_schemes([(
            "Publication Test".to_string(),
            aterm_types::scheme::builtin("Dracula").expect("builtin theme"),
        )]);
        let theme_only = catalog(replacement.nyan_sprite.clone(), themes);
        assert_eq!(app.publish_config_assets(Arc::clone(&theme_only)), 2);
        assert_exact_catalog_on_every_window(&app, &theme_only);
        assert!(app.windows.values().all(|window| {
            Arc::ptr_eq(
                window.word_decos.nyan_sprite_rgba().expect("retained RGBA"),
                &second_rgba,
            )
        }));
        assert!(!app.install_window_config_assets(WindowId(0)));
    }
}

#[cfg(test)]
mod trail_present_pacing_tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn useful_present_consumes_near_pending_animation_tick() {
        let frame = Instant::now();
        let mut next = Some(frame + Duration::from_millis(1));
        let mut anchor = None;
        rebase_pending_trail_tick_after_present(&mut next, &mut anchor, frame);
        assert_eq!(next, None, "no immediate effect-only frame doublet");
        assert_eq!(anchor, Some(frame), "next sample phases from this frame");
    }

    #[test]
    fn only_a_due_tick_transfers_ownership_to_the_fired_anchor() {
        let base = Instant::now();
        let fired = base + Duration::from_millis(2);
        let mut next = Some(fired);
        let mut anchor = None;
        assert!(!take_due_trail_tick(&mut next, &mut anchor, base));
        assert_eq!((next, anchor), (Some(fired), None));
        assert!(take_due_trail_tick(&mut next, &mut anchor, fired));
        assert_eq!((next, anchor), (None, Some(fired)));
        assert!(!take_due_trail_tick(
            &mut next,
            &mut anchor,
            fired + Duration::from_millis(1)
        ));
        assert_eq!((next, anchor), (None, Some(fired)));
    }
    #[test]
    fn timer_driven_present_preserves_its_fired_deadline_anchor() {
        let fired = Instant::now();
        let frame = fired + Duration::from_millis(2);
        let mut next = None;
        let mut anchor = Some(fired);
        rebase_pending_trail_tick_after_present(&mut next, &mut anchor, frame);
        assert_eq!(anchor, Some(fired), "timer phase lock remains intact");
    }
    #[test]
    fn brisk_tail_does_not_add_render_cost_to_each_period() {
        let interval = Duration::from_millis(16);
        let fired = Instant::now();
        let after_render = fired + Duration::from_millis(2);
        assert_eq!(
            phase_locked_effect_deadline(after_render, interval, Some(fired), None, true),
            fired + interval,
            "the next slot is anchored to the timer fire, not redraw completion"
        );
    }

    #[test]
    fn overloaded_tail_never_catches_up_in_a_burst() {
        let interval = Duration::from_millis(16);
        let fired = Instant::now();
        let overloaded = fired + Duration::from_millis(20);
        assert_eq!(
            phase_locked_effect_deadline(overloaded, interval, Some(fired), None, true),
            overloaded + interval,
            "a missed slot starts one full period from now"
        );
    }

    #[test]
    fn coarse_engine_deadline_is_preserved_exactly() {
        let now = Instant::now();
        let coarse = now + Duration::from_millis(90);
        assert_eq!(
            phase_locked_effect_deadline(
                now,
                Duration::from_millis(16),
                Some(now),
                Some(coarse),
                false,
            ),
            coarse
        );
    }

    #[test]
    fn content_present_rebase_conforms_to_derived_model() {
        let model = aterm_spec::derive::effect_present_rebase_model();
        let state = |pending: i64, anchor: i64, after_content: i64, steps: i64| {
            BTreeMap::from([
                ("pending", pending),
                ("anchor", anchor),
                ("after_content", after_content),
                ("steps", steps),
            ])
        };
        let base = Instant::now();
        let frame = base + Duration::from_millis(2);
        let mut next = None;
        let mut anchor = None;

        // Reach the pending state through the model's real Arm transition;
        // the former test started at pending=1,steps=0, which is unreachable.
        let initial = state(0, 0, 0, 0);
        assert_eq!(model.init_state(), initial);
        assert_eq!((next, anchor), (None, None));
        next = Some(base + Duration::from_millis(3));
        let armed = state(1, 0, 0, 1);
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &initial,
            &armed,
            Some("Arm"),
            "effect-present arm conformance",
        );
        assert!(ok, "shipping arm transition rejected: {why}");

        rebase_pending_trail_tick_after_present(&mut next, &mut anchor, frame);
        let anchor_tick = anchor
            .expect("a useful present becomes the cadence anchor")
            .duration_since(base)
            .as_millis() as i64;
        let projected = state(i64::from(next.is_some()), anchor_tick, 1, 2);
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &armed,
            &projected,
            Some("ContentPresent"),
            "effect-present rebase conformance",
        );
        assert!(ok, "shipping rebase transition rejected: {why}");

        let stale = state(1, 2, 1, 2);
        let (ok, _) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &armed,
            &stale,
            Some("ContentPresent"),
            "effect-present rebase negative control",
        );
        assert!(!ok, "leaving the stale timer armed must fail conformance");

        let unanchored = state(0, 0, 1, 2);
        let (ok, _) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &armed,
            &unanchored,
            Some("ContentPresent"),
            "effect-present anchor negative control",
        );
        assert!(
            !ok,
            "clearing the timer without rebasing its anchor must fail"
        );
    }

    #[test]
    fn phase_locked_deadline_conforms_to_derived_model() {
        let model = aterm_spec::derive::effect_phase_lock_model();
        let state = |now: i64,
                     anchor: i64,
                     schedule_base: i64,
                     deadline: i64,
                     pending: i64,
                     rendering: i64,
                     phase_locked: i64,
                     fresh: i64| {
            BTreeMap::from([
                ("now", now),
                ("anchor", anchor),
                ("schedule_base", schedule_base),
                ("deadline", deadline),
                ("pending", pending),
                ("rendering", rendering),
                ("phase_locked", phase_locked),
                ("fresh", fresh),
            ])
        };
        let base = Instant::now();
        let interval = Duration::from_millis(2);
        let actual = phase_locked_effect_deadline(
            base + Duration::from_millis(3),
            interval,
            Some(base + Duration::from_millis(2)),
            None,
            true,
        );
        let actual_tick = actual.duration_since(base).as_millis() as i64;
        let prev = state(3, 2, 0, 2, 0, 1, 0, 0);
        let next = state(3, 2, 2, actual_tick, 1, 0, 1, 1);
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &prev,
            &next,
            Some("Rearm"),
            "effect phase-lock conformance",
        );
        assert!(ok, "shipping phase-lock transition rejected: {why}");

        let slid = state(3, 2, 3, 5, 1, 0, 1, 1);
        let (ok, _) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &prev,
            &slid,
            Some("Rearm"),
            "effect phase-lock negative control",
        );
        assert!(!ok, "render-cost cadence slide must fail conformance");
    }

    #[test]
    fn event_loop_fire_present_rearm_trace_conforms_end_to_end() {
        let model = aterm_spec::derive::effect_phase_lock_model();
        let state = |now: i64,
                     anchor: i64,
                     schedule_base: i64,
                     deadline: i64,
                     pending: i64,
                     rendering: i64,
                     phase_locked: i64,
                     fresh: i64| {
            BTreeMap::from([
                ("now", now),
                ("anchor", anchor),
                ("schedule_base", schedule_base),
                ("deadline", deadline),
                ("pending", pending),
                ("rendering", rendering),
                ("phase_locked", phase_locked),
                ("fresh", fresh),
            ])
        };
        let validate = |before: &BTreeMap<&'static str, i64>,
                        after: &BTreeMap<&'static str, i64>,
                        action: &'static str,
                        label: &'static str| {
            let (ok, why) = aterm_spec::verify::validate_transition_tiered(
                &model,
                &[],
                before,
                after,
                Some(action),
                label,
            );
            assert!(ok, "shipping {action} transition rejected: {why}");
        };

        let base = Instant::now();
        let interval = Duration::from_millis(2);
        let fired = base + interval;
        let mut next = Some(fired);
        let mut anchor = None;

        let mut reachable = model.init_state();
        assert!(model.fire("Tick", &mut reachable));
        assert!(model.fire("Tick", &mut reachable));
        let before_fire = state(2, 0, 0, 2, 1, 0, 1, 0);
        assert_eq!(
            reachable, before_fire,
            "the on-time Fire prestate is reachable"
        );

        // `new_events`: transfer the due timer's exact deadline into the
        // cadence anchor before requesting the redraw.
        assert!(take_due_trail_tick(&mut next, &mut anchor, fired));
        assert_eq!((next, anchor), (None, Some(fired)));
        let after_fire = state(2, 2, 0, 2, 0, 1, 0, 0);
        validate(
            &before_fire,
            &after_fire,
            "Fire",
            "real event-loop trail fire",
        );

        // The actual present path runs while the timer is disarmed. Its
        // post-present rebase must preserve the fired anchor.
        let presented = fired + Duration::from_millis(1);
        rebase_pending_trail_tick_after_present(&mut next, &mut anchor, presented);
        assert_eq!((next, anchor), (None, Some(fired)));
        let after_render = state(3, 2, 0, 2, 0, 1, 0, 0);
        validate(
            &after_fire,
            &after_render,
            "RenderCost",
            "real timer-driven present cost",
        );

        // `about_to_wait`: consume the transferred anchor and arm the next
        // phase-locked slot. A completion-relative implementation would return
        // tick 5 here and is rejected by the model.
        let last_fire = anchor.take();
        let deadline = phase_locked_effect_deadline(presented, interval, last_fire, None, true);
        next = Some(deadline);
        assert_eq!(next, Some(deadline));
        assert_eq!(deadline, base + Duration::from_millis(4));
        let rearmed = state(3, 2, 2, 4, 1, 0, 1, 1);
        validate(
            &after_render,
            &rearmed,
            "Rearm",
            "real event-loop trail rearm",
        );

        let slid = state(3, 2, 3, 5, 1, 0, 1, 1);
        let (ok, _) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &after_render,
            &slid,
            Some("Rearm"),
            "event-loop phase-slide negative control",
        );
        assert!(!ok, "completion-relative event-loop rearm must be rejected");

        // Late delivery plus an overloaded present is the other real branch.
        // The due helper retains the original deadline as anchor, while re-arm
        // starts a fresh full interval once the next phase slot is already past.
        let late_base = Instant::now();
        let late_fired = late_base + interval;
        let delivered = late_base + Duration::from_millis(5);
        let mut late_next = Some(late_fired);
        let mut late_anchor = None;
        let mut late_reachable = model.init_state();
        for _ in 0..5 {
            assert!(model.fire("Tick", &mut late_reachable));
        }
        let late_before_fire = state(5, 0, 0, 2, 1, 0, 1, 0);
        assert_eq!(
            late_reachable, late_before_fire,
            "the late Fire prestate is reachable"
        );
        assert!(take_due_trail_tick(
            &mut late_next,
            &mut late_anchor,
            delivered
        ));
        assert_eq!((late_next, late_anchor), (None, Some(late_fired)));
        let late_after_fire = state(5, 2, 0, 2, 0, 1, 0, 0);
        validate(
            &late_before_fire,
            &late_after_fire,
            "Fire",
            "late event-loop trail fire",
        );

        let late_presented = late_base + Duration::from_millis(6);
        rebase_pending_trail_tick_after_present(&mut late_next, &mut late_anchor, late_presented);
        assert_eq!((late_next, late_anchor), (None, Some(late_fired)));
        let late_after_render = state(6, 2, 0, 2, 0, 1, 0, 0);
        validate(
            &late_after_fire,
            &late_after_render,
            "RenderCost",
            "overloaded timer-driven present cost",
        );

        let late_last_fire = late_anchor.take();
        let late_deadline =
            phase_locked_effect_deadline(late_presented, interval, late_last_fire, None, true);
        late_next = Some(late_deadline);
        assert_eq!(late_next, Some(late_deadline));
        assert_eq!(
            late_deadline,
            late_base + Duration::from_millis(8),
            "overload starts one fresh full interval"
        );
        let late_rearmed = state(6, 2, 6, 8, 1, 0, 0, 1);
        validate(
            &late_after_render,
            &late_rearmed,
            "Rearm",
            "overloaded event-loop trail rearm",
        );

        // A catch-up rearm at the already-passed phase slot is the overload
        // counterpart to the completion-relative slide above.
        let catch_up = state(6, 2, 2, 4, 1, 0, 0, 1);
        let (ok, _) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &late_after_render,
            &catch_up,
            Some("Rearm"),
            "event-loop catch-up negative control",
        );
        assert!(!ok, "an already-due catch-up rearm must be rejected");
    }
}

/// Derive the smallest sound logical repaint from two products of the pure
/// native compiler. Each paint node is individually clipped to `node.clip`, so
/// a content-only change can be bounded by its stable clip. Any node-set,
/// ordering, or geometry change is reflow and deliberately promotes to `All`.
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
        include(before.clip);
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
    // `All` is an authority boundary, not a hint.  Settings route/service
    // reducers use it when a semantic transition can alter several retained
    // regions at once (for example Top Settings -> Packages).  Narrowing that
    // request to the compiler's bounding rectangle made the transparent tile
    // replace unchanged sidebar pixels with zeroes while the compiled semantic
    // tree still reported every route.  Local animations request `Rect`
    // explicitly and retain the regional fast path below.
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

/// Resolve whether one retained native leaf needs work and, when it does, the
/// narrowest reducer-declared damage that remains sound for its live identity.
/// Geometry, service, appearance/theme/font, and document changes are encoded in
/// the compile stamp and therefore widen a local request to a full leaf.
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

/// Lower one semantic leaf into its retained device-pixel allocation. This is
/// the single patch/full policy for both ordinary native tabs and native leaves
/// inside heterogeneous split trees.
fn retain_native_leaf_raster(
    cache: &mut crate::LeafRenderCache,
    scene: crate::app_native::NativeLeafScene,
    width: u32,
    height: u32,
    scale: f32,
    theme: Theme,
) {
    let effective_damage = cache
        .native
        .as_ref()
        .and_then(|raster| compiled_native_paint_damage(&raster.compiled, &scene.compiled))
        .map_or(scene.damage, |derived| {
            union_native_render_damage(scene.damage, derived)
        });
    let tray = scene.compiled.tray(theme, 13.0);
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
                raster.presented = false;
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
            rgba,
            width,
            height,
            presented_x: 0,
            presented_y: 0,
            presented: false,
            last_work: crate::NativeRasterWork::Full { pixels },
            full_rasters,
            regional_rasters,
        });
    }
    cache.native_damage = None;
}

#[cfg(test)]
mod native_damage_tests {
    use super::*;

    fn assert_bytes_outside_region_unchanged(
        before: &[u8],
        after: &[u8],
        width: u32,
        height: u32,
        region: crate::tray_raster::RasterRect,
    ) {
        for y in 0..height {
            for x in 0..width {
                if x >= region.x
                    && x < region.x + region.width
                    && y >= region.y
                    && y < region.y + region.height
                {
                    continue;
                }
                let index = ((y * width + x) * 4) as usize;
                assert_eq!(
                    &after[index..index + 4],
                    &before[index..index + 4],
                    "regional repaint changed device pixel ({x}, {y}) outside its tile"
                );
            }
        }
    }

    fn paint(
        key: &str,
        rect: crate::native_ui::LogicalRect,
        text: &str,
    ) -> crate::native_ui::PaintNode {
        crate::native_ui::PaintNode {
            key: crate::native_ui::UiKey::new(key),
            rect,
            clip: rect,
            content: crate::native_ui::UiContent::Text(crate::native_ui::TextSpec::text(text)),
        }
    }

    #[test]
    fn compiler_diff_narrows_unknown_damage_to_changed_paint_bounds() {
        let bounds = crate::native_ui::LogicalRect::new(0.0, 0.0, 800.0, 600.0);
        let unchanged = paint(
            "unchanged",
            crate::native_ui::LogicalRect::new(24.0, 24.0, 160.0, 32.0),
            "Stable",
        );
        let changed_rect = crate::native_ui::LogicalRect::new(212.25, 91.5, 180.5, 44.25);
        let before = crate::native_ui::CompiledUi {
            bounds,
            paint: vec![unchanged.clone(), paint("changed", changed_rect, "Before")],
            ..crate::native_ui::CompiledUi::default()
        };
        let after = crate::native_ui::CompiledUi {
            bounds,
            paint: vec![unchanged, paint("changed", changed_rect, "After")],
            ..crate::native_ui::CompiledUi::default()
        };
        assert_eq!(
            compiled_native_paint_damage(&before, &after),
            Some(crate::native_app::DamageRegion::Rect {
                x: 212,
                y: 91,
                width: 181,
                height: 45,
            })
        );
        assert_eq!(
            union_native_render_damage(
                crate::native_app::DamageRegion::All,
                compiled_native_paint_damage(&before, &after).unwrap(),
            ),
            crate::native_app::DamageRegion::All,
            "a reducer's full semantic repaint must never become a transparent tile patch"
        );
        assert_eq!(
            union_native_render_damage(
                crate::native_app::DamageRegion::Rect {
                    x: 220,
                    y: 100,
                    width: 8,
                    height: 9,
                },
                compiled_native_paint_damage(&before, &after).unwrap(),
            ),
            crate::native_app::DamageRegion::Rect {
                x: 212,
                y: 91,
                width: 181,
                height: 45,
            },
            "an explicitly local repaint still uses the compiler-derived tile"
        );
    }

    #[test]
    fn desktop_settings_route_repaint_keeps_full_damage_authoritative() {
        let bounds = crate::native_ui::LogicalRect::new(0.0, 0.0, 1_200.0, 810.0);
        let stable_sidebar = paint(
            "settings/navigation",
            crate::native_ui::LogicalRect::new(0.0, 0.0, 216.0, 810.0),
            "Settings navigation",
        );
        let route_rect = crate::native_ui::LogicalRect::new(216.0, 0.0, 984.0, 810.0);
        let before = crate::native_ui::CompiledUi {
            bounds,
            paint: vec![
                stable_sidebar.clone(),
                paint("settings/page", route_rect, "Top Settings"),
            ],
            ..crate::native_ui::CompiledUi::default()
        };
        let after = crate::native_ui::CompiledUi {
            bounds,
            paint: vec![
                stable_sidebar,
                paint("settings/page", route_rect, "Packages"),
            ],
            ..crate::native_ui::CompiledUi::default()
        };
        let derived = compiled_native_paint_damage(&before, &after).unwrap();
        assert!(matches!(
            derived,
            crate::native_app::DamageRegion::Rect { .. }
        ));
        assert_eq!(
            union_native_render_damage(crate::native_app::DamageRegion::All, derived),
            crate::native_app::DamageRegion::All,
            "Top -> Packages at the shipped desktop viewport must repaint the retained page, including its sidebar"
        );

        // Bind the policy check to the real Settings reducer and retained
        // raster path as well.  The route action asks for a full repaint, and
        // the next preparation must perform one instead of turning the page
        // diff into a transparent regional patch.
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).expect("native Settings view");
        assert!(app.prepare_native_input_scratch(wid));
        let full_rasters = app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .expect("Top Settings retained raster")
            .full_rasters;

        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Packages));
        assert!(app.prepare_native_input_scratch(wid));
        let packages = app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .expect("Packages retained raster");
        assert!(matches!(
            packages.last_work,
            crate::NativeRasterWork::Full { .. }
        ));
        assert_eq!(packages.full_rasters, full_rasters + 1);
    }

    #[test]
    fn compiler_diff_promotes_reflow_to_full_damage() {
        let bounds = crate::native_ui::LogicalRect::new(0.0, 0.0, 400.0, 300.0);
        let before = crate::native_ui::CompiledUi {
            bounds,
            paint: vec![paint(
                "moving",
                crate::native_ui::LogicalRect::new(10.0, 20.0, 40.0, 30.0),
                "Move",
            )],
            ..crate::native_ui::CompiledUi::default()
        };
        let after = crate::native_ui::CompiledUi {
            bounds,
            paint: vec![paint(
                "moving",
                crate::native_ui::LogicalRect::new(110.0, 80.0, 40.0, 30.0),
                "Move",
            )],
            ..crate::native_ui::CompiledUi::default()
        };
        assert_eq!(
            compiled_native_paint_damage(&before, &after),
            Some(crate::native_app::DamageRegion::All)
        );
    }

    #[test]
    fn single_native_retains_and_patches_one_device_tile() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).expect("native Settings view");
        assert!(app.prepare_native_input_scratch(wid));

        let initial = app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .expect("ordinary native tab has one retained leaf raster");
        let allocation = initial.rgba.as_ptr() as usize;
        let before = initial.rgba.clone();
        let full_rasters = initial.full_rasters;
        let regional_rasters = initial.regional_rasters;
        app.invalidate_native_view_cache(
            wid,
            view,
            crate::native_app::DamageRegion::Rect {
                x: 7,
                y: 11,
                width: 19,
                height: 23,
            },
        );

        assert!(app.prepare_native_input_scratch(wid));
        let patched = app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .expect("retained raster after local damage");
        assert_eq!(
            patched.rgba.as_ptr() as usize,
            allocation,
            "single-native regional work must patch the retained allocation"
        );
        let crate::NativeRasterWork::Region { rect, pixels } = patched.last_work else {
            panic!("single-native local damage widened to a full raster")
        };
        assert_eq!(pixels, rect.pixels());
        assert!(pixels < u64::from(patched.width) * u64::from(patched.height));
        assert_eq!(patched.full_rasters, full_rasters);
        assert_eq!(patched.regional_rasters, regional_rasters + 1);
        assert_eq!(
            app.windows[&wid]
                .settings_card
                .as_ref()
                .expect("presented native card after regional work")
                .rgba,
            patched.rgba,
            "the settings_card presentation mirrors the newly patched retained leaf"
        );
        assert_bytes_outside_region_unchanged(
            &before,
            &patched.rgba,
            patched.width,
            patched.height,
            rect,
        );

        let full_before_theme = patched.full_rasters;
        let regional_before_theme = patched.regional_rasters;
        app.theme.fg ^= 0x0001_0101;
        app.invalidate_native_view_cache(
            wid,
            view,
            crate::native_app::DamageRegion::Rect {
                x: 7,
                y: 11,
                width: 19,
                height: 23,
            },
        );
        assert!(app.prepare_native_input_scratch(wid));
        let themed = app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .expect("theme-changed raster");
        assert!(matches!(
            themed.last_work,
            crate::NativeRasterWork::Full { .. }
        ));
        assert_eq!(themed.full_rasters, full_before_theme + 1);
        assert_eq!(themed.regional_rasters, regional_before_theme);

        let full_before_geometry = themed.full_rasters;
        let regional_before_geometry = themed.regional_rasters;
        app.windows.get_mut(&wid).unwrap().rows += 1;
        app.invalidate_native_view_cache(
            wid,
            view,
            crate::native_app::DamageRegion::Rect {
                x: 7,
                y: 11,
                width: 19,
                height: 23,
            },
        );
        assert!(app.prepare_native_input_scratch(wid));
        let resized = app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .expect("geometry-changed raster");
        assert!(matches!(
            resized.last_work,
            crate::NativeRasterWork::Full { .. }
        ));
        assert_eq!(resized.full_rasters, full_before_geometry + 1);
        assert_eq!(resized.regional_rasters, regional_before_geometry);
    }

    #[test]
    fn settings_preview_tick_patches_only_the_retained_preview_band() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::CursorMotion));
        let (_, view) = app.active_native_view(wid).expect("native Settings view");
        assert!(app.prepare_native_input_scratch(wid));
        let before = app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .expect("retained Cursor & Motion raster");
        let full_rasters = before.full_rasters;
        let regional_rasters = before.regional_rasters;
        let total_pixels = u64::from(before.width) * u64::from(before.height);
        let before_pixels = before.rgba.clone();

        assert!(
            app.invalidate_active_native_settings_preview(wid, 720),
            "the default live cursor preview is ticking"
        );
        assert!(app.prepare_native_input_scratch(wid));
        let ticked = app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .expect("retained raster after preview tick");
        let crate::NativeRasterWork::Region { rect, pixels } = ticked.last_work else {
            panic!("a preview tick widened to a full-page raster")
        };
        assert!(pixels < total_pixels);
        assert_eq!(ticked.full_rasters, full_rasters);
        assert_eq!(ticked.regional_rasters, regional_rasters + 1);
        assert_bytes_outside_region_unchanged(
            &before_pixels,
            &ticked.rgba,
            ticked.width,
            ticked.height,
            rect,
        );
    }

    #[test]
    fn appearance_paint_identity_forces_full_native_raster() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (instance, view) = app.active_native_view(wid).expect("native Settings view");
        assert!(app.prepare_native_input_scratch(wid));
        let mut cache = app
            .windows
            .get_mut(&wid)
            .unwrap()
            .leaf_render_cache
            .remove(&view)
            .unwrap();
        let retained = cache.native.as_ref().expect("initial retained raster");
        let previous_stamp = retained.stamp;
        let compiled = retained.compiled.clone();
        let width = retained.width;
        let height = retained.height;
        let full_rasters = retained.full_rasters;
        let regional_rasters = retained.regional_rasters;
        let appearance_stamp = crate::app_native::NativeUiCompileStamp {
            // Native appearance preferences are folded into paint_revision.
            // Vary that identity directly so this test remains isolated from the
            // process-global platform preference observer used by parallel tests.
            paint_revision: previous_stamp.paint_revision.wrapping_add(1),
            ..previous_stamp
        };
        let viewport = app.native_ui_viewport(wid).unwrap();

        retain_native_leaf_raster(
            &mut cache,
            crate::app_native::NativeLeafScene {
                stamp: appearance_stamp,
                instance,
                view,
                viewport,
                damage: crate::native_app::DamageRegion::Rect {
                    x: 7,
                    y: 11,
                    width: 19,
                    height: 23,
                },
                compiled,
            },
            width,
            height,
            1.0,
            app.theme,
        );
        let raster = cache.native.as_ref().unwrap();
        assert!(matches!(
            raster.last_work,
            crate::NativeRasterWork::Full { .. }
        ));
        assert_eq!(raster.full_rasters, full_rasters + 1);
        assert_eq!(raster.regional_rasters, regional_rasters);
    }

    #[test]
    fn heterogeneous_modal_changes_fingerprint_and_is_painted_last() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, native_view) = app.active_native_view(wid).expect("native Settings view");
        let (_, terminal_view) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        let plan = app
            .active_visible_leaf_plan(wid)
            .expect("mixed visible plan");
        let native_leaf = plan.leaf(native_view).expect("native leaf").clone();
        let terminal_leaf = plan.leaf(terminal_view).expect("terminal leaf").clone();
        assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
        let base = app.windows[&wid]
            .settings_card
            .as_ref()
            .expect("mixed native tray");
        let base_fp = base.fp;
        let base_rgba = base.rgba.clone();
        let (width, height) = (base.pw, base.ph);

        app.palette_enter();
        assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
        let combined = app.windows[&wid]
            .settings_card
            .as_ref()
            .expect("mixed native + modal tray");
        assert_ne!(
            combined.fp, base_fp,
            "modal fingerprint joins the cache key"
        );
        assert_ne!(combined.rgba, base_rgba, "modal changes captured pixels");

        let (cw, ch) = app.win_cell_size(wid);
        let cols = usize::from(app.windows[&wid].cols);
        let scale = app.windows[&wid].scale.max(f64::EPSILON) as f32;
        let mut modal_prims = Vec::new();
        assert!(app.append_native_modal_prims(wid, &mut modal_prims, 0.0, (cw, ch, cols), scale,));
        let modal = crate::tray_raster::rasterize_tray_pixels(
            &modal_prims,
            width,
            height,
            scale,
            [0, 0, 0, 0],
        );
        let device_rect = |leaf: crate::tab_model::VisibleLeaf| {
            (
                (leaf.rect.origin.x * cw as f32).round() as u32,
                (leaf.rect.origin.y * ch as f32).round() as u32,
                (leaf.rect.size.width * cw as f32).round() as u32,
                (leaf.rect.size.height * ch as f32).round() as u32,
            )
        };
        let find_modal_pixel = |leaf: crate::tab_model::VisibleLeaf, opaque: bool| {
            let (x, y, w, h) = device_rect(leaf);
            (y..y.saturating_add(h).min(height)).find_map(|py| {
                (x..x.saturating_add(w).min(width)).find_map(|px| {
                    let index = ((py * width + px) * 4) as usize;
                    let alpha = modal[index + 3];
                    (alpha > 0
                        && (!opaque || alpha == u8::MAX)
                        && modal[index..index + 4] != base_rgba[index..index + 4])
                        .then_some(index)
                })
            })
        };
        let terminal_pixel = find_modal_pixel(terminal_leaf, false)
            .expect("palette paints over the terminal lane as well as native leaves");
        assert_eq!(
            &combined.rgba[terminal_pixel..terminal_pixel + 4],
            &modal[terminal_pixel..terminal_pixel + 4],
            "transparent terminal lane receives the modal raster"
        );
        let native_pixel = find_modal_pixel(native_leaf, true)
            .expect("an opaque palette pixel overlaps the native leaf");
        assert_eq!(
            &combined.rgba[native_pixel..native_pixel + 4],
            &modal[native_pixel..native_pixel + 4],
            "opaque modal pixels replace native pixels, proving last-paint order"
        );
    }
}

/// How long after the last REPAINT-BLINK (`Terminal::repaint_blink_epoch`
/// advance — a DECTCEM hide inside a DEC-2026 synchronized update, Claude
/// Code's per-keystroke repaint bracket) the alt-screen ERASE-POOF row probe
/// stays enabled. Comfortably spans a kill press landing during a lull in the
/// app's repaints (the blink is stamped by the app's PREVIOUS burst) while
/// still shutting the probe off promptly when a non-blinking alt-screen app
/// (vim/less) takes over the same pane. Shared with the headless capture path
/// (`app_introspect`), which mirrors this LOCK A derivation exactly.
pub(crate) const BLINK_RECENT_MAX: Duration = Duration::from_secs(1);

/// Resolve the companion's local terminal palette from every grid cell its
/// prospective sprite intersects. The explicit cap keeps this cold emission
/// path allocation-free and O(1), even under degenerate cell metrics.
pub(crate) fn cursor_cat_color_key(
    cells: &[Vec<RenderCell>],
    geom: aterm_effects::word_decorations::EffectGeom,
    footprint: aterm_effects::word_decorations::CatFootprint,
    fallback_bg: u32,
    fallback_fg: u32,
    fallback_accent: u32,
) -> aterm_effects::cat_baker::CatColorKey {
    const MAX_SAMPLES: u32 = 64;
    if geom.cell_w == 0 || geom.cell_h == 0 || cells.is_empty() {
        return aterm_effects::cat_baker::CatColorKey::from_rgb(
            fallback_bg,
            fallback_fg,
            fallback_accent,
        );
    };

    let cw = i64::from(geom.cell_w);
    let ch = i64::from(geom.cell_h);
    let x0 = i64::from(footprint.x).max(0);
    let y0 = i64::from(footprint.y).max(0);
    let x1 = (i64::from(footprint.x) + i64::from(footprint.w)).min(i64::from(geom.cols) * cw);
    let y1 = (i64::from(footprint.y) + i64::from(footprint.h)).min(i64::from(geom.rows) * ch);
    if x1 <= x0 || y1 <= y0 {
        return aterm_effects::cat_baker::CatColorKey::from_rgb(
            fallback_bg,
            fallback_fg,
            fallback_accent,
        );
    }
    let c0 = usize::try_from(x0 / cw).unwrap_or(0);
    let c1 = usize::try_from((x1 - 1) / cw).unwrap_or(usize::MAX);
    let r0 = usize::try_from(y0 / ch).unwrap_or(0);
    let r1 = usize::try_from((y1 - 1) / ch).unwrap_or(usize::MAX);

    let mut bg_sum = [0u32; 3];
    let mut fg_sum = [0u32; 3];
    let mut sampled = 0u32;
    let mut visible = 0u32;
    let mut min_background_band = 3u8;
    let mut max_background_band = 0u8;
    let mut principal_fg = None;
    let fallback_bg_rgb = [
        (fallback_bg >> 16) as u8,
        (fallback_bg >> 8) as u8,
        fallback_bg as u8,
    ];
    let pack =
        |rgb: [u8; 3]| (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2]);
    'rows: for line in (r0..=r1).map(|row| cells.get(row)) {
        for col in c0..=c1 {
            let cell = line.and_then(|line| line.get(col));
            let background_rgb = cell.map_or(fallback_bg_rgb, |cell| cell.bg);
            let background = pack(background_rgb);
            let band = aterm_effects::cat_baker::CatColorKey::background_band(background);
            min_background_band = min_background_band.min(band);
            max_background_band = max_background_band.max(band);
            for (dst, src) in bg_sum.iter_mut().zip(background_rgb) {
                *dst += u32::from(src);
            }
            sampled += 1;
            if let Some(cell) = cell
                && !cell.wide
                && !cell.ch.is_whitespace()
                && cell.ch != '\0'
            {
                principal_fg.get_or_insert(cell.fg);
                for (dst, src) in fg_sum.iter_mut().zip(cell.fg) {
                    *dst += u32::from(src);
                }
                visible += 1;
            }
            if sampled == MAX_SAMPLES {
                break 'rows;
            }
        }
    }
    if sampled == 0 {
        return aterm_effects::cat_baker::CatColorKey::from_rgb(
            fallback_bg,
            fallback_fg,
            fallback_accent,
        );
    }
    let background = pack(bg_sum.map(|channel| (channel / sampled) as u8));
    let foreground = principal_fg.map_or(fallback_fg, pack);
    let surrounding = if visible == 0 {
        fallback_accent
    } else {
        pack(fg_sum.map(|channel| (channel / visible) as u8))
    };
    aterm_effects::cat_baker::CatColorKey::from_rgb_span(
        background,
        foreground,
        surrounding,
        min_background_band,
        max_background_band,
    )
}

/// Whether ordinary forward-typing momentum is allowed to own the cursor cat.
///
/// The trail master owns the Nyan companion just as surely as it owns the
/// ribbon behind it.  Collection/typed hellos deliberately do not use this
/// predicate: they are bounded, independently promised presentations and the
/// host ORs `collection_hello` around this ordinary-flight gate.
#[inline]
pub(crate) fn ordinary_nyan_cursor_cat_enabled(
    cursor_trail_enabled: bool,
    style: crate::cursor_glow::GlowStyle,
) -> bool {
    cursor_trail_enabled && matches!(style, crate::cursor_glow::GlowStyle::Nyan)
}

/// Consume one echo-correlated cursor pulse and forward it only when the Nyan
/// companion's trail owner is enabled. The pulse is consumed even while gated
/// off so toggling the owner cannot replay stale typing.
#[inline]
pub(crate) fn forward_nyan_cursor_cat_momentum(
    cursor_trail_enabled: bool,
    style: crate::cursor_glow::GlowStyle,
    pulse: Option<Instant>,
    cat: &mut crate::nyan_cursor::CursorCat,
) {
    if ordinary_nyan_cursor_cat_enabled(cursor_trail_enabled, style)
        && let Some(at) = pulse
    {
        cat.on_key(at, true);
    }
}

/// Exact host presentation gate shared by glass and styled captures. A
/// collection hello bypasses both the ordinary trail owner and the animation
/// requirement; an ordinary flight must satisfy both.
#[inline]
pub(crate) fn cursor_cat_presentation_enabled(
    animate_cat: bool,
    cursor_trail_enabled: bool,
    style: crate::cursor_glow::GlowStyle,
    collection_hello: bool,
) -> bool {
    collection_hello
        || (animate_cat && ordinary_nyan_cursor_cat_enabled(cursor_trail_enabled, style))
}

#[cfg(test)]
mod cursor_cat_context_tests {
    use super::*;
    use aterm_core::terminal::UnderlineStyle;

    fn cell(ch: char, fg: [u8; 3], bg: [u8; 3]) -> RenderCell {
        RenderCell {
            ch,
            fg,
            bg,
            wide: false,
            emoji_presentation: false,
            bold: false,
            italic: false,
            underline: UnderlineStyle::None,
            strikethrough: false,
            overline: false,
            underline_color: None,
        }
    }

    #[test]
    fn cursor_companion_samples_its_actual_multiline_footprint() {
        let geom = aterm_effects::word_decorations::EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 2,
            cols: 3,
        };
        let footprint = aterm_effects::word_decorations::CatFootprint {
            x: 0,
            y: 0,
            w: 30,
            h: 40,
        };
        let make = |neighbor, top_bg, bottom_bg| {
            vec![
                vec![cell('x', neighbor, top_bg); 3],
                vec![cell(' ', [240, 240, 240], bottom_bg); 3],
            ]
        };
        let red = cursor_cat_color_key(
            &make([255, 20, 20], [8, 8, 8], [8, 8, 8]),
            geom,
            footprint,
            0,
            0x00FF_FFFF,
            0,
        );
        let blue = cursor_cat_color_key(
            &make([20, 80, 255], [8, 8, 8], [8, 8, 8]),
            geom,
            footprint,
            0,
            0x00FF_FFFF,
            0,
        );
        let light = cursor_cat_color_key(
            &make([255, 20, 20], [248, 248, 248], [248, 248, 248]),
            geom,
            footprint,
            0,
            0x00FF_FFFF,
            0,
        );
        let mixed = cursor_cat_color_key(
            &make([255, 20, 20], [4, 4, 4], [248, 248, 248]),
            geom,
            footprint,
            0,
            0x00FF_FFFF,
            0,
        );
        assert_ne!(
            red.accent, blue.accent,
            "neighbor text changes the hue family"
        );
        assert_ne!(
            red.background, light.background,
            "backgrounds across the sprite footprint change contrast ink"
        );
        assert_eq!(
            mixed.background, 4,
            "a dark+light footprint must not collapse to its RGB-average band"
        );
    }

    #[test]
    fn cursor_companion_ignores_alternating_backgrounds_outside_its_footprint() {
        let geom = aterm_effects::word_decorations::EffectGeom {
            cell_w: 10,
            cell_h: 10,
            rows: 2,
            cols: 4,
        };
        let footprint = aterm_effects::word_decorations::CatFootprint {
            x: 20,
            y: 0,
            w: 20,
            h: 20,
        };
        let cells = vec![
            vec![
                cell('x', [255, 0, 0], [255, 255, 255]),
                cell('x', [255, 0, 0], [255, 255, 255]),
                cell('x', [0, 0, 255], [4, 4, 4]),
                cell('x', [0, 0, 255], [4, 4, 4]),
            ];
            2
        ];
        let sampled = cursor_cat_color_key(&cells, geom, footprint, 0, 0, 0);
        assert!(
            sampled.dark(),
            "only the dark right-hand footprint is sampled"
        );
    }
}

/// The redraw early-out decision (D-1), as a PURE function so it is unit
/// testable without a window/event loop.
///
/// Returns `true` (must repaint) iff this is the first frame (`prev` is `None`)
/// or any presented-state term changed since the last present. Returns `false`
/// (skip the extract + rasterize + present) only when the previously presented
/// key is byte-identical to the current one — i.e. a steady screen with the same
/// blink phase, no bell flash, the same selection and cursor override. This is
/// what eliminates the steady-screen and blink-only-wake full-frame redraws.
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "AsymmetricPadLayout",
        action = "RenderWithLayoutCache",
        project = "aterm_gui::app_render::project_asymmetric_pad_layout_key"
    )
)]
pub(crate) fn should_repaint(prev: Option<RepaintKey>, cur: RepaintKey) -> bool {
    prev != Some(cur)
}

/// A requested recovery frame bypasses the content-key cache until a real
/// present/drop acknowledges it. Shared by single-pane and split composition so
/// a raw resize/de-occlusion cannot be swallowed merely because terminal cells
/// stayed byte-identical.
#[inline]
pub(crate) fn should_repaint_or_recover(
    prev: Option<RepaintKey>,
    cur: RepaintKey,
    recovery_redraw_outstanding: bool,
) -> bool {
    recovery_redraw_outstanding || should_repaint(prev, cur)
}

/// Concrete GUI projection named by the asymmetric-layout refinement anchors.
/// It reads the load-bearing field directly from the exact key the redraw gate
/// stores and compares; no parallel fingerprint can drift from it.
#[cfg(test)]
pub(crate) fn project_asymmetric_pad_layout_key(key: &RepaintKey) -> usize {
    key.grid_top
}

/// Async fallback-face convergence action (PURE, so it is unit-tested without a
/// window/GPU). A font zoom / config reload rebuilds the renderer, which
/// re-parses the broad-Unicode + symbol fallback faces on a BACKGROUND thread;
/// until they land, uncovered code points (bullets/icons like Claude Code's
/// `⏺`/`●`) render as provisional `.notdef` BOXES. Given whether a parse is in
/// flight THIS present (`pending`) and whether one was in flight at the PREVIOUS
/// present (`was`), returns `(rearm, invalidate)`:
///
/// * `rearm` — request another redraw AND clear the window's `last_present`.
///   The clear is load-bearing: the content early-out ([`should_repaint`] and
///   the `redraw_compose` twin) has no `RepaintKey` term for the parse state or
///   the renderer's `font_epoch`, so a bare `request_redraw` on an OTHERWISE-
///   IDLE screen is swallowed by that gate and the poll loop dies after ONE
///   iteration — the parse is never re-polled/installed and the tofu boxes are
///   stranded until unrelated activity (a keystroke / PTY output). This is
///   exactly the "font zoom turns bullets into boxes" bug on a settled screen.
///   (Startup converged only because its screen was still busy.)
/// * `invalidate` — on the pending→landed FALLING EDGE only, drop the GPU
///   present cache so the re-resolved glyphs actually reach glass (the per-
///   window damage diff cannot see `font_epoch`, so it would gate-hit the
///   unchanged-content tofu rows otherwise).
///
/// Steady state (`pending == was == false`) is `(false, false)` — one branch,
/// no per-present work.
#[must_use]
pub(crate) fn fallback_convergence_action(pending: bool, was: bool) -> (bool, bool) {
    let converging = pending || was;
    (converging, converging && !pending)
}

/// VI-1: map a vi cursor grid `line` (live-top-relative; negative = scrolled into
/// history) to a VISIBLE screen row, given the `display_offset` (rows the viewport is
/// scrolled up from the live bottom) and the pane's `rows`. `None` when the vi cursor
/// sits off the current viewport (then the normal cursor is shown). Pure, so the
/// coordinate law is unit-tested without a window.
#[must_use]
fn vi_screen_row(vi_line: i32, display_offset: i32, rows: usize) -> Option<usize> {
    let r = vi_line + display_offset;
    (r >= 0 && (r as usize) < rows).then_some(r as usize)
}

/// SYNC-1 pure frame-hold decision for DEC-2026 synchronized output. Given the pane's
/// current sync state (`sync_active`), whether it was active on the previous present
/// (`was_active`), the currently-armed release deadline (`armed`), the present's clock
/// (`now`), the safety-valve `timeout`, and the terminal's monotonic sync-CLOSE
/// counter (`end_seq`, vs the value recorded at arm time `armed_end_seq`), returns
/// `(new_deadline, hold, new_armed_end_seq)`:
///
/// * **Arm** on the FALSE→TRUE rising edge (`sync_active && !was_active`) at
///   `now + timeout` — the first held frame of an episode — recording `end_seq`.
/// * **Hold** (`hold == true`) while armed AND unexpired (`now < deadline`): the caller
///   skips the present so the app's multi-write update lands tear-free.
/// * **Close-and-re-arm** when the level still samples TRUE but `end_seq` advanced:
///   the bracket the hold was armed for CLOSED (a complete frame is ready) and a NEW
///   bracket is already open — the batch ended mid-bracket. PRESENT (`hold == false`)
///   and arm afresh for the new episode. This is the case the level alone cannot see:
///   an app bracketing every repaint under flood keeps the sampled level true across
///   presents, and treating that as ONE episode pins presents to ~1/`timeout` — the
///   whole-window freeze. The close counter restores one present per closed bracket.
/// * **Release** — `hold == false` and `new_deadline == None` — when sync ends (`?2026l`
///   ⇒ `!sync_active`, released even mid-window) OR the deadline passes. The deadline is
///   cleared on release so `about_to_wait` never folds a stale PAST instant (which would
///   busy-spin the event loop), and — because arming is rising-edge-only — a stuck-sync
///   app that timed out presents normally afterward instead of re-holding every `timeout`
///   (until its bracket genuinely closes, which reads as a fresh episode via `end_seq`).
///
/// Pure over its inputs, so the state machine is unit-tested without a present target.
#[must_use]
fn sync_frame_hold(
    sync_active: bool,
    was_active: bool,
    armed: Option<std::time::Instant>,
    now: std::time::Instant,
    timeout: std::time::Duration,
    end_seq: u64,
    armed_end_seq: u64,
) -> (Option<std::time::Instant>, bool, u64) {
    if !sync_active {
        return (None, false, end_seq);
    }
    if was_active && end_seq != armed_end_seq {
        // The armed bracket closed and another opened within one batch: present
        // the completed frame now, arm for the new episode.
        return (Some(now + timeout), false, end_seq);
    }
    let mut deadline = if !was_active {
        Some(now + timeout) // rising edge: arm
    } else {
        armed // still active mid-episode: keep the armed deadline (may be expired)
    };
    let hold = deadline.is_some_and(|d| now < d);
    if !hold {
        deadline = None; // released: no stale past-deadline for about_to_wait to fold
    }
    (deadline, hold, end_seq)
}

#[cfg(test)]
mod vi_render_tests {
    use super::vi_screen_row;

    /// VI-1: the vi cursor grid line → visible screen row map. `screen = line +
    /// display_offset`, drawn only when it lands inside `[0, rows)` (else the normal
    /// cursor shows).
    #[test]
    fn vi_screen_row_maps_line_through_display_offset() {
        // At the bottom (offset 0): live top line 0 → row 0; line rows-1 → last row.
        assert_eq!(vi_screen_row(0, 0, 24), Some(0));
        assert_eq!(vi_screen_row(23, 0, 24), Some(23));
        // A scrollback line (negative) is off-viewport at offset 0 → not drawn.
        assert_eq!(vi_screen_row(-3, 0, 24), None);
        // Scrolled up 5: that same scrollback line -3 now sits at row 2.
        assert_eq!(vi_screen_row(-3, 5, 24), Some(2));
        // A live line pushed past the bottom by the scroll → off-viewport.
        assert_eq!(vi_screen_row(23, 5, 24), None);
        // Exactly at the bottom edge is out of range (rows is exclusive).
        assert_eq!(vi_screen_row(24, 0, 24), None);
    }
}

#[cfg(test)]
mod sync_hold_tests {
    use super::sync_frame_hold;
    use std::time::{Duration, Instant};

    /// SYNC-1 frame-hold state machine: arm on the rising edge, hold while unexpired,
    /// release on `2026l` or timeout, no re-hold within an episode, re-arm on a fresh
    /// episode. Covers the full lifecycle the present path drives.
    #[test]
    fn sync_frame_hold_state_machine() {
        let t0 = Instant::now();
        let timeout = Duration::from_millis(1000);
        let d = |ms| t0 + Duration::from_millis(ms);

        // Inactive throughout: never holds, no deadline armed.
        assert_eq!(
            sync_frame_hold(false, false, None, t0, timeout, 0, 0),
            (None, false, 0)
        );

        // Rising edge (false->true): arm at now + timeout and HOLD, recording the
        // close counter at arm time.
        assert_eq!(
            sync_frame_hold(true, false, None, d(0), timeout, 7, 0),
            (Some(d(1000)), true, 7),
            "rising edge arms at now+timeout, holds, records end_seq"
        );

        // Sustained hold (active, not rising, unexpired, counter unmoved): keep the
        // SAME deadline, hold.
        assert_eq!(
            sync_frame_hold(true, true, Some(d(1000)), d(500), timeout, 7, 7),
            (Some(d(1000)), true, 7),
            "the deadline is not re-armed while holding"
        );

        // `2026l` mid-window (sync inactive before the deadline): release immediately.
        assert_eq!(
            sync_frame_hold(false, true, Some(d(1000)), d(500), timeout, 8, 7),
            (None, false, 8),
            "sync end releases the held frame at once"
        );

        // Timeout expiry (active, not rising, now >= deadline): release + disarm so
        // about_to_wait folds no stale PAST instant (which would busy-spin).
        assert_eq!(
            sync_frame_hold(true, true, Some(d(1000)), d(1000), timeout, 7, 7),
            (None, false, 7),
            "an expired hold releases and clears the deadline"
        );

        // After a timeout release, sync still active but NOT rising and no deadline: it
        // must NOT re-hold (a stuck-sync app presents normally, not at 1 Hz forever).
        assert_eq!(
            sync_frame_hold(true, true, None, d(1500), timeout, 7, 7),
            (None, false, 7),
            "no re-hold within an episode after a timeout"
        );

        // A NEW episode (sync dropped then set again → rising edge) re-arms.
        assert_eq!(
            sync_frame_hold(true, false, None, d(2000), timeout, 9, 7),
            (Some(d(3000)), true, 9),
            "a fresh 2026h re-arms the hold"
        );
    }

    /// The flood case the level-sampled machine could not see: an app bracketing
    /// every repaint in `2026h…2026l` under sustained PTY flood keeps the SAMPLED
    /// level true on every redraw (batches end mid-bracket), which used to alias
    /// every bracket into one episode and pin presents to ~1/timeout — the
    /// whole-window freeze. The close counter turns each closed bracket into a
    /// PRESENT + fresh arm, so glass keeps updating bracket by bracket.
    #[test]
    fn closed_bracket_presents_even_while_level_stays_true() {
        let t0 = Instant::now();
        let timeout = Duration::from_millis(150);
        let d = |ms| t0 + Duration::from_millis(ms);

        // Frame 0: rising edge — arm and hold (bracket #1 open, counter 0).
        let (armed, hold, seq) = sync_frame_hold(true, false, None, d(0), timeout, 0, 0);
        assert!(hold && armed == Some(d(150)) && seq == 0);

        // Frame 1 (16 ms later): bracket #1 CLOSED and bracket #2 opened inside one
        // reader batch — the level still samples true, but the counter advanced.
        // The completed frame must PRESENT, with a fresh deadline armed for #2.
        let (armed, hold, seq) = sync_frame_hold(true, true, armed, d(16), timeout, 1, seq);
        assert!(
            !hold,
            "a closed bracket presents even while the level reads true"
        );
        assert_eq!(
            armed,
            Some(d(166)),
            "a fresh episode is armed for bracket #2"
        );
        assert_eq!(seq, 1);

        // Frame 2: bracket #2 still open (counter unmoved) — hold under its deadline.
        let (armed2, hold, seq) = sync_frame_hold(true, true, armed, d(32), timeout, 1, seq);
        assert!(hold && armed2 == armed && seq == 1);

        // Frame 3: bracket #2 closed, #3 open — present again. Steady state: one
        // present per closed bracket, never a 1 Hz pin.
        let (_, hold, seq) = sync_frame_hold(true, true, armed2, d(48), timeout, 2, seq);
        assert!(!hold && seq == 2);
    }
}

#[cfg(test)]
mod fallback_convergence_tests {
    use super::fallback_convergence_action;

    // The rising edge + steady-pending frames: keep re-arming (which clears
    // `last_present`, forcing the frame past the content early-out) but do NOT
    // invalidate the GPU present cache yet — the real glyphs have not landed.
    #[test]
    fn rearms_without_invalidate_while_pending() {
        assert_eq!(
            fallback_convergence_action(true, false),
            (true, false),
            "first pending frame: re-arm, no invalidate"
        );
        assert_eq!(
            fallback_convergence_action(true, true),
            (true, false),
            "steady pending frame: re-arm, no invalidate"
        );
    }

    // The pending→landed FALLING EDGE: re-arm AND invalidate the GPU present
    // cache so the re-resolved glyphs reach glass (the damage diff cannot see
    // `font_epoch`). This is the frame that actually replaces the tofu boxes.
    #[test]
    fn invalidates_on_landing_edge() {
        assert_eq!(
            fallback_convergence_action(false, true),
            (true, true),
            "landing edge: re-arm AND invalidate"
        );
    }

    // Steady state — no parse in flight, none just landed — is a pure no-op, so
    // an idle screen never churns presents. This is the regression guard for the
    // "font zoom turns bullets into boxes" bug: the SECOND value staying paired
    // with re-arm proves a converging frame always clears `last_present`, so the
    // poll loop cannot die on an idle screen after one iteration.
    #[test]
    fn steady_state_is_a_noop() {
        assert_eq!(
            fallback_convergence_action(false, false),
            (false, false),
            "not converging: no re-arm, no invalidate"
        );
    }
}

/// Coalesce a per-row dirty set into inclusive `(first, last)` runs of
/// consecutive dirty rows — the CPU present path copies one surface band and
/// reports one damage rect per run. A PURE function (no window, no renderer)
/// so the run computation the damage rects rely on is unit testable.
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

/// The `RepaintKey::system_dark` term (and the settings preview's
/// `PreviewCtx::system_dark`), as a PURE function of the App-tracked OS
/// appearance (`sync_app_theme_to_appearance`). The ONE population source for
/// every key/ctx construction site — `os_appearance_flip_repaints` pins that a
/// tracked appearance flip moves this term, so it cannot silently decay to a
/// constant (which would let the auto titlebar mock composite stale through an
/// OS appearance flip whenever the rest of the key is idle).
pub(crate) fn repaint_system_dark(appearance: aterm_types::Appearance) -> bool {
    appearance == aterm_types::Appearance::Dark
}

/// I-2: invert a frame's RGB in place when a visual-bell flash is `active`,
/// matching the on-screen present's invert (CPU `src ^ 0x00ff_ffff`; the GPU
/// blit shader does the same). Packed `0x00RRGGBB`, so XOR the low 24 bits and
/// leave the unused top byte clear. A no-op when no flash is active, so the
/// steady-screen snapshot path is byte-identical to before.
pub(crate) fn apply_bell_invert(frame: &mut Frame, active: bool) {
    if !active {
        return;
    }
    for px in &mut frame.pixels {
        *px ^= 0x00ff_ffff;
    }
}

/// Device-pixel thickness of the drop-target accent border, scaled to the window
/// and clamped so it stays a thin frame on small windows and never dominates a
/// large one.
fn drop_border_px(w: usize, h: usize) -> usize {
    (w.min(h) / 200).clamp(2, 6)
}

/// Alpha (out of 255) of the faint full-grid accent wash and the inset border.
const DROP_WASH_ALPHA: u32 = 28; // ~11% — readable content underneath
const DROP_BORDER_ALPHA: u32 = 235; // ~92% — a crisp but not harsh frame

/// The parameters of ONE inset-accent-border overlay pass: the drag-and-drop drop
/// target (fixed [`DROP_WASH_ALPHA`]/[`DROP_BORDER_ALPHA`]) OR the LEVEL-UP celebration
/// glow (a breathing alpha off [`crate::level_up`]). Threading a single descriptor
/// through the CPU present ([`apply_overlay_at`]), the GPU blit ([`aterm_gpu::DropOverlay`]),
/// and the SACRED `image`/`snapshot` compositor keeps on-glass == what an AI reads.
#[derive(Clone, Copy)]
pub(crate) struct OverlayGlow {
    /// Overlay accent (packed `0x00RRGGBB`; the top byte is ignored).
    pub(crate) accent: u32,
    /// Interior full-grid wash alpha (0..255).
    pub(crate) wash_a: u8,
    /// Inset-border alpha (0..255).
    pub(crate) border_a: u8,
}

/// Blend `fg` over `bg` (both packed `0x00RRGGBB`) at alpha `a` (0..=255), per
/// channel, leaving the top byte clear. `a == 0` returns `bg`; `a == 255` returns
/// `fg`. The canonical coverage blend (mirrors the renderer's private `blend`).
fn blend_rgb(bg: u32, fg: u32, a: u32) -> u32 {
    let inv = 255 - a;
    let r = (((bg >> 16) & 0xff) * inv + ((fg >> 16) & 0xff) * a) / 255;
    let g = (((bg >> 8) & 0xff) * inv + ((fg >> 8) & 0xff) * a) / 255;
    let b = ((bg & 0xff) * inv + (fg & 0xff) * a) / 255;
    (r << 16) | (g << 8) | b
}

/// Composite the drag-and-drop drop-target highlight over a packed `0x00RRGGBB`
/// framebuffer: a faint `accent` wash across the whole grid plus a near-opaque
/// `accent` border inset at the window edge (the chosen "inset accent border +
/// faint wash" treatment). `pixels` is row-major `w * h` (any trailing pixels are
/// ignored). Pure + allocation-free, and shared by the live CPU present and the
/// headless `image`/`snapshot` so on-glass and introspection match. The GPU
/// backend reproduces the same look in its blit shader.
pub(crate) fn apply_drop_overlay(pixels: &mut [u32], w: usize, h: usize, accent: u32) {
    apply_drop_overlay_at(pixels, w, h, 0, 0, w, h, accent);
}

/// W1 band-aware twin of [`apply_drop_overlay`]: composite the drop-target
/// highlight over the CONTENT frame (`fw`×`fh`, sitting at `(ox, oy)` — the
/// centred [`aterm_render::band_offset`], possibly negative on a transient
/// crop) inside a RAW-window-sized surface (`sw`×`sh`). Border thickness and
/// edge distances are frame-relative, matching the GPU blit shader (whose band
/// early-out means the highlight never touches the padding bands); drawing is
/// clipped to the surface. With `ox == oy == 0` and `sw/sh == fw/fh` this is
/// byte-identical to the historical whole-frame overlay.
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
    // The drag-and-drop drop target is the fixed-alpha instance of the general glow.
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
        },
    );
}

/// The alpha-parametrized CORE of the inset-accent-border overlay (the drop target's
/// fixed alphas OR the level-up glow's breathing alpha), band-aware exactly like
/// [`apply_drop_overlay_at`] and pure + allocation-free. With the drop-overlay constants
/// this is byte-identical to the historical fixed-alpha pass — that equivalence is
/// pinned by `band_aware_overlay_twins_shift_without_touching_bands`.
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
    let border = drop_border_px(fw, fh);
    let accent = glow.accent & 0x00ff_ffff;
    let (wash_a, border_a) = (u32::from(glow.wash_a), u32::from(glow.border_a));
    // The frame rows/cols visible on the surface (intersection; crop-safe).
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
            *px = blend_rgb(*px & 0x00ff_ffff, accent, a);
        }
    }
}

/// `off.max(0)` clamped into a `0..=len` surface coordinate.
fn ox_clamp(off: i64, len: usize) -> usize {
    off.clamp(0, len as i64) as usize
}

/// P3: composite the rasterized frosted Settings card (straight-alpha RGBA8, `cw*ch`
/// device px) over a packed `0x00RRGGBB` framebuffer at device offset `(ox, oy)`. Pure +
/// allocation-free src-over (`out = src*a + dst*(1-a)`), the per-pixel-alpha twin of
/// [`apply_drop_overlay`], so the live CPU present and the headless `image`/`snapshot`
/// share ONE compositor and on-glass == introspection. Fully clamped to the surface;
/// transparent card pixels (`a == 0`) leave the live terminal beneath untouched.
pub(crate) fn composite_tray(
    pixels: &mut [u32],
    fb_w: usize,
    fb_h: usize,
    card: &crate::SettingsCard,
) {
    composite_tray_at(pixels, fb_w, fb_h, 0, 0, card);
}

/// W1 band-aware twin of [`composite_tray`]: the card's `dx`/`dy` are FRAME
/// device coordinates, so on a raw-window-sized surface it lands shifted by the
/// frame's band offset `(ox, oy)` (possibly negative on a transient crop). With
/// `ox == oy == 0` this is byte-identical to the historical compositor. Fully
/// clamped to the surface, like the original.
pub(crate) fn composite_tray_at(
    pixels: &mut [u32],
    fb_w: usize,
    fb_h: usize,
    ox: i64,
    oy: i64,
    card: &crate::SettingsCard,
) {
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
            *slot = (r << 16) | (g << 8) | b;
        }
    }
}

#[allow(
    clippy::items_after_test_module,
    reason = "these unit tests sit next to the drop-overlay helpers they cover; the rest of the file is the App render inherent-impl, not stray items"
)]
#[cfg(test)]
mod drop_overlay_tests {
    use super::{
        DROP_WASH_ALPHA, apply_drop_overlay, apply_drop_overlay_at, blend_rgb, composite_tray,
        composite_tray_at, drop_border_px,
    };

    /// W1 regression: the band-aware overlay twins are byte-identical to the
    /// historical whole-frame compositors at offset 0, and at a band offset they
    /// paint the SAME frame-relative pixels shifted — never touching the bands.
    #[test]
    fn band_aware_overlay_twins_shift_without_touching_bands() {
        let accent = 0x0050_FA7B;
        let (fw, fh) = (40usize, 24usize);
        // Offset 0 == the historical compositor, byte-for-byte.
        let mut a = vec![0x0010_2030u32; fw * fh];
        let mut b = a.clone();
        apply_drop_overlay(&mut a, fw, fh, accent);
        apply_drop_overlay_at(&mut b, fw, fh, 0, 0, fw, fh, accent);
        assert_eq!(
            a, b,
            "offset 0 must be byte-identical to the legacy overlay"
        );

        // Frame at (3, 3) inside a +7px surface: the overlaid content equals the
        // offset-0 overlay shifted by the band, and every band pixel is untouched.
        let band = 0x0001_0203u32;
        let (sw, sh) = (fw + 7, fh + 7);
        let mut s = vec![band; sw * sh];
        for y in 0..fh {
            for x in 0..fw {
                s[(y + 3) * sw + (x + 3)] = 0x0010_2030;
            }
        }
        apply_drop_overlay_at(&mut s, sw, sh, 3, 3, fw, fh, accent);
        for y in 0..sh {
            for x in 0..sw {
                let got = s[y * sw + x];
                let (fx, fy) = (x as i64 - 3, y as i64 - 3);
                if fx >= 0 && fy >= 0 && (fx as usize) < fw && (fy as usize) < fh {
                    let want = a[fy as usize * fw + fx as usize];
                    assert_eq!(got, want, "shifted overlay at ({x},{y})");
                } else {
                    assert_eq!(got, band, "band at ({x},{y}) must be untouched");
                }
            }
        }

        // composite_tray_at: offset 0 == legacy; a band offset shifts the card.
        let card = crate::SettingsCard {
            rgba: vec![0xAA, 0xBB, 0xCC, 0xFF],
            pw: 1,
            ph: 1,
            dx: 1,
            dy: 0,
            fp: 0,
            geom: 0,
        };
        let mut t0 = vec![0u32; 3 * 2];
        composite_tray(&mut t0, 3, 2, &card);
        let mut t1 = vec![0u32; 3 * 2];
        composite_tray_at(&mut t1, 3, 2, 0, 0, &card);
        assert_eq!(t0, t1, "tray offset 0 must be byte-identical");
        let mut t2 = vec![0u32; 4 * 3];
        composite_tray_at(&mut t2, 4, 3, 1, 2, &card);
        assert_eq!(
            t2[2 * 4 + 2],
            0x00AA_BBCC,
            "card shifted by the band offset"
        );
        assert_eq!(t2.iter().filter(|&&p| p != 0).count(), 1);
        // A negative (crop) offset clips off-surface instead of panicking.
        let mut t3 = vec![0u32; 1];
        composite_tray_at(&mut t3, 1, 1, -2, -2, &card);
        assert_eq!(t3[0], 0);
    }

    /// src-over math: `a==255` ⇒ src, `a==0` ⇒ dst untouched, `a==128` ⇒ rounded blend;
    /// and an offset past the surface never panics / writes OOB.
    #[test]
    fn composite_tray_src_over_and_clamps() {
        let card = |rgba: Vec<u8>, dx: u32, dy: u32| crate::SettingsCard {
            rgba,
            pw: 1,
            ph: 1,
            dx,
            dy,
            fp: 0,
            geom: 0,
        };
        // 1×1 card, full alpha → exactly the source color.
        let mut fb = vec![0x0010_2030u32; 4];
        composite_tray(&mut fb, 2, 2, &card(vec![0xAA, 0xBB, 0xCC, 0xFF], 0, 0));
        assert_eq!(fb[0], 0x00AA_BBCC);
        // Fully transparent → dst preserved (the live terminal shows through).
        let mut fb2 = vec![0x0012_3456u32; 1];
        composite_tray(&mut fb2, 1, 1, &card(vec![0xFF, 0xFF, 0xFF, 0x00], 0, 0));
        assert_eq!(fb2[0], 0x0012_3456);
        // Half alpha over black → ~half the source per channel (rounded). Dst is 0, so
        // the src-over reduces to `(s*a + 127) / 255`.
        let mut fb3 = vec![0u32; 1];
        composite_tray(&mut fb3, 1, 1, &card(vec![0xFF, 0x00, 0x80, 0x80], 0, 0));
        let exp = |s: u32, a: u32| (s * a + 127) / 255;
        assert_eq!(fb3[0], (exp(0xFF, 0x80) << 16) | exp(0x80, 0x80));
        // Offset entirely off-surface: no panic, no change.
        let mut fb4 = vec![0x00FF_FFFFu32; 1];
        composite_tray(&mut fb4, 1, 1, &card(vec![0, 0, 0, 0xFF], 5, 5));
        assert_eq!(fb4[0], 0x00FF_FFFF);
    }

    fn channel_dist(a: u32, b: u32) -> u32 {
        let d = |s: u32| (((a >> s) & 0xff) as i32 - ((b >> s) & 0xff) as i32).unsigned_abs();
        d(16) + d(8) + d(0)
    }

    /// The border pixels land much closer to the accent than the interior wash,
    /// and the interior is exactly the faint accent blend over the background.
    #[test]
    fn border_is_accent_heavy_interior_is_faint() {
        let (w, h) = (400usize, 300usize);
        let accent = 0x0050_FA7B;
        let mut px = vec![0x0000_0000u32; w * h]; // black background
        apply_drop_overlay(&mut px, w, h, accent);

        let corner = px[0]; // on the border
        let interior = px[(h / 2) * w + w / 2]; // far from any edge
        assert_ne!(corner, 0, "border pixel must be tinted");
        assert_ne!(interior, 0, "interior pixel must be washed");
        assert!(
            channel_dist(corner, accent) < channel_dist(interior, accent),
            "border should be nearer the accent than the interior"
        );
        assert_eq!(interior, blend_rgb(0, accent, DROP_WASH_ALPHA));
        assert!(drop_border_px(w, h) >= 2);
    }

    /// Degenerate dimensions and a no-window case are no-ops, never panics.
    #[test]
    fn zero_dims_is_noop() {
        let mut px = vec![0x0011_2233u32; 4];
        apply_drop_overlay(&mut px, 0, 0, 0x00ff_ffff);
        assert_eq!(px, vec![0x0011_2233u32; 4]);
    }

    /// The packed format is preserved: the unused top byte stays clear.
    #[test]
    fn top_byte_stays_clear() {
        let mut px = vec![0x00ab_cdefu32; 10 * 10];
        apply_drop_overlay(&mut px, 10, 10, 0x00ff_ffff);
        assert!(px.iter().all(|p| p & 0xff00_0000 == 0));
    }
}

/// Pure pixel→TERMINAL-cell mapping (the body of [`App::pixel_to_cell`], extracted
/// so the tab-strip row offset is unit-testable without a backend/window). Three
/// insets are removed from the raw window pixel before mapping, in order:
///   * `head` — the chrome headroom (titlebar band) above the padded grid,
///     subtracted from `y` ONLY (x carries no chrome band); a click in the band
///     saturates toward row 0 like the pad;
///   * `pad` — the interior padding border around the WHOLE window (strip included),
///     subtracted from BOTH `x` and `y` (a saturating subtract maps a click in the
///     top/left border to row/col 0);
///   * `strip_rows * ch` — the tab strip occupies the top `strip_rows` pixel rows
///     of the (already pad-inset) grid, so a click in the terminal region lands on
///     the right terminal row and a click in the strip clamps to terminal row 0.
///
/// The result is clamped to the terminal grid. `pad == 0` && `strip_rows == 0` &&
/// `head == 0` is the byte-identical pre-strip, pre-pad mapping.
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
    // X insets by `pad`; Y insets by the (possibly tighter) top pad + head, so a
    // click maps to the same cell the grid renders at `grid_top = pad_top + head`.
    let gy = (y as usize).saturating_sub(pad_top + head);
    let strip_px = strip_rows as usize * ch.max(1);
    let term_y = gy.saturating_sub(strip_px);
    let col = (gx / cw.max(1)).min(cols.saturating_sub(1) as usize) as u16;
    let row = (term_y / ch.max(1)).min(rows.saturating_sub(1) as usize) as u16;
    (row, col)
}

/// Pure "is this pixel in the tab strip, and if so which strip column?" (the body
/// of [`App::strip_col_at`], extracted for unit tests). The chrome `head` band and
/// the interior `pad` border are removed from `y` (and `pad` from `x` — the strip
/// lives inside the pad, below the headroom) first, then `None` when the inset `y`
/// is at/below the strip's pixel height (`strip_rows * ch`) — i.e. in the terminal
/// region. A click in the top `head + pad` band over the strip still maps to the
/// strip (gy saturates to 0). `pad == 0` && `head == 0` is byte-identical.
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

/// Selection-drag AUTOSCROLL trigger: given the pointer's raw window pixel `y`, the
/// interior `pad`, the tab-strip pixel height (`strip_rows * ch`), the cell height
/// `ch`, and the terminal `rows`, return the number of scrollback lines to move so a
/// drag PAST the top/bottom viewport edge extends the selection into off-screen
/// content. Positive = scroll toward OLDER history (drag above the top edge); negative
/// = scroll toward the live BOTTOM (drag below the bottom edge); `0` = the pointer is
/// inside the grid, no autoscroll.
///
/// The magnitude grows with how far past the edge the pointer is (one line per cell
/// height of overshoot, min 1), so a fast flick to the window edge scrolls briskly
/// while a hair past the edge creeps — the familiar text-editor feel. Pure (no
/// window/term), so the edge math is unit-testable.
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
        // Above the top edge → scroll into history. One line per cell-height of
        // overshoot (min 1), so the further out, the faster.
        let over = (top - y) as usize;
        (over / ch + 1) as i32
    } else if y >= bottom {
        // Below the bottom edge → scroll toward the live bottom (negative offset).
        let over = (y - bottom) as usize;
        -((over / ch + 1) as i32)
    } else {
        0
    }
}

/// Shift the composed frame `dst` DOWN by `strip_rows.len()` rows and prepend those
/// painted tab-strip rows at the top, keeping every per-row vector
/// (`cells`/`clusters`/`combining`/`images`/`line_sizes`) aligned and moving the
/// cursor + row count down with the content. Pure (the body of
/// [`App::splice_tab_strip`]'s mutation), so the row-offset math is unit-testable on
/// a bare [`RenderInput`]. An empty `strip_rows` is a no-op (byte-identical).
/// `cell_h` is the pixel cell height, needed to shift the GRID streams' pixel
/// quads down with the grid.
///
/// WINDOW-SPACE effect streams (`fire_patch`/`cursor_glow_add`/`glow_under`/
/// `glow_halo`) arrive in window-ABSOLUTE pixels whose `origin_y`
/// already includes the strip band, so their pixel coordinates are NOT shifted
/// here — only their damage row TAGS move. `grid_top` is the window's
/// `pad + head` (the grid-interior top before the strip): a quad above
/// `grid_top + strip·cell_h` (terminal row 0's top) pins its tag to composed
/// row 0, opening the top damage band.
///
/// `strip_rows` is BORROWED (the caller's per-window row cache must survive for the
/// next present), so its rows are copied into `dst`; each container is shifted with a
/// single `splice` — one O(rows) header move per container per frame, not one per
/// strip row — this runs on the latency-gating present path.
///
/// `pool` is the caller's resident strip-row buffer pool: the only heap-allocating
/// rows are `cells`, so each cell row is built by popping a reclaimed buffer and
/// `clone_from`-ing the cached row into it (capacity retained, zero fresh alloc after
/// warmup). An empty pool falls back to a fresh `clone`, so the spliced bytes are
/// identical either way.
pub(crate) fn prepend_strip_rows(
    dst: &mut RenderInput,
    strip_rows: &[Vec<RenderCell>],
    cell_h: usize,
    grid_top: usize,
    pool: &mut Vec<Vec<RenderCell>>,
) {
    let strip = strip_rows.len();
    if strip == 0 {
        return;
    }
    dst.cells.splice(
        0..0,
        strip_rows.iter().map(|src| match pool.pop() {
            Some(mut buf) => {
                buf.clone_from(src);
                buf
            }
            None => src.clone(),
        }),
    );
    // Per-row sparse / sized data: prepend empty/default rows so indices stay aligned
    // with `cells`. `clusters`/`combining`/`images` are sparse (empty vecs);
    // `line_sizes` defaults to single-width. `(0..strip).map` keeps the iterators
    // exact-size so each splice shifts the tail exactly once.
    dst.clusters.splice(0..0, (0..strip).map(|_| Vec::new()));
    dst.combining.splice(0..0, (0..strip).map(|_| Vec::new()));
    dst.images.splice(0..0, (0..strip).map(|_| Vec::new()));
    dst.line_sizes.splice(
        0..0,
        (0..strip).map(|_| aterm_core::grid::LineSize::SingleWidth),
    );
    // The cursor (terminal-grid row) is now `strip` rows lower in the window;
    // the motion-trail cells move down with it so they stay under the cursor.
    dst.cursor_row += strip;
    for t in &mut dst.cursor_trail {
        t.row += strip;
    }
    // WINDOW-SPACE streams: the pixel coordinates are window-absolute (their
    // producers' `origin_y` already includes the strip band) — do NOT shift them.
    // Only the damage row TAG moves with the splice, and it is RE-DERIVED from
    // the pixel y against the COMPOSED frame's bands (row r spans
    // `grid_top + r·cell_h ..`, row 0 opening to the window top): a quad above
    // the terminal grid may land in a STRIP row's band (strip_rows ≥ 2 puts
    // composed rows 1..strip above the terminal grid — adversarial review
    // caught the old pin-to-0 rule leaving those bands' damage stale), and an
    // in-grid quad lands exactly on its strip-shifted terminal row. Deriving
    // from the pixel makes both cases one law.
    let dy = u16::try_from(strip * cell_h).unwrap_or(u16::MAX);
    let shift_tag = |_row: u16, y: usize| (y.saturating_sub(grid_top) / cell_h.max(1)) as u16;
    // The LUMEN aurora quads are window-absolute pixel rects tagged with a row.
    for q in &mut dst.cursor_glow_add {
        q.row = shift_tag(q.row, q.y as usize);
    }
    // Under-ink flame body (EMBERFORGE dark cores): same pixel-rect shape as
    // the aurora.
    for q in &mut dst.glow_under {
        q.row = shift_tag(q.row, q.y as usize);
    }
    // Per-pixel fire patches (campaign 2): window-absolute quad + flame-root
    // pixels ride untouched; only the row tag moves.
    for q in &mut dst.fire_patch {
        q.row = shift_tag(q.row, q.y as usize);
    }
    // Charred-ink overrides are cell-tagged like ink (a GRID stream): shift the
    // row down.
    for c in &mut dst.char_fg {
        c.row = c.row.saturating_add(strip as u16);
    }
    // Fire contrast-halo strengths are cell-tagged like char_fg (a GRID
    // stream): shift the row down so the ring stays under its glyph.
    for c in &mut dst.fire_halo {
        c.row = c.row.saturating_add(strip as u16);
    }
    // Radial cursor-effect halos (EMBERFORGE round light): window-absolute quad
    // + falloff centre ride untouched; only the row tag moves.
    for h in &mut dst.glow_halo {
        h.row = shift_tag(h.row, h.y as usize);
    }
    // Sparkle-word decorations are cell-row tagged (no pixel y); shift the row down.
    for d in &mut dst.word_decorations {
        d.row = d.row.saturating_add(strip as u16);
    }
    // Animated-ink overrides are cell-row tagged too; emitted pre-splice (the one
    // splice rule) so the uniform shift keeps them on their matched glyphs.
    for c in &mut dst.ink {
        c.row = c.row.saturating_add(strip as u16);
    }
    // Peeking-cat quads carry a row band AND a pixel-y dest rect (like the
    // aurora): shift both by the strip — the same single splice rule.
    for q in &mut dst.cat_quads {
        q.row = q.row.saturating_add(strip as u16);
        q.y = q.y.saturating_add(dy);
    }
    // Free-overlay sprites (overlay Phase 3 / v3 §5) are pure pixel rects
    // with NO row tag: shift the signed dest y alone by the strip — the
    // FreeSprite arm of the same single splice rule. (The dirty row-union
    // re-derives the covered bands from the shifted extent.)
    let dy_free = i32::from(dy);
    for s in &mut dst.free_sprites {
        s.y = s.y.saturating_add(dy_free);
    }
    // Supernova additive quads stay a GRID stream (their word_decorations
    // producer emits grid-relative pixels; the renderers add the grid origin):
    // shift BOTH the row tag and the pixel y — the historical splice rule.
    for q in &mut dst.nova_add {
        q.row = q.row.saturating_add(strip as u16);
        q.y = q.y.saturating_add(dy);
    }
    // PHOSPHOR rain sprites carry a row band AND a pixel-y dest rect (like the
    // cat quads): shift both by the strip — the same single splice rule.
    for q in &mut dst.rain_quads {
        q.row = q.row.saturating_add(strip as u16);
        q.y = q.y.saturating_add(dy);
    }
    // Rain bright-head halos are row-tagged pixel rects like the nova quads —
    // plus a falloff CENTRE (cx, cy) that must ride the same vertical shift, or
    // the radial light misregisters against its shifted quad.
    for q in &mut dst.rain_add {
        q.row = q.row.saturating_add(strip as u16);
        q.y = q.y.saturating_add(dy);
        q.cy = q.cy.saturating_add(dy);
    }
    dst.rows += strip;
    // The strip changes the presented pixels; bump the snapshot seq so the renderer's
    // content cache sees the new frame.
    dst.snapshot_seq = dst.snapshot_seq.wrapping_add(1);
}

/// Decide whether the native host must extract an authoritative live grid
/// snapshot for rain. Literal-mode sampling is independent of the damage
/// epoch so a classic-to-literal config reload cannot wait for unrelated PTY
/// output before the on-screen alphabet becomes real.
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

/// Observe the current session's payload-free OSC execution phase. The first
/// observation of a session is a baseline, not an event; thereafter exactly a
/// same-session `false -> true` edge returns `true`. Keeping this edge detector
/// outside the rain engine prevents a long-lived `ShellState::Executing` level
/// from refreshing semantic TTL on every present.
fn rain_shell_execute_rising_edge(
    last: &mut Option<(u64, bool)>,
    session: u64,
    executing: bool,
) -> bool {
    let rising = matches!(*last, Some((sid, false)) if sid == session) && executing;
    *last = Some((session, executing));
    rising
}

/// Maintain the PHOSPHOR hidden-cursor band (design §6): the resident ring of
/// the last [`crate::matrix_rain::HIDDEN_CURSOR_BAND_ROWS`] recently-damaged
/// viewport rows, most recent first. When DECTCEM hides the cursor, Ink parks
/// it at a meaningless position — damage recency stands in for it, and the
/// band is where Claude Code's inline input box actually lives. Must run
/// under the terminal lock BEFORE the frame's `take_damage` (damage is gone
/// after). A FULL-damage frame (resize / alt-swap / first frame) locates
/// nothing — every row is "damaged" — so it leaves the ring unchanged rather
/// than flooding it with arbitrary rows. In-place on the resident ring: no
/// per-frame allocation once the ring reaches capacity.
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
        // Promote to most-recent-first; ascending iteration means the frame's
        // BOTTOM-most damaged rows end up at the ring's head (the §6 bottom-
        // region bias).
        if let Some(p) = band.iter().position(|&x| x == r) {
            band.remove(p);
        }
        band.insert(0, r);
        band.truncate(crate::matrix_rain::HIDDEN_CURSOR_BAND_ROWS);
    }
}

/// Translate one pane's PANE-LOCAL rain emission into window-content coords
/// and clip it to the pane's grid-interior box (split-pane audit: rain light
/// must never cross a divider). Rain streams are GRID-INTERIOR pixels + a
/// viewport row tag (unlike the window-absolute cursor-effect streams), so
/// the shift is `row += row_off`, `x += col_off·cell_w`, `y += row_off·cell_h`
/// — the tab-strip splice discipline. Quads land inside the pane box by
/// construction (the emitter is bounded by the pane-local geometry); the clip
/// is defensive for halo spill, and a clipped halo renders byte-identical
/// pixels over the surviving area because its falloff is a pure function of
/// the (shifted) centre + per-quad params — the FirePatch continuity law.
/// Quads that would leave the u16 pixel space drop whole (nothing to draw
/// there anyway).
#[allow(
    clippy::too_many_arguments,
    reason = "the pane placement (row/col offset + rows/cols) and cell metrics are one flat geometry tuple; a wrapper struct would relocate the list, not simplify it"
)]
pub(crate) fn translate_rain_into_pane(
    quads: &mut Vec<aterm_render::SpriteQuad>,
    add: &mut Vec<aterm_render::RainHalo>,
    row_off: u16,
    col_off: u16,
    pane_rows: u16,
    pane_cols: u16,
    cell_w: u32,
    cell_h: u32,
) {
    let dx = u32::from(col_off) * cell_w;
    let dy = u32::from(row_off) * cell_h;
    let x1 = (u32::from(col_off) + u32::from(pane_cols)) * cell_w;
    let y1 = (u32::from(row_off) + u32::from(pane_rows)) * cell_h;
    let clip = move |x: u32, y: u32, w: u32, h: u32| -> Option<(u16, u16, u16, u16)> {
        let (nx0, ny0) = (x + dx, y + dy);
        let nx1 = nx0.saturating_add(w).min(x1);
        let ny1 = ny0.saturating_add(h).min(y1);
        // LAZY `then` (the pane_clip closure's law): `then_some` would
        // evaluate the subtractions even when the guard is false.
        (nx1 > nx0 && ny1 > ny0 && nx1 <= u32::from(u16::MAX) && ny1 <= u32::from(u16::MAX)).then(
            || {
                (
                    nx0 as u16,
                    ny0 as u16,
                    (nx1 - nx0) as u16,
                    (ny1 - ny0) as u16,
                )
            },
        )
    };
    quads.retain_mut(|q| {
        let Some((x, y, w, h)) = clip(
            u32::from(q.x),
            u32::from(q.y),
            u32::from(q.w),
            u32::from(q.h),
        ) else {
            return false;
        };
        (q.x, q.y, q.w, q.h) = (x, y, w, h);
        q.row = q.row.saturating_add(row_off);
        true
    });
    add.retain_mut(|q| {
        let Some((x, y, w, h)) = clip(
            u32::from(q.x),
            u32::from(q.y),
            u32::from(q.w),
            u32::from(q.h),
        ) else {
            return false;
        };
        (q.x, q.y, q.w, q.h) = (x, y, w, h);
        q.row = q.row.saturating_add(row_off);
        // The falloff CENTRE shifts with the quad (it may lie outside the
        // clipped rect — the radial math is absolute, so light stays exact).
        q.cx = (u32::from(q.cx) + dx).min(u32::from(u16::MAX)) as u16;
        q.cy = (u32::from(q.cy) + dy).min(u32::from(u16::MAX)) as u16;
        true
    });
}

/// A divider cell for the gaps BETWEEN split panes: a blank glyph filled with a
/// mid-tone background so the 1-cell line reads as a visible seam regardless of
/// font glyph coverage. The colour is a 50/50 blend of the theme's foreground and
/// background, so it contrasts on both dark and light themes.
pub(crate) fn divider_cell(theme: Theme) -> RenderCell {
    let mix = |shift: u32| {
        let a = ((theme.fg >> shift) & 0xff) as u16;
        let b = ((theme.bg >> shift) & 0xff) as u16;
        ((a + b) / 2) as u8
    };
    let seam = [mix(16), mix(8), mix(0)];
    RenderCell {
        ch: ' ',
        fg: seam,
        bg: seam,
        wide: false,
        emoji_presentation: false,
        bold: false,
        italic: false,
        underline: aterm_core::terminal::UnderlineStyle::None,
        strikethrough: false,
        overline: false,
        underline_color: None,
    }
}

/// Resolve the cell that an UNMATERIALIZED terminal column represents. Grid rows
/// are intentionally sparse: a missing tail is `Cell::EMPTY`, not a request to
/// inherit the last stored cell's SGR. Resolve that implicit cell from the pane's
/// live defaults so OSC 10/11 and terminal-wide reverse video (DECSCNM) survive
/// split composition exactly like materialized cells do.
pub(crate) fn terminal_blank_cell(term: &Terminal) -> RenderCell {
    let (fg, bg) = aterm_core::terminal::color_resolve::resolve_colors(
        &aterm_core::grid::Cell::EMPTY,
        None,
        term.color_palette(),
        term.default_foreground(),
        term.default_background(),
        term.reverse_video(),
    );
    RenderCell {
        ch: ' ',
        fg: [fg.r, fg.g, fg.b],
        bg: [bg.r, bg.g, bg.b],
        wide: false,
        emoji_presentation: false,
        bold: false,
        italic: false,
        underline: aterm_core::terminal::UnderlineStyle::None,
        strikethrough: false,
        overline: false,
        underline_color: None,
    }
}

/// SPLIT-PANE composition: fill `dst` with a `rows`×`cols` grid of divider cells
/// (the seam colour), reset to no cursor / no clusters / single-width rows. The
/// per-pane blit then overwrites each pane's rectangle; the cells left untouched
/// are exactly the 1-cell divider gaps between panes.
pub(crate) fn fill_divider_grid(dst: &mut RenderInput, rows: usize, cols: usize, theme: Theme) {
    fill_divider_grid_cells(dst, rows, cols, theme);
    clear_effect_overlays_for_compose(dst);
}

/// The CELL half of [`fill_divider_grid`]: divider-seam grid + cluster/
/// combining/image/line-size/cursor resets, effect overlays UNTOUCHED. The
/// composed `image`/`snapshot` capture uses this alone (split-pane audit): a
/// capture must show the retained last-present effect quads WYSIWYG, not wipe
/// them — the live compose path clears them separately because it re-produces
/// every overlay each frame.
pub(crate) fn fill_divider_grid_cells(
    dst: &mut RenderInput,
    rows: usize,
    cols: usize,
    theme: Theme,
) {
    let seam = divider_cell(theme);
    dst.rows = rows;
    dst.cols = cols;
    dst.cells.resize_with(rows, Vec::new);
    for row in &mut dst.cells {
        row.clear();
        row.resize(cols, seam);
    }
    dst.clusters.clear();
    dst.clusters.resize_with(rows, Vec::new);
    dst.combining.clear();
    dst.combining.resize_with(rows, Vec::new);
    // Inline-image placements are per-row sparse like clusters/combining; reset them
    // to exactly `rows` empty rows each compose frame, mirroring `cell_frame_into`'s
    // `images.resize_with(rows, Vec::new)`. Without this, the chrome splices
    // `prepend_strip_rows` is the only chrome mutator of `dst.images`
    // in the compose path, so it grows unbounded (leak) and drifts out of length
    // alignment with `cells`, corrupting the per-row damage gate.
    dst.images.clear();
    dst.images.resize_with(rows, Vec::new);
    dst.line_sizes.clear();
    dst.line_sizes
        .resize(rows, aterm_core::grid::LineSize::SingleWidth);
    // Per-pane line-size runs, rebuilt from scratch each compose frame like the
    // other per-row sparse lists. Rows stay EMPTY unless a pane actually lands a
    // non-single DEC line size on them, so the ordinary split keeps the uniform
    // `line_sizes` fast path.
    dst.line_size_spans.clear();
    dst.line_size_spans.resize_with(rows, Vec::new);
    // The per-row `images` vec is part of the `RenderInput` length==rows contract
    // the CPU renderer indexes by row unguarded; the previous code never resized
    // it here, so a compose frame (after a grow) left `images` shorter than `rows`
    // and `full_render` panicked on `input.images[r]`. Reset it to the new row
    // count so each row starts image-free (panes re-add their own below).
    dst.images.clear();
    dst.images.resize_with(rows, Vec::new);
    dst.cursor_visible = false;
    dst.cursor_row = 0;
    dst.cursor_col = 0;
    dst.display_offset = 0;
}

/// The EFFECT-OVERLAY half of [`fill_divider_grid`]: clear every host-owned
/// bling channel so a compose frame starts overlay-free (each active effect
/// re-installs its own quads afterwards; empty vecs + `None` atlases are
/// byte-identical off).
pub(crate) fn clear_effect_overlays_for_compose(dst: &mut RenderInput) {
    dst.cursor_glow_add.clear();
    dst.glow_halo.clear();
    dst.fire_patch.clear();
    dst.glow_under.clear();
    dst.char_fg.clear();
    dst.fire_halo.clear();
    // The cadence-comet trail is re-produced per compose frame off the focused pane's
    // cursor; clear any stale single-pane cells so a comet-free split frame is
    // byte-identical to no trail (the compose splice below overwrites it when active).
    dst.cursor_trail.clear();
    dst.word_decorations.clear();
    // Sparkle effects are single-pane only (v1 rule kept for v2 ink + cats +
    // novas): the compose path carries none, and empty vecs / a null atlas are
    // byte-identical off.
    dst.ink.clear();
    dst.cat_quads.clear();
    dst.cat_atlas = None;
    dst.free_sprites.clear();
    dst.free_atlas = None;
    dst.nova_add.clear();
    // PHOSPHOR rain: the compose path re-installs the FOCUSED pane's
    // translated emission after this clear (split-pane audit) — starting
    // channel-empty keeps a suspended/drained frame byte-identical off.
    dst.rain_quads.clear();
    dst.rain_atlas = None;
    dst.rain_add.clear();
}

/// Blit one pane's snapshot `src` (sized to the pane's sub-rect) into the
/// composite `dst` at cell offset `(row_off, col_off)`. Copies the resolved cells,
/// fills each sparse row's omitted tail through `src.cols` with `blank`, then copies
/// the sparse emoji-cluster / combining-mark / inline-image per-row data
/// (column-shifted by `col_off`) and the per-row line size. Bounds-checked so a pane
/// that slightly overflows a degenerate tiny window can never write past the
/// composite. Painting the full declared pane rectangle is what confines the
/// divider-grid sentinel to actual divider columns.
pub(crate) fn blit_pane_into(
    dst: &mut RenderInput,
    src: &RenderInput,
    row_off: usize,
    col_off: usize,
    blank: RenderCell,
) {
    let dst_cols = dst.cols;
    for sr in 0..src.rows {
        let Some(dr) = row_off.checked_add(sr) else {
            break;
        };
        let Some(dst_row) = dst.cells.get_mut(dr) else {
            break;
        };
        let src_row = src.cells.get(sr);
        let materialized = src_row.map_or(0, |row| row.len().min(src.cols));
        if let Some(src_row) = src_row {
            for (sc, cell) in src_row.iter().take(materialized).enumerate() {
                let Some(dc) = col_off.checked_add(sc) else {
                    break;
                };
                if let Some(slot) = dst_row.get_mut(dc) {
                    *slot = *cell;
                }
            }
        }
        let blank_start = col_off.saturating_add(materialized).min(dst_row.len());
        let pane_end = col_off.saturating_add(src.cols).min(dst_row.len());
        if blank_start < pane_end {
            dst_row[blank_start..pane_end].fill(blank);
        }
        // DEC line size is a PER-PANE fact, and a composite row can hold several
        // panes. Writing it to the row-level `dst.line_sizes[dr]` made the last
        // pane blitted win the whole row, so a neighbour sitting on an ordinary
        // line had its glyphs scaled by someone else's DECDWL. Record the pane's
        // own column run instead, and leave the row-level value alone.
        //
        // Only NON-single runs are recorded: a column covered by no span already
        // renders `SingleWidth` (the composite's reset default), which is exactly
        // what a single-width pane wants. So the ordinary split — every pane on
        // ordinary lines — leaves `line_size_spans` empty and keeps the uniform
        // fast path, byte-identical to before.
        if let Some(ls) = src.line_sizes.get(sr).copied()
            && ls != aterm_core::grid::LineSize::SingleWidth
        {
            let end_col = col_off.saturating_add(src.cols).min(dst_cols);
            if col_off < end_col
                && let Some(spans) = dst.line_size_spans.get_mut(dr)
            {
                spans.push(aterm_core::render::LineSizeSpan {
                    start_col: col_off,
                    end_col,
                    line_size: ls,
                });
                // Panes are not guaranteed to blit left-to-right, and
                // `line_size_run_at` binary-searches by `start_col`.
                spans.sort_unstable_by_key(|s| s.start_col);
                // Row-level SUMMARY (see the `line_sizes` field docs): "some pane
                // on this row is a DEC line". Placement reads the runs, but the
                // row-level consumers that cannot be per-column — the sparkle-word
                // cat suppressor, the double-height damage gate — still read this,
                // and must not go blind just because the truth moved into runs.
                // It also means a placement site not yet converted to the run seam
                // degrades to the OLD whole-row scaling rather than to something
                // new, which is the safer way to be wrong.
                if let Some(dst_ls) = dst.line_sizes.get_mut(dr) {
                    *dst_ls = ls;
                }
            }
        }
        // The per-row (col, _) lists must stay sorted by column with one entry per
        // column so the renderers' per-column lookups binary-search instead of
        // O(cols²)-scanning. `src` is sorted, but panes are not guaranteed to blit
        // left-to-right, so appending a pane at `col_off` can interleave columns
        // below an already-placed pane — re-sort after each append (O(k) on the
        // already-sorted single-pane case).
        if let Some(dst_clusters) = dst.clusters.get_mut(dr)
            && let Some(src_clusters) = src.clusters.get(sr)
        {
            for (c, s) in src_clusters {
                dst_clusters.push((col_off + c, s.clone()));
            }
            dst_clusters.sort_unstable_by_key(|(c, _)| *c);
        }
        if let Some(dst_comb) = dst.combining.get_mut(dr)
            && let Some(src_comb) = src.combining.get(sr)
        {
            for (c, m) in src_comb {
                dst_comb.push((col_off + c, m.clone()));
            }
            dst_comb.sort_unstable_by_key(|(c, _)| *c);
        }
        // Composite the pane's inline (iTerm2 OSC 1337 / kitty) images, column-
        // shifted by `col_off` and clipped to the composite width (`ImageRef` is
        // Arc-backed, so the clone is a cheap refcount bump). Without this a split
        // pane silently dropped every inline image (the single-pane path fills
        // `images` via `cell_frame_into`, but the compose path did not / it resets
        // `dst.images` to empty rows).
        if let Some(dst_imgs) = dst.images.get_mut(dr)
            && let Some(src_imgs) = src.images.get(sr)
        {
            for (c, img) in src_imgs {
                if col_off + c < dst_cols {
                    dst_imgs.push((col_off + c, img.clone()));
                }
            }
            dst_imgs.sort_unstable_by_key(|(c, _)| *c);
        }
    }
}

/// Straight-alpha source-over blit for a native leaf raster into the one
/// window-owned tray texture. Bounds checks make a stale/rounded leaf harmless;
/// the geometry plan remains the authority for where pixels and hits land.
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
    for sy in 0..src_height.min(dst_height.saturating_sub(y)) {
        for sx in 0..src_width.min(dst_width.saturating_sub(x)) {
            let si = ((sy * src_width + sx) * 4) as usize;
            let di = (((y + sy) * dst_width + x + sx) * 4) as usize;
            let Some(source) = src.get(si..si + 4) else {
                return;
            };
            let Some(destination) = dst.get_mut(di..di + 4) else {
                return;
            };
            let sa = u32::from(source[3]);
            let da = u32::from(destination[3]);
            let out_a = sa + (da * (255 - sa) + 127) / 255;
            if out_a == 0 {
                destination.fill(0);
                continue;
            }
            for channel in 0..3 {
                let source_premultiplied = u32::from(source[channel]) * sa;
                let destination_premultiplied =
                    u32::from(destination[channel]) * da * (255 - sa) / 255;
                destination[channel] =
                    ((source_premultiplied + destination_premultiplied) / out_a).min(255) as u8;
            }
            destination[3] = out_a.min(255) as u8;
        }
    }
}

/// Overlay pending speculative-echo GHOSTS onto a terminal-coordinate snapshot —
/// `input_scratch` on the single-pane path, the focused pane's `pane_scratch`
/// (pane-local coords, BEFORE the blit offsets it into window space) on the
/// composed path. Rows are trimmed to content width, so a guess past the line end
/// EXTENDS the row with blanks first (cloning the row's last cell to keep its
/// background, else `blank`) — otherwise the prediction silently vanishes exactly
/// where typing happens. The glyph is set with inherited geometry/SGR DROPPED (a
/// `wide` continuation cell is SKIPPED by the renderer, and bold/italic/underline
/// would garble it) and the fg dimmed toward the cell's own bg, so it stays
/// legible on any theme yet reads as tentative until the real echo lands. A guess
/// at or past `cols` is dropped — on the composed path the blit does not clip to
/// the pane's sub-rect, so an unclipped ghost would bleed into the neighbor pane.
/// Returns whether any ghost was painted.
fn paint_prediction_ghosts(
    scratch: &mut RenderInput,
    preds: &[crate::predict::Prediction],
    cols: usize,
    blank: RenderCell,
) -> bool {
    let mut painted = false;
    for p in preds {
        let col = p.col as usize;
        if col >= cols {
            continue;
        }
        let Some(rowv) = scratch.cells.get_mut(p.row as usize) else {
            continue;
        };
        let template = rowv.last().copied().unwrap_or(blank);
        let mut pad = template;
        pad.ch = ' ';
        while rowv.len() <= col {
            rowv.push(pad);
        }
        let cell = &mut rowv[col];
        let (fg, bg) = (cell.fg, cell.bg);
        cell.ch = p.ch;
        cell.fg = [
            ((fg[0] as u16 + bg[0] as u16) / 2) as u8,
            ((fg[1] as u16 + bg[1] as u16) / 2) as u8,
            ((fg[2] as u16 + bg[2] as u16) / 2) as u8,
        ];
        cell.wide = false;
        cell.emoji_presentation = false;
        cell.bold = false;
        cell.italic = false;
        cell.underline = aterm_core::terminal::UnderlineStyle::None;
        cell.strikethrough = false;
        cell.overline = false;
        cell.underline_color = None;
        painted = true;
    }
    painted
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
        // An unset live background never overrides the theme.
        if let Some(ws) = app.windows.get_mut(&wid0) {
            ws.input_scratch.default_bg = aterm_core::render::COLOR_UNSET;
        }
        assert_eq!(app.overlay_card_theme(wid0).bg, 0x00FF_FFFF);
    }
}

#[cfg(test)]
mod prediction_ghost_tests {
    use super::paint_prediction_ghosts;
    use crate::predict::Prediction;
    use aterm_core::terminal::RenderCell;
    use aterm_render::RenderInput;

    fn blank() -> RenderCell {
        RenderCell {
            ch: ' ',
            fg: [200, 200, 200],
            bg: [0, 0, 0],
            wide: false,
            emoji_presentation: false,
            bold: false,
            italic: false,
            underline: aterm_core::terminal::UnderlineStyle::None,
            strikethrough: false,
            overline: false,
            underline_color: None,
        }
    }

    fn scratch(rows: usize) -> RenderInput {
        let mut s = RenderInput::empty();
        s.cells = vec![Vec::new(); rows];
        s
    }

    /// A guess past a trimmed row's end extends it with pad cells, sets the glyph,
    /// and dims the fg toward the cell's bg (tentative, legible on any theme).
    #[test]
    fn ghost_extends_row_and_dims() {
        let mut s = scratch(2);
        let preds = [Prediction::test_at(1, 3, 'x')];
        assert!(paint_prediction_ghosts(&mut s, &preds, 10, blank()));
        assert_eq!(s.cells[1].len(), 4, "row extended to hold the ghost");
        let c = &s.cells[1][3];
        assert_eq!(c.ch, 'x');
        assert_eq!(c.fg, [100, 100, 100], "fg dimmed halfway to bg");
        assert!(!c.bold && !c.wide, "inherited SGR/geometry dropped");
    }

    /// A guess at or past `cols` is DROPPED: on the composed path the pane blit
    /// does not clip to the pane's sub-rect, so an unclipped ghost would bleed
    /// into the neighboring pane. Out-of-range rows are ignored too.
    #[test]
    fn ghost_clips_to_pane_rect() {
        let mut s = scratch(2);
        let preds = [
            Prediction::test_at(0, 5, 'a'), // at cols → clipped
            Prediction::test_at(0, 9, 'b'), // past cols → clipped
            Prediction::test_at(7, 0, 'c'), // past rows → ignored
        ];
        assert!(
            !paint_prediction_ghosts(&mut s, &preds, 5, blank()),
            "nothing painted when everything clips"
        );
        assert!(s.cells[0].is_empty(), "clipped ghosts never extend the row");
        let ok = [Prediction::test_at(0, 4, 'd')]; // last in-rect column paints
        assert!(paint_prediction_ghosts(&mut s, &ok, 5, blank()));
        assert_eq!(s.cells[0][4].ch, 'd');
    }
}

#[cfg(test)]
mod comet_trail_tests {
    use super::forge_cursor_fill;
    use crate::App;
    use crate::cursor_glow::GlowStyle;

    /// The "comet" style is the best-of-all: the additive aurora light CROWN
    /// (bloom + landing ring, per config), the continuous anti-aliased BEAM along
    /// the swept path (the spatial-continuity fix — without it the grid-quantized
    /// cell body reads as gappy blocks), AND the faint cadence-comet `TrailCell`
    /// ember bed under both (its coverage capped low; see READABLE_ALPHA_CAP). So
    /// `glow_config` reports an enabled `Comet` aurora (its own variant since the
    /// icy-tail/nucleus upgrade — icy ramp, debris glitter, glacial default hue)
    /// with crown, ring, and `beam = true`, AND `trail_config` reports an enabled
    /// comet trail body.
    #[test]
    fn comet_style_keeps_the_light_crown_and_enables_the_trail() {
        let mut app = App::headless_for_test();
        app.config.cursor_trail = Some(true); // master switch; this test covers STYLE mapping
        app.config.cursor_trail_style = Some("comet".into());
        let g = app.glow_config();
        assert!(g.enabled, "comet keeps an ENABLED aurora crown");
        assert_eq!(
            g.style,
            GlowStyle::Comet,
            "the comet owns its own light now"
        );
        assert_eq!(
            g.color,
            crate::cursor_glow::COMET_DEFAULT_COLOR,
            "no pinned trail colour ⇒ the comet defaults GLACIAL BLUE"
        );
        assert!(
            g.radius > 0.0,
            "comet keeps the bloom crown (not beam-only)"
        );
        assert!(g.ring, "comet keeps the landing ring");
        assert!(
            g.beam,
            "comet layers the continuous AA beam under its ember bed — the beam is \
             what makes the streak spatially seamless instead of gappy cell blocks"
        );
        // The comet body (TrailCell trail) is produced only for this style.
        let t = app.trail_config();
        assert!(t.enabled, "comet enables the cadence-comet trail body");
        assert!(app.trail_is_comet());
    }

    /// The "beam" style is the first-class steady TUBE (`GlowStyle::Beam`, its
    /// PHOTON ICE-BLUE default hue) and keeps its old preset's discipline: aurora
    /// enabled but radius 0 / ring off, and NO trail body — even when crown/ring
    /// are explicitly configured on.
    #[test]
    fn beam_style_is_beam_only_no_trail() {
        let mut app = App::headless_for_test();
        app.config.cursor_trail = Some(true); // master switch (default OFF, 6272bd7a); this test covers STYLE mapping
        app.config.cursor_trail_style = Some("beam".into());
        app.config.cursor_trail_radius = Some(1.5);
        app.config.cursor_trail_ring = Some(true);
        let g = app.glow_config();
        assert!(g.enabled, "beam maps to an ENABLED aurora");
        assert_eq!(g.style, GlowStyle::Beam, "beam is its own style now");
        assert_eq!(
            g.color,
            crate::cursor_glow::BEAM_DEFAULT_COLOR,
            "beam defaults to photon ice-blue, not the theme cursor"
        );
        assert_eq!(
            g.radius, 0.0,
            "beam-only: no bloom crown even when configured"
        );
        assert!(!g.ring, "beam-only: no landing ring even when configured");
        assert!(g.beam, "the beam style IS the pure additive beam (WATER-2)");
        assert!(!app.trail_config().enabled, "beam has no comet trail body");
        assert!(!app.trail_is_comet());
        // An explicit trail colour still overrides the ice-blue default.
        app.config.cursor_trail_color = Some("#ff00ff".into());
        assert_eq!(app.glow_config().color, 0x00FF_00FF);
    }

    /// The additive-only styles (lumen/rainbow/…) keep their configured crown+ring
    /// AND their own additive beam, and produce NO trail body (the comet trail is
    /// comet-exclusive). Water is the exception — it drops the beam (WATER-1).
    #[test]
    fn additive_styles_keep_crown_and_have_no_trail() {
        let mut app = App::headless_for_test();
        app.config.cursor_trail = Some(true); // master switch (default OFF, 6272bd7a); this test covers STYLE mapping
        app.config.cursor_trail_style = Some("lumen".into());
        let g = app.glow_config();
        assert!(
            g.enabled && g.radius > 0.0 && g.ring,
            "lumen keeps crown+ring"
        );
        assert!(g.beam, "lumen shows its additive beam");
        assert!(!app.trail_config().enabled, "lumen has no comet trail body");
        // Water keeps the crown/ring/droplets but drops the laser-like beam (WATER-1).
        app.config.cursor_trail_style = Some("water".into());
        let w = app.glow_config();
        assert!(w.enabled && w.radius > 0.0, "water keeps its crown");
        assert!(
            !w.beam,
            "water drops the beam — droplets only, not a recolored laser"
        );
        // Laser (the beam it was being conflated with) KEEPS its beam.
        app.config.cursor_trail_style = Some("laser".into());
        assert!(app.glow_config().beam, "laser keeps its monochrome beam");
        // "off" disables both layers.
        app.config.cursor_trail_style = Some("off".into());
        assert!(!app.glow_config().enabled, "off disables the aurora");
        assert!(!app.trail_config().enabled, "off disables the trail");
    }

    /// The master `cursor_trail = false` switch disables BOTH layers regardless of
    /// the style (comet included).
    #[test]
    fn master_off_disables_both_layers() {
        let mut app = App::headless_for_test();
        app.config.cursor_trail_style = Some("comet".into());
        app.config.cursor_trail = Some(false);
        assert!(!app.glow_config().enabled, "master off kills the crown");
        assert!(!app.trail_config().enabled, "master off kills the trail");
    }

    #[test]
    fn forge_cursor_fill_obeys_body_master_and_style_gates() {
        let mut app = App::headless_for_test();
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("fire".into());
        let fire = app.glow_config();
        assert_eq!(
            forge_cursor_fill(true, &fire, || Some(0x00FE_DCBA)),
            Some(0x00FE_DCBA),
            "negative control: an enabled Fire cursor may use the forge fill"
        );

        assert_eq!(
            forge_cursor_fill(false, &fire, || panic!("suppressed forge state sampled")),
            None,
            "serious mode's CursorBody gate suppresses retained Fire fill"
        );

        let mut zero_intensity = fire;
        zero_intensity.intensity = 0.0;
        assert_eq!(
            forge_cursor_fill(true, &zero_intensity, || {
                panic!("zero-intensity forge state sampled")
            }),
            None,
            "reduced motion and zero-intensity configurations suppress Fire fill"
        );

        app.config.cursor_trail = Some(false);
        let disabled = app.glow_config();
        assert_eq!(
            forge_cursor_fill(true, &disabled, || panic!("disabled forge state sampled")),
            None,
            "the cursor-trail master disables Fire fill too"
        );

        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("lumen".into());
        let lumen = app.glow_config();
        assert_eq!(
            forge_cursor_fill(true, &lumen, || panic!("non-Fire forge state sampled")),
            None
        );
    }
}

#[cfg(test)]
mod gpu_backpressure_tests {
    use super::gpu_backpressure_excess_ns;
    use std::time::Duration;

    /// Healthy display pacing must contribute EXACTLY zero to the load-shed EMA —
    /// that invariant is why swapchain acquire was excluded from the causal cost in
    /// the first place, and breaking it would shed every effect on an idle machine
    /// that simply presents at the refresh rate.
    #[test]
    fn healthy_pacing_charges_nothing() {
        let fi = Duration::from_micros(8333); // 120 Hz
        assert_eq!(gpu_backpressure_excess_ns(0, fi), 0);
        assert_eq!(gpu_backpressure_excess_ns(1_000_000, fi), 0); // 1 ms wait
        // A wait of exactly one full refresh is still pacing, not back-pressure.
        assert_eq!(gpu_backpressure_excess_ns(8_333_000, fi), 0);
    }

    /// Past a full refresh the drawable pool is exhausted because the GPU has not
    /// finished prior frames — genuine back-pressure, and the ONLY condition under
    /// which shedding the (pure-GPU) bloom and shimmer passes can help.
    #[test]
    fn exhausted_pool_charges_only_the_excess() {
        let fi = Duration::from_micros(8333);
        assert_eq!(gpu_backpressure_excess_ns(10_333_000, fi), 2_000_000);
        assert_eq!(gpu_backpressure_excess_ns(25_000_000, fi), 16_667_000);
        // At 60 Hz the same absolute wait is proportionally less alarming.
        let fi60 = Duration::from_micros(16667);
        assert_eq!(gpu_backpressure_excess_ns(25_000_000, fi60), 8_333_000);
    }
}

#[cfg(test)]
mod motion_policy_tests {
    use super::{PERF_HYSTERESIS_FRAMES, causal_render_cost_ns};
    use crate::App;
    use crate::motion::{MotionEffect, MotionPolicy};

    /// W11(a+b): `App::motion_policy` is the ONE call every animation consumer
    /// makes — it folds config `motion`, the live OS "Reduce Motion" flag, and
    /// the window focus. `auto` (default) follows the system flag; `full`
    /// overrides it; NOTHING overrides the unfocused demotion.
    #[test]
    fn app_motion_policy_folds_config_flag_and_focus() {
        let mut app = App::headless_for_test();
        // Default: auto + no system reduce + focused ⇒ Full.
        assert_eq!(app.motion_policy(true), MotionPolicy::Full);
        // The OS flag flips (what the Wake::ReduceMotionChanged handler stores).
        app.system_reduce_motion = true;
        assert_eq!(app.motion_policy(true), MotionPolicy::Reduced);
        assert_eq!(
            app.motion_policy(true).amplitude(MotionEffect::CursorGlow),
            0.0,
            "a Reduced policy zeroes the aurora amplitude"
        );
        // motion = "full" overrides the OS flag…
        app.config.motion = Some("full".into());
        assert_eq!(app.motion_policy(true), MotionPolicy::Full);
        // …but never the unfocused demotion (W11b).
        assert_eq!(app.motion_policy(false), MotionPolicy::Reduced);
        // motion = "reduced" forces static even with the OS flag off.
        app.system_reduce_motion = false;
        app.config.motion = Some("reduced".into());
        assert_eq!(app.motion_policy(true), MotionPolicy::Reduced);
    }

    /// The `motion_focus` RECORDING PIN: an in-flight `video` capture feeds
    /// `true` into the policy's FOCUS input for the RECORDED window only — a
    /// control-socket recording is a watcher, so the unfocused demotion (W11b)
    /// must not zero the very effects the recording was started to observe.
    /// The pin never touches the MODE inputs: OS Reduce-Motion still demotes
    /// a recorded window exactly as `MotionPolicy::resolve` proves.
    #[test]
    fn video_recording_pins_motion_focus() {
        use crate::WindowId;
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // Unfocused, no recording: the W11b baseline demotion holds.
        assert!(!app.motion_focus(wid, false));
        assert_eq!(
            app.motion_policy(app.motion_focus(wid, false)),
            MotionPolicy::Reduced,
            "unfocused + unrecorded stays demoted"
        );
        // Start a recording of THIS window: the focus input pins true…
        let (reply, _rx) = std::sync::mpsc::channel();
        app.video_rec = Some(crate::VideoRec {
            window: wid,
            deadline: std::time::Instant::now(),
            started_us: 0,
            keys: false,
            key_log: Vec::new(),
            pace: true,
            mode: crate::VideoMode::OffscreenPresentReal,
            next_frame: None,
            dir: std::path::PathBuf::new(),
            reply,
        });
        assert!(
            app.motion_focus(wid, false),
            "recording pins the focus input"
        );
        assert_eq!(
            app.motion_policy(app.motion_focus(wid, false)),
            MotionPolicy::Full,
            "a recorded unfocused window animates"
        );
        // …for the recorded window ONLY (a sibling stays demoted)…
        assert!(
            !app.motion_focus(WindowId(1), false),
            "the pin is per-window, not app-global"
        );
        // …and never overrides the accessibility mode.
        app.system_reduce_motion = true;
        assert_eq!(
            app.motion_policy(app.motion_focus(wid, false)),
            MotionPolicy::Reduced,
            "OS Reduce-Motion still demotes a recorded window"
        );
    }

    /// LOAD-ADAPTIVE EFFECT SHEDDING (Change #1): the EMA + hysteresis latch flips
    /// `perf_reduced` TRUE only after K consecutive over-budget content presents (a
    /// lone slow frame does not trip it) and FALSE only after K consecutive
    /// under-budget presents. Under the DEFAULT (auto) policy a set latch forces
    /// `Reduced`; but the user's EXPLICIT intent overrides it — `motion = "full"` and
    /// `load_adaptive_motion = false` both keep animating while latched. Deterministic:
    /// the render cost is INJECTED, so no real GPU/timing is involved.
    #[test]
    fn perf_reduced_hysteresis_and_motion_fold() {
        use std::time::Duration;
        let mut app = App::headless_for_test();
        // Default motion (auto) + focused + no OS reduce would OTHERWISE animate at full
        // amplitude; the shed latch is what forces Reduced under this default policy.
        app.config.motion = Some("auto".into());
        app.system_reduce_motion = false;
        assert_eq!(
            app.motion_policy(true),
            MotionPolicy::Full,
            "baseline: an unloaded focused window animates"
        );

        // 60 Hz budget: shed above 1.5×16 = 24 ms, re-engage below 0.8×16 = 12.8 ms.
        let fi = Duration::from_millis(16);
        let slow = 25_000_000u64; // 25 ms render — just over the 24 ms shed threshold.
        let fast = 0u64; // an instant render — well under the 12.8 ms clear threshold.
        // Synthetic frame clock (the timing is INJECTED — no wall-clock in the test).
        let t0 = std::time::Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);

        // A SINGLE slow frame must NOT trip the latch (hysteresis).
        assert!(
            !app.note_present_cost(slow, fi, at(0)),
            "one slow frame is not a transition"
        );
        assert!(
            !app.perf_reduced,
            "hysteresis: a lone slow present does not shed"
        );
        assert_eq!(app.motion_policy(true), MotionPolicy::Full);
        // The 2nd extends the run; the 3rd (K = 3) trips it and reports the EDGE.
        assert!(
            !app.note_present_cost(slow, fi, at(16)),
            "second slow frame: still no transition"
        );
        assert!(!app.perf_reduced);
        assert!(
            app.note_present_cost(slow, fi, at(32)),
            "K-th slow frame flips the latch"
        );
        assert!(app.perf_reduced, "sustained overload sheds effects");
        assert!(
            app.load_shed_active(),
            "the raw latch is effective under default auto policy"
        );

        // Under the DEFAULT (auto) policy a load-shed window forces Reduced …
        assert_eq!(
            app.motion_policy(true),
            MotionPolicy::Reduced,
            "a load-shed window forces Reduced under the default (auto) motion policy"
        );
        assert_eq!(
            app.motion_policy(true).amplitude(MotionEffect::CursorGlow),
            0.0,
            "shed ⇒ the aurora amplitude is exactly 0 (the proven Reduced state)"
        );

        // … but the user's EXPLICIT intent overrides the shed while it is STILL latched.
        // `motion = "full"` means "always animate":
        app.config.motion = Some("full".into());
        assert_eq!(
            app.motion_policy(true),
            MotionPolicy::Full,
            "motion = \"full\" overrides the shed latch (explicit always-animate)"
        );
        assert!(
            !app.load_shed_active(),
            "direct effect gates must honor the same Full override"
        );
        // …and opting out of the heuristic (auto + load_adaptive_motion = false) does too:
        app.config.motion = Some("auto".into());
        app.config.load_adaptive_motion = Some(false);
        assert_eq!(
            app.motion_policy(true),
            MotionPolicy::Full,
            "load_adaptive_motion = false opts out of shedding entirely"
        );
        assert!(
            !app.load_shed_active(),
            "direct effect gates must honor the adaptive opt-out"
        );
        // Restore the default (auto + shedding on): the latch is still set, so Reduced.
        app.config.load_adaptive_motion = None;
        assert_eq!(
            app.motion_policy(true),
            MotionPolicy::Reduced,
            "default policy still sheds while latched"
        );
        assert!(app.load_shed_active());

        // A SINGLE fast frame must NOT clear the latch (hysteresis the other way).
        assert!(
            !app.note_present_cost(fast, fi, at(48)),
            "one fast frame is not a transition"
        );
        assert!(
            app.perf_reduced,
            "hysteresis: a lone fast present does not re-engage"
        );
        assert_eq!(app.motion_policy(true), MotionPolicy::Reduced);
        // Even a sustained fast run must NOT clear before the anti-flap dwell —
        // the post-shed cost no longer contains the effects' own cost, so
        // without the dwell this exact sequence is the relaxation oscillator.
        assert!(
            !app.note_present_cost(fast, fi, at(64)),
            "second fast frame: still shed"
        );
        assert!(
            !app.note_present_cost(fast, fi, at(80)),
            "K-th fast frame INSIDE the dwell must not clear"
        );
        assert!(app.perf_reduced, "dwell holds the latch shed");
        // Past the dwell the accumulated fast run clears it on the next sample.
        assert!(
            app.note_present_cost(fast, fi, at(32 + 1600)),
            "a fast frame past the dwell clears the latch"
        );
        assert!(!app.perf_reduced, "sustained recovery re-engages effects");
        assert!(!app.load_shed_active());
        assert_eq!(
            app.motion_policy(true),
            MotionPolicy::Full,
            "cleared latch restores the config-resolved policy"
        );
    }

    /// The adaptive loop must include CPU work associated with effect-heavy GPU
    /// submission. A compose-only sample is blind to that slice; folding the
    /// injected encode/submit CPU wall time into the causal sample trips the
    /// latch, and the cheaper reduced frame then provides negative feedback.
    /// This test intentionally makes no claim about completed shader execution.
    #[test]
    fn gpu_submit_cpu_work_is_part_of_the_load_shed_feedback_signal() {
        use std::time::Duration;

        let fi = Duration::from_millis(16);
        let compose_ns = 2_000_000;
        let heavy_gpu_submit_cpu_ns = 28_000_000;
        let reduced_gpu_submit_cpu_ns = 1_000_000;
        let t0 = std::time::Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);

        // The pre-regression compose-only signal stays far below the 24 ms
        // threshold even while fake GPU encode/submit CPU work costs 28 ms.
        let mut compose_only = App::headless_for_test();
        for frame in 0..PERF_HYSTERESIS_FRAMES {
            assert!(!compose_only.note_present_cost(compose_ns, fi, at(u64::from(frame) * 16)));
        }
        assert!(
            !compose_only.perf_reduced,
            "compose-only accounting is blind to GPU-submit CPU work"
        );

        let mut app = App::headless_for_test();
        let heavy = causal_render_cost_ns(compose_ns, heavy_gpu_submit_cpu_ns);
        assert_eq!(heavy, 30_000_000);
        for frame in 1..=PERF_HYSTERESIS_FRAMES {
            assert_eq!(
                app.note_present_cost(heavy, fi, at(u64::from(frame) * 16)),
                frame == PERF_HYSTERESIS_FRAMES,
                "only the debounced final heavy frame may flip the latch"
            );
        }
        assert!(
            app.perf_reduced,
            "causal GPU-submit CPU work trips effect shedding"
        );

        // In production the latch removes decorative passes and therefore can
        // lower their command-encoding/submit CPU cost. Model that consequence
        // directly: the sample falls immediately, and EMA + clear hysteresis —
        // once the anti-flap dwell has elapsed — restore the full policy.
        let reduced = causal_render_cost_ns(compose_ns, reduced_gpu_submit_cpu_ns);
        assert!(reduced < heavy, "shedding must lower its own causal signal");
        let mut cleared = false;
        for i in 0..8u64 {
            if app.note_present_cost(reduced, fi, at(2000 + i * 16)) {
                cleared = true;
                break;
            }
        }
        assert!(cleared, "sustained causal recovery clears the shed latch");
        assert!(!app.perf_reduced);
    }

    /// The shed metric contains the effects' own cost only while they run, so an
    /// effect-heavy session sits above the shed threshold when engaged and below
    /// the clear threshold when shed — a relaxation oscillator that used to flip
    /// (and wipe the trail) every ~6 presents. The dwell + flap-backoff must
    /// bound the flip rate: each re-shed inside the quick window doubles the
    /// next dwell, converging to long shed holds with brief restore probes.
    #[test]
    fn shed_latch_flap_backoff_bounds_transitions() {
        use std::time::Duration;
        let mut app = App::headless_for_test();
        let fi = Duration::from_millis(16);
        let t0 = std::time::Instant::now();
        // Worst-case oscillator input: over budget whenever effects are on,
        // instantly under budget whenever they are shed.
        let heavy = 30_000_000u64;
        let cheap = 1_000_000u64;
        let mut transitions = 0u32;
        let mut shed_frames = 0u32;
        let total_frames = 60_000 / 16; // one simulated minute at 60 Hz
        for frame in 0..total_frames {
            let now = t0 + Duration::from_millis(u64::from(frame) * 16);
            let cost = if app.perf_reduced { cheap } else { heavy };
            if app.note_present_cost(cost, fi, now) {
                transitions += 1;
            }
            shed_frames += u32::from(app.perf_reduced);
        }
        assert!(
            transitions <= 14,
            "flap backoff must bound the limit cycle (got {transitions} transitions/min)"
        );
        assert!(
            shed_frames > total_frames * 3 / 4,
            "a genuinely overloaded session converges to mostly-shed \
             ({shed_frames}/{total_frames} frames shed)"
        );
        assert!(
            app.perf_shed_dwell > crate::app_render::PERF_SHED_DWELL_MIN,
            "repeated flaps must have backed the dwell off"
        );
    }

    /// A latch flip drives the cursor glow/trail through the SOFT envelope —
    /// ramping over the fade window, anchored mid-fade on reversal — instead of
    /// the old hard step that cleared the engines' buffers the same frame.
    #[test]
    fn shed_envelope_fades_and_anchors_mid_fade() {
        use std::time::Duration;
        let mut app = App::headless_for_test();
        let fi = Duration::from_millis(16);
        let t0 = std::time::Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        assert_eq!(app.shed_envelope(at(0)), 1.0, "no flip yet: full amplitude");
        for f in 0..3u64 {
            app.note_present_cost(30_000_000, fi, at(f * 16));
        }
        assert!(app.perf_reduced, "latched at t=32ms");
        // Fade-out: still audible at the flip, ~half at half the window, 0 after.
        assert_eq!(app.shed_envelope(at(32)), 1.0);
        let mid = app.shed_envelope(at(32 + 225));
        assert!(
            (0.3..0.7).contains(&mid),
            "mid-fade is a partial amplitude, not a step (got {mid})"
        );
        assert_eq!(
            app.shed_envelope(at(32 + 1000)),
            0.0,
            "fade-out completes to the proven zero"
        );
        // Restore past the dwell: the envelope ramps back up from where it was.
        // (Several fast frames — the EMA must decay below the clear threshold
        // and then hold there for the debounce run.)
        for f in 0..8u64 {
            app.note_present_cost(1_000_000, fi, at(2000 + f * 16));
        }
        assert!(!app.perf_reduced, "restored past the dwell");
        let rising = app.shed_envelope(at(2128 + 100));
        assert!(
            rising > 0.0 && rising < 1.0,
            "restore ramps back up (got {rising})"
        );
        assert_eq!(
            app.shed_envelope(at(2032 + 1000)),
            1.0,
            "fade-in completes to full amplitude"
        );
    }

    #[test]
    fn motion_transition_settlement_invalidates_retained_native_frame() {
        use crate::WindowId;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::CursorMotion));
        let stamp = app.native_ui_compile_stamp(wid).unwrap();
        let compiled = app.compiled_native_ui(wid).unwrap();
        app.windows.get_mut(&wid).unwrap().native_ui_compiled =
            Some(crate::app_native::NativeCompiledFrame {
                stamp,
                phase: crate::app_native::NativeCompiledPhase::Presented,
                compiled,
            });

        app.settle_motion_policy_transition();

        assert!(
            app.windows.get(&wid).unwrap().native_ui_compiled.is_none(),
            "the post-present latch edge must not retain a full-motion native frame"
        );
    }
}

/// One frame's terminal-state snapshot for [`App::tick_cursor_fx`] — the exact
/// per-frame inputs `redraw_window`'s LOCK A reads for the cursor-effect pass,
/// snapshotted under the caller's Terminal lock (cursor + colours stay one
/// coherent observation), plus the animation clock and the window's cell grid.
pub(crate) struct CursorFxInputs {
    /// The animation clock (ONE `Instant` per redraw / capture).
    pub now: Instant,
    /// Window grid rows (terminal content — no tab-strip rows).
    pub rows: usize,
    /// Window grid cols.
    pub cols: usize,
    /// The visible cursor cell (`None` when hidden) — the engines' move sensor.
    pub cur: Option<(u16, u16)>,
    /// Raw cursor visibility (feeds the `ATERM_TRACE_SPAWN` diagnostic).
    pub cursor_visible: bool,
    /// Live cursor shape (the rainbow cursor requires a BLOCK).
    pub cursor_style: CursorStyle,
    /// This window's GUI blink phase — the rainbow twinkle's flip source (its
    /// edges become star flares once the nyan style pins the shape steady).
    pub blink_phase: bool,
    /// Live OSC-12 cursor colour (`None` ⇒ no program set one) — rewires the
    /// glow/trail colours exactly like the windowed present.
    pub live_cursor_rgb: Option<[u8; 3]>,
    /// Live default background (DECSCNM-folded) — the rainbow's dark-theme input.
    pub default_bg: u32,
    /// ERASE-POOF row probe `(row, caret)` for this frame — the chars ride in
    /// the window's `poof_row_buf`, captured under the SAME term lock as the
    /// cursor sample (row/caret in the same coordinate space as `cur`).
    /// `None` when the viewport is scrolled back, history shifted this frame,
    /// or the caller doesn't probe (headless without a capture) — the engine
    /// then keeps its previous probe and the poof detector idles.
    pub row_probe: Option<(u16, u16)>,
}

/// What [`App::tick_cursor_fx`] resolved and produced for one frame: the fold
/// inputs `redraw_window` consumes downstream (motion policy, resolved configs,
/// cell geometry) plus the tick outputs (fingerprints + cursor-fill overrides).
/// The quads themselves land in the window's `glow_scratch`/`trail_scratch`.
pub(crate) struct CursorFxTick {
    /// The `motion_focus`-folded focus (recording pin included).
    pub win_focused: bool,
    /// The one resolved MOTION POLICY (W11) every decorative consumer folds.
    pub motion: crate::motion::MotionPolicy,
    /// The (OSC-12-rewired, amplitude-scaled) aurora config.
    pub glow_cfg: crate::cursor_glow::GlowConfig,
    /// The (OSC-12-rewired, ignition heat-blended) comet colour the presented
    /// `cursor_trail` cells render at.
    pub trail_color: u32,
    /// Cell width used for the effect geometry (this window's own metrics).
    pub glow_cw: usize,
    /// Cell height used for the effect geometry.
    pub glow_ch: usize,
    /// Aurora fingerprint, rainbow-folded (0 when idle-empty).
    pub glow_fp: u64,
    /// Comet-trail fingerprint (0 when idle-empty).
    pub trail_fp: u64,
    /// FORGE fire fill for the block cursor (`None` ⇒ ordinary themed cursor).
    pub forge_fill: Option<u32>,
    /// Rainbow (nyan) block fill — wins over `forge_fill` at the splice.
    pub rainbow_fill: Option<u32>,
    /// Water DROPLET block fill — spliced after the rainbow/forge fills (the
    /// styles are mutually exclusive, so at most one is ever `Some`).
    pub droplet_fill: Option<u32>,
    /// Beam EMITTER block fill (the light-rod's block form) — spliced last.
    pub beamrod_fill: Option<u32>,
    /// ☄ Comet NUCLEUS block fill (frosted ice under the coma) — same seam.
    pub comet_fill: Option<u32>,
    /// 🔮 Phaser EMITTER block fill (locked to the beam's live hue) — same seam.
    pub phaser_fill: Option<u32>,
    /// ⚡ Whether the LIGHTNING-BOLT cursor is live (the `laser` style on a
    /// focused, visible block cursor): the caller applies the
    /// `CursorStyle::Bolt` style override — the cursor IS the lightning.
    pub bolt_cursor: bool,
    /// The bolt's flashing fill (storm violet whitening with the storm's
    /// blaze) — `Some` exactly while `bolt_cursor` holds.
    pub bolt_fill: Option<u32>,
    /// 🌟 Whether the nyan BLINK-TWINKLE is live (the `nyan` style on a focused,
    /// visible BLINKING block): the caller pins the rendered shape to
    /// `CursorStyle::SteadyBlock` so the block never vanishes on the off phase —
    /// the blink flips become the rainbow cursor's star flares instead.
    pub twinkle_cursor: bool,
}

/// Return the first match whose selection row is at least `target`, plus the
/// exact number of row comparisons performed. Search results are maintained in
/// row/column order, so the compositor can reject arbitrarily deep scrollback
/// without walking it once per presented frame.
fn lower_bound_match_row(matches: &[(i32, u16, u16)], target: i64) -> (usize, usize) {
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

/// Slice sorted selection-space matches to the half-open terminal row band.
/// A visible terminal row is `selection_row + offset`, hence selection rows in
/// `[-offset, term_rows-offset)` are the only ones the compositor can touch.
fn visible_match_range(
    matches: &[(i32, u16, u16)],
    offset: i64,
    term_rows: usize,
) -> (Range<usize>, usize) {
    let lower_row = offset.saturating_neg();
    let upper_row = i64::try_from(term_rows)
        .unwrap_or(i64::MAX)
        .saturating_sub(offset);
    let (start, lower_comparisons) = lower_bound_match_row(matches, lower_row);
    let (end, upper_comparisons) = lower_bound_match_row(matches, upper_row);
    (start..end, lower_comparisons + upper_comparisons)
}

impl App {
    /// The predictive-echo mode, resolved once per config generation (the
    /// cache is invalidated by `reload_config`) instead of re-parsing the
    /// config string on every keystroke — and now on every presented frame:
    /// the render paths (single-pane + compose) share this with the input
    /// path's press-site resolution.
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

    /// Metrics for a logical window that has not reached glass attach yet. All
    /// post-startup creation seams call this and pass the result into
    /// the typed [`WindowState`] constructors, so an additional headless/test
    /// window cannot fall back to an unrelated automatic-font seed. The renderer
    /// is ready for every such seam; the sole pending-backend construction is
    /// startup, which supplies its already-resolved metrics directly in `main`.
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

    /// Resize every pane of EVERY tab of window `wid`'s engine + PTY to its computed
    /// sub-rect (cell geometry). A pane that fills its whole tab (no split) gets the
    /// full window grid — byte-identical to the single-session resize. Records the
    /// geometry change into each session's asciicast, exactly like `apply_term_resize`.
    ///
    /// SCOPE: a live window-drag tick sets [`App::resize_live_drag`], which restricts
    /// this to the ACTIVE tab's panes (a big drag would otherwise reflow every hidden
    /// tab's scrollback per throttle tick, cost scaling with tab count though only the
    /// active tab is visible). SHARED (Cmd-Shift-O) sessions are excluded from that
    /// scoping — they stay eager on every tab so their element-wise-min geometry can't
    /// desync a co-viewer. Every non-drag caller (settle, split, activate, control
    /// `resize` verb, scale/font re-grid) runs eager AllTabs.
    pub(crate) fn resize_panes(&mut self, wid: WindowId) {
        self.resize_panes_scoped(wid, self.resize_live_drag);
    }

    /// The body of [`Self::resize_panes`]. `active_only` skips the panes of background
    /// tabs (SHARED sessions still resize on every tab). `active_only` leaves the
    /// window's `panes_stale` flag set so `redraw_window` / the trailing settle flushes
    /// the deferred tabs; an AllTabs pass clears it. Also called directly by
    /// `redraw_window` to size the active tab before presenting.
    pub(crate) fn resize_panes_scoped(&mut self, wid: WindowId, active_only: bool) {
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        let (rows, cols) = (ws.rows, ws.cols);
        let active = ws.tab_set.active_index().unwrap_or(0);
        let ntabs = ws.tab_set.len();
        // Collect (tab_index, session_id, sub_rows, sub_cols) for every pane of every
        // tab. The tab index lets the second pass skip background tabs under
        // `active_only` while its `views` lookup (below) still keeps SHARED sessions
        // eager on every tab.
        let mut targets: Vec<(usize, u64, u16, u16)> = Vec::new();
        for (ti, tab) in ws.tab_set.tabs().iter().enumerate() {
            let plan = tab.visible_plan(
                crate::tab_model::LogicalRect::new(
                    0.0,
                    0.0,
                    f32::from(cols.max(1)),
                    f32::from(rows.max(1)),
                ),
                1.0,
                |view| match self.view_store.get(view) {
                    Some(crate::tab_model::View::Terminal(_)) => crate::tab_model::LeafSizing::new(
                        crate::tab_model::LogicalSize::new(2.0, 1.0),
                        crate::tab_model::LogicalSize::new(80.0, 24.0),
                    ),
                    Some(crate::tab_model::View::Native(_)) | None => {
                        crate::tab_model::LeafSizing::new(
                            crate::tab_model::LogicalSize::new(24.0, 10.0),
                            crate::tab_model::LogicalSize::new(72.0, 36.0),
                        )
                    }
                },
            );
            for leaf in plan.leaves {
                let Some(session) = self
                    .view_store
                    .get(leaf.view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                else {
                    continue;
                };
                targets.push((
                    ti,
                    session,
                    (leaf.rect.size.height.round() as u16).max(1),
                    (leaf.rect.size.width.round() as u16).max(1),
                ));
            }
        }
        // Proxy for waking the loop to repaint after an off-thread scrollback
        // reflow re-attaches history (see the offload in the loop below).
        let reflow_wake_proxy = self.proxy.clone();
        let mut shared_changed: Vec<u64> = Vec::new();
        for (ti, id, sub_rows, sub_cols) in targets {
            // A SHARED (Cmd-Shift-O) session has ONE grid co-viewed by several
            // windows; it can't be two sizes. Drive it to the element-wise MIN across
            // all viewers so no window over-reads it (a bigger viewer letterboxes the
            // surplus; a smaller one sees the min) — instead of reflowing the shared
            // grid to whichever window happened to resize. A non-shared session keeps
            // its own computed sub-rect (byte-identical to before).
            let shared = self.pool.views(id).is_some_and(|v| v > 1);
            // Live-drag scope: skip a BACKGROUND tab's non-shared panes this tick (the
            // trailing settle / tab-switch flush sizes them). A SHARED session stays
            // eager on every tab so its co-viewed min-geometry can't desync.
            if active_only && ti != active && !shared {
                continue;
            }
            let (sub_rows, sub_cols) = if shared {
                self.shared_target_geometry(id)
            } else {
                (sub_rows, sub_cols)
            };
            let Some(s) = self.pool.get(id) else { continue };
            let pending = {
                let mut term = term_lock(&s.term);
                if term.rows() == sub_rows && term.cols() == sub_cols {
                    continue; // already this size: no engine/PTY churn
                }
                // Resize the visible grid + PTY synchronously (bounded by the
                // viewport), but OFFLOAD the unbounded width-change scrollback
                // rewrap off the main thread and off the `term` lock — the L0
                // whole-Mac-freeze fix (a 42s reflow used to run right here under
                // this lock). Returns a Send job iff there is tiered history to
                // rewrap; otherwise this is a plain, bounded resize.
                // (Temporal spine: this geometry change is recorded by the PTY reader,
                // which diffs the engine geometry under term_lock and emits an ordered
                // `Op::Resize` before its next `RawIn` — so EVERY resize path, main or
                // cross-session, is captured without a per-path enqueue. See spawn.rs.)
                term.resize_offloading_scrollback(sub_rows, sub_cols)
            };
            if let Some(pending) = pending {
                // `pending` OWNS the entire detached off-screen scrollback. If
                // `Builder::spawn` fails (thread/FD exhaustion), the closure — and
                // `pending` with it — would be dropped, destroying ALL history
                // (audit bug A). Park the job in a shared slot so the failure arm can
                // reclaim it and rewrap inline (a bounded one-off stall beats losing
                // scrollback). While a reflow is in flight the grid has no tiered
                // store, so a racing resize detaches nothing and spawns no worker —
                // self-throttling to one heavy worker per completed cycle.
                //
                // SUPERSEDE (measured, deliberate): that racing width-resize does NOT
                // cancel this job. Re-attach keeps a width-stale result — content
                // intact, wrapping self-heals on the next width change (see
                // `Grid::reattach_reflowed_scrollback`) — whereas cancel + abort
                // would drop the ENTIRE tiered history and leave the session
                // permanently ring-only (`abort_reflow_offload` re-attaches no
                // store). Bounded waste (at most ONE rewrap at the old width, by the
                // self-throttle above) beats unbounded, routine data loss, so
                // cancellation is wired ONLY to session teardown (`Session::drop`
                // raises `reflow_cancel`), where the history was dying anyway.
                let job = std::sync::Arc::new(std::sync::Mutex::new(Some((
                    pending,
                    s.term.clone(),
                    reflow_wake_proxy.clone(),
                ))));
                let cancel = s.reflow_cancel.clone();
                let run = {
                    let job = job.clone();
                    move || {
                        if let Some((pending, term, proxy)) =
                            job.lock().unwrap_or_else(|p| p.into_inner()).take()
                        {
                            // The expensive decompress + rewrap, OFF the lock — STEPPED
                            // (`drive_reflow_job`), not one-shot, so a session teardown
                            // cancels a now-pointless rewrap within ~one step instead of
                            // completing it into a dead Terminal (content-identical to
                            // the one-shot otherwise: the any-schedule property). Guard
                            // it: a panic here (corrupt cold block, alloc failure, reflow
                            // bug) would otherwise drop `pending` and leave
                            // `scrollback_detached_for_reflow` stuck true for the rest
                            // of the session — an unbounded lazy-buffer leak + all tiered
                            // history invisible (audit #5). Cancellation reuses that SAME
                            // recovery path (abort → ring-only), not a second one.
                            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                drive_reflow_job(pending, &cancel, REFLOW_WORKER_STEP_LINES)
                            })) {
                                Ok(Some(reflowed)) => {
                                    term_lock(&term).finish_resize_offload(reflowed);
                                }
                                Ok(None) => {
                                    aterm_log::info!(
                                        "reflow worker cancelled rewrapping session {id} \
                                         scrollback (session teardown); aborting the offload \
                                         (detached history released, grid recovered to \
                                         ring-only)"
                                    );
                                    term_lock(&term).abort_resize_offload();
                                }
                                Err(_) => {
                                    aterm_log::error!(
                                        "reflow worker panicked rewrapping session {id} \
                                         scrollback; aborting the offload (tiered history \
                                         lost, grid recovered to ring-only)"
                                    );
                                    term_lock(&term).abort_resize_offload();
                                }
                            }
                            // Repaint either way: rewrapped history on success, or the
                            // ring-only fallback after an abort.
                            if let Some(proxy) = proxy {
                                let _ = proxy.send_event(crate::Wake::Output {
                                    session: id,
                                    window: wid,
                                });
                            }
                        }
                    }
                };
                if std::thread::Builder::new()
                    .name("aterm-reflow".into())
                    .spawn(run)
                    .is_err()
                    && let Some((pending, term, proxy)) =
                        job.lock().unwrap_or_else(|p| p.into_inner()).take()
                {
                    // Spawn failed (thread/FD exhaustion). Rewrapping inline preserves
                    // history but runs the O(session-history) reflow on the MAIN thread —
                    // the very L0 freeze this module removes. Bound it: rewrap inline only
                    // when the history is small enough to stay imperceptible; above that,
                    // drop the tiered history (bounded, logged) rather than freeze the UI
                    // and trip the stall watchdog (audit #6).
                    const INLINE_REFLOW_MAX_LINES: usize = 20_000;
                    if pending.line_count() > INLINE_REFLOW_MAX_LINES {
                        aterm_log::error!(
                            "reflow worker spawn failed and session {id} history ({} lines) \
                             is too large to rewrap inline without a main-thread freeze; \
                             dropping tiered scrollback (grid recovered to ring-only)",
                            pending.line_count()
                        );
                        drop(pending);
                        term_lock(&term).abort_resize_offload();
                    } else {
                        // Small history: safe to rewrap inline. Still guard the panic
                        // path so it can't wedge the detach window (audit #5).
                        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            pending.reflow()
                        })) {
                            Ok(reflowed) => {
                                term_lock(&term).finish_resize_offload(reflowed);
                                aterm_log::warn!(
                                    "reflow worker spawn failed; rewrapped session {id} \
                                     scrollback synchronously on the main thread \
                                     (history preserved)"
                                );
                            }
                            Err(_) => {
                                aterm_log::error!(
                                    "reflow worker spawn failed AND inline rewrap of session \
                                     {id} panicked; aborting the offload (grid recovered)"
                                );
                                term_lock(&term).abort_resize_offload();
                            }
                        }
                    }
                    if let Some(proxy) = proxy {
                        let _ = proxy.send_event(crate::Wake::Output {
                            session: id,
                            window: wid,
                        });
                    }
                }
            }
            if shared {
                shared_changed.push(id);
            }
            aterm_pty::resize(s.master, sub_rows, sub_cols);
            // Record the geometry change into this pane's asciicast (A.5.1 #1):
            // `[t, "r", "<cols>x<rows>"]` on the recorder's own timeline. Off the
            // reader hot path; main thread, lock uncontended here.
            {
                let mut rec = s.ctx.cast.lock().unwrap_or_else(|p| p.into_inner());
                let t = rec.now();
                rec.record_resize(t, sub_cols, sub_rows);
            }
            // Temporal spine (B.9): the resize is NOT recorded here. The PTY reader
            // diffs the engine geometry under term_lock and emits an ordered
            // `Op::Resize` before its next `RawIn` chunk — so the spine records every
            // resize (main OR cross-session) exactly where the engine observed it,
            // self-healing on a dropped enqueue, with no per-path main-thread append
            // (which could jump ahead of already-queued RawIn). See spawn.rs.
        }
        // A shared session's grid changed → every co-viewing window's framed view of
        // it changed (different letterbox / sub-view), so repaint them all. The
        // resizing window `wid` also repaints via its own resize path; a duplicate
        // `request_redraw` is coalesced. Empty in the common (non-shared) case.
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
        // An AllTabs pass leaves every tab at the current grid; a scoped (live-drag)
        // pass with more than one tab may have deferred a background tab. Record which
        // so `redraw_window` / the trailing settle flushes them.
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.panes_stale = active_only && ntabs > 1;
        }
    }

    /// The glyph cell size in pixels, from the live rasterizer (GPU's internal
    /// CPU face, or the standalone CPU renderer).
    pub(crate) fn cell_size(&self) -> (usize, usize) {
        self.backend.cell_size()
    }

    /// W12 per-window pixel authority: the cell size (px) for window `wid` from
    /// ITS OWN [`MetricsView`] (`font_px`), resolved PURELY against the shared
    /// faces via [`crate::Backend::cell_geometry`]. Correct even while the shared
    /// renderer is currently activated to a DIFFERENT window's size — so a
    /// background, different-DPI window's hit-test / IME / grid geometry never
    /// reads the drawing window's cell box (the mixed-DPI bug). Falls back to the
    /// live shared backend for an unknown window (headless / pre-attach), which is
    /// byte-identical to the pre-W12 `cell_size()` there.
    pub(crate) fn win_cell_size(&self, wid: WindowId) -> (usize, usize) {
        match self.windows.get(&wid) {
            Some(ws) => {
                let (cw, ch, _) = self.backend.cell_geometry(ws.metrics.font_px);
                (cw, ch)
            }
            None => self.backend.cell_size(),
        }
    }

    /// Interior padding (device px per edge) for window `wid`, from its own
    /// per-window [`MetricsView`] (mixed-DPI). Falls back to the live shared pad
    /// for an unknown window.
    pub(crate) fn win_pad(&self, wid: WindowId) -> usize {
        self.windows
            .get(&wid)
            .map_or_else(|| self.backend.pad(), |ws| ws.metrics.pad)
    }

    /// TOP interior padding (device px) for window `wid` — the grid's Y-origin
    /// inset, tighter than [`Self::win_pad`] on an attached window (every other
    /// edge keeps `win_pad`). Falls back to the live shared `pad_top` for an
    /// unknown window.
    pub(crate) fn win_pad_top(&self, wid: WindowId) -> usize {
        self.windows
            .get(&wid)
            .map_or_else(|| self.backend.pad_top(), |ws| ws.metrics.pad_top)
    }

    /// Chrome headroom (device px) above window `wid`'s padded grid — the
    /// titlebar band the effects layer may draw into — from its own per-window
    /// [`MetricsView`]. An UNATTACHED window (headless — no attach ever writes
    /// its record — or mid-attach) falls back to the `$ATERM_HEADROOM_PX`
    /// override, else the live shared head; both are 0 by default, keeping every
    /// headless / pre-attach path byte-identical.
    pub(crate) fn win_head(&self, wid: WindowId) -> usize {
        match self.windows.get(&wid) {
            Some(ws) if ws.os_window.is_some() => ws.metrics.head,
            _ => {
                let h = crate::headroom_override();
                if h != 0 { h } else { self.backend.head() }
            }
        }
    }

    /// WINDOW-SPACE effects geometry for window `wid`'s effect streams
    /// (fire/glow/halo/nova): `(origin_x, origin_y, win_w, win_h)` in window px —
    /// the grid-interior top-left (`pad`, `pad + head + strip_px`) and the FULL
    /// frame extent, so producers emit window-absolute pixels once and clamp to
    /// the window (not the grid). `rows`/`cols` are the TERMINAL grid; `ch` the
    /// cell height the strip rows occupy. The one derivation every Geom
    /// production site shares, so the three streams can never disagree. With
    /// `head == 0`, `strip == 0` and an exact-fit frame this is the identity
    /// (origin == pad, win == padded grid) the regression law pins.
    pub(crate) fn effects_origin_win(
        &self,
        wid: WindowId,
        rows: usize,
        cols: usize,
        ch: usize,
    ) -> (u16, u16, u16, u16, u16) {
        let pad = self.win_pad(wid);
        // The effects layer's Y-origin must match the renderer's grid_top
        // (`pad_top + head`) so fire/glow/cursor-trail streams stay aligned with
        // the glyphs after the top-pad tightening; X keeps `pad`.
        let pad_top = self.win_pad_top(wid);
        let head = self.win_head(wid);
        let strip_px = self.tab_strip_rows as usize * ch.max(1);
        let (rows16, cols16) = (
            u16::try_from(rows).unwrap_or(u16::MAX),
            u16::try_from(cols).unwrap_or(u16::MAX),
        );
        let size = self.window_frame_px(rows16, cols16);
        (
            pad.min(u16::MAX as usize) as u16,
            (pad_top + head + strip_px).min(u16::MAX as usize) as u16,
            size.width.min(u32::from(u16::MAX)) as u16,
            size.height.min(u32::from(u16::MAX)) as u16,
            // The RISE ALLOWANCE component alone (the chrome band): the fire /
            // halo clamps relax by exactly this, so head == 0 keeps the
            // effects box == the grid box (the identity law).
            head.min(u16::MAX as usize) as u16,
        )
    }

    /// Glyph rasterization size (physical px) for window `wid`, from its own
    /// per-window [`MetricsView`] (mixed-DPI) — so an overlay card
    /// on a different-DPI window sizes its UI font from THAT window's px, not the
    /// shared renderer's currently-active size. Falls back to the shared `font_px`
    /// for an unknown window.
    pub(crate) fn win_font_px(&self, wid: WindowId) -> f32 {
        self.windows
            .get(&wid)
            .map_or(self.font_px, |ws| ws.metrics.font_px)
    }

    /// The window/swapchain pixel size for a `total_rows`×`cols` grid, INCLUDING
    /// the renderer's interior padding border (`2·pad` horizontally,
    /// `pad_top + pad` vertically). `total_rows` is
    /// the WHOLE composed grid the renderer presents — the terminal rows PLUS the
    /// tab-strip rows above them (the strip is spliced in as real grid rows). This
    /// is the single place window geometry is derived, so the on-screen surface,
    /// the GPU swapchain, and the offscreen framebuffer the `image` verb reads all
    /// agree. With `pad == 0` and `tab_strip_rows == 0` this is the historical
    /// `cols·cell_w × rows·cell_h`.
    pub(crate) fn frame_px(&self, total_rows: u16, cols: u16) -> PhysicalSize<u32> {
        let (w, h) = self.backend.frame_size(total_rows as usize, cols as usize);
        PhysicalSize::new(w as u32, h as u32)
    }

    /// The window pixel size for the CURRENT terminal grid: the terminal rows plus
    /// the tab strip above, padded. The canonical window/swapchain size — every
    /// window-create / resize / grid-resize path routes through this so the strip
    /// AND the interior padding are always accounted for in lockstep.
    pub(crate) fn window_frame_px(&self, rows: u16, cols: u16) -> PhysicalSize<u32> {
        self.frame_px(rows.saturating_add(self.tab_strip_rows), cols)
    }

    /// Resolve the MOTION POLICY (W11) for a window whose focus state is
    /// `focused` — THE one call every decorative-animation consumer (and every
    /// future motion feature: smooth scroll, ink-fade, …) makes. Pure + total:
    /// config `motion` (auto/full/reduced) folded with the live OS "Reduce
    /// Motion" flag and the window focus (unfocused always demotes to static).
    /// See [`crate::motion`] for the proven reduced-motion totality invariant.
    pub(crate) fn motion_policy(&self, focused: bool) -> crate::motion::MotionPolicy {
        // LOAD-ADAPTIVE EFFECT SHEDDING (Change #1): a sustained RENDER-overload session
        // forces the SAME proven zero-amplitude state as OS Reduce Motion so aterm stays
        // responsive — every governed effect (aurora/comet/sparkle/scene/stream-fade)
        // drops to amplitude 0 and the `should_repaint` content early-out re-engages.
        // Routed through the existing `MotionPolicy::Reduced` (byte-tested in `motion.rs`)
        // rather than any new per-effect gate.
        //
        // But it YIELDS to the user's EXPLICIT intent: `motion = "full"` means "always
        // animate" (it already overrides the OS Reduce-Motion flag), and
        // `load_adaptive_motion = false` opts out of the heuristic entirely. Only the
        // default (auto + shedding on) lets the latch win — so a user who asked for the
        // effects always-on keeps them even while the latch is engaged.
        let mode = self.config.motion_mode();
        if self.load_shed_active_with(mode) {
            return crate::motion::MotionPolicy::Reduced;
        }
        crate::motion::MotionPolicy::resolve(mode, self.system_reduce_motion, focused)
    }

    /// The focus bit the MOTION seam (W11) consumes for window `id`: the real
    /// OS focus, OR'd with "this window's presents are being RECORDED" (an
    /// in-flight `video` introspection capture, [`crate::VideoRec`]). W11's
    /// unfocused demotion exists so an UNWATCHED window does no decorative
    /// work — but a control-socket recording IS a watcher (typically an AI
    /// auditing the very effects the demotion zeroes), and on a busy desktop
    /// it cannot hold OS focus. Pinning the FOCUS INPUT (never the mode) keeps
    /// the capture honest — the glass itself animates while recorded — while
    /// OS Reduce-Motion, `motion = "reduced"`, and load-shed all still demote
    /// exactly as [`crate::motion::MotionPolicy::resolve`] proves.
    pub(crate) fn motion_focus(&self, id: WindowId, focused: bool) -> bool {
        focused || self.video_rec.as_ref().is_some_and(|v| v.window == id)
    }

    /// Whether the raw overload latch is allowed to suppress decorative work
    /// under the current user policy. `perf_reduced` is diagnostic history;
    /// explicit `motion = "full"` and `load_adaptive_motion = false` both opt
    /// out, so direct hot-path gates must use this projection rather than the
    /// raw latch or they contradict [`Self::motion_policy`].
    pub(crate) fn load_shed_active(&self) -> bool {
        self.load_shed_active_with(self.config.motion_mode())
    }

    /// [`Self::load_shed_active`] with the caller's ALREADY-RESOLVED
    /// [`MotionMode`](crate::motion::MotionMode), so the per-frame
    /// [`Self::motion_policy`] gate parses the `motion` config string ONCE instead
    /// of twice per present. `mode != Full` is the identical compare, so every
    /// caller sees the same result as before.
    pub(crate) fn load_shed_active_with(&self, mode: crate::motion::MotionMode) -> bool {
        self.perf_reduced
            && mode != crate::motion::MotionMode::Full
            && self.config.load_adaptive_motion_or_default()
    }

    /// A load-shed latch edge occurs after the frame which supplied the decisive
    /// timing sample has already presented. Invalidate retained native pixels and
    /// request exactly one follow-up frame so every window settles to the newly
    /// resolved motion policy. Without this edge frame a Settings preview can
    /// disarm its own timer in Reduced mode while the last full-motion raster
    /// remains on glass indefinitely.
    fn settle_motion_policy_transition(&mut self) {
        let native_windows = self
            .windows
            .keys()
            .copied()
            .filter(|wid| self.active_tab_has_native(*wid))
            .collect::<Vec<_>>();
        for wid in native_windows {
            self.invalidate_native_ui_cache(wid);
        }
        self.request_redraw_all_windows();
    }

    /// LOAD-ADAPTIVE EFFECT SHEDDING (Change #1). Fold ONE real present's causal
    /// render cost (`render_ns` — compose plus CPU raster/copy, or CPU wall time
    /// spent encoding GPU commands and calling `queue.submit`; NOT completed
    /// shader execution or the output→present wait) into the rolling EMA and
    /// re-evaluate the hysteretic
    /// [`Self::perf_reduced`] latch against this window's `frame_interval` budget.
    /// Returns `true` EXACTLY on a latch transition, so the caller toggles the GPU
    /// bloom pass only on the edge (not every frame). Deterministic in its inputs — the
    /// timing is INJECTED — so the unit test drives the whole hysteresis without a GPU.
    ///
    /// Hysteresis band: shed above [`PERF_SHED_FACTOR`]× the frame budget, re-engage
    /// below [`PERF_CLEAR_FACTOR`]× it, each debounced by [`PERF_HYSTERESIS_FRAMES`]
    /// consecutive qualifying content presents (`perf_run`).
    /// GPU/compositor back-pressure charged to this window's most recent present,
    /// as the load-shed EMA's second input. See [`gpu_backpressure_excess_ns`] for
    /// why only the excess over one frame interval counts, and why the latch was
    /// blind to GPU-bound overload without it. Zero on the CPU backend and on any
    /// healthily-paced present.
    fn gpu_backpressure_ns(&self, id: WindowId) -> u64 {
        let Some(ws) = self.windows.get(&id) else {
            return 0;
        };
        gpu_backpressure_excess_ns(
            self.last_acquire_wait_ns(id),
            ws.frame_interval.unwrap_or(self.frame_interval),
        )
    }

    /// Wall time this window's most recent present spent BLOCKED acquiring a
    /// swapchain drawable. 0 on the CPU backend (no swapchain to wait on).
    fn last_acquire_wait_ns(&self, id: WindowId) -> u64 {
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
        // Light EMA smoothing; SEEDED on the first sample so a cold start reflects the
        // true first cost instead of ramping up from 0.
        let ema = match self.present_cost_ema_ns {
            Some(prev) => PERF_EMA_ALPHA * sample + (1.0 - PERF_EMA_ALPHA) * prev,
            None => sample,
        };
        self.present_cost_ema_ns = Some(ema);
        let fi = frame_interval.as_nanos() as f64;
        let before = self.perf_reduced;
        if self.perf_reduced {
            // Re-engage effects only after a sustained run comfortably under budget —
            // AND only once the anti-flap dwell has elapsed. The post-shed cost no
            // longer contains the effects' own cost, so the clear threshold is
            // near-always satisfied; without the dwell that asymmetry is a
            // relaxation oscillator (see PERF_SHED_DWELL_MIN).
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
            // Shed only after a sustained run over budget (a lone slow frame — GC, a
            // resize hitch — must NOT trip it).
            if ema > PERF_SHED_FACTOR * fi {
                self.perf_run += 1;
            } else {
                self.perf_run = 0;
            }
            if self.perf_run >= PERF_HYSTERESIS_FRAMES {
                // A re-shed hot on the heels of the restore means the restored
                // effects immediately re-overloaded the budget: back the dwell
                // off so the next restore probe waits longer. A restore that
                // survived the quick window earns the dwell reset.
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

    /// Flip the latch, anchoring the soft envelope at its CURRENT value so a
    /// mid-fade reversal continues from where it was (no visual jump).
    fn flip_shed_latch(&mut self, now: std::time::Instant, shed: bool) {
        self.perf_env_at_flip = self.shed_envelope(now);
        self.perf_flip_at = Some(now);
        self.perf_reduced = shed;
        self.perf_run = 0;
        if shed {
            // Adaptive shedding is another Full→Reduced policy source. Settle
            // retained scroll motion on the latch edge itself; explicit
            // `motion = "full"` / adaptive opt-out remain Full because the
            // shared resolver checks the effective policy before mutating.
            self.settle_reduced_scroll_motion(now);
        }
    }

    /// The SOFT load-shed envelope in [0, 1] at `now`: ramps toward 0 while the
    /// raw latch is shed and back toward 1 while it is clear, anchored at the
    /// last flip. Callers that fold it must gate on the user's policy
    /// themselves (`motion = "full"` / `load_adaptive_motion = false` opt out
    /// of shedding entirely, so they never apply the envelope). A pure function
    /// of the injected clock — no per-frame integration — so a stalled frame
    /// cadence simply computes the caught-up value on its next frame.
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

    /// Install the exact admitted config-asset generation into one window's
    /// effects state.  This presentation/capture seam is deliberately bounded
    /// to Arc clones and scalar state: all path expansion, reads, and PNG decode
    /// happened before the config revision was published.
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
        let nyan_asset_fp = assets.nyan_sprite.fingerprint();
        let source = match &assets.nyan_sprite {
            crate::app_config::NyanSpriteAsset::BuiltIn => {
                aterm_effects::word_decorations::NyanSpriteSource::BuiltIn
            }
            crate::app_config::NyanSpriteAsset::Ready { w, h, rgba, fp, .. } => {
                aterm_effects::word_decorations::NyanSpriteSource::Custom {
                    source_fp: *fp,
                    w: *w,
                    h: *h,
                    rgba: std::sync::Arc::clone(rgba),
                }
            }
            crate::app_config::NyanSpriteAsset::Invalid { .. } => {
                aterm_effects::word_decorations::NyanSpriteSource::Disabled
            }
        };
        window.word_decos.set_nyan_sprite_source(source);
        window.installed_nyan_asset_fp = nyan_asset_fp;
        window.installed_config_assets = Some(assets);
        true
    }

    /// Resolve the cursor-aurora (additive LIGHT) config for this frame: the style +
    /// timing + brightness from config, the base colour defaulting to the themed
    /// cursor colour, and the accent defaulting to a brightened cursor. Enabled for
    /// the additive styles (the default nyan, plus phaser/lumen/sparkle/fire/laser/
    /// water and their documented aliases), the "beam" tube (no bloom crown, no
    /// ring), AND "comet" — where the aurora is the LIGHT CROWN wrapped around the
    /// [`trail_config`] cadence-comet body (best-of-both). "off" leaves it disabled
    /// (empty quads → byte-identical to no aurora).
    pub(crate) fn glow_config(&self) -> crate::cursor_glow::GlowConfig {
        let style_raw = self.config.cursor_trail_style_raw();
        let style =
            crate::app_config::resolve_trail_style(style_raw, &self.config_assets.trail_packs);
        crate::app_config::resolve_cursor_glow(
            crate::app_config::CursorGlowInputs {
                enabled: self.config.cursor_trail_or_default()
                    && self
                        .serious_mode_policy()
                        .allows(crate::motion::SeriousEffect::CursorGlow),
                style_raw,
                color: self.config.cursor_trail_color_u32(),
                accent: self.config.cursor_trail_accent_u32(),
                duration_ms: self.config.cursor_trail_ms_or_default(),
                length: self.config.cursor_trail_length_or_default(),
                intensity: self.config.cursor_trail_intensity_or_default(),
                radius: self.config.cursor_trail_radius_or_default(),
                ring: self.config.cursor_trail_ring_or_default(),
                wake_persist_s: self.config.cursor_trail_wake_persist_or_default(),
            },
            style,
            self.theme.cursor,
            // Folded to the live theme each frame in `tick_cursor_fx`.
            true,
            0.5,
        )
    }

    /// Whether the ACTIVE trail style is the native cadence-comet — the one style
    /// (`cursor_trail_style = "comet"`) that produces the directional
    /// [`aterm_render::TrailCell`] comet body (the other additive styles are the
    /// LIGHT crown only). Case-insensitive, trimmed — mirrors `glow_config`.
    pub(crate) fn trail_is_comet(&self) -> bool {
        self.config
            .cursor_trail_style_raw()
            .eq_ignore_ascii_case("comet")
    }

    /// Resolve the cadence-comet MOTION-TRAIL config for this frame: the directional
    /// comet of fading [`aterm_render::TrailCell`]s the cursor sweeps (the body the
    /// additive [`glow_config`] crown wraps). Enabled ONLY for the "comet"
    /// style; the timing/length/colour come from the shared `cursor_trail_*` tunables
    /// (the colour defaults to the themed cursor, like the glow). `intensity` starts
    /// at `0.0` — the caller stamps the live typing-cadence ignition with
    /// [`crate::cursor_trail::ignite`] just before the tick. Reduced-motion (W11) is
    /// applied by the caller (it forces `enabled` off), so an unfocused / reduced
    /// window emits ZERO comet cells (byte-identical to no trail).
    pub(crate) fn trail_config(&self) -> crate::cursor_trail::TrailConfig {
        // The cadence-comet body exists ONLY for the "comet" style, whose whole
        // palette (beam, glitter, nucleus) defaults to GLACIAL BLUE — the body
        // must match or the streak splits into two hues. An explicit
        // `cursor_trail_color` still wins, and OSC 12 recolours ride on top
        // (the caller overwrites `color` in the live-cursor block).
        let color = self
            .config
            .cursor_trail_color_u32()
            .unwrap_or(crate::cursor_glow::COMET_DEFAULT_COLOR);
        crate::cursor_trail::TrailConfig {
            enabled: self.config.cursor_trail_or_default()
                && self.trail_is_comet()
                && self
                    .serious_mode_policy()
                    .allows(crate::motion::SeriousEffect::CursorTrail),
            duration: std::time::Duration::from_millis(self.config.cursor_trail_ms_or_default()),
            max_len: self.config.cursor_trail_length_or_default(),
            color,
            // Baked per frame from the typing cadence by the caller via `ignite`.
            intensity: 0.0,
            warmth: 0.0,
        }
    }

    /// The per-frame CURSOR-EFFECT pass for one window, EXTRACTED VERBATIM from
    /// `redraw_window`'s single-pane path: resolve the MOTION POLICY (W11, with
    /// the `motion_focus` recording pin) and the glow/trail configs, rewire them
    /// to a live OSC-12 cursor colour, then advance the LUMEN aurora, the FORGE
    /// fire fill, the rainbow (nyan) cursor, and the cadence-comet trail off the
    /// `fx` snapshot — filling this window's `glow_scratch`/`trail_scratch` and
    /// returning the fingerprints + fills the caller folds into its frame.
    ///
    /// TWO callers, ONE engine clock: the windowed present (`redraw_window`) and
    /// the HEADLESS `image` capture ([`App::splice_cursor_fx`]) — so a glass-less
    /// capture composes the SAME live effect state (the fire at its true
    /// heat/decay) a real present would have. `None` only when the window is
    /// gone (the caller bails).
    pub(crate) fn tick_cursor_fx(
        &mut self,
        id: WindowId,
        fx: CursorFxInputs,
    ) -> Option<CursorFxTick> {
        let CursorFxInputs {
            now: frame_started,
            rows,
            cols,
            cur,
            cursor_visible,
            cursor_style,
            blink_phase,
            live_cursor_rgb,
            default_bg,
            row_probe,
        } = fx;
        // MOTION POLICY (W11): the one resolved gate for every decorative
        // animation this present composes — config `motion` × the live OS
        // "Reduce Motion" flag × THIS window's focus (unfocused demotes to
        // static). Each consumer takes its amplitude from this single value.
        // An in-flight `video` recording of THIS window pins the focus input
        // (`motion_focus`): the recording is a watcher, so the glass animates.
        let raw_focused = self.windows.get(&id)?.focused;
        let win_focused = self.motion_focus(id, raw_focused);
        let motion = self.motion_policy(win_focused);
        let cursor_body_allowed = self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::CursorBody);
        let terminal_sound_allowed = self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::TerminalSound);
        // The policy WITHOUT the load-shed fold, and the SOFT shed envelope. The
        // cursor glow/trail take load shed as a fade (spawns die out with the
        // amplitude; in-flight sparks decay visibly) instead of the hard
        // `MotionPolicy::Reduced` step — the old step `.clear()`ed the engines'
        // buffers the frame the latch flipped, which under a flapping latch read
        // as the trail "gapping". Accessibility (OS Reduce Motion /
        // `motion = "reduced"` / unfocused) stays a HARD zero via `policy`;
        // opting out of shedding (`motion = "full"` / `load_adaptive_motion =
        // false`) never applies the envelope at all.
        let mode = self.config.motion_mode();
        let policy =
            crate::motion::MotionPolicy::resolve(mode, self.system_reduce_motion, win_focused);
        let shed_env = if mode != crate::motion::MotionMode::Full
            && self.config.load_adaptive_motion_or_default()
        {
            self.shed_envelope(frame_started)
        } else {
            1.0
        };
        // LUMEN aurora config for this frame (on/off + style + timing + colours
        // from config). `now` is the animation clock (one `Instant` per frame,
        // captured by the caller). The aurora needs the cell geometry to map
        // cells → grid-interior pixels; read it before borrowing windows. `mut`
        // so the cursor WAKE colour can follow a live OSC-12 cursor colour below.
        let mut glow_cfg = self.glow_config();
        // Reduced motion ⇒ amplitude EXACTLY 0: the animator then clears its
        // state and emits nothing (proven zero, not merely dimmed). Load shed
        // rides in as the soft envelope instead (0 only after the fade-out).
        glow_cfg.intensity *= policy.amplitude(crate::motion::MotionEffect::CursorGlow) * shed_env;
        // Fold the LIVE theme ground: on light themes the vapor (smoke/steam)
        // switches to source-over veils (HaloMode::Over) so it reads on white.
        glow_cfg.dark_theme = aterm_render::theme_is_dark(default_bg);
        // Cadence-comet motion-trail config (the directional `TrailCell` body under the
        // aurora crown). Reduced motion (W11) forces the whole comet OFF — the trail
        // shares the CursorGlow motion seam, so an unfocused / reduced window emits no
        // comet cells at all (not merely a dimmer one). `mut` so the OSC-12 live cursor
        // colour recolours the comet body, exactly like the aurora.
        let mut trail_cfg = self.trail_config();
        trail_cfg.enabled &=
            policy.animate(crate::motion::MotionEffect::CursorGlow) && shed_env > 0.0;
        let (glow_cw, glow_ch) = self.win_cell_size(id);
        // Cursor WAKE follows the live cursor colour: when a program set OSC 12 and
        // the user did NOT pin an explicit `cursor_trail_color`, the LUMEN aurora
        // recolours to it (the auto accent re-brightens off it too). An explicit
        // config trail colour still wins, and with no OSC 12 this is a no-op.
        // The LASER is exempt: lightning is ELECTRIC YELLOW, not whatever pale
        // hue a shell prompt pushed through OSC 12 — a rosewater storm reads as
        // a smudge, not a strike (live review: "I need more yellow"). Only an
        // explicit `cursor_trail_color` may recolour the lightning.
        if self.config.cursor_trail_color_u32().is_none()
            && !matches!(glow_cfg.style, crate::cursor_glow::GlowStyle::Laser)
            && let Some(rgb) = live_cursor_rgb
        {
            let live = aterm_render::rgb_to_u32(rgb);
            glow_cfg.color = live;
            // The comet body follows the live cursor colour too (the ignition
            // heat-blend rides on top of it), so the trail matches the recoloured
            // cursor rather than the static theme colour.
            trail_cfg.color = live;
            if self.config.cursor_trail_accent_u32().is_none() {
                let m = |sh: u32| ((((live >> sh) & 0xff) as f32) * 1.5).min(255.0) as u32;
                glow_cfg.accent = (m(16) << 16) | (m(8) << 8) | m(0);
            }
        }
        // DIAGNOSTIC (env-gated): log every cursor position the effect engines
        // SEE, frame by frame — the sensor for ConPTY echo choreography (hide /
        // jump / repaint / jump-back) spawning phantom trail sparks. The env is a
        // static debug gate, so it is SAMPLED ONCE (a mid-run mutation is not a
        // supported use): the per-present cost off is a single cached bool load,
        // not the env-lock + environ scan `var_os` does every frame.
        static TRACE_SPAWN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *TRACE_SPAWN.get_or_init(|| std::env::var_os("ATERM_TRACE_SPAWN").is_some()) {
            static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
            static LAST: std::sync::Mutex<Option<Option<(u16, u16)>>> = std::sync::Mutex::new(None);
            let e = *EPOCH.get_or_init(std::time::Instant::now);
            let mut last = LAST.lock().unwrap_or_else(|p| p.into_inner());
            if *last != Some(cur) {
                eprintln!(
                    "SPAWNSRC t_us={} cur={:?} vis={}",
                    frame_started.saturating_duration_since(e).as_micros(),
                    cur,
                    cursor_visible
                );
                *last = Some(cur);
            }
        }
        // WINDOW-SPACE effects layer: the grid-interior origin + full frame extent
        // (shared derivation), so the producers emit window-absolute pixels.
        let (origin_x, origin_y, win_w, win_h, fx_head) =
            self.effects_origin_win(id, rows, cols, glow_ch);
        let ws = self.windows.get_mut(&id)?;
        // Advance the LUMEN aurora off the cursor cell (terminal coords →
        // window-absolute pixels via the cell geometry + origin); the effect
        // streams need no later splice shift (only their damage row tags move).
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
        // BAR-CURSOR ANCHOR: with a thin bar shape (DECSCUSR bar /
        // `cursor_style beam`) the streak must nose INTO the bar — the
        // classic cell-centre attach overshoots the insertion point by half
        // a cell, so the light reads as detached from the cursor.
        glow_cfg.head_dx = if matches!(
            cursor_style,
            aterm_core::terminal::CursorStyle::BlinkingBar
                | aterm_core::terminal::CursorStyle::SteadyBar
        ) {
            0.08
        } else {
            0.5
        };
        // FRESH-INK reduced-motion arm: fold the SAME motion policy the
        // amplitude multiply above resolved into the engine's own seam (the
        // sparkle/rain reduced twins' pattern — aterm-effects cannot depend on
        // `crate::motion`). Under today's policy a Reduced window already runs
        // at `intensity == 0` (nothing draws, pops included — the load-shed
        // drop), so this keeps the engine's step-fade contract wired for
        // hosts/policies that reduce DYNAMICS at nonzero amplitude.
        ws.cursor_glow
            .set_reduced_motion(!policy.animate(crate::motion::MotionEffect::CursorGlow));
        // ERASE-POOF probe feed, IMMEDIATELY before the tick: hand the engine
        // this frame's cursor-row content (captured under the caller's term
        // lock into `poof_row_buf`) so the kill detector diffs the last
        // PRESENTED truth against this frame's row. A `None` probe (scrolled
        // back / history shifted / an unwired caller) leaves the previous
        // probe in place — the detector idles rather than forgetting.
        if let Some((prow, pcaret)) = row_probe {
            ws.cursor_glow
                .observe_row(prow, pcaret, &ws.poof_row_buf, frame_started);
            // STAR-LANDING NEIGHBORS, same probe generation: the flanking
            // rows captured beside `poof_row_buf` under the caller's term
            // lock. A grid-edge neighbor was not captured — `None` tells the
            // engine that landing band is off-grid padding (provably
            // glyph-free), while a host that skips this call entirely leaves
            // the neighbors UNKNOWN and displaced stars fall back in-cell.
            ws.cursor_glow.observe_neighbor_rows(
                (prow > 0).then_some(ws.poof_row_above_buf.as_slice()),
                (usize::from(prow) + 1 < rows).then_some(ws.poof_row_below_buf.as_slice()),
            );
        }
        let glow_fp = ws.cursor_glow.tick(
            cur,
            frame_started,
            &glow_cfg,
            glow_geom,
            &mut ws.glow_scratch,
        );
        // CORRELATED FORWARD MOMENTUM (M2): the ribbon took one typed advance
        // this tick iff a real printable keystroke paired with its forward /
        // wrap / coalesced echo (the "earned by real typing only" gate). Drive
        // the cursor cat's OWN metric from that SAME pulse — Nyan style only,
        // matching the summon gate — so the cat and the ribbon build momentum
        // from one echo-correlated source and cannot diverge. Key-only input
        // that never echoes forward (a password prompt, vim vertical nav)
        // pulses on neither, so it summons no cat over a dark ribbon.
        let momentum_pulse = ws.cursor_glow.take_momentum_pulse();
        forward_nyan_cursor_cat_momentum(
            self.config.cursor_trail_or_default(),
            glow_cfg.style,
            momentum_pulse,
            &mut ws.cursor_cat,
        );
        // TRAIL SOUND: hand the spawn edge's cues to the synth. The engine
        // only records cues when it actually spawned light, so every gate the
        // aurora honours (style off, reduced motion ⇒ intensity 0, unfocused
        // demotion) already silences the sound by construction; the explicit
        // raw-focus check additionally mutes the recording-pinned case
        // (`motion_focus`) — a watched background window may animate but must
        // not talk over the foreground one. Cues are ALWAYS drained (a muted
        // session must not carry a backlog into an unmute).
        {
            let gain = trail_sound_gain(
                raw_focused,
                self.config.trail_sounds_or_default()
                    && terminal_sound_allowed
                    // Resize repaint storms drain silently (RESIZE_SOUND_QUIET).
                    && !ws.resize_sound_quiet(std::time::Instant::now()),
                self.config.trail_sound_volume(),
            );
            // The melody's tone: the window's cached typed-line verdict, or
            // the neutral identity with the knob off (a disabled knob must
            // sound bit-exactly like the pre-tone build even if a stale
            // verdict is still cached from before the toggle).
            let tone = if self.config.tone_melody_or_default() {
                ws.tone_tracker.current()
            } else {
                aterm_effects::tone::Tone::Technical
            };
            drain_trail_sound_cues(
                &mut ws.cursor_glow,
                glow_cfg.style,
                cols.min(u16::MAX as usize) as u16,
                TrailSoundPolicy {
                    voice: self.config.trail_sound_voice(),
                    gain,
                    tone,
                    bed: self.config.trail_sound_bed_or_default(),
                },
                |event| self.trail_audio.push(event),
            );
        }
        // The FORGE cursor fill (fire style): the block cursor heats along
        // the black-body ramp with sustained forward momentum and cools
        // back to the plain theme fill. Rides the same contrast-floored
        // `cursor_fill_override` seam as the rainbow cursor below; its
        // colour is already folded into `glow_fp`, so a cooling cursor
        // keeps presenting until it settles.
        let forge_fill = forge_cursor_fill(cursor_body_allowed, &glow_cfg, || {
            ws.cursor_glow.forge_fill()
        });
        // Typing-reactive RAINBOW CURSOR (the `nyan` block-cursor glow): the block
        // fill evolves from white/black toward a spinning rainbow and a rainbow halo
        // blooms, both scaled by the live typing momentum, cooling to a dim ember.
        // Active only for the `nyan` style on a FOCUSED, visible BLOCK cursor. The
        // halo joins the SAME additive aurora scratch; the fill override rides the
        // snapshot to the renderer (which floors it for glyph contrast). Reduced-
        // motion / load-shed is folded in via the same amplitude the aurora uses.
        let rainbow_block = cursor_body_allowed
            && win_focused
            && matches!(
                cursor_style,
                aterm_core::terminal::CursorStyle::BlinkingBlock
                    | aterm_core::terminal::CursorStyle::SteadyBlock
            );
        let rainbow_cfg = crate::cursor_rainbow::RainbowConfig {
            enabled: matches!(glow_cfg.style, crate::cursor_glow::GlowStyle::Nyan) && rainbow_block,
            intensity: policy.amplitude(crate::motion::MotionEffect::CursorGlow) * shed_env,
            blinking: matches!(
                cursor_style,
                aterm_core::terminal::CursorStyle::BlinkingBlock
            ),
        };
        let rainbow_energy = ws.typing_cadence.intensity(frame_started);
        let rainbow_frame = ws.cursor_rainbow.tick(
            cur,
            frame_started,
            rainbow_energy,
            blink_phase,
            aterm_render::theme_is_dark(default_bg),
            glow_geom,
            &rainbow_cfg,
            &mut ws.glow_scratch,
        );
        let rainbow_fill = rainbow_frame.fill;
        // 🌟 The nyan BLINK-TWINKLE: with the rainbow live on a BLINKING block,
        // the rendered shape is pinned steady (the caller applies the override —
        // this fn holds the window borrow, like the bolt) so the block never
        // vanishes black-and-white; the blink flips fed to the tick above fire
        // little star flares instead. Reduced motion / load-shed leaves `fill`
        // None, so the plain on/off blink is provably restored.
        let twinkle_cursor =
            rainbow_frame.fill.is_some() && rainbow_cfg.blinking && ws.cursor_rainbow.is_active();
        // Fold the rainbow-cursor fingerprint into the aurora key so an evolving
        // cursor forces a present and a settled one early-outs to idle.
        let glow_fp = glow_fp ^ rainbow_frame.fp.rotate_left(23);
        // LIQUID DROPLET CURSOR (the `water` block-cursor body): the block fill
        // turns to cool aqua and an additive bead of water with a specular
        // glint wraps the cell, beading drips off its belly and rolling ripple
        // rings across the waterline, all riding the aurora's SURGE (typing
        // heat / jump splash — read AFTER `cursor_glow.tick` above, which
        // applies the lazy heat/flare decay) so the droplet, the fluid wake,
        // and the splash belong to one body of water. Same gating as the
        // rainbow: `water` style on a FOCUSED, visible BLOCK cursor; the bead
        // joins the SAME additive aurora scratch, the fill rides the snapshot
        // (contrast-floored by the renderer).
        let droplet_cfg = crate::cursor_droplet::DropletConfig {
            enabled: matches!(glow_cfg.style, crate::cursor_glow::GlowStyle::Water)
                && rainbow_block,
            intensity: policy.amplitude(crate::motion::MotionEffect::CursorGlow) * shed_env,
        };
        let droplet_frame = ws.cursor_droplet.tick(
            cur,
            frame_started,
            ws.cursor_glow.blaze(),
            glow_geom,
            &droplet_cfg,
            &mut ws.glow_scratch,
        );
        let droplet_fill = droplet_frame.fill;
        let glow_fp = glow_fp ^ droplet_frame.fp.rotate_left(47);
        // ☄ COMET NUCLEUS CURSOR (the `comet` block-cursor body): the block
        // fill frosts to ice and an additive round COMA with twinkling rim
        // glints wraps the cell, riding the aurora's BLAZE (read AFTER
        // `cursor_glow.tick`, like the droplet) so nucleus, coma, and icy
        // dust tail belong to one comet. Same gating as its siblings:
        // `comet` style on a FOCUSED, visible BLOCK cursor; the coma joins
        // the SAME additive aurora scratch, the fill rides the snapshot.
        // Colours come from the post-OSC-12 glow config, so a live cursor
        // recolour re-tints the whole comet coherently.
        let comet_cfg = crate::cursor_comet::CometConfig {
            enabled: matches!(glow_cfg.style, crate::cursor_glow::GlowStyle::Comet)
                && rainbow_block,
            intensity: policy.amplitude(crate::motion::MotionEffect::CursorGlow) * shed_env,
            color: glow_cfg.color,
            accent: glow_cfg.accent,
        };
        let comet_frame = ws.cursor_comet.tick(
            cur,
            frame_started,
            ws.cursor_glow.blaze(),
            glow_geom,
            &comet_cfg,
            &mut ws.glow_scratch,
        );
        let comet_fill = comet_frame.fill;
        let glow_fp = glow_fp ^ comet_frame.fp.rotate_left(29);
        // 🔮 PHASER EMITTER CURSOR (the `phaser` block-cursor body): with the
        // default phaser trail active the block cursor IS the emitter — the
        // fill locks to the beam's rolling hue (read AFTER `cursor_glow.tick`
        // above, which advances the sweep on a move) so the streak reads as
        // light LEAVING the cursor, and additive beam-axis energy wings charge
        // with the typing cadence. Same gating as its siblings (`phaser` style
        // on a FOCUSED, visible BLOCK cursor); the wings join the SAME
        // additive aurora scratch, the fill rides the snapshot
        // (contrast-floored by the renderer).
        let phaser_cfg = crate::cursor_phaser::PhaserConfig {
            enabled: matches!(glow_cfg.style, crate::cursor_glow::GlowStyle::Phaser)
                && rainbow_block,
            intensity: policy.amplitude(crate::motion::MotionEffect::CursorGlow) * shed_env,
        };
        let phaser_frame = ws.cursor_phaser.tick(
            cur,
            frame_started,
            ws.cursor_glow.beam_hue(),
            rainbow_energy,
            aterm_render::theme_is_dark(default_bg),
            glow_geom,
            &phaser_cfg,
            &mut ws.glow_scratch,
        );
        let phaser_fill = phaser_frame.fill;
        let glow_fp = glow_fp ^ phaser_frame.fp.rotate_left(53);
        // 🔦 LIGHT-ROD CURSOR — every style's shape-completion seam. The thin
        // BAR becomes a vertical rod of the ACTIVE STYLE's light (the bar
        // KEEPS its DECSCUSR shape and meaning; the light is purely
        // additive) — so vim insert mode carries the water/fire/comet/…
        // identity the block bodies already own. For the styles WITHOUT a
        // bespoke block body (lumen, sparkle, trail packs) the BLOCK becomes
        // the charged emitter too (hue-locked fill, floored by the renderer,
        // inside a soft aura; sparkle's shimmers). Beam keeps both, with its
        // indigo nebula sleeve; everyone else's sleeve is their own colour
        // deepened. Same seam as the droplet: rides the aurora's blaze read
        // AFTER `cursor_glow.tick`, joins the SAME additive scratch, the
        // fill rides the snapshot.
        let bar_shape = cursor_body_allowed
            && win_focused
            && matches!(
                cursor_style,
                aterm_core::terminal::CursorStyle::BlinkingBar
                    | aterm_core::terminal::CursorStyle::SteadyBar
            );
        // Styles whose BLOCK has no bespoke body ride the emitter treatment.
        let emitter_block = matches!(
            glow_cfg.style,
            crate::cursor_glow::GlowStyle::Beam
                | crate::cursor_glow::GlowStyle::Lumen
                | crate::cursor_glow::GlowStyle::Sparkle
                | crate::cursor_glow::GlowStyle::Custom
        );
        // The rod's tint: `glow_cfg.color` where that already carries the
        // style identity (beam/comet/laser defaults, lumen/sparkle = theme,
        // any explicit user colour), but the styles that PAINT their trails
        // outside the config colour get their signature shade instead —
        // water the droplet's crest aqua, fire a warm ember, nyan the cat's
        // pink, phaser the LIVE sweep hue — so rod and trail stay one light.
        let user_tinted = self.config.cursor_trail_color_u32().is_some();
        let (rod_color, rod_haze) = match glow_cfg.style {
            crate::cursor_glow::GlowStyle::Beam => (glow_cfg.color, aterm_render::BEAM_SPACE_HAZE),
            crate::cursor_glow::GlowStyle::Water if !user_tinted => {
                (0x0032_DCDE, 0x000E_66B4) // droplet crest over deep ocean
            }
            crate::cursor_glow::GlowStyle::Fire if !user_tinted => {
                (0x00FF_9632, 0x0078_1E00) // ember over char
            }
            crate::cursor_glow::GlowStyle::Nyan if !user_tinted => {
                (0x00FF_66CC, 0x0046_1E64) // the cat's pink over dusk purple
            }
            crate::cursor_glow::GlowStyle::Phaser if !user_tinted => {
                let c = aterm_effects::color_math::hsv2rgb(ws.cursor_glow.beam_hue(), 0.75, 0.95);
                (c, (c >> 1) & 0x007F_7F7F)
            }
            _ => (glow_cfg.color, (glow_cfg.color >> 1) & 0x007F_7F7F),
        };
        let beamrod_cfg = crate::cursor_beam::BeamRodConfig {
            enabled: glow_cfg.enabled
                && glow_cfg.intensity > 0.0
                && (bar_shape || (rainbow_block && emitter_block)),
            intensity: policy.amplitude(crate::motion::MotionEffect::CursorGlow) * shed_env,
            color: rod_color,
            haze: rod_haze,
            bar: bar_shape,
            shimmer: matches!(glow_cfg.style, crate::cursor_glow::GlowStyle::Sparkle),
        };
        let beamrod_frame = ws.cursor_beamrod.tick(
            cur,
            frame_started,
            ws.cursor_glow.blaze(),
            glow_geom,
            &beamrod_cfg,
            &mut ws.glow_scratch,
        );
        let beamrod_fill = beamrod_frame.fill;
        let glow_fp = glow_fp ^ beamrod_frame.fp.rotate_left(17);
        // ⚡ LIGHTNING-BOLT CURSOR (the `laser` block-cursor shape): with the
        // laser trail active the block cursor re-forges as a jagged bolt in
        // the beam's own hue — the cursor IS the lightning. Same gating as
        // the rainbow/droplet treatments (`laser` style on a FOCUSED, visible
        // BLOCK cursor), so DECSCUSR bar/underline shapes — vim insert mode
        // and friends — keep their meaning. The SHAPE override is applied by
        // the caller (`bolt_cursor` rides the tick output — this fn holds the
        // window borrow, not the backend); the flashing fill rides the same
        // contrast-floored override seam as its siblings. The bolt FLASHES
        // with the storm (live review: "flashing yellow"): typing heat /
        // strike flare whiten the fill toward white-hot and it cools back to
        // storm violet as the air calms — blaze read AFTER
        // `cursor_glow.tick` (its lazy decay already ran), folded into the
        // aurora key so each flash step re-presents and a calm bolt settles.
        let bolt_cursor = rainbow_block
            && glow_cfg.enabled
            && glow_cfg.intensity > 0.0
            && matches!(glow_cfg.style, crate::cursor_glow::GlowStyle::Laser);
        let bolt_fill = bolt_cursor.then(|| {
            let k = 0.55 * ws.cursor_glow.blaze();
            let mix = |sh: u32| {
                let c = ((glow_cfg.color >> sh) & 0xff) as f32;
                (c + (255.0 - c) * k).min(255.0) as u32
            };
            (mix(16) << 16) | (mix(8) << 8) | mix(0)
        });
        let glow_fp = glow_fp ^ u64::from(bolt_fill.unwrap_or(0)).rotate_left(59);
        // Advance the cadence-comet MOTION TRAIL off the SAME cursor cell (terminal
        // coords; the strip splice shifts its rows down like the aurora). Stamp the
        // live typing-cadence ignition onto the config first: a fast sustained burst
        // heats the comet (longer, hotter, capped under the readability ceiling) while
        // a few keys / slow typing stay a gentle whisper. The cadence read is
        // non-mutating (idle simply decays to 0), so a steady screen produces no cells
        // → `trail_fp == 0` → the early-out returns to 0% idle.
        crate::cursor_trail::ignite(
            &mut trail_cfg,
            ws.typing_cadence.intensity(frame_started),
            ws.typing_cadence.warmth(frame_started),
        );
        let trail_fp = ws
            .cursor_trail
            .tick(cur, frame_started, &trail_cfg, &mut ws.trail_scratch);
        Some(CursorFxTick {
            win_focused,
            motion,
            glow_cfg,
            trail_color: trail_cfg.color,
            glow_cw,
            glow_ch,
            glow_fp,
            trail_fp,
            forge_fill,
            rainbow_fill,
            droplet_fill,
            beamrod_fill,
            comet_fill,
            phaser_fill,
            bolt_cursor,
            bolt_fill,
            twinkle_cursor,
        })
    }

    /// Push the current blink phase into the rasterizer.
    pub(crate) fn sync_blink_phase(&mut self) {
        let phase = self.front().is_none_or(|ws| ws.blink_phase);
        self.backend.set_cursor_blink_phase(phase);
    }

    /// Force the blink phase ON (cursor solid) and restart the blink period —
    /// the standard "cursor is solid while you type" behavior. Repaints only
    /// if the phase actually changed.
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
    /// Native focus carries no terminal capability, even while hidden tabs or
    /// sibling leaves still own live terminal sessions.
    pub(crate) fn active_native_view(
        &self,
        wid: WindowId,
    ) -> Option<(crate::tab_model::AppInstanceId, crate::tab_model::ViewId)> {
        self.windows.get(&wid)?.front_content?.native()
    }

    /// Resolve/request one Settings font candidate before semantic compilation.
    /// This is the only bridge from a native view to the parked font worker;
    /// the returned private fork is injected through `ViewCx` and remains pure
    /// thereafter. It works for any visible leaf, not only the focused tab.
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
            self.theme,
        )?;
        Some(crate::tray_raster::prepare_semantic_font(&candidate))
    }

    /// The active native Settings view only while its semantic workbench has a
    /// moving subject. This is the shared arm/fire predicate for the existing
    /// bounded Settings-demo timer, keeping every other native route at 0% idle.
    pub(crate) fn active_native_settings_preview(
        &self,
        wid: WindowId,
        phase_ms: u64,
    ) -> Option<(
        crate::tab_model::ViewId,
        crate::settings_preview::PreviewAnimation,
    )> {
        if !self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::SettingsPreview)
        {
            return None;
        }
        let (_, view) = self.active_native_view(wid)?;
        let viewport = self.native_ui_viewport(wid).ok()?;
        match self.native_runtime.view_state(view) {
            Some(crate::native_app::AppViewState::Settings(state)) => {
                let motion = self.native_view_motion_cx(wid, view);
                let font_px = self.win_font_px(wid);
                let mut animation =
                    state.preview_animation(phase_ms, motion, font_px, self.theme, viewport);
                // Explicit host preparation seam: the backend-owned parked
                // service is requested/polled here, before view compilation or
                // paint. Its bounded one-shot convergence cadence invalidates
                // exactly this active Settings view, then returns to pure Wait.
                if self
                    .prepare_native_semantic_font(wid, view, phase_ms)
                    .is_some_and(|prepared| prepared.snapshot.pending)
                {
                    animation = crate::settings_preview::PreviewAnimation::Continuous;
                }
                (animation != crate::settings_preview::PreviewAnimation::None)
                    .then_some((view, animation))
            }
            _ => None,
        }
    }

    /// Advance one active Settings preview and dirty only the logical preview
    /// band retained on glass.  The scheduler calls this after the preview has
    /// already been presented once, so the retained compiled tree is the exact
    /// geometry authority.  If that authority is unavailable we fail closed to
    /// a full repaint; route, service, and appearance transitions continue to
    /// request `All` independently.
    pub(crate) fn invalidate_active_native_settings_preview(
        &mut self,
        wid: WindowId,
        phase_ms: u64,
    ) -> bool {
        let Some((view, _animation)) = self.active_native_settings_preview(wid, phase_ms) else {
            return false;
        };
        let damage = self
            .windows
            .get(&wid)
            .and_then(|window| window.leaf_render_cache.get(&view))
            .and_then(|cache| cache.native.as_ref())
            .and_then(|raster| preview_damage_from_compiled(&raster.compiled))
            .unwrap_or(crate::native_app::DamageRegion::All);
        self.invalidate_native_view_cache(wid, view, damage);
        true
    }

    /// Append the active modal surface after native application primitives.
    /// `logical_x` maps the terminal-grid overlay into the destination tray:
    /// ordinary native cards include the left window pad, while heterogeneous
    /// cards are already placed at that pad and therefore pass zero.
    fn append_native_modal_prims(
        &self,
        id: WindowId,
        prims: &mut Vec<crate::widget::DrawPrim>,
        logical_x: f32,
        grid: (usize, usize, usize),
        scale: f32,
    ) -> bool {
        let (cw, ch, cols) = grid;
        let Some(window) = self.windows.get(&id) else {
            return false;
        };
        let Some(overlay) = window.overlay.as_ref() else {
            return false;
        };
        let geom = crate::settings::SettingsGeom {
            cw: cw as f32 / scale,
            ch: ch as f32 / scale,
            font_px: 13.0,
            cols,
            panel_rows: window.overlay_rows(),
        };
        let ctx = crate::settings::PreviewCtx {
            system_dark: repaint_system_dark(self.os_appearance),
            scale,
            trail_color: self.config.cursor_trail_color_u32(),
            trail_accent: self.config.cursor_trail_accent_u32(),
        };
        let mut modal = overlay.model().tray(&geom, self.theme, ctx);
        crate::widget::translate_prims(&mut modal.prims, logical_x, 0.0);
        prims.extend(modal.prims);
        true
    }

    /// Present one native app through the same retained framebuffer, tray quad,
    /// CPU/GPU composite, and image-capture seam as the rest of aterm. The
    /// semantic tree is the sole source for paint and (in the input path) hits.
    /// Build the canonical native-app `RenderInput` + semantic tray used by
    /// both glass presentation and the control `image` capture. Keeping the
    /// preparation here makes paint pixels, hit regions, inspection, and PNG
    /// capture projections of the same compiled UI tree.
    pub(crate) fn prepare_native_input_scratch(&mut self, id: WindowId) -> bool {
        let Some((instance, view)) = self.active_native_view(id) else {
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
        let Ok(viewport) = self.native_ui_viewport(id) else {
            return false;
        };
        let Ok(stamp) = self.native_ui_compile_stamp(id) else {
            return false;
        };
        let Ok(compiled) = self.compiled_native_ui(id) else {
            return false;
        };
        let width = u32::try_from(
            cols.saturating_mul(cw)
                .saturating_add(pad.saturating_mul(2)),
        )
        .unwrap_or(u32::MAX);
        let height = u32::try_from(rows.saturating_mul(ch).saturating_add(pad)).unwrap_or(u32::MAX);
        #[cfg(feature = "a11y-accesskit")]
        let overlay_open = self
            .windows
            .get(&id)
            .is_some_and(|window| window.overlay.is_some());

        let theme = self.theme;
        // A native app and a modal palette occupy one retained overlay texture. Fold both
        // semantic fingerprints so opening, filtering, selecting, or closing the palette
        // necessarily rebuilds the WYSIWYG raster instead of leaving an invisible input
        // boundary over the previous native frame.
        let overlay_fp = self
            .windows
            .get(&id)
            .map_or(0, |window| window.overlay_fp());
        let fp = {
            use std::hash::{Hash, Hasher};
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            compiled.fingerprint().hash(&mut hash);
            overlay_fp.hash(&mut hash);
            stamp.hash(&mut hash);
            hash.finish() | 1
        };
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
        let mut cache = self
            .windows
            .get_mut(&id)
            .and_then(|window| window.leaf_render_cache.remove(&view))
            .unwrap_or_default();
        let leaf_rasterized =
            if let Some(damage) = pending_native_leaf_damage(&cache, stamp, width, height) {
                retain_native_leaf_raster(
                    &mut cache,
                    crate::app_native::NativeLeafScene {
                        stamp,
                        instance,
                        view,
                        viewport,
                        damage,
                        compiled: compiled.clone(),
                    },
                    width,
                    height,
                    scale,
                    theme,
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
        let should_raster = leaf_rasterized
            || self.windows.get(&id).is_some_and(|ws| {
                ws.settings_card
                    .as_ref()
                    .is_none_or(|card| card.fp != fp || card.geom != geom)
            });
        let raster = should_raster.then(|| {
            let mut prims = Vec::new();
            if self.append_native_modal_prims(
                id,
                &mut prims,
                pad as f32 / scale,
                (cw, ch, cols),
                scale,
            ) {
                // A modal must be lowered after the app in one raster pass: its
                // translucent edges then blend in the same linear-light space as
                // the native surface underneath.
                let mut tray = compiled.tray(theme, 13.0);
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
        let blank = chrome_band::blank_cell(theme);
        let Some(ws) = self.windows.get_mut(&id) else {
            return false;
        };
        fill_divider_grid(&mut ws.input_scratch, rows, cols, theme);
        // No pane fx clip on a native tab (the reused scratch may carry a
        // prior split frame's box — present-time post-fx must not inherit it).
        ws.input_scratch.fx_clip = None;
        for row in &mut ws.input_scratch.cells {
            row.fill(blank);
        }
        ws.input_scratch.default_bg = theme.bg;
        ws.input_scratch.cursor_color = theme.cursor;
        ws.input_scratch.cursor_visible = false;
        ws.input_scratch.selection = aterm_core::selection::TextSelection::new();
        ws.input_scratch.snapshot_seq = ws.input_scratch.snapshot_seq.wrapping_add(1);
        ws.native_ui_compiled = Some(crate::app_native::NativeCompiledFrame {
            stamp,
            phase: crate::app_native::NativeCompiledPhase::Staged,
            compiled: compiled.clone(),
        });
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
                rgba,
                pw,
                ph,
                dx: 0,
                dy,
                fp,
                geom,
            });
        }
        self.splice_tab_strip_with(id, tab_strip);
        #[cfg(feature = "a11y-accesskit")]
        if !overlay_open {
            self.stage_native_accessibility(id, view, &compiled);
        }
        true
    }

    /// Compose every visible canonical leaf in a mixed/native split. Terminal
    /// leaves are snapshotted into stable per-view buffers and native leaves are
    /// compiled/rasterized into stable per-view caches, then all lanes are merged
    /// into the ordinary terminal framebuffer plus its one transparent tray quad.
    pub(crate) fn prepare_heterogeneous_input_scratch(&mut self, id: WindowId) -> Option<String> {
        use std::hash::{Hash, Hasher};

        let plan = self.active_visible_leaf_plan(id)?;
        if plan.leaves.len() <= 1 || !self.active_tab_has_native(id) {
            return None;
        }
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
        let width = u32::try_from(cols.saturating_mul(cw)).unwrap_or(u32::MAX);
        let height = u32::try_from(rows.saturating_mul(ch)).unwrap_or(u32::MAX);
        let byte_len = usize::try_from(width)
            .ok()?
            .checked_mul(usize::try_from(height).ok()?)?
            .checked_mul(4)?;
        let mut native_layer = vec![0u8; byte_len];
        let mut modal_native_prims = Vec::new();
        let mut native_fp = std::collections::hash_map::DefaultHasher::new();
        let mut title = "aterm".to_string();
        let mut focused_compiled = None;
        let visible: std::collections::BTreeSet<_> =
            plan.leaves.iter().map(|leaf| leaf.view).collect();

        if let Some(window) = self.windows.get_mut(&id) {
            fill_divider_grid(&mut window.input_scratch, rows, cols, self.theme);
            window.input_scratch.default_bg = aterm_core::render::COLOR_UNSET;
            window.input_scratch.cursor_color = aterm_core::render::COLOR_UNSET;
            window.input_scratch.selection = aterm_core::selection::TextSelection::new();
        }

        for leaf in &plan.leaves {
            match self.view_store.get(leaf.view).copied()? {
                crate::tab_model::View::Terminal(terminal) => {
                    let session = self.pool.get(terminal.session)?;
                    let term = session.term.clone();
                    let mut cache = self
                        .windows
                        .get_mut(&id)?
                        .leaf_render_cache
                        .remove(&leaf.view)
                        .unwrap_or_default();
                    let sub_rows = (leaf.rect.size.height.round() as usize).max(1);
                    let sub_cols = (leaf.rect.size.width.round() as usize).max(1);
                    let (terminal_title, blank) = {
                        let mut terminal = term_lock(&term);
                        terminal.cell_frame_into(&mut cache.input, sub_rows, sub_cols);
                        let blank = terminal_blank_cell(&terminal);
                        terminal.take_damage();
                        // Lock diet: an Arc clone under the hold; the owned
                        // String materializes after the guard drops (below).
                        (terminal.title_arc(), blank)
                    };
                    cache.native = None;
                    cache.native_damage = None;
                    let row = leaf.rect.origin.y.round().max(0.0) as usize;
                    let col = leaf.rect.origin.x.round().max(0.0) as usize;
                    if let Some(window) = self.windows.get_mut(&id) {
                        if leaf.focused && cache.input.cursor_visible {
                            window.input_scratch.cursor_row = row + cache.input.cursor_row;
                            window.input_scratch.cursor_col = col + cache.input.cursor_col;
                            window.input_scratch.cursor_style = cache.input.cursor_style;
                            window.input_scratch.cursor_visible = true;
                        }
                        blit_pane_into(&mut window.input_scratch, &cache.input, row, col, blank);
                        window.leaf_render_cache.insert(leaf.view, cache);
                    }
                    if leaf.focused && !terminal_title.is_empty() {
                        title = terminal_title.to_string();
                    }
                }
                crate::tab_model::View::Native(native) => {
                    let leaf_width = (leaf.rect.size.width * cw as f32).round().max(1.0) as u32;
                    let leaf_height = (leaf.rect.size.height * ch as f32).round().max(1.0) as u32;
                    let viewport = crate::native_ui::LogicalRect::new(
                        0.0,
                        0.0,
                        leaf_width as f32 / scale,
                        leaf_height as f32 / scale,
                    );
                    let stamp = self
                        .native_ui_compile_stamp_for(id, native.instance, leaf.view, viewport)
                        .ok()?;
                    stamp.hash(&mut native_fp);
                    let mut cache = self
                        .windows
                        .get_mut(&id)?
                        .leaf_render_cache
                        .remove(&leaf.view)
                        .unwrap_or_default();
                    if let Some(damage) =
                        pending_native_leaf_damage(&cache, stamp, leaf_width, leaf_height)
                    {
                        let scene = self
                            .build_native_leaf_scene(
                                id,
                                native.instance,
                                leaf.view,
                                viewport,
                                damage,
                            )
                            .ok()?;
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
                            self.theme,
                        );
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
                    let raster = cache.native.as_ref()?;
                    raster.compiled.fingerprint().hash(&mut native_fp);
                    blit_rgba_over(
                        &mut native_layer,
                        (width, height),
                        &raster.rgba,
                        (raster.width, raster.height),
                        (x, y),
                    );
                    if overlay_open {
                        let logical_x = x as f32 / scale;
                        let logical_y = y as f32 / scale;
                        modal_native_prims.push(crate::widget::DrawPrim::ClipPush {
                            x: logical_x,
                            y: logical_y,
                            w: leaf_width as f32 / scale,
                            h: leaf_height as f32 / scale,
                        });
                        let mut tray = raster.compiled.tray(self.theme, 13.0);
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

        overlay_fp.hash(&mut native_fp);
        if overlay_open {
            // Re-lower the visible native leaves and append the modal as the
            // final primitives in one full-window tray. This is the same
            // linear-light ordering as a single native tab, while transparent
            // terminal lanes remain visible below the modal card.
            let appended = self.append_native_modal_prims(
                id,
                &mut modal_native_prims,
                0.0,
                (cw, ch, cols),
                scale,
            );
            debug_assert!(appended, "an open overlay must provide modal primitives");
            native_layer = crate::tray_raster::rasterize_tray_pixels(
                &modal_native_prims,
                width,
                height,
                scale,
                [0, 0, 0, 0],
            );
        }

        let mut geometry = std::collections::hash_map::DefaultHasher::new();
        width.hash(&mut geometry);
        height.hash(&mut geometry);
        cw.hash(&mut geometry);
        ch.hash(&mut geometry);
        scale.to_bits().hash(&mut geometry);
        for leaf in &plan.leaves {
            leaf.view.hash(&mut geometry);
            leaf.rect.origin.x.to_bits().hash(&mut geometry);
            leaf.rect.origin.y.to_bits().hash(&mut geometry);
            leaf.rect.size.width.to_bits().hash(&mut geometry);
            leaf.rect.size.height.to_bits().hash(&mut geometry);
        }
        let geom = geometry.finish();
        let fp = native_fp.finish() | 1;
        let tab_strip = self.redraw_tab_strip_state(id);
        let card_dx = u32::try_from(self.win_pad(id)).unwrap_or(u32::MAX);
        let card_dy = u32::try_from(self.native_content_origin_y(id)).unwrap_or(u32::MAX);
        if let Some(window) = self.windows.get_mut(&id) {
            window
                .leaf_render_cache
                .retain(|view, _| visible.contains(view));
            window.settings_card = Some(crate::SettingsCard {
                rgba: native_layer,
                pw: width,
                ph: height,
                dx: card_dx,
                dy: card_dy,
                fp,
                geom,
            });
            window.native_ui_compiled =
                focused_compiled.map(|(stamp, compiled)| crate::app_native::NativeCompiledFrame {
                    stamp,
                    phase: crate::app_native::NativeCompiledPhase::Staged,
                    compiled,
                });
            window.input_scratch.snapshot_seq = window.input_scratch.snapshot_seq.wrapping_add(1);
        }
        #[cfg(feature = "a11y-accesskit")]
        if !overlay_open {
            self.stage_visible_native_accessibility(id);
        }
        self.splice_tab_strip_with(id, tab_strip);
        Some(title)
    }

    /// A surface transaction returned no successful commit. Clear the
    /// optimistic repaint stamp and schedule only a bounded, strictly-future
    /// recovery attempt. Persistent occlusion/validation/mismatch failures park
    /// immediately; retryable failures use exponential backoff then park. Any
    /// later genuine external stimulus opens the redraw gate for the same
    /// unstamped frame.
    pub(crate) fn rearm_dropped_present(
        &mut self,
        id: WindowId,
        reason: metrics::PresentDropReason,
    ) {
        if let Some(state) = self.windows.get_mut(&id) {
            state.on_present_dropped();
            let parked = state
                .present_retry
                .on_drop(reason, Instant::now())
                .is_none();
            metrics::note_present_drop(reason, parked);
        }
    }

    /// Detect a loss latched during either surface acquisition or submission and
    /// recover before the generic retry scheduler can consume fuel against a
    /// permanently dead device. Returns true when this attempt is fully handled.
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

    /// Execute the transaction after a GPU loss has been observed and routed.
    /// The injected recovery operation is the Tier-1 seam: tests can force the
    /// otherwise platform-dependent CPU-builder result while exercising the
    /// exact shipping accounting, recording-abort, and retry scheduling branch.
    /// Its typed result prevents a globally-installed CPU backend from being
    /// mistaken for a source target that is ready and owns a redraw request.
    #[must_use]
    pub(crate) fn complete_latched_gpu_loss_recovery(
        &mut self,
        id: WindowId,
        failed_reason: Option<metrics::PresentDropReason>,
        frame_started: Instant,
        recover: impl FnOnce(&mut Self, WindowId, bool) -> GpuRecoveryOutcome,
    ) -> GpuRecoveryOutcome {
        let source_drop_counted = failed_reason.is_some();

        // Publish the failed GPU transaction BEFORE fallback. If constructing
        // this window's CPU target also fails, that later `CpuAcquire` remains
        // the final diagnostic and agrees with its live retry state.
        if let Some(reason) = failed_reason {
            if let Some(state) = self.windows.get_mut(&id) {
                state.on_present_dropped();
            }
            metrics::note_present_drop(reason, false);
        }
        // A recording target is allocated from the lost device too. End it
        // honestly before replacing the backend; otherwise a headless Virtual
        // target keeps its pacing deadline but can never produce another frame.
        let _ = self.video_abort_device_loss();
        let recovered = recover(self, id, source_drop_counted);
        // Include fallback construction in the whole-redraw wall clock. This is
        // intentionally outside `frame_render` (it is recovery, not steady-state
        // raster work) but must remain visible when diagnosing a one-off stall.
        metrics::record_redraw_total(frame_started.elapsed().as_nanos() as u64);
        if recovered == GpuRecoveryOutcome::BackendUnavailable {
            // A CPU-backend construction failure is transient and must own a
            // typed bounded retry even when device loss was reported after a
            // nominally successful GPU present (`failed_reason == None`).
            // Reusing the original surface reason here would both double-count
            // that one dropped frame and could park forever when the original
            // reason was a persistent GPU condition. Keep one count per lost
            // frame while making the final diagnostic agree with the live
            // CPU-recovery retry state.
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

    /// The single failed-present funnel for terminal, native, and heterogeneous
    /// windows. Device loss is resolved first; only a live device/surface may
    /// consume the ordinary bounded retry budget.
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

    fn redraw_heterogeneous_window(
        &mut self,
        id: WindowId,
        frame_started: Instant,
        window: &Option<Arc<winit::window::Window>>,
    ) {
        let Some(title) = self.prepare_heterogeneous_input_scratch(id) else {
            return;
        };
        if let Some(window) = window {
            self.apply_title(id, window, &title);
        }
        let compose_ns = u64::try_from(frame_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        metrics::note_pre_present(compose_ns);
        let raster_submit_ns = match self.present_input_scratch(id, false, None) {
            Ok(work_ns) => work_ns,
            Err(reason) => {
                self.handle_failed_present(id, reason, frame_started);
                return;
            }
        };
        let render_ns = causal_render_cost_ns(compose_ns, raster_submit_ns);
        if self.recover_latched_gpu_loss(id, None, frame_started) {
            return;
        }
        if let Some(window) = window {
            self.update_accessibility(id, window);
        }
        metrics::record_present(self.present_latency_ns(id), render_ns);
        metrics::record_redraw_total(frame_started.elapsed().as_nanos() as u64);
        self.first_present_done = true;
        if let Some(state) = self.windows.get_mut(&id) {
            state.content_pending = false;
            state.redraw_pending = false;
            state.on_capture_presented();
            state.on_present_displayed();
            if let Some(frame) = state.native_ui_compiled.as_mut() {
                frame.phase = crate::app_native::NativeCompiledPhase::Presented;
            }
            for cache in state.leaf_render_cache.values_mut() {
                if let Some(raster) = cache.native.as_mut() {
                    raster.presented = true;
                }
            }
        }
    }

    fn redraw_native_window(
        &mut self,
        id: WindowId,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        frame_started: Instant,
        window: &Option<Arc<winit::window::Window>>,
    ) {
        if !self.prepare_native_input_scratch(id) {
            return;
        }

        let title = self
            .native_runtime
            .presentation(instance, view)
            .map_or_else(|_| "aterm".to_string(), |presentation| presentation.title);
        if let Some(window) = window {
            self.apply_title(id, window, &title);
        }
        let compose_ns = u64::try_from(frame_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        metrics::note_pre_present(compose_ns);
        let raster_submit_ns = match self.present_input_scratch(id, false, None) {
            Ok(work_ns) => work_ns,
            Err(reason) => {
                self.handle_failed_present(id, reason, frame_started);
                return;
            }
        };
        let render_ns = causal_render_cost_ns(compose_ns, raster_submit_ns);
        if self.recover_latched_gpu_loss(id, None, frame_started) {
            return;
        }
        if let Some(window) = window {
            self.update_accessibility(id, window);
        }
        metrics::record_present(self.present_latency_ns(id), render_ns);
        metrics::record_redraw_total(frame_started.elapsed().as_nanos() as u64);
        self.first_present_done = true;
        let presented_stamp = self.native_ui_compile_stamp(id).ok();
        if let Some(ws) = self.windows.get_mut(&id) {
            ws.content_pending = false;
            ws.redraw_pending = false;
            ws.on_capture_presented();
            ws.on_present_displayed();
            if let Some(frame) = ws.native_ui_compiled.as_mut()
                && Some(frame.stamp) == presented_stamp
            {
                frame.phase = crate::app_native::NativeCompiledPhase::Presented;
            }
            if let Some(raster) = ws
                .leaf_render_cache
                .get_mut(&view)
                .and_then(|cache| cache.native.as_mut())
                && Some(raster.stamp) == presented_stamp
            {
                raster.presented = true;
            }
        }
    }

    pub(crate) fn redraw_window(&mut self, id: WindowId) {
        metrics::note_redraw_attempt();
        // A retry deadline owns the next surface attempt until it is consumed;
        // after exhaustion/persistent failure the episode remains parked until
        // a genuine external stimulus (surface lifecycle, user input, tab
        // switch, or explicit visual reconfiguration) resets it. This guard is
        // deliberately before sprite polling, EDR work, and both terminal locks:
        // animation timers may still request redraws, but can never recreate the
        // failed-present -> full-grid-extraction CPU loop.
        match self.windows.get(&id) {
            Some(ws) if !ws.present_retry.present_attempt_allowed() => {
                metrics::note_redraw_retry_gated();
                return;
            }
            None => return,
            Some(_) => {}
        }
        // Frame wall-clock start, read back into the `metrics` verb's
        // `last_frame_render_ms` on an actual present (early-out frames return before
        // `record_present`, so they never count). One `Instant::now()` per redraw.
        let frame_started = Instant::now();
        // GPU-loss fallback changes the shared backend before every per-window
        // CPU surface is guaranteed to exist. Rebuild any missing/mismatched
        // target here, inside the same bounded failed-present funnel: a transient
        // softbuffer allocation failure backs off instead of leaving a dead GPU
        // target that the CPU backend rejects forever.
        if let Err(reason) = self.ensure_cpu_present_target(id) {
            self.handle_failed_present(id, reason, frame_started);
            return;
        }
        // Re-sample this panel's live EDR headroom (throttled) before composing the
        // aurora: brightness changes on the SAME monitor move the headroom with no
        // Moved / screen-parameter event, so a monitor-change-only refresh leaves it
        // stale and the >1.0 EDR boost clips or under-fills. No-op off HDR/GPU.
        self.refresh_edr_headroom(id, frame_started);
        // SYNC-1: the frame-hold safety-valve timeout for this present, resolved from the
        // engine config the sessions actually run (default 1 s). Read BEFORE the
        // per-window borrows below so it stays a plain local (a method call once `ws` is
        // borrowed would re-borrow all of `self`).
        //
        // The VISUAL hold is additionally capped at [`SYNC_HOLD_CAP`]: the protocol
        // timeout (1 s) is the terminal-side mode clear for a crashed app, but a
        // present withheld that long IS the frozen screen the mode exists to prevent.
        // A tear on a pathological bracket is strictly better than a 1 s freeze.
        let sync_hold_timeout = std::time::Duration::from_millis(
            self.session_factory
                .terminal_config
                .as_ref()
                .map_or(1000, |tc| tc.sync_timeout_ms),
        )
        .min(SYNC_HOLD_CAP);
        // VI-1: the on-viewport vi (copy-mode) cursor screen `(row, col)`, computed inside
        // the snapshot block (where the term-locked reads live) and applied to the cursor
        // AFTER the prediction override below — both blocks are otherwise out of each
        // other's scope, so hoist it here. `None` when vi is off or scrolled off-viewport.
        let mut vi_cursor_override: Option<(usize, usize)> = None;
        let Some(ws0) = self.windows.get(&id) else {
            return;
        };
        // HEADLESS PRESENT-REAL: `window` is `None` ONLY on the Virtual present
        // path (a glass-less recording target — the match below returns for
        // every other windowless state), so each OS-window-dependent call site
        // below (title chrome, redraw pacing, accessibility) gates on `Some`;
        // the Virtual loop is timer-driven and needs none of them.
        let window = ws0.os_window.clone();
        // No present target yet (surface not created): nothing to draw into, and
        // we must NOT consume damage, so bail before touching the lock.
        match ws0.present.as_ref() {
            Some(PresentTarget::Gpu { .. }) if self.backend.is_gpu() && window.is_some() => {}
            Some(PresentTarget::Cpu { .. }) if !self.backend.is_gpu() && window.is_some() => {}
            // The glass-less recording target (attached ONLY while an
            // offscreen-present-real `video` is in flight — the mode-honesty
            // law): the WaitUntil recording loop drives this same redraw into
            // the virtual target.
            Some(PresentTarget::Virtual { .. }) if self.backend.is_gpu() => {}
            // Present target absent, or backend/target kind mismatch (transient
            // during a backend rebuild): nothing valid to draw into.
            _ => return,
        }
        // Per-window DPI (W12): SELECT this window's own resolved metrics on the
        // shared renderer before composing (guarded no-op when already active), so a
        // window on a different-DPI monitor renders at its own scale rather than
        // whichever window last drew. This is now the LIGHT `activate_px` switch —
        // it keeps every other window's warm glyph atlas resident (sizes coexist by
        // `px_q`), no teardown — so run it before the borrows below.
        self.apply_window_scale(id);
        if self.active_tab_has_native(id)
            && self
                .active_visible_leaf_plan(id)
                .is_some_and(|plan| plan.leaves.len() > 1)
        {
            self.redraw_heterogeneous_window(id, frame_started, &window);
            return;
        }
        if let Some((instance, view)) = self.active_native_view(id) {
            self.redraw_native_window(id, instance, view, frame_started, &window);
            return;
        }
        let Some(front_terminal) = self.front_terminal_mirror(id) else {
            return;
        };
        // A live window-drag sized only the active tab (`resize_panes` scope); if the
        // user SWITCHED to a tab that was deferred, size it to the current grid before
        // we snapshot it — else it would present at the pre-drag geometry. Gated on the
        // per-window flag so the steady path pays one bool load; the resize itself
        // early-outs per pane already at the right size (the just-drawn active tab), so
        // it is cheap even while the flag stands. Cleared by the AllTabs settle.
        if self.windows.get(&id).is_some_and(|ws| ws.panes_stale) {
            self.resize_panes_scoped(id, true);
        }
        let Some(ws0) = self.windows.get(&id) else {
            return;
        };
        let (rows, cols) = (ws0.rows as usize, ws0.cols as usize);
        let grid_top = self.win_pad_top(id) + self.win_head(id);
        // Strip row count for the resident strip-row pool reclaim at the single-pane
        // `cell_frame_into` refill boundary below (0 when the strip is disabled).
        let strip_rows_n = self.tab_strip_rows as usize;
        // Visual bell: the presented frame has its RGB inverted while a flash is
        // active. The flash state machine decides "active"; `about_to_wait` wakes
        // the loop at its deadline so the normal frame returns.
        // ANY modal overlay (Settings, About, Palette, Update) suppresses the bell-flash
        // invert, the drag-drop wash (below), and the level-up glow (at the OverlayGlow
        // build) so the card and the terminal behind it stay stable and render IDENTICALLY
        // on CPU + GPU. The GPU bakes the card into the offscreen, so a whole-frame invert
        // there would photo-negative the modal; the CPU composites the card last (pristine).
        // `overlay_open()` is the ONE gate — snapshot and the `image` verb consult the same
        // method, so glass and capture cannot disagree (the SACRED WYSIWYG invariant).
        let overlay_open = ws0.overlay_open();
        let invert = ws0.bell_flash.is_active(Instant::now()) && !overlay_open;
        // Unfocused windows force a hollow cursor (mirrors `on_focus`); part of
        // the visual state the grid damage tracker doesn't see. A GLASS-LESS
        // window (the Virtual recording path) is never OS-focused, but its
        // recording must show the FOCUSED solid-block look, never a hollow
        // ghost — the `motion_focus` recording pin's twin for the cursor shape
        // (windowed recordings keep WYSIWYG: an unfocused window on glass shows
        // hollow, so its recording does too).
        let cursor_override =
            (!ws0.focused && window.is_some()).then_some(CursorStyle::HollowBlock);
        let blink_phase = ws0.blink_phase;
        // Drag-and-drop hover: while a file is dragged over the window we paint a
        // drop-target highlight at present time (like the bell invert above).
        let drag_hover = ws0.drag_hover && !overlay_open;
        let last_present = ws0.last_present;

        // Renderer-global cursor state belongs to whichever window we are about to
        // encode: the shared backend's blink phase + focus-driven hollow override are
        // not per-window, so re-apply THIS window's values right before the encode
        // (last-writer-wins once more than one window exists). Redundant but harmless
        // at n==1 (sync_blink_phase/on_focus already set the same values).
        self.backend.set_cursor_blink_phase(blink_phase);
        self.backend.set_cursor_style_override(cursor_override);
        // W5c: fold the configured `selection_inactive` with THIS window's live
        // focus, so the selection band dims exactly while the window is
        // unfocused (renderer-global like the blink/override above;
        // last-writer-wins per encode). Default-off config keeps this `false`
        // — byte-identical. `on_focus` invalidates the present caches on a
        // flip (the band colour is not part of the damage key).
        self.backend
            .set_selection_inactive(self.render_knobs.selection_inactive && !ws0.focused);

        // The cursor-effect resolution that used to live here (MOTION POLICY fold,
        // glow/trail configs, cell geometry) moved VERBATIM into `tick_cursor_fx`,
        // which the single-pane path calls after LOCK A — shared with the headless
        // `image` capture. The multi-pane compose resolves its own copies (below).

        // M2 "ink that dries": resolve the stream-fade settings once per redraw.
        // The per-frame bypass gate (input_hot / alt-screen / scrolled-back /
        // Reduced motion — the proven `fade_permitted` policy) is folded at the
        // diff site below, where those facts live under the term lock.
        let fade_on = self.config.stream_fade_or_default()
            && self
                .serious_mode_policy()
                .allows(crate::motion::SeriousEffect::StreamFade);
        let fade_ms = self.config.stream_fade_ms_or_default();

        // D-1 early-out. Hold the Terminal mutex only long enough to read the
        // damage epoch + selection + title and, IF we decide to repaint, refill
        // the persistent RenderInput in place and consume the damage — all
        // atomically so no PTY damage is dropped. The early-out compares this
        // frame's RepaintKey to the last presented one: a steady screen with the
        // same blink phase / bell-flash / selection / focus skips the entire
        // extract + rasterize + present (the coarse screen-level skip, on top of
        // the renderer's own row-level damage cache in `render_input_cached`).
        //
        // SPLIT PANES: a multi-pane tab composes the frame from EVERY visible pane
        // (see `redraw_compose`), so its early-out folds all visible panes' damage.
        // The single-pane path below is the EXACT original, byte-identical.
        // ZOOM-AWARE (split-pane audit): a zoomed tab composes to ONE
        // full-window rect at `(0, 0)` whose terminal `visible_plan` sizes to
        // the whole window — coordinate-identical to a single-pane tab — so it
        // takes the original single-pane path (rain/selection/vi/pill/fade all
        // live), matching `WindowState::is_split`.
        let multi_pane = self
            .active_tree(id)
            .is_some_and(|t| t.len() > 1 && !t.is_zoomed());
        // A single-pane ⇄ split layout change relocates the cursor between coordinate
        // spaces (single-pane window-content vs compose pane-local), so reset the cursor
        // animators on the transition — otherwise the next tick spawns a one-frame comet
        // from a stale cross-space position.
        if let Some(ws) = self.windows.get_mut(&id)
            && ws.last_composed != Some(multi_pane)
        {
            ws.last_composed = Some(multi_pane);
            ws.cursor_glow.reset();
            // The comet's last-cursor position lives in the same coordinate space as
            // the aurora's; drop it on the layout transition too, or the next tick
            // spawns a spurious one-frame comet from the stale cross-space position.
            ws.cursor_trail.reset();
            ws.predictor.reset();
            // M2: the stream-fade age map mirrors the single-pane window-content
            // view; a layout-space change invalidates it (the next single-pane
            // frame re-baselines — settled ink, nothing fades).
            ws.stream_fade.reset();
            // PHOSPHOR rain: RETAIN the engine across the layout transition
            // (suspend-not-drop, split-pane audit — v1 dropped it here, so
            // opening a split killed rain window-wide in one frame, merely
            // VISITING a split tab wiped an unrelated tab's field, and
            // un-splitting restarted from empty). The occupancy bitset and
            // material bank ARE coordinate-space state, so rebaseline them
            // (`note_grid_replaced` forces the next Tier-A rescan in the new
            // space); the field/weather survive — the swap is a viewpoint
            // change, not a restart. Stale quads must not leak across the
            // space change, so the emit scratches clear here and the hidden
            // damage band (window-content rows) drops with them. The compose
            // path now runs its own focused-pane rain tick, so a retained
            // engine winds down honestly there (never a leaked wake).
            if let Some(engine) = ws.matrix_rain.as_mut() {
                engine.note_grid_replaced();
            }
            ws.rain_scratch.clear();
            ws.rain_add_scratch.clear();
            ws.rain_hidden_band.clear();
        }
        // The tab-strip titles must be read OUTSIDE the term lock (reading each tab's
        // title try-locks its term — non-blocking, keep-stale on contention, see
        // `refill_strip_titles`); read them ONCE here and reuse for BOTH the RepaintKey
        // fingerprint and the strip splice below (instead of locking every tab twice).
        // The fingerprint is part of the RepaintKey, so it MUST be computed before the
        // early-out — a title change has to invalidate it. A single tab is deliberately
        // visible title identity too, so every non-empty enabled strip participates in
        // the same non-blocking title/metadata fingerprint. Strip disabled remains the
        // byte-identical pre-strip path (empty, fp 0, no-op).
        let tab_strip = self.redraw_tab_strip_state(id);
        // Single-pane IME caret: captured under the term lock below, reported AFTER the
        // lock drops (report_ime_cursor_area needs &mut self). The multi-pane path
        // reports its own caret inside redraw_compose, so this stays None there.
        let mut single_pane_ime_caret: Option<((u16, u16), bool)> = None;
        // Sparkle-words: ensure the App-cached resolved state (config + compiled
        // lexicon) is current. Rebuilt ONLY when marked dirty (startup / reload /
        // toggle), never per frame — so the per-frame path does no config
        // re-resolution and no lexicon rebuild. `None` ⇒ feature off (byte-identical).
        if self.sparkle_dirty {
            self.recompute_sparkle();
        }
        // PHOSPHOR rain: same dirty-rebuild discipline (startup / reload /
        // appearance flip / toggle — never per frame). `None` ⇒ feature off
        // (byte-identical; no engine is ever constructed).
        if self.rain_dirty {
            self.recompute_matrix_rain();
        }
        // Kitty Log (§F4): the host-side recorder gate, read before the `ws`
        // borrow below (a couple of Option reads — no cache to invalidate).
        // The retired Settings overlay's snapshot sync is test-only; native
        // Settings receives its projection through the native app service.
        let kitty_log_on = self.kitty_log_enabled();
        // Curse-BONK policy, read before the `ws` borrow like the Kitty Log
        // recorder gate (§F4.7): the profanity bonk knobs, the shared trail
        // volume, and the active trail style (it keys the bonk's clash
        // register to the melody actually playing).
        let bonk_enabled = self.curse_bonk_enabled();
        let bonk_detonations = self.curse_bonk_detonation_enabled();
        let bonk_volume = self.config.trail_sound_volume();
        let bonk_style = self.glow_config().style;
        let title = if multi_pane {
            match self.redraw_compose(
                id,
                rows,
                cols,
                invert,
                drag_hover,
                cursor_override,
                tab_strip,
                frame_started,
            ) {
                Some(title) => title,
                None => {
                    // Nothing visible changed across any pane: refresh chrome, skip.
                    let title = term_lock(&front_terminal.term).title_arc();
                    if let Some(w) = &window {
                        self.apply_title(id, w, &title);
                    }
                    return;
                }
            }
        } else {
            let load_shed = self.load_shed_active();
            // PHOSPHOR: the FRONT session's effective rain state (its runtime
            // override, else the config `enabled` bit). Read before the `ws`
            // borrow below (`session_rain_enabled` needs `self.pool`); a tab
            // switch re-evaluates it naturally because the front session
            // changes under the same redraw.
            let rain_session_on = self.session_rain_enabled(front_terminal.session);
            let Some(ws) = self.windows.get_mut(&id) else {
                return;
            };
            // M2 stream fade: identity of the front terminal's engine
            // allocation. A tab switch / session migration changes this Arc,
            // which must re-baseline the age map (a
            // switched-to screen is settled content, not fresh ink).
            let fade_token = std::sync::Arc::as_ptr(&front_terminal.term) as usize;
            // Resident strip-row pool reclaim, hoisted AHEAD of LOCK A so it also
            // salvages a sparkle-RESCAN frame's surplus rows. The prior present's
            // strip splice left `strip` surplus rows on `input_scratch.cells`; both
            // the LOCK A rescan extract and the LOCK B extract are about to
            // `resize_with(rows)` and DROP those tail buffers. On a rescan frame that
            // free lands under the Terminal mutex and the pool (only refilled here)
            // never sees the buffers, so `prepend_strip_rows` clones fresh every frame
            // a full-screen TUI redraws — the dominant path now that the alt screen
            // decorates by default. Reclaiming here (pure host memory: no lock, no
            // alloc, no free) feeds the pool on BOTH paths and leaves `cells.len() ==
            // rows`, so each `cell_frame_into` refills in place and the splice reuses
            // the pooled capacity. The `drain` removes the whole surplus range
            // regardless of the `strip_rows_n` cap (the cap only bounds pool growth).
            if strip_rows_n > 0 && ws.input_scratch.cells.len() > rows {
                let pool = &mut ws.strip_row_pool;
                for buf in ws.input_scratch.cells.drain(rows..) {
                    if pool.len() >= strip_rows_n {
                        break;
                    }
                    pool.push(buf);
                }
            }
            // ---- SHORT LOCK A (snapshot). The single-pane path previously held the
            // Terminal mutex across EVERY per-frame effect tick, the should_repaint
            // decision, the O(rows×cols) stream-fade pass, and ~8 clone_from buffer
            // copies — starving the PTY reader (which contends on the SAME mutex per
            // 8 KiB chunk) under load. Only `cell_frame_into` + `take_damage` + these
            // cheap scalar reads NEED the lock, so mirror the multi-pane compose path
            // (`redraw_compose`): snapshot the cheap Terminal state into host locals
            // here, tick + decide UNLOCKED, and re-lock ONLY to extract the grid +
            // consume damage (LOCK B, below). Byte-parity is preserved — no concurrent
            // writer interleaves in the single-threaded tests, so every read yields
            // exactly the pre-split value; under a real load burst the PTY reader now
            // makes progress across the ticks instead of blocking on this hold. ----
            let sparkle_on = self.sparkle.is_some();
            // PHOSPHOR rain: the resolved parameters (Copy) for this frame,
            // ENGAGED only while the front session is effectively on OR this
            // window still holds an engine that must wind down through the
            // suspended/drain path. `None` ⇒ fully off, and NOTHING below this
            // line runs for rain (the D-1 zero-cost pin: a session that never
            // enables rain never constructs an engine and pays nothing).
            let rain_cfg = self
                .rain
                .filter(|_| rain_session_on || ws.matrix_rain.is_some());
            // TYPING-3 effective load-shed snapshot (disjoint field, like
            // `sparkle_on`): under an allowed sustained-overload latch we suspend
            // expensive per-frame decoration work. Explicit Full motion and the
            // adaptive opt-out leave this false, matching `motion_policy()`.
            let mut term = term_lock(&front_terminal.term);
            // Cursor (terminal coords). A pure cursor move marks no grid damage, so
            // these key terms force the post-move repaint; they also feed the aurora's
            // move detection.
            let cpos = term.cursor();
            let cursor_visible = term.cursor_visible();
            let cursor_style = term.cursor_style();
            // VI-1: when keyboard copy-mode is active, its cursor (grid line/col) is
            // painted INSTEAD of the terminal cursor — mapped to a screen row via the
            // display offset below. `None` when vi mode is off (the normal cursor shows).
            let vi_point = term.vi_is_active().then(|| term.vi_cursor_point());
            // Live OSC-12 cursor colour (None ⇒ no program set one). Applied to the
            // aurora/comet AFTER the lock drops, and the source of the presented
            // `cursor_color` below — read ONCE so both stay consistent.
            let live_cursor_rgb = term.cursor_color().map(|c| [c.r, c.g, c.b]);
            let epoch = term.damage_epoch();
            // PHOSPHOR: the weather machine's agent-output clock — advances on
            // CONTENT mutation only (never a pure viewport scroll), read here
            // under the same lock as the epoch (one u64; the engine registers
            // only actual changes, so feeding it every frame is free).
            let content_seq = term.content_seq();
            // Most recent COMPLETED shell command (OSC 133/633) — the
            // exit-status weather probe: (monotonic completion seq,
            // exit_code). The seq is strictly per-completion, so two commands
            // finishing in the same millisecond still read as two edges.
            let cmd_done = term
                .last_completed_command()
                .and_then(|m| Some((term.completed_command_seq(), m.exit_code?)));
            // OSC 133/633 C is an authenticated, observable shell execution
            // phase. It carries no command text; its rising edge gives the
            // native rain one bounded Execute choreography license.
            let shell_executing = term.shell_state() == aterm_core::terminal::ShellState::Executing;
            let is_alt = term.is_alternate_screen();
            // SYNC-1: is this (single) pane holding DEC-2026 synchronized output? Read
            // under the SAME lock, then arm/hold so the app's multi-write update lands
            // tear-free. Arm ONLY on the FALSE->TRUE rising edge (so a timeout release
            // does not immediately re-hold), and clear whenever we are NOT holding (sync
            // ended OR the safety-valve deadline passed) — a stale past deadline would
            // busy-spin the event loop (see the `next_deadline`-in-the-past guards). The
            // `sync_hold` result gates BOTH the rescan's `take_damage` below AND the
            // present (via the early return before LOCK B), so a held frame strands
            // nothing: the accumulated writes stay pending for the release frame.
            let sync_active = term.modes().synchronized_output();
            let sync_end_seq = term.sync_end_seq();
            let (sync_deadline, sync_hold, sync_armed_seq) = sync_frame_hold(
                sync_active,
                ws.sync_was_active,
                ws.sync_hold_until,
                frame_started,
                sync_hold_timeout,
                sync_end_seq,
                ws.sync_armed_end_seq,
            );
            // SYNC-1 observability: count arms and classify releases (before the
            // state is overwritten below) so a pathological hold/timeout loop —
            // presents pinned to ~1/timeout — is measurable over the `metrics`
            // verb instead of invisible. The event-loop's between-frames disarm
            // (`new_events`) counts its own timeout releases. A close-and-re-arm
            // (bracket closed, new one already open) is BOTH an end release and
            // a fresh arm.
            let closed_and_rearmed = sync_active
                && ws.sync_was_active
                && sync_end_seq != ws.sync_armed_end_seq
                && !sync_hold;
            if (sync_active && !ws.sync_was_active) || closed_and_rearmed {
                metrics::note_sync_armed();
            }
            if closed_and_rearmed {
                metrics::note_sync_release_end();
            } else if ws.sync_hold_until.is_some() && sync_deadline.is_none() {
                if sync_active {
                    metrics::note_sync_release_timeout();
                } else {
                    metrics::note_sync_release_end();
                }
            }
            metrics::set_sync_holding(sync_hold);
            ws.sync_was_active = sync_active;
            ws.sync_hold_until = sync_deadline;
            ws.sync_armed_end_seq = sync_armed_seq;
            let display_offset = term.grid().display_offset();
            let scrollback_lines = term.grid().scrollback_lines();
            // ERASE-POOF probe: capture the cursor row's per-column chars under
            // this SAME lock (disjoint `ws` field, the `rain_hidden_band` /
            // `cell_frame_into` pattern) — one O(cols) pass into the resident
            // buffer, zero steady-state alloc. Gated on the live bottom
            // (`display_offset == 0`; scrolled-back rows are history, not the
            // editing surface) AND an unmoved scrollback count (output that
            // scrolled the screen slides content through the cursor row — skip
            // one frame so the slide can never read as a kill). The probe rides
            // to `tick_cursor_fx` as `(row, caret)`; `None` leaves the engine's
            // previous probe in place.
            // REPAINT-BLINK edge detector, on the SAME LOCK A read (zero new
            // acquisitions): the epoch advances when the app hides the cursor
            // INSIDE a DEC-2026 synchronized update — Claude Code's
            // per-keystroke repaint bracket (live byte capture), which vim/
            // less/ConPTY never emit. An advance stamps the window and feeds
            // both cursor engines their blink + alt context, so the engine-
            // side re-anchor conjunct discriminates by observed repaint
            // behavior instead of keyboard-protocol inference (Claude Code
            // negotiates NO kitty flags — the v0.48 kitty gates never opened).
            let blink_epoch = term.repaint_blink_epoch();
            if ws.blink_reseed {
                // Tab/pane switch: adopt the NEW terminal's epoch silently —
                // a cross-terminal mismatch is not a repaint (see sync_window).
                ws.blink_reseed = false;
                ws.blink_epoch_seen = blink_epoch;
            } else if blink_epoch != ws.blink_epoch_seen {
                ws.blink_epoch_seen = blink_epoch;
                ws.last_blink_at = Some(frame_started);
                ws.cursor_glow.note_repaint_blink(frame_started);
                ws.cursor_trail.note_repaint_blink(frame_started);
            }
            ws.cursor_glow.note_context(is_alt);
            ws.cursor_trail.note_context(is_alt);
            let blink_recent = ws
                .last_blink_at
                .is_some_and(|t| frame_started.saturating_duration_since(t) <= BLINK_RECENT_MAX);
            // NEVER in a PLAIN alt-screen app (vim/less — no repaint blink):
            // Ctrl-U/Ctrl-W are page-scroll / window commands there, and
            // region scrolls slide content through the cursor row with no
            // scrollback trace — a fresh kill hint would pair with that slide
            // into a phantom poof (adversarial review). A repaint-BLINKING
            // alt-screen TUI (Claude Code — live frame evidence) keeps the
            // probe: its kill keys are real kills.
            let probe_ok = !is_alt || blink_recent;
            let row_probe = if probe_ok
                && display_offset == 0
                && ws.poof_scrollback == Some(scrollback_lines)
            {
                let _fill = term.row_cols_into(cpos.row as usize, &mut ws.poof_row_buf);
                // STAR-LANDING NEIGHBORS: capture the rows flanking the
                // cursor row under the SAME lock, so the displaced nyan
                // stars' TEXT-FIRST gate can prove their landing cells blank
                // (they paint in the ADJACENT rows' pixel bands — the poof
                // probe's own row says nothing about those). A grid-edge
                // neighbor is skipped; the feed below encodes it as provably
                // glyph-free (the landing band is padding the effects box
                // clips into).
                if cpos.row > 0 {
                    term.row_cols_into(cpos.row as usize - 1, &mut ws.poof_row_above_buf);
                }
                if (cpos.row as usize) + 1 < rows {
                    term.row_cols_into(cpos.row as usize + 1, &mut ws.poof_row_below_buf);
                }
                Some((cpos.row, cpos.col))
            } else {
                None
            };
            // A PTY-driven scroll this frame: translate the trail anchors (the
            // caret's previous cell now sits N rows higher — without this, a
            // wrap that grows a bottom-anchored TUI box in a full-transcript
            // Claude Code session flattens to dr==0 and the line-fill returns)
            // and DROP the row probe (row_prev outliving the fenced frame
            // would diff pre-scroll against post-scroll content one frame
            // later — the same phantom-poof hole, delayed).
            let scrolled_rows = ws
                .poof_scrollback
                .map_or(0, |prev| scrollback_lines.saturating_sub(prev));
            if scrolled_rows > 0 {
                let d = scrolled_rows.min(rows).min(u16::MAX as usize) as u16;
                ws.cursor_glow.note_scroll(d);
                ws.cursor_trail.note_scroll(d);
                ws.cursor_glow.drop_row_probe();
            }
            ws.poof_scrollback = Some(scrollback_lines);
            // Selection COPY (owned): the sparkle nova view borrows it AND the
            // RepaintKey fingerprints it — both after the lock drops.
            let selection = term.text_selection().clone();
            // Live default-bg (folded with DECSCNM reverse-video) + cursor colour for
            // the renderer PADDING band + cursor, resolved under the lock so OSC
            // 11/111/12/112 (and DECSCNM ?5) track. Applied to `input_scratch` after
            // LOCK B. Default path (no OSC) is byte-identical to the theme.
            let dbg = if term.modes().reverse_video() {
                term.default_foreground()
            } else {
                term.default_background()
            };
            let default_bg_u32 = aterm_render::rgb_to_u32([dbg.r, dbg.g, dbg.b]);
            let cursor_color_u32 =
                live_cursor_rgb.map_or(self.theme.cursor & 0x00FF_FFFF, aterm_render::rgb_to_u32);
            let title = term.title_arc();
            // Sparkle rescan is the ONE grid extraction that must precede the decision
            // (a rescan always forces a present via `deco_rescan`, so the extract is
            // never wasted). Extract AND consume the damage HERE, under the SAME lock,
            // so the presented cells and the `take_damage` are the same grid state —
            // if a PTY write landed between this lock and a later one, consuming it
            // without also extracting it would strand the new content (its damage
            // cleared but the pixels never shown, and the epoch not advanced to force
            // a repaint). The scan + animate run unlocked below off this snapshot;
            // non-rescan frames instead extract + consume at LOCK B.
            // Alt-screen sparkles are on by default; `suppress_in_alt_screen` opts back
            // into the v1 behavior (vim/less/htop undecorated). ONE flag gates both the
            // rescan here and the deco render below so the two can never disagree.
            let deco_suppress_alt = is_alt
                && self
                    .sparkle
                    .as_ref()
                    .is_some_and(|rs| rs.cfg.suppress_in_alt_screen);
            // Suspend decorations when the config suppresses the alt screen OR the
            // effective load-shed gate is engaged (TYPING-3): ONE flag gates the
            // rescan here and the tick below, so the two can never disagree. Under
            // adaptive overload this skips the full-grid rescan entirely;
            // decorations rebuild when the effective gate clears.
            let deco_suspend = deco_suppress_alt || load_shed;
            // PHOSPHOR rain shares the same shaped gate (design §7: ONE flag
            // gates the rescan here and the tick below so the two can never
            // disagree), folding its OWN `suppress_in_alt_screen` knob and the
            // TYPING-3 effective load-shed gate.
            let rain_suppress_alt = is_alt && rain_cfg.is_some_and(|c| c.suppress_in_alt_screen);
            // A session whose effective state is OFF folds into the SAME
            // suspended/drain gate as alt-screen suppression: the engine (if
            // one exists) ticks suspended — weather starves, the drain
            // completes, `is_active` self-disarms — instead of being dropped,
            // so switching back to a rain-enabled session resumes honestly.
            let rain_suspend = rain_suppress_alt || load_shed || !rain_session_on;
            // PHOSPHOR hidden-cursor band (design §6): fold this frame's
            // damaged rows into the resident ring BEFORE any `take_damage`
            // below consumes them. Runs only while rain is live — the
            // disabled path reads nothing.
            if rain_cfg.is_some() && !rain_suspend && !sync_hold {
                update_rain_hidden_band(&mut ws.rain_hidden_band, term.grid().damage(), rows);
            }
            let mut deco_rescan = false;
            // `!sync_hold` (SYNC-1): a held frame must not consume damage — the rescan's
            // `take_damage` below would strand the accumulated sync'd writes (damage
            // cleared but never presented). Skipping it keeps the damage pending for the
            // release frame.
            if sparkle_on && !deco_suspend && !sync_hold && ws.word_decos.needs_rescan(epoch) {
                deco_rescan = true;
            }
            // PHOSPHOR rain is grid-scanned like the decorations: its Tier-A
            // occupancy bitset rebuilds on the same epoch trigger, off the
            // same snapshot, with the same extract + take_damage etiquette. A
            // not-yet-built engine (lazy: first enabled tick) always needs
            // its first scan.
            let rain_refresh = rain_refresh_needed(
                rain_cfg.is_some(),
                rain_suspend,
                sync_hold,
                display_offset,
                ws.matrix_rain.as_deref(),
                epoch,
            );
            // ONE extraction per refresh frame regardless of which engine asked
            // (same lock, same epoch — no torn read); scans/sampling below run
            // unlocked off this snapshot.
            if deco_rescan || rain_refresh {
                term.cell_frame_into(&mut ws.input_scratch, rows, cols);
                term.take_damage();
            }
            drop(term);
            // ---- END LOCK A. Everything below runs on host state until LOCK B. ----
            // Caret for the IME candidate window (single pane → no pane offset). Reported
            // after the block, so the default one-pane layout anchors the CJK / dead-key
            // / Option-compose window at the caret, not the window origin.
            single_pane_ime_caret = Some(((cpos.row, cpos.col), cursor_visible));
            // SCROLLBACK: the cursor lives in ACTIVE-grid coords; while the
            // viewport shows history (`display_offset != 0`) those rows map to
            // unrelated scrollback lines, so the effect engines see NO cursor —
            // no spawn, no wake anchor, live light decays in place (the
            // predictions precedent: "never PAINT them over the scrollback
            // view"). Scrolling back to the bottom restores the anchor.
            let cur = (cursor_visible && display_offset == 0).then_some((cpos.row, cpos.col));
            // The whole per-frame cursor-effect pass — the MOTION POLICY fold, the
            // glow/trail config resolution, the OSC-12 live-colour rewire, the
            // ATERM_TRACE_SPAWN diagnostic, and the glow/forge/rainbow/trail ticks —
            // is `tick_cursor_fx` (extracted VERBATIM from this path), shared with
            // the headless `image` capture so a glass-less capture composes the SAME
            // live effect state this present would. The `&mut self` call ends the
            // `ws` borrow; re-take it right after (a window cannot vanish mid-redraw
            // on this single thread — the re-borrow only satisfies the checker).
            let Some(fx) = self.tick_cursor_fx(
                id,
                CursorFxInputs {
                    now: frame_started,
                    rows,
                    cols,
                    cur,
                    cursor_visible,
                    cursor_style,
                    blink_phase,
                    live_cursor_rgb,
                    default_bg: default_bg_u32,
                    row_probe,
                },
            ) else {
                return;
            };
            let CursorFxTick {
                win_focused,
                motion,
                glow_cfg,
                trail_color,
                glow_cw,
                glow_ch,
                glow_fp,
                trail_fp,
                forge_fill,
                rainbow_fill,
                droplet_fill,
                beamrod_fill,
                comet_fill,
                phaser_fill,
                bolt_cursor,
                bolt_fill,
                twinkle_cursor,
            } = fx;
            // The same window-space geometry the ticks used (shared derivation, so
            // this site can never disagree with `tick_cursor_fx`).
            let (origin_x, origin_y, win_w, win_h, fx_head) =
                self.effects_origin_win(id, rows, cols, glow_ch);
            // ⚡ The bolt SHAPE rides the renderer's style-override channel (the
            // one the unfocused hollow uses; no clash — the bolt requires
            // focus). Applied here — the tick fn holds the window borrow, not
            // the backend.
            if bolt_cursor {
                self.backend
                    .set_cursor_style_override(Some(CursorStyle::Bolt));
            }
            // 🌟 The nyan twinkle pins the rendered shape STEADY on the same
            // channel (applied after the prologue's hollow reset; never live
            // together with the bolt — the styles are mutually exclusive): the
            // terminal still reports a blinking block (DECRQSS unaffected —
            // the override is render-only), but the block stops vanishing on
            // the off phase and each blink flip twinkles like a little star.
            if twinkle_cursor {
                self.backend
                    .set_cursor_style_override(Some(CursorStyle::SteadyBlock));
            }
            self.install_window_config_assets(id);
            let Some(ws) = self.windows.get_mut(&id) else {
                return;
            };
            // The same cell geometry the ticks used (terminal coords → window-
            // absolute pixels), rebuilt for the cat exit-flourish emitter below.
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
            // NYAN CAT in FRONT of the cursor: flies ahead while you have forward
            // typing momentum (a forward-vs-backspace score, tolerant of a stray
            // backspace), pulling the cursor forward so the rainbow ribbon grows
            // behind it. Gated on the `nyan` style + focus + full motion; the
            // fade alpha (0 when momentum has decayed) both hides it and, folded
            // into the aurora key, re-presents the fade and settles at rest.
            // A USER Nyan sprite (`cursor_nyan_sprite`) overrides the built-in
            // homage. Its bounded async worker result was installed before any
            // window borrow at the top of `redraw_window`; this block is now
            // deliberately presentation-only.
            // homage only when the admitted catalog contains a Ready asset. An
            // Invalid asset disables the companion and remains diagnosable; no
            // presentation path expands a path, reads a file, or decodes PNG.
            // O(1) scalar sync from the durable collection (no ledger scan/I/O).
            // A discovery later in this tick calls `on_collect`; this keeps new
            // windows and restored sessions on the latest collected identity.
            // TWO-PATH RULE (owner: switching character mid-flight is
            // distracting): this per-frame sync is LATCHED per appearance —
            // while the companion is on screen `set_look` parks the change and
            // the one body keeps the same latched look until the next wake.
            // Only `on_collect` below swaps mid-appearance, because the discovery
            // hello legitimately presents the newly unlocked collectible.
            if let Some(look) = self.kitty_log.companion_look() {
                ws.cursor_cat.set_look(look);
            }
            // Full motion advances the fade/bob machine. Reduced motion samples
            // a collection hello as one opaque still; ordinary earned flights
            // remain hidden and the scheduler arms only its one erase deadline.
            let cat_presentable = win_focused && !deco_suspend;
            ws.cursor_cat
                .set_collection_presentable(frame_started, cat_presentable);
            // FULL-NYAN SING-ALONG (`aterm_effects::nyan_sing`): resolve the
            // held-key celebration drive for this present. STYLE-GATED to
            // the Nyan trail (other styles read a hard 0). While any drive
            // is live, the documented MOMENTUM BYPASS pins the canonical
            // metric through both existing instances (`CursorGlow::celebrate`
            // → ribbon saturation/star shower via the one spine;
            // `CursorCat::set_singing` → threshold-gated summon + the beat-synced
            // dance + singing face) — no parallel render path, so every
            // legibility cap holds at full drive.
            let sing_drive = if matches!(glow_cfg.style, crate::cursor_glow::GlowStyle::Nyan) {
                ws.nyan_sing.drive(frame_started)
            } else {
                0.0
            };
            if sing_drive > 0.0 {
                ws.cursor_glow.celebrate(frame_started, sing_drive);
                // The RIFF: one `Celebration(RiffBar)` gesture per visual
                // bar while ARMED (wind-down schedules none — the synth's
                // sing-duck release is the audio crossfade). Sound policy is
                // the trail-sound law (RAW focus × `trail_sounds` ×
                // volume): muted/unfocused ⇒ visuals only. Deliberately NOT
                // reduced-motion-gated — reduced motion is a MOTION
                // contract, so the static celebration keeps its song when
                // sound is on (unlike the bonk, whose gain models the
                // glow's intensity-0 silence).
                if let Some(bar) = ws.nyan_sing.bar(frame_started)
                    && ws.sing_riff_bar != Some(bar)
                {
                    ws.sing_riff_bar = Some(bar);
                    if let Some(gain) = trail_sound_gain(
                        ws.focused,
                        self.config.trail_sounds_or_default(),
                        self.config.trail_sound_volume(),
                    ) {
                        self.trail_audio
                            .push(aterm_effects::trail_sound::SoundEvent {
                                style: crate::cursor_glow::GlowStyle::Nyan,
                                // The sing-along riff is its own authored song
                                // — the `trail_sound_style` override never
                                // re-voices it.
                                voice: aterm_effects::trail_sound::SoundVoice::Style,
                                kind: aterm_effects::trail_sound::SoundGesture::Celebration(
                                    aterm_effects::trail_sound::CelebrationGesture::RiffBar {
                                        bar: (bar & 0xffff) as u16,
                                    },
                                ),
                                pan: 0.0,
                                // Momentum is pinned to 1.0 while armed — that
                                // IS maximal flow; the riff warms accordingly.
                                heat: 1.0,
                                hue: 0.0,
                                gain,
                                // The riff is its own authored song: tone-blind
                                // like the bonk, and it never feeds the bed.
                                tone: aterm_effects::tone::Tone::Technical,
                                bed: false,
                            });
                    }
                }
            } else {
                // Drained: settle the detector to byte-identical rest and
                // re-open the bar latch for the next celebration.
                ws.nyan_sing.settle(frame_started);
                ws.sing_riff_bar = None;
            }
            ws.cursor_cat.set_singing(
                frame_started,
                sing_drive,
                ws.nyan_sing.beat(frame_started).unwrap_or(0.0),
            );
            let animate_cat = motion.animate(crate::motion::MotionEffect::CursorGlow);
            let cat_frame = if animate_cat {
                ws.cursor_cat.frame(frame_started)
            } else {
                ws.cursor_cat.static_frame(frame_started)
            };
            // Reduced motion: the STATIC CELEBRATION has no frame cadence of
            // its own (the state machine's 60 fps rearm rides `animate_cat`),
            // so keep one-present-ahead wakes flowing while any sing drive
            // remains — the discovery-hello wake pattern extended over the
            // drive — or the still would freeze on glass past its one-step
            // disappearance and the detector would never settle. Bounded by
            // the celebration itself (hold + ~1 s wind-down).
            if !animate_cat
                && sing_drive > 0.0
                && let Some(window) = ws.os_window.as_ref()
            {
                window.request_redraw();
            }
            // `sparkle_on`: the cat sprite (and the collection hello) can ONLY be
            // drawn inside the `self.sparkle` branch below — with the master off,
            // an earned flight is invisible, so folding its fp / exit flourish
            // here would force real ~60fps presents of unchanged pixels and then
            // materialize a heart/star from nowhere on fade-out (the audit's
            // invisible-cat wake train). Gate everything on what can be drawn.
            let nyan_enabled = win_focused
                && !deco_suspend
                && sparkle_on
                && cursor_cat_presentation_enabled(
                    animate_cat,
                    glow_cfg.enabled,
                    glow_cfg.style,
                    cat_frame.collection_hello,
                )
                || (win_focused
                    && !deco_suspend
                    && sparkle_on
                    // The reduced-motion STATIC CELEBRATION presents like a
                    // hello: `cat_frame.sing` is non-zero only when the host
                    // resolved a live Nyan-gated drive above, so this arm can
                    // never draw a non-Nyan or idle frame.
                    && cat_frame.sing > 0.0);
            let nyan_alpha = if nyan_enabled { cat_frame.alpha } else { 0 };
            let glow_fp = glow_fp ^ if nyan_enabled { cat_frame.fp() } else { 0 };
            // On the way out, the cat sometimes does a flourish — a heart rising
            // (heart meow) or a sparkling star (star wink). It is emitted LATER
            // (just after the halo stream is assembled below) because its
            // LIGHT-theme arm is a SOURCE-OVER veil that must land in `glow_halo`
            // — which is cleared+refilled from `cursor_glow.halos()` after this
            // point — while its DARK-theme arm is additive light. See the
            // `emit_exit_fx` call site below the `glow_halo` refill.
            // Sparkle-word decorations: rescan the visible grid only when it changed
            // (the damage epoch advanced), animate every frame (motion policy
            // permitting — Reduced forces the static path), and fingerprint for
            // the present early-out. The ALTERNATE screen decorates too by default
            // (full-screen TUIs like Claude Code are a primary surface); the
            // `suppress_in_alt_screen` knob restores the old vim/less/htop
            // suppression. A screen swap is just the §3.6 occlusion case: the
            // persist map carries identities across a sub-GRACE_TTL round-trip,
            // so an alt⇄main flip neither re-rolls genomes nor re-fires novas.
            //
            // A rescan frame builds the RenderInput snapshot HERE (still under the
            // one lock) and scans THOSE rows, so the full per-cell color/style
            // resolve runs once per presented frame — not once for the scan and
            // again for the frame. The flag also bypasses the present early-out
            // below: the snapshot was refilled, so the frame MUST present (covers
            // the reset()-with-unchanged-epoch edges: alt-screen exit, sparkle
            // re-enable) and the `cell_frame_into` at the commit point is skipped.
            let deco_fp = if let Some(rs) = self.sparkle.as_ref() {
                if deco_suspend {
                    // Alt-screen suppression OR the load-shed latch (TYPING-3): clear
                    // every sparkle scratch and skip the tick (cat sim included) — no
                    // per-frame decoration CPU while suspended. v3 §1.1 reset table:
                    // BOTH suspension causes are freeze/thaw, not resets — recovery
                    // resumes every episode exactly where it paused instead of the
                    // v2 mass replay (freeze is idempotent per suspended frame).
                    ws.word_decos.freeze(frame_started);
                    ws.deco_scratch.clear();
                    ws.ink_scratch.clear();
                    ws.free_scratch.clear();
                    ws.nova_scratch.clear();
                    0
                } else {
                    // Resume from a freeze (perf_reduced cleared / alt-screen exit
                    // with suppression): shift every stored timestamp forward by the
                    // freeze duration BEFORE the rescan/tick read the clock. A no-op
                    // when not frozen.
                    ws.word_decos.thaw(frame_started);
                    // Live geometry is part of the cold palette scan as well as
                    // emission: the context key samples the prospective cat's
                    // exact multi-row footprint and then freezes for the episode.
                    let effect_geom = crate::word_decorations::EffectGeom {
                        cell_w: glow_cw as u16,
                        cell_h: glow_ch as u16,
                        rows: rows as u16,
                        cols: cols as u16,
                    };
                    // The grid was already extracted into `input_scratch` under LOCK A
                    // when a rescan was due (`deco_rescan`, same `epoch` — no torn
                    // read); scan those cells here on host state, no lock held.
                    if deco_rescan {
                        ws.word_decos.rescan_from_cells_with_geom_at_cursor(
                            &ws.input_scratch.cells,
                            &ws.input_scratch.line_sizes,
                            rows,
                            cols,
                            &rs.lexicon,
                            &rs.cfg,
                            epoch,
                            frame_started,
                            effect_geom,
                            default_bg_u32,
                            (display_offset == 0).then_some((cpos.row, cpos.col)),
                        );
                        // This on-glass present supplied the real birth tick. Do
                        // not let a later capture reuse its output stamp for an
                        // unrelated rescan.
                        ws.pending_deco_birth = None;
                    }
                    // `cur` (the visible cursor cell) was read under this SAME
                    // Terminal lock above — the §5.8 gaze target costs no new
                    // locking; the selection view (§6.4 nova ignition deferral
                    // + per-quad attenuation) rides the same lock too.
                    // `ws.focused` gates the idle-life one-shots (blink/twitch
                    // fire only while focused, §5.6).
                    let sel_view = crate::word_decorations::SelView {
                        sel: &selection,
                        display_offset: display_offset as i32,
                    };
                    // W11: fold the MotionPolicy into the effects engine's own
                    // `reduced_motion` seam (aterm-effects cannot depend on
                    // `crate::motion`), so a Reduced/unfocused window renders STATIC
                    // decorations. Zero-alloc on the hot Full path — the config is
                    // cloned ONLY when the policy actually demotes and the config
                    // wasn't already reduced.
                    let reduced_cfg = (!motion.animate(crate::motion::MotionEffect::WordSparkles)
                        && !rs.cfg.reduced_motion)
                        .then(|| {
                            let mut c = rs.cfg.clone();
                            c.reduced_motion = true;
                            c
                        });
                    let tick_cfg = reduced_cfg.as_ref().unwrap_or(&rs.cfg);
                    let mut fp = ws.word_decos.tick(
                        frame_started,
                        tick_cfg,
                        effect_geom,
                        cur,
                        Some(sel_view),
                        ws.focused,
                        &mut ws.deco_scratch,
                        &mut ws.ink_scratch,
                        &mut ws.free_scratch,
                        &mut ws.nova_scratch,
                    );
                    // Kitty Log drain (§F4.3): PROMPTLY after the tick — the
                    // sightings vec clears at the next tick's start, so an
                    // undrained frame's sightings would be lost. Dedupe keys on
                    // (session, ident): two windows sharing this session log a
                    // cat once. Language codes resolve against the SAME lexicon
                    // build that produced the match (LangIds are build-scoped).
                    // `kitty_log_on = false` drains-and-drops; recording is
                    // observation-only — nothing rendered changes.
                    let discovered = self.kitty_log.observe(
                        front_terminal.session,
                        ws.word_decos.drain_kitty_sightings(),
                        &rs.lexicon,
                        frame_started,
                        kitty_log_on,
                    );
                    if let Some(look) = discovered {
                        ws.cursor_cat.on_collect(frame_started, look);
                        ws.cursor_cat
                            .set_collection_presentable(frame_started, cat_presentable);
                        // `cat_frame` was resolved before this tick. Full
                        // motion already owns a frame cadence; reduced motion
                        // needs one immediate redraw to paint its static hello
                        // before the single expiry wake erases it.
                        if !animate_cat && let Some(window) = ws.os_window.as_ref() {
                            window.request_redraw();
                        }
                    }
                    // Curse-BONK drain (the sparkle-words sound seam):
                    // PROMPTLY after the tick, beside the kitty drain — the
                    // cue vec clears at the next tick's start. A disabled
                    // knob drains-and-drops so no backlog crosses an enable.
                    // Policy is host-owned: the profanity `bonk` knob, RAW
                    // focus (`ws.focused` — never the synthetic motion_focus
                    // a recording pins), the same reduced-motion demotion
                    // this tick rendered under, and the shared trail volume;
                    // `bonk_detonations` separately admits the on-screen
                    // detonation kind (typed provenance stays typed-only).
                    let bonk_gain = bonk_sound_gain(
                        ws.focused,
                        bonk_enabled
                            // Resize repaint storms drain silently
                            // (RESIZE_SOUND_QUIET) — a re-scanned curse on a
                            // reflowed frame is a repaint, not a keystroke.
                            && !ws.resize_sound_quiet(std::time::Instant::now()),
                        tick_cfg.reduced_motion,
                        bonk_volume,
                    );
                    let curse_drain = drain_curse_bonk_cues(
                        &mut ws.word_decos,
                        bonk_style,
                        self.config.trail_sound_voice(),
                        effect_geom.cols,
                        bonk_gain,
                        bonk_detonations,
                        |event| self.trail_audio.push(event),
                    );
                    // The cursor companion reacts to the visual event, not to
                    // the audio policy: muted/reduced/unfocused sound never
                    // suppresses an on-screen wince. The cat frame for this
                    // present was resolved above, so request one follow-up
                    // draw when an active companion accepts the cue.
                    if ws
                        .cursor_cat
                        .on_curse(frame_started, curse_drain.wince_hits)
                        && let Some(window) = ws.os_window.as_ref()
                    {
                        window.request_redraw();
                    }
                    // The rare EARNED cat leading the cursor (gated on
                    // !deco_suspend by living in this branch — an alt-screen /
                    // load-shed frame draws none).
                    if nyan_alpha > 0
                        && let Some(cell) = cur
                    {
                        let layout = aterm_effects::word_decorations::NyanCursorLayout {
                            geom: effect_geom,
                            cursor: cell,
                            look: cat_frame.render_look(),
                            bob: cat_frame.bob,
                        };
                        if let Some(footprint) = ws.word_decos.nyan_cursor_footprint(layout) {
                            let colors = ws.cursor_cat.episode_colors().unwrap_or_else(|| {
                                let sampled = cursor_cat_color_key(
                                    &ws.input_scratch.cells,
                                    effect_geom,
                                    footprint,
                                    default_bg_u32,
                                    cursor_color_u32,
                                    glow_cfg.accent,
                                );
                                ws.cursor_cat.colors_for_episode(sampled)
                            });
                            if let Some(cursor_art_fp) = ws.word_decos.nyan_cursor(
                                aterm_effects::word_decorations::NyanCursorFrame {
                                    geom: effect_geom,
                                    cursor: cell,
                                    look: layout.look,
                                    colors,
                                    bob: cat_frame.bob,
                                    alpha: nyan_alpha,
                                    // The living-cartoon pose: banking squash/
                                    // stretch, forward lean, and the baked eye
                                    // frame (blink/squint), all state-machine
                                    // derived from the eased typing momentum.
                                    pose: cat_frame.pose,
                                    // The sing-along: the drive scales the
                                    // note alpha (wind-down crossfade), and
                                    // the ring spawns on the shared beat
                                    // clock — reduced motion pins the notes
                                    // static (no bob), the same demotion
                                    // this tick rendered under. Living in
                                    // this branch is the load-shed law: a
                                    // suspended frame sheds notes with every
                                    // other decoration.
                                    sing: cat_frame.sing,
                                    notes: {
                                        ws.music_notes.update(
                                            frame_started,
                                            cat_frame.sing,
                                            ws.nyan_sing.beat(frame_started),
                                        );
                                        ws.music_notes
                                            .frame_array(frame_started, tick_cfg.reduced_motion)
                                    },
                                },
                                &mut ws.free_scratch,
                            ) {
                                fp ^= cursor_art_fp.rotate_left(29);
                            }
                        }
                    }
                    fp
                }
            } else {
                // Master off in config: every
                // sparkle scratch — ink + cats + novas included — clears to
                // byte-identical off. v3 §1.1 reset table: the master toggle is a
                // hard_reset (fresh start is user intent — done marks clear too).
                ws.word_decos.hard_reset();
                ws.deco_scratch.clear();
                ws.ink_scratch.clear();
                ws.free_scratch.clear();
                ws.nova_scratch.clear();
                0
            };
            // VI-1: the on-viewport vi (copy-mode) cursor screen position, if any — used
            // for the RepaintKey (so a vi motion, which damages no grid cell, still
            // forces a repaint), the render override below (same mapping, so they never
            // disagree), AND the rain cursor band (the band must follow the cursor the
            // user is actually steering, not the parked terminal cursor). `None` when
            // vi is off or scrolled off-viewport → the normal cursor.
            let vi_screen = vi_point.and_then(|p| {
                vi_screen_row(p.line, display_offset as i32, rows)
                    .map(|row| (row, (p.col as usize).min(cols.saturating_sub(1))))
            });
            vi_cursor_override = vi_screen; // hoist for the post-prediction override
            let active_id = front_terminal.session;
            let shell_execute_edge = rain_shell_execute_rising_edge(
                &mut ws.rain_shell_executing,
                active_id,
                shell_executing,
            );
            // PHOSPHOR rain tick (design §5/§6): grid-scanned like the sparkle
            // words (Tier-A occupancy on epoch change, Tier-B live predicates
            // per tick), running in this same unlocked region off LOCK-A
            // locals only. Emits into the resident scratch; the fingerprint
            // joins the RepaintKey so a live field is never skipped by the
            // early-out — and is EXACTLY 0 whenever the feature is off or the
            // field is drained empty (idle = byte-identical).
            let rain_fp = if let Some(cfg) = rain_cfg {
                if rain_suspend {
                    // Alt-screen suppression OR the TYPING-3 load-shed latch:
                    // clear the scratch and skip the EMISSION path — the
                    // shared gate above already skipped the rescan, so rescan
                    // and tick can never disagree (design §7). The WEATHER
                    // machine still advances via the cheap suspended tick
                    // (no bake / field walk / quads): notes starve, the
                    // weather sleeps, the drain completes, and `is_active`
                    // self-disarms — a suspended pane must never leak
                    // perpetual wakes off a frozen Working/Calm state.
                    if let Some(engine) = ws.matrix_rain.as_mut() {
                        engine.tick_suspended(frame_started);
                    }
                    // Keep the completion latch BASELINED while suspended: a
                    // command finishing during a long suppression must not be
                    // observed as "new" minutes later on resume and fire a
                    // stale ember/wave (codex round-3). Suspension-era
                    // completions are silently absorbed.
                    ws.rain_last_cmd =
                        Some((front_terminal.session, cmd_done.map_or(0, |(e, _)| e)));
                    ws.rain_scratch.clear();
                    ws.rain_add_scratch.clear();
                    0
                } else {
                    // Lazy build on the FIRST enabled tick — a default-off
                    // config never constructs the engine (the zero-cost pin).
                    // `seed = 0` resolves to a stable per-window derivation
                    // here (never wall-clock randomness).
                    let engine = ws.matrix_rain.get_or_insert_with(|| {
                        Box::new(crate::matrix_rain::MatrixRain::new(
                            crate::rain_config_for_window(cfg, id),
                        ))
                    });
                    // W11: a Reduced policy (OS flag, config `motion`, or the
                    // unfocus demotion) means the engine emits NOTHING (fp 0)
                    // — bypass-to-final-state (the drained-empty frame), the
                    // StreamFade precedent, proven exactly-zero by the motion
                    // totality tests.
                    engine.set_reduced_motion(
                        !motion.animate(crate::motion::MotionEffect::MatrixRain),
                    );
                    // Focus → visibility every tick (cheap: edge-detected in
                    // the engine). `on_focus` also flips it live, but a
                    // lazily-built engine must observe the CURRENT focus.
                    engine.set_visibility(if ws.focused {
                        crate::matrix_rain::RainVisibility::Focused
                    } else {
                        crate::matrix_rain::RainVisibility::VisibleUnfocused
                    });
                    // The agent-output weather signal (LOCK-A read above):
                    // only an actual seq change registers.
                    engine.note_activity(content_seq);
                    if shell_execute_edge {
                        engine.note_signal(crate::matrix_rain::RainSignal::Execute as u32, 4);
                    }
                    // EXIT STATUS → weather (OSC 133/633): fire once per NEW
                    // completion. Keyed by (session, end_ms) so a tab switch
                    // re-BASELINES (no stale tint replay from another
                    // session's history) and only a genuinely new completion
                    // in the same session notes the engine.
                    {
                        // seq 0 = "watching, none seen": the None→Some edge
                        // within one session is a REAL first completion and
                        // fires; a tab switch still re-baselines silently.
                        let seq = cmd_done.map_or(0, |(e, _)| e);
                        let key = (front_terminal.session, seq);
                        if ws.rain_last_cmd != Some(key) {
                            let same_session = ws
                                .rain_last_cmd
                                .is_some_and(|(sid, _)| sid == front_terminal.session);
                            ws.rain_last_cmd = Some(key);
                            if same_session && let Some((_, code)) = cmd_done {
                                engine.note_exit_status(code != 0);
                            }
                        }
                    }
                    if rain_refresh && engine.can_emit() {
                        // The grid was extracted into `input_scratch` under
                        // LOCK A at this same `epoch` (no torn read); scan
                        // those cells here on host state, no lock held.
                        // Scrolled-back frames — and engines that CANNOT emit
                        // (reduced motion / unfocused past the drain) — SKIP
                        // the O(rows·cols) rescan (round-3 audits): emission
                        // is gated there anyway, and `last_epoch` only
                        // advances inside the rescan, so `needs_rescan` stays
                        // true and the scan runs on the first eligible frame.
                        let needs_grid_rescan = engine.needs_rescan(epoch);
                        let needs_material_sample = engine.needs_material_sample()
                            || (needs_grid_rescan
                                && rain_cfg.is_some_and(|cfg| cfg.output_material));
                        if needs_grid_rescan {
                            engine.rescan_from_cells(
                                &ws.input_scratch.cells,
                                &ws.input_scratch.line_sizes,
                                &ws.input_scratch.images,
                                rows,
                                cols,
                                default_bg_u32,
                                epoch,
                            );
                        }
                        // OUTPUT MATERIAL BANK: same snapshot, same gate — the
                        // rain's alphabet becomes supported literal codepoints
                        // from program output (current typing/composer bands
                        // excluded, mirroring Tier-B).
                        // Scrolled-back frames are SKIPPED (workflow audit):
                        // the snapshot rows are display-translated there while
                        // the cursor is grid-space, so the typed line could be
                        // sampled — and emission is display-gated anyway; the
                        // previous table simply persists until live again.
                        if needs_material_sample {
                            engine.sample_material(
                                &ws.input_scratch.cells,
                                rows,
                                cur,
                                &ws.rain_hidden_band,
                            );
                        }
                    }
                    let effect_geom = crate::word_decorations::EffectGeom {
                        cell_w: glow_cw as u16,
                        cell_h: glow_ch as u16,
                        rows: rows as u16,
                        cols: cols as u16,
                    };
                    // Tier-B live inputs, all LOCK-A snapshot state: the
                    // visible-cursor band (±2 rows in-engine), the hidden-
                    // cursor damage band maintained above, the live selection
                    // (mutates with zero damage marking — never baked into
                    // Tier A), the scrolled-back gate, and the alt-screen
                    // scroll-quiet/suppression gate.
                    let input = crate::matrix_rain::RainTickInput {
                        // In vi copy-mode the band follows the PAINTED vi cursor
                        // (the one the user is steering); otherwise the normal
                        // visible cursor. The parked terminal cursor is
                        // meaningless while vi navigates.
                        cursor: vi_screen.map(|(r, c)| (r as u16, c as u16)).or(cur),
                        hidden_band: &ws.rain_hidden_band,
                        sel: Some(crate::word_decorations::SelView {
                            sel: &selection,
                            display_offset: display_offset as i32,
                        }),
                        display_offset: display_offset as i32,
                        is_alt_screen: is_alt,
                    };
                    engine.tick(
                        frame_started,
                        effect_geom,
                        &input,
                        &mut ws.rain_scratch,
                        &mut ws.rain_add_scratch,
                    )
                }
            } else {
                // Fully off: the front session's effective state is off AND no
                // engine lingers (either it never existed, or the layout paths
                // dropped it), so NOTHING runs here — no engine is ever
                // constructed on the disabled path (the D-1 zero-cost pin).
                // A still-draining engine takes the suspended branch above
                // instead, until `is_active` self-disarms.
                0
            };
            // M1 SCROLL PILL (single-pane path): the iOS-style auto-fading
            // position indicator at the grid's right edge, emitted as
            // `GlowQuad`s (premultiplied additive light) — the SAME
            // parity-proven channel as the cursor aurora, so the CPU fill and
            // the GPU quad land identical pixels by construction, and the
            // splice below shifts it down with the grid. Sliced per cell-row
            // band to honor the GlowQuad row-scope invariant. Geometry is the
            // proven `pill_geometry` law (thumb ∝ viewport/history, monotone
            // in offset, exact endpoints); the fade envelope is the proven
            // `pill_alpha` (W11 Reduced ⇒ binary show/hide, no fade ramp).
            // `pill_fp` joins the RepaintKey so fade frames present; 0 when
            // invisible — byte-identical to the pre-pill path. Alt screen
            // suppressed (no scrollback there, like the sparkle words).
            let pill_fp = {
                let animated = motion.animate(crate::motion::MotionEffect::ScrollPill);
                let alpha = ws.scroll_pill.alpha(frame_started, animated);
                let hist = scrollback_lines;
                let mut fp = 0u64;
                if alpha > 0 && hist > 0 && rows > 0 && !is_alt {
                    let (cw, ch) = (glow_cw, glow_ch);
                    // 2px inset track inside the grid interior; thumb ≥ one cell.
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
                        // WINDOW-ABSOLUTE coords (the cursor_glow_add stream is a
                        // window-space effects stream now — the renderer adds no
                        // offset and the splice shifts tags only): fold the grid
                        // origin in here, exactly like the cursor_glow producers.
                        // (Adversarial review: the grid-relative form painted the
                        // pill origin-up-left of its track.)
                        // (origin_x/origin_y: the shared derivation captured above,
                        // before the `ws` borrow — the same values the glow tick used.)
                        let (gox, goy) = (origin_x as usize, origin_y as usize);
                        let x = gox + (cols * cw).saturating_sub(pw + 2);
                        // Premultiplied neutral light, scaled by the fade alpha.
                        let l = (0xA8u32 * u32::from(alpha)) / 255;
                        let color = (l << 16) | (l << 8) | l;
                        // Slice the thumb into per-row-band quads (window px).
                        let (top, bot) = (goy + 2 + y as usize, goy + 2 + (y + len) as usize);
                        for r in 0..rows {
                            let (b0, b1) = (goy + r * ch, goy + (r + 1) * ch);
                            let (s0, s1) = (top.max(b0), bot.min(b1));
                            if s0 < s1 {
                                ws.glow_scratch.push(aterm_render::GlowQuad {
                                    row: r as u16,
                                    x: x as u16,
                                    y: s0 as u16,
                                    w: pw as u16,
                                    h: (s1 - s0) as u16,
                                    color,
                                });
                            }
                        }
                        // Alpha + geometry fingerprint (nonzero: alpha > 0 here).
                        fp = (u64::from(alpha) << 48)
                            | (u64::from(y) << 24)
                            | u64::from(len)
                            | (1 << 63);
                    }
                }
                ws.pill_shown = fp != 0;
                fp
            };
            // Fold the open overlay (Settings OR About) into the key so a panel change
            // repaints without the old `last_present = None` side-channel; `0` when closed.
            let settings_fp = ws.overlay_fp();
            // Fold the OPEN find bar's displayed state in so a no-match keystroke / ^S^R
            // toggle repaints the bar; `0` when not searching (byte-identical idle).
            let find_fp = ws.search.as_ref().map_or(0, |s| s.fingerprint());
            // GLOBAL Rung 2 relaunch nudge (App-level, painted into every window): fold
            // its fingerprint in so the banner's appear/dismiss presents on a static grid.
            let relaunch_fp = self.relaunch.as_ref().map_or(0, |n| n.fingerprint());
            // Subtle top-right build/version badge (paint-only). `0` when the setting is off.
            let badge_fp =
                crate::build_badge::fingerprint(self.config.show_build_badge_or_default());
            // Transient update notice — quantized over its fade so each step re-presents.
            let notice_fp = self
                .notice
                .as_ref()
                .map_or(0, |n| n.fingerprint(std::time::Instant::now()));
            // LEVEL-UP celebration — quantized to its ~30fps step so the glow/arrow re-
            // present every frame while up; `0` when idle (byte-identical no-celebration).
            let level_up_fp = self
                .level_up
                .as_ref()
                .map_or(0, |l| l.fingerprint(frame_started));
            let key = RepaintKey {
                damage_epoch: epoch,
                grid_top,
                blink_phase,
                invert,
                drag_hover,
                cursor_override,
                cursor_row: vi_screen.map_or(cpos.row as usize, |(r, _)| r),
                cursor_col: vi_screen.map_or(cpos.col as usize, |(_, c)| c),
                cursor_visible,
                cursor_style,
                glow_fp,
                trail_fp,
                deco_fp,
                rain_fp,
                selection: SelectionFingerprint::of(&selection),
                tab_strip,
                settings_fp,
                find_fp,
                pill_fp,
                // M1b sub-row scroll: the banked residual PRESENTED this frame (gated
                // by the SmoothScroll motion policy — Reduced ⇒ 0 ⇒ whole-row snap).
                // A frac-only change dirties no cell, so it must live in the key or
                // the smooth glide's re-present is swallowed by the early-out. The
                // matching `input_scratch.scroll_frac_px` is set in the compose tail
                // (`set_scroll_band`) from the SAME gate, so key and present agree.
                scroll_frac_px: if motion.animate(crate::motion::MotionEffect::SmoothScroll) {
                    ws.scroll_frac_px
                } else {
                    0
                },
                relaunch_fp,
                badge_fp,
                notice_fp,
                level_up_fp,
                // An OS appearance flip must reach the glass: the Settings preview's
                // auto titlebar mock splits on it and no other term moves (main.rs).
                system_dark: repaint_system_dark(self.os_appearance),
            };
            // SYNC-1: HOLD — the pane is mid atomic update (DEC 2026 synchronized
            // output). Skip the present so the last frame stays on screen until the app
            // ends sync or the `sync_hold_until` safety-valve deadline (folded into
            // `about_to_wait`, released in `new_events`) fires. LOCK A already dropped and
            // its rescan's `take_damage` was gated off under the hold, and LOCK B is
            // skipped by this return, so NO damage is consumed — the accumulated writes
            // present intact on the release frame. Chrome-only refresh, like the
            // no-change early-out below.
            if sync_hold {
                metrics::note_redraw_sync_hold();
                if let Some(w) = &window {
                    self.apply_title(id, w, &title);
                }
                return;
            }
            if !deco_rescan
                // A rain refresh refilled the snapshot too — the frame MUST
                // present (covers the engine-reset-with-unchanged-epoch edges:
                // toggle re-enable, layout return) or stale quads strand.
                && !rain_refresh
                && !should_repaint_or_recover(
                    last_present,
                    key,
                    ws.present_retry.recovery_redraw_outstanding,
                )
                && !ws.predictor.is_displaying(frame_started)
                && !ws.pred_shown
                // M2 stream fade: while ink is still drying (or the LAST
                // present painted a tint that must now advance or settle to
                // exact bytes), never skip the frame — the fade perturbs
                // pixels without perturbing the RepaintKey.
                && !ws.fade_shown
                && !ws.stream_fade.is_active(frame_started)
            {
                metrics::note_redraw_early_out();
                // Nothing visible changed since the last present. No lock is held
                // (LOCK A already dropped, LOCK B not yet taken), so no damage was
                // consumed — refresh only the window chrome (a title-only change
                // needs no pixel repaint) and skip the frame entirely.
                if let Some(w) = &window {
                    self.apply_title(id, w, &title);
                }
                return;
            }
            // ---- LOCK B (commit). The ONLY operations that still need the Terminal
            // mutex: re-extract the LATEST grid (freshness invariant — we always
            // present the newest content, never a stale snapshot) and consume its
            // damage, both under one short hold so they stay consistent. REFILL the
            // reused snapshot in place (no per-frame container-Vec alloc when dims are
            // stable). A-3: the ENGINE builds the snapshot (`Terminal::cell_frame_into`);
            // the renderer is a pure consumer of `RenderInput`. A sparkle rescan frame
            // already extracted AND consumed under LOCK A (same epoch — no torn read),
            // so extraction runs exactly once per presented frame; the refresh
            // guard (sparkle OR rain — either one extracted at LOCK A) skips
            // LOCK B entirely for that path. ----
            if !(deco_rescan || rain_refresh) {
                // LOCK B (upstream fc766a99): the tight Terminal-mutex hold around
                // just the fresh re-extract + damage consume. The strip-row pool was
                // already reclaimed ahead of LOCK A, so `input_scratch.cells` is at
                // `rows` here and this `cell_frame_into` refills in place. It stamps
                // `snapshot_seq` with the grid's CURRENT damage epoch (render_cells.rs).
                let mut term = term_lock(&front_terminal.term);
                term.cell_frame_into(&mut ws.input_scratch, rows, cols);
                term.take_damage();
                drop(term);
                // Freshness vs. decoration consistency: this is a NON-rescan frame, so
                // the word decorations were emitted (`tick`, above) against the LOCK A
                // grid at `epoch`. If a PTY write interleaved between LOCK A and LOCK B
                // (the lock split invites exactly that), the grid just extracted is a
                // NEWER damage session — `snapshot_seq != epoch` — so the decorations'
                // cell positions no longer match the presented cells. Drop the
                // word-deco overlay (deco/ink/cat/nova) for THIS frame: present the
                // fresh grid undecorated rather than paint ink/sprites a cell off. The
                // `take_damage` above advanced the epoch, so the next frame is a rescan
                // (deco_rescan == true → forced present) that re-emits at the correct
                // positions — the artifact is bounded to one frame and never mispaints.
                // Cursor trail/glow/pill are cursor-positioned, not grid-scanned, so
                // they stay untouched.
                if ws.input_scratch.snapshot_seq != epoch {
                    ws.deco_scratch.clear();
                    ws.ink_scratch.clear();
                    ws.free_scratch.clear();
                    ws.nova_scratch.clear();
                    // PHOSPHOR rain is grid-scanned too: its occupancy (and so
                    // this frame's quads) matched the LOCK-A grid at `epoch`,
                    // not the newer grid just extracted — drop the overlay for
                    // THIS frame rather than rain over relocated text. The
                    // next frame is a rescan (forced present) that re-emits at
                    // the correct cells.
                    ws.rain_scratch.clear();
                    ws.rain_add_scratch.clear();
                }
            }
            // ---- END LOCK B. The stream-fade pass + clone_from copies below touch
            // host memory only (not `term`), so they run UNLOCKED — the whole point
            // of the split. ----
            // M2 "ink that dries" (stream fade): diff the fresh engine mirror
            // into the per-window age map, then tint cells younger than
            // `stream_fade_ms` toward their own cell background (the EXACT
            // linear-light blend on an ease-out envelope) so streamed output
            // fades in. Every taste gate is a BYPASS TO INSTANT (the proven
            // `fade_permitted` policy): keystroke echo (`input_hot`), the
            // alternate screen, a scrolled-back viewport, and a Reduced motion
            // policy (W11) all render exact bytes — and a bypassed frame dries
            // ALL ink, so a mid-flight fade never resumes (no flicker). The
            // mutation happens strictly AFTER `cell_frame_into`, on the host's
            // snapshot only: the engine grid, copied text, and recordings are
            // untouched, and CPU/GPU byte-parity holds by construction (both
            // backends consume the same tinted `RenderInput` bytes).
            let fade_alt = is_alt;
            let fade_scrolled = display_offset != 0;
            let fade_permitted = crate::stream_fade::fade_permitted(
                fade_on,
                ws.input_hot,
                fade_alt,
                fade_scrolled,
                !motion.animate(crate::motion::MotionEffect::StreamFade),
            );
            // Perf (idle): skip the whole call when the feature is off and no
            // tint from last frame still needs erasing — `update`'s step-1
            // fingerprint pass is O(rows×cols) and would run every committed
            // frame purely to be discarded. Invalidate the age map on the skip
            // path so a later re-enable re-baselines (settled ink) instead of
            // fading in everything that changed while it was off.
            let fade_tinted =
                if crate::stream_fade::fade_update_needed(fade_on, ws.fade_shown) && !load_shed {
                    // Under load-shed, skip the O(rows×cols) stream-fade fingerprint pass
                    // (TYPING-3) — the `else` resets the age map, so ink settles to exact
                    // bytes with no fade rather than costing a whole-grid diff every frame.
                    ws.stream_fade.update(
                        &mut ws.input_scratch.cells,
                        ws.input_scratch.cols,
                        (fade_token, fade_alt, fade_scrolled),
                        fade_permitted,
                        fade_ms,
                        frame_started,
                    )
                } else {
                    ws.stream_fade.reset();
                    false
                };
            if fade_tinted || ws.fade_shown {
                // Post-fill cell mutation (or the frame that ERASES the last
                // tint) must not be masked by the snapshot-keyed render cache —
                // the ghost-paint discipline (see `paint_prediction_ghosts`).
                ws.input_scratch.snapshot_seq = ws.input_scratch.snapshot_seq.wrapping_add(1);
            }
            ws.fade_shown = fade_tinted;
            // Hand the renderer this frame's aurora (grid-interior pixels; the
            // tab-strip splice below shifts it down with the cursor).
            // `cell_frame_into` does not touch this field, so set it after the refill.
            ws.input_scratch
                .cursor_glow_add
                .clone_from(&ws.glow_scratch);
            // …and this frame's RADIAL halos (fire embers / crown / impact
            // flash — EMBERFORGE round light), same coords, same splice rules.
            ws.input_scratch.glow_halo.clear();
            ws.input_scratch
                .glow_halo
                .extend_from_slice(ws.cursor_glow.halos());
            // The cat's fade-out FLOURISH (heart meow / star wink) rides the same
            // frame streams, emitted HERE so its theme arms land correctly: the
            // LIGHT-theme SOURCE-OVER veil appends to `glow_halo` (just refilled
            // above), the DARK-theme additive quads append to the already-cloned
            // `cursor_glow_add`. `emit_exit_fx` writes exactly one sink per theme.
            // Both streams are spliced together by the tab strip below, so the
            // flourish tracks the grid exactly as it did from `glow_scratch`.
            if nyan_alpha > 0
                && cat_frame.fade_out > 0.0
                && !matches!(cat_frame.exit, crate::nyan_cursor::CatExit::Plain)
                && let Some((crow, ccol)) = cur
            {
                // WINDOW-ABSOLUTE anchor (emit_exit_fx's contract — the effects
                // layer adds no renderer offset).
                let ax = i32::from(glow_geom.origin_x) + (i32::from(ccol) + 1) * glow_cw as i32;
                let ay = i32::from(glow_geom.origin_y) + crow as i32 * glow_ch as i32;
                crate::nyan_cursor::emit_exit_fx(
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
            // …and the PER-PIXEL FIRE (campaign 2): the flame body evaluated at
            // every device pixel by the shared field.
            ws.input_scratch.fire_patch.clear();
            ws.input_scratch
                .fire_patch
                .extend_from_slice(ws.cursor_glow.patches());
            // …and the UNDER-INK flame body + CHARRED ink (P6 dark cores).
            ws.input_scratch.glow_under.clear();
            ws.input_scratch
                .glow_under
                .extend_from_slice(ws.cursor_glow.under_quads());
            ws.input_scratch.char_fg.clear();
            ws.input_scratch
                .char_fg
                .extend_from_slice(ws.cursor_glow.charred());
            // …and the fire CONTRAST-HALO strengths (the colour-free
            // legibility stream — a GRID stream; the tab-strip splice below
            // shifts its rows down with the glyphs, like char_fg).
            ws.input_scratch.fire_halo.clear();
            ws.input_scratch
                .fire_halo
                .extend_from_slice(ws.cursor_glow.halo_cells());
            // The block-fill override: the rainbow (Nyan) fill, else the fire
            // FORGE fill, else the phaser EMITTER fill, else the laser BOLT
            // fill, else the comet NUCLEUS fill, else the water DROPLET fill,
            // else the beam EMITTER fill — all ride the same contrast-floored
            // seam (None ⇒ ordinary themed cursor; at most one is Some — the
            // styles are mutually exclusive). The focus resolver wins last:
            // a real inactive window uses a neutral white hollow outline.
            ws.input_scratch.cursor_fill_override = window_cursor_fill(
                cursor_override,
                rainbow_fill
                    .or(forge_fill)
                    .or(phaser_fill)
                    .or(bolt_fill)
                    .or(comet_fill)
                    .or(droplet_fill)
                    .or(beamrod_fill),
            );
            // Present-time latency hint (TYPING-2): a keystroke-echo frame defers the
            // whole-framebuffer present-time bloom halo to the next settle frame. Read
            // before the `ws.input_hot = false` reset below.
            ws.input_scratch.input_hot = ws.input_hot;
            // Hand the renderer this frame's cadence-comet trail cells + the (ignited,
            // heat-blended) comet colour they render at. Empty when idle / not the comet
            // style → byte-identical to no trail.
            ws.input_scratch.cursor_trail.clone_from(&ws.trail_scratch);
            ws.input_scratch.cursor_trail_color = trail_color;
            // Sparkle-word decorations for this frame (viewport coords; the
            // tab-strip splice below shifts them down with the grid).
            ws.input_scratch
                .word_decorations
                .clone_from(&ws.deco_scratch);
            // Animated-ink fg overrides (host-resolved final bytes): the renderer
            // substitutes them at the fg seam BEFORE its legibility floors, so
            // min-contrast/selection guarantees apply to the FINAL ink color.
            ws.input_scratch.ink.clone_from(&ws.ink_scratch);
            // Overlay Phase 4: the engine no longer produces legacy per-row
            // cat quads — keep the channel empty (nothing else feeds it in
            // this host).
            ws.input_scratch.cat_quads.clear();
            ws.input_scratch.cat_atlas = None;
            // Free-overlay sprites (overlay Phase 4: one FreeSprite per
            // peeking cat + its gaze dots; empty when no cats — byte-
            // identical off). The versioned atlas Arc rides only when sprites
            // do; the tab-strip splice below shifts the (row-free) pixel y
            // with the grid.
            ws.input_scratch.free_sprites.clone_from(&ws.free_scratch);
            ws.input_scratch.free_atlas = if ws.free_scratch.is_empty() {
                None
            } else {
                ws.word_decos.free_atlas()
            };
            // Supernova additive light (premultiplied nova_add quads; the
            // tab-strip splice below shifts row + pixel y with the grid).
            ws.input_scratch.nova_add.clone_from(&ws.nova_scratch);
            // PHOSPHOR rain glyph sprites + bright-head halos (viewport
            // coords; the tab-strip splice shifts row + pixel y with the
            // grid). The versioned atlas Arc rides ONLY when quads do — a
            // rain-free frame is byte-identical to the pre-feature input
            // (empty vecs + a None atlas), the free_atlas idiom.
            ws.input_scratch.rain_quads.clone_from(&ws.rain_scratch);
            ws.input_scratch.rain_add.clone_from(&ws.rain_add_scratch);
            ws.input_scratch.rain_atlas = if ws.rain_scratch.is_empty() {
                None
            } else {
                ws.matrix_rain.as_mut().and_then(|e| e.rain_atlas())
            };
            // Single-pane frames carry NO pane fx clip — clear any box a prior
            // composed frame left on the reused scratch (present-time post-fx
            // must cover the whole window again).
            ws.input_scratch.fx_clip = None;
            // Live default-bg (folded with DECSCNM reverse-video) + cursor colour, so
            // the renderer's PADDING band and the cursor track OSC 11/111 and OSC 12/112
            // (and DECSCNM ?5), not just the static config theme. Both were resolved
            // under LOCK A (`default_bg_u32` / `cursor_color_u32`) — terminal STATE that
            // `cell_frame_into` does not touch, so the snapshot equals the pre-split
            // read; the cursor colour falls back to the configured theme cursor when no
            // program has set OSC 12, so the default path is byte-identical (default_bg
            // then equals the existing theme.bg too).
            ws.input_scratch.default_bg = default_bg_u32;
            ws.input_scratch.cursor_color = cursor_color_u32;
            ws.stamp_present_decision(key);
            title
        };
        // Anchor the IME candidate/compose window at the caret for the SINGLE-PANE layout
        // (the default). The term guard is now dropped, so &mut self is free.
        // report_ime_cursor_area's own last_ime_cell gate keeps it cheap, and a steady
        // (skipped) frame can't move the caret, so reporting on rendered frames suffices.
        if let Some((pos, vis)) = single_pane_ime_caret {
            self.report_ime_cursor_area(id, pos, (0, 0), vis);
        }
        // Predictive local echo: reconcile pending guesses against the now-current
        // grid, then overlay any survivors onto the terminal-sized `input_scratch`
        // BEFORE the tab-strip splice shifts everything together. This block is the
        // SINGLE-PANE path; the composed multi-pane path runs the same reconcile +
        // ghost paint for its FOCUSED pane inside `redraw_compose` (pane-local
        // coords, before the blit).
        let mut painted_pred = false;
        // IDLE GUARD (predict findings): with nothing pending and no ghost
        // still on glass, the whole block is skipped — no pmode resolve, no
        // EXTRA term-lock acquisition (this was a third lock after LOCK A/B),
        // no reconcile — so an idle predictor costs zero per presented frame
        // (Claude Code repaints per keystroke; its no-echo gate keeps the
        // predictor permanently idle there). `pred_shown` keeps the one erase
        // frame after a flush empties a displayed set; every flush site
        // leaves `preds` empty, so no stale expiry deadline can strand.
        if !multi_pane
            && self
                .windows
                .get(&id)
                .is_some_and(|ws| !ws.predictor.idle() || ws.pred_shown)
        {
            let pmode = self.predict_mode();
            let pred_blank = self.pred_blank_cell();
            // Config flipped to OFF with guesses still pending: FLUSH them here —
            // every other flush site (reconcile/overlay/set_mode) lives behind the
            // `pmode != Off` gate below, so without this the stranded guesses keep
            // `next_deadline()` in the past forever and the deadline-driven repaint
            // spins the event loop at 100% CPU until some focus/layout change
            // resets the predictor. (A no-op when nothing is pending.)
            if pmode == crate::predict::PredictMode::Off {
                if let Some(ws) = self.windows.get_mut(&id) {
                    ws.predictor.reset();
                }
            } else if let Some(ws) = self.windows.get_mut(&id) {
                ws.predictor.set_mode(pmode);
                let term = front_terminal.term.clone();
                let now = std::time::Instant::now();
                // Predictions live in ACTIVE-grid coords; while scrolled back the
                // viewport shows scrollback, so neither reconcile nor overlay applies.
                let scrolled = {
                    let g = term_lock(&term);
                    if g.grid().display_offset() != 0 {
                        // Still run the expiry flush while scrolled into history.
                        // Predictions live in ACTIVE-grid coords, so we never PAINT
                        // them over the scrollback view (the returned slice is
                        // discarded) — but a guess that was in flight when the user
                        // scrolled up must still self-heal. Otherwise `overlay` (the
                        // only flush site) never runs, `next_deadline()` stays pinned
                        // at a past instant, and the deadline-driven repaint spins at
                        // 100% CPU re-presenting full frames until the user scrolls
                        // back to the bottom.
                        let _ = ws.predictor.overlay(now);
                        true
                    } else {
                        let cur = g.cursor();
                        // No-echo gate, matching the arm site in `App::input`: the alt
                        // screen OR an app-owned Kitty composer (REPORT_EVENT_TYPES /
                        // REPORT_ALL_KEYS_AS_ESC). Passing it to `reconcile` flushes any
                        // in-flight guesses the moment the mode flips, so a ghost armed
                        // just before Codex negotiated its composer mode is erased once
                        // and never re-armed. Read-only projection — never bytes.
                        let no_echo =
                            g.is_alternate_screen() || g.kitty_suppresses_predictive_echo();
                        // Resolve through a resident scratch row memoised by row index:
                        // every pending guess shares one row (same-row model + at most
                        // one wrapped head), so a type-ahead queue of N used to rebuild
                        // the identical row N times — N allocations and N full-row
                        // resolves — inside the term-lock scope the PTY reader needs.
                        // Moved OUT of `ws` so the closure does not re-borrow it
                        // alongside `ws.predictor`, and moved back after.
                        let mut scratch = std::mem::take(&mut ws.pred_row_scratch);
                        let mut cached: Option<u16> = None;
                        ws.predictor
                            .reconcile(Some((cur.row, cur.col)), no_echo, now, |r, c| {
                                if cached != Some(r) {
                                    scratch.clear();
                                    g.render_row_into(r as usize, &mut scratch);
                                    cached = Some(r);
                                }
                                scratch
                                    .get(c as usize)
                                    .map(|cell| cell.ch)
                                    .filter(|ch| *ch != ' ')
                            });
                        ws.pred_row_scratch = scratch;
                        false
                    }
                };
                if !scrolled {
                    let preds = ws.predictor.overlay(now).to_vec();
                    paint_prediction_ghosts(&mut ws.input_scratch, &preds, cols, pred_blank);
                    if !preds.is_empty() {
                        painted_pred = true;
                        // Our post-fill cell mutation must not be masked by the
                        // snapshot-keyed render cache, so invalidate it.
                        ws.input_scratch.snapshot_seq =
                            ws.input_scratch.snapshot_seq.wrapping_add(1);
                        // Advance the DRAWN cursor one past the newest displayed
                        // guess (mosh-style): otherwise the cursor sits at the real
                        // (unechoed) position visibly trailing the type-ahead by a
                        // full RTT. Purely visual — the engine cursor is untouched,
                        // reconcile still compares against the real grid, and every
                        // displayed-prediction frame already bypasses the early-out
                        // (`is_displaying` / `pred_shown`), so no staleness window.
                        if let Some(last) = preds.last() {
                            ws.input_scratch.cursor_row = last.row as usize;
                            ws.input_scratch.cursor_col =
                                (last.col as usize + 1).min(cols.saturating_sub(1));
                        }
                    }
                }
            }
        }
        // VI-1: paint the vi (copy-mode) cursor instead of the terminal cursor while
        // active — at its grid line mapped through the display offset. Off-viewport keeps
        // the normal cursor. A pure render override (the engine cursor is untouched); runs
        // AFTER the prediction override so copy-mode navigation wins (there is no
        // type-ahead prediction while navigating). The tab-strip splice shifts it down
        // with the normal cursor.
        if let Some((row, col)) = vi_cursor_override
            && let Some(ws) = self.windows.get_mut(&id)
        {
            ws.input_scratch.cursor_row = row;
            ws.input_scratch.cursor_col = col;
            ws.input_scratch.cursor_visible = true;
        }
        // Record whether THIS present painted a guess so the early-out repaints the
        // frame that ERASES a ghost: a backspace/flush emptying a displayed set leaves
        // the grid (and RepaintKey) unchanged, so without this the erase frame is
        // skipped and a stale glyph lingers for a full echo RTT. SINGLE-PANE ONLY:
        // on the composed path `redraw_compose` just set the flag from ITS OWN
        // paint, and this block's local `painted_pred` is always false there —
        // writing it would clobber the compose value and skip the split erase.
        if !multi_pane && let Some(ws) = self.windows.get_mut(&id) {
            ws.pred_shown = painted_pred;
        }
        // SPLICE the visible tab strip ABOVE the just-filled terminal grid (shifting
        // the content + cursor down by `tab_strip_rows`). A no-op when the strip is
        // disabled, so `input_scratch` is then the terminal grid exactly as before
        // (byte-identical). Both the single-pane and composed paths funnel here.
        self.splice_tab_strip_with(id, tab_strip);
        // M1b sub-row scroll: after the tab strip is prepended (the grid slid down
        // by `strip` chrome rows), the terminal-content band is exactly
        // `[strip, input_scratch.rows)`. Record
        // that partition + the banked sub-row residual so the CPU present translate
        // (and the GPU band shift) glide the grid by the pixel — chrome pinned.
        // Multi-pane frames set frac 0 (whole-composite shift would slide every
        // pane together), matching the compose RepaintKey.
        if !multi_pane {
            self.set_scroll_band(id);
        } else if let Some(ws) = self.windows.get_mut(&id) {
            // Splits stay whole-row: clear any grid band / residual a PRIOR
            // single-pane frame left on the reused scratch, so the composite is
            // never sub-row translated (grid_bot_row == 0 ⇒ no band ⇒ no shift).
            ws.input_scratch.scroll_frac_px = 0;
            ws.input_scratch.grid_top_row = 0;
            ws.input_scratch.grid_bot_row = 0;
        }
        // SPLICE the Cmd-F find bar over the bottom terminal row (a no-op when not
        // searching).
        self.splice_find_bar(id);
        // OVERLAY the modal Settings panel over the TOP rows (a no-op when closed), last
        // so it paints on top of the grid + tab strip.
        self.splice_settings_panel(id);
        // Subtle top-right build/version badge — paint-only, its own slot; the composite
        // prefers the modal card, so it shows only when no overlay is open. A no-op when
        // the setting is off.
        self.splice_build_badge(id);
        // Transient update notice — paint-only, its own slot, priority over the badge.
        self.splice_notice(id);
        // LEVEL-UP rising up-arrow — paint-only, its own slot, priority over the notice
        // pill (the burst momentarily supersedes it). The border glow rides the overlay
        // pass below, not this card. A no-op when no celebration / the arrow has faded.
        self.splice_level_up(id);
        self.splice_config_notice(id);
        // Reflect the program-set title (OSC 0/2) in the window chrome, falling
        // back to "aterm" when nothing has set one. Only calls set_title on an
        // actual change (a cheap String compare on the already-unlocked path).
        if let Some(w) = &window {
            self.apply_title(id, w, &title);
        }

        // Present the just-filled `input_scratch` into this window's surface. A
        // target mismatch or backend present failure aborts the frame WITHOUT
        // recording metrics and routes through the dropped-frame retry below.
        // The inset-border overlay pass paints, in the theme accent (`theme.cursor`):
        // the drop-target highlight while a file is dragged (fixed alpha, priority), ELSE
        // the LEVEL-UP celebration's breathing border glow while it is up. `None` keeps
        // the present path byte-identical to before either feature (the idle invariant).
        // Both arms honor the modal-overlay suppression: `drag_hover` was gated at its
        // read, and the level-up arm consults the same `overlay_open` — matching the
        // snapshot/`image` capture paths (the SACRED WYSIWYG invariant).
        let overlay = if drag_hover {
            Some(OverlayGlow {
                accent: self.theme.cursor,
                wash_a: DROP_WASH_ALPHA as u8,
                border_a: DROP_BORDER_ALPHA as u8,
            })
        } else {
            self.level_up
                .as_ref()
                .filter(|_| !overlay_open)
                .map(|l| OverlayGlow {
                    accent: self.theme.cursor,
                    wash_a: l.wash_alpha(frame_started),
                    border_a: l.border_alpha(frame_started),
                })
        };
        // End the causal compose slice immediately before surface acquisition.
        // `present_input_scratch` supplies only post-acquire CPU raster/copy or
        // GPU encode/queue-submit CPU wall time (not shader completion), keeping
        // FIFO/nextDrawable pacing out of the adaptive feedback signal.
        let compose_ns = u64::try_from(frame_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        metrics::note_pre_present(compose_ns);
        let raster_submit_ns = match self.present_input_scratch(id, invert, overlay) {
            Ok(work_ns) => work_ns,
            Err(reason) => {
                self.handle_failed_present(id, reason, frame_started);
                // Failed acquire/commit work is the path most likely to explain
                // a typing stall. Publish its WHOLE redraw wall time before the
                // bounded retry returns; otherwise the bad attempt is invisible
                // and only a later successful retry appears in diagnostics.
                return;
            }
        };
        // The drawable-park slice, now measured rather than inferred: it sits between
        // `note_pre_present` and the renderer's post-acquire work timer, so before this
        // it existed only as a noisy subtraction out of `redraw_total`. On macOS it is
        // the single largest known typing-stall mechanism (a blocked `nextDrawable`
        // parks the winit thread and queues keyDowns behind it).
        let acquire_wait_ns = self.last_acquire_wait_ns(id);
        metrics::note_acquire_wait(acquire_wait_ns);
        let render_ns = causal_render_cost_ns(compose_ns, raster_submit_ns)
            .saturating_add(self.gpu_backpressure_ns(id));
        // OVERLAP HANDOFF reveal: this window's first REAL frame just presented —
        // put it on glass NOW (it was created hidden), so the only pixels that
        // ever cover the parked parent's frozen frame are the carried content.
        // Then re-check readiness: this present may have been the last condition.
        // ABOVE the device-lost check: the present itself completed (the
        // dropped-present path already returned), so a loss latched during it
        // must not leave the window hidden while its `last_present` stamp lets
        // the readiness byte fire — an invisible window under an exited parent.
        if let Some(ws) = self.windows.get_mut(&id)
            && ws.pending_reveal.take().is_some()
            && let Some(w) = ws.os_window.clone()
        {
            w.set_visible(true);
            self.maybe_signal_handoff_ready();
        } else if self.handoff_ready.is_some() {
            self.maybe_signal_handoff_ready();
        }
        // A GPU device loss latches on the renderer during a present (driver update /
        // TDR reset); recover here — the present borrow is now released — by downgrading
        // to the CPU backend so this window keeps rendering instead of freezing forever.
        if self.recover_latched_gpu_loss(id, None, frame_started) {
            return;
        }
        let present_latency_ns = self.present_latency_ns(id);
        // Publish this frame's timing to the process-global metrics counters, read
        // back over the control socket's `metrics` verb so a driving AI can measure
        // responsiveness directly. Off the correctness path; only on a real present.
        // `render_ns` is causal CPU wall time: compose+raster/copy, or compose +
        // GPU command encode/queue-submit. It is not completed GPU execution.
        // Surface acquisition and final-present waits live in `redraw_total`.
        metrics::record_present(present_latency_ns, render_ns);
        // FIRST-PRESENT hook: warm the font cmap-coverage index on a background
        // thread now that the first frame is on glass — moved off the pre-window
        // critical path (its "read every system font's cmap" IO contended with GPU
        // init + shell spawn on time-to-glass). The `OnceLock` inside the warm makes
        // it coordinate with any live uncovered-glyph lookup on ONE build (a racing
        // lookup blocks on the in-progress warm — bounded, correct). Detached; the
        // process exiting first is harmless.
        {
            static FONT_WARM: std::sync::Once = std::sync::Once::new();
            FONT_WARM.call_once(|| {
                std::thread::Builder::new()
                    .name("aterm-font-warm".into())
                    .spawn(aterm_render::warm_font_coverage_index)
                    .ok();
            });
        }
        // First real content present is on glass — let `about_to_wait` run the deferred
        // session restore (extra tabs/windows) now, off the first-paint critical path.
        self.first_present_done = true;
        // Whole-pass wall time (redraw entry → here), which INCLUDES the present
        // wait `render_ns` deliberately excludes — the slice where a main-thread
        // `nextDrawable` stall under GPU contention hides (2026-07-05 incident
        // candidate). `frame_started` is this redraw's entry stamp.
        metrics::record_redraw_total(frame_started.elapsed().as_nanos() as u64);
        // Frame-pacing: stamp this present so the soft cap in the `Wake::Output`
        // handler coalesces sub-`MIN_FRAME_INTERVAL` bursts against it. Reached only
        // on a REAL present (the D-1 early-out returns before this when the screen is
        // unchanged), so the cap measures from genuine frames, not skipped ones.
        // CRITICAL: stamp ONLY when this present carried genuine CONTENT
        // (`content_pending`, set by the `Wake::Output` handler) — an aurora-only
        // animation tick must NOT update the cap, or the ~60fps aurora tail after a
        // cursor move keeps it "fresh" and defers the next keystroke's echo by up to a
        // frame interval, adding input-to-photon latency to essentially every keystroke.
        // App-wide default read BEFORE the window borrow below (a window on an unknown
        // monitor falls back to it). EVERY real windowed present feeds the load-shed
        // EMA (effect-only animation frames included — see below); only the
        // frame-PACING stamp stays content-gated.
        let app_frame_interval = self.frame_interval;
        let mut present_frame_interval: Option<std::time::Duration> = None;
        let mut offscreen_present = false;
        if let Some(ws) = self.windows.get_mut(&id) {
            // HEADLESS PRESENT-REAL: whether THIS present landed in the Virtual
            // target. Read here (the one post-present `ws` borrow) to gate the
            // load-shed EMA below.
            offscreen_present = matches!(ws.present, Some(PresentTarget::Virtual { .. }));
            // This real present already sampled/ticked the live cursor effects.
            // Consume any still-pending animation timer and phase the next one
            // from this frame, preventing content-frame/timer-frame doublets.
            rebase_pending_trail_tick_after_present(
                &mut ws.next_trail_tick,
                &mut ws.last_trail_fire,
                frame_started,
            );
            // Load-shed EMA budget for THIS present — content or effect-only. The
            // ~60fps animation tail is real render work on the real frame budget:
            // feeding only content presents left the latch blind to an effect-
            // dominated load (it could neither trip under a heavy fire tail nor
            // CLEAR during a pure animation recovery — the audit's EMA blind spot).
            present_frame_interval = Some(ws.frame_interval.unwrap_or(app_frame_interval));
            ws.on_capture_presented();
            if std::mem::take(&mut ws.content_pending) {
                ws.on_present_displayed();
            }
        }
        // The cursor animation is paced by the `next_trail_tick` WaitUntil timer
        // (see `about_to_wait`), NOT by a present-driven pump: an earlier pump that
        // re-requested a redraw after every present backed the keystroke-echo
        // pipeline up to ~180ms input->present (it flooded the loop and, under the
        // Fifo present it needed, blocked the UI thread). Timer pacing keeps the
        // loop parked between frames so a keystroke wakes it and echoes immediately.
        //
        // VIDEO introspection post-present hook: this frame just PRESENTED (the
        // dropped-present path returned above) — stamp it on the shared clock,
        // harvest completed copies (non-blocking), keep pacing when asked, and
        // finalize when the recording's deadline has passed.
        if self.video_rec.as_ref().is_some_and(|r| r.window == id) {
            let t_us = crate::metrics::now_us();
            if let (Some(gpu), Some(ws)) = (self.backend.gpu_mut(), self.windows.get_mut(&id))
                && let Some(
                    crate::PresentTarget::Gpu { window_gpu, .. }
                    | crate::PresentTarget::Virtual { window_gpu },
                ) = &mut ws.present
            {
                gpu.video_after_present(window_gpu, t_us);
            }
            let (pace, done) = self
                .video_rec
                .as_ref()
                .map(|r| (r.pace, std::time::Instant::now() >= r.deadline))
                .unwrap_or((false, false));
            if done {
                self.video_finalize();
            } else if pace && let Some(w) = &window {
                // Windowed pacing only: the Virtual loop is WaitUntil-driven
                // (`VideoRec::next_frame`, re-armed in the `new_events` sweep —
                // never here, where the RepaintKey early-out would starve it).
                w.request_redraw();
            }
        }
        // LOAD-ADAPTIVE EFFECT SHEDDING (Change #1): fold THIS real present's RENDER
        // cost into the rolling EMA and re-evaluate the `perf_reduced` latch against this
        // window's frame budget. The signal is `render_ns` (causal CPU wall time:
        // compose plus CPU raster/copy, or GPU command encode/queue-submit) — NOT
        // completed shader execution and NOT `present_latency_ns`, which is the
        // output→present WAIT:
        // that value is dominated by the deliberate frame-pacing/coalescing hold and a
        // process-global output stamp shared across every session, so on a perfectly
        // healthy machine (render ~0.1 ms) it still reads tens of ms during ordinary
        // streaming and would shed the effects for no reason. Swapchain acquire is
        // excluded up to ONE frame interval for the same reason — but the EXCESS past
        // a full interval is charged (`gpu_backpressure_ns`), because a pool that stays
        // exhausted longer than a refresh is the GPU failing to keep up, not pacing.
        // Without that term the latch was blind to GPU-bound overload, which is the one
        // regime its two levers (bloom, shimmer — both pure GPU passes with ~constant
        // CPU encode cost) actually relieve. The included render work
        // is what shedding actually lowers, making the feedback loop causal. On a latch
        // TRANSITION only, gate the GPU
        // bloom pass off (entering) or restore the configured value (leaving) and log the
        // edge (the latch was previously invisible); the motion-policy fold handles every
        // other decorative effect via `MotionPolicy::Reduced`.
        // FLAGGED EXCLUSION (design): OFFSCREEN presents never feed the EMA —
        // recording overhead must not trip `perf_reduced` and shed the very
        // effects being recorded (the capture answers "what WOULD glass have
        // shown on a healthy display"). A latch already set by real windowed
        // load elsewhere stays honestly in force and is recorded as-is.
        if let Some(fi) = present_frame_interval
            && !offscreen_present
            && self.note_present_cost(render_ns, fi, frame_started)
        {
            aterm_log::info!(
                "load-shed: perf_reduced -> {} (render EMA vs {:.1}ms frame budget)",
                self.perf_reduced,
                fi.as_nanos() as f64 / 1e6,
            );
            metrics::note_shed_transition(self.perf_reduced);
            let shed = self.load_shed_active();
            let gpu_post_fx = self
                .serious_mode_policy()
                .allows(crate::motion::SeriousEffect::GpuPostFx);
            let bloom_on = !shed && gpu_post_fx && self.config.cursor_trail_bloom_or_default();
            // The heat shimmer sheds with the bloom (same latch, same
            // transition) and restores to its configured value on recovery.
            let shimmer_on = !shed && gpu_post_fx && self.config.cursor_fire_shimmer_or_default();
            if let Some(g) = self.backend.gpu_mut() {
                g.set_bloom(bloom_on);
                g.set_shimmer(shimmer_on);
            }
            self.settle_motion_policy_transition();
        }
        // Publish the freshly-presented screen to assistive tech (macOS VoiceOver)
        // when the `a11y-appkit` feature is on. Reaches here only on an ACTUAL
        // present (the D-1 early-out returns before this), so a steady screen costs
        // nothing; a no-op on the default build, off-macOS, and off-glass (no
        // window handle to publish through).
        if let Some(w) = &window {
            self.update_accessibility(id, w);
        }
        // Async fallback-face convergence (see [`fallback_convergence_action`]).
        // While a background broad-Unicode / symbol fallback parse is in flight,
        // provisional `.notdef` cells are on glass; keep re-arming until it lands
        // so the frame that follows picks up the real glyphs. `last_present` is
        // cleared each convergence frame so the re-armed redraw is NOT swallowed
        // by the content early-out (both pane paths read this field) — otherwise
        // a font zoom on an idle screen strands the tofu boxes forever. On the
        // pending→landed edge the GPU present cache is also dropped (its damage
        // diff cannot see the renderer's `font_epoch`). Steady state is one
        // bool check + a `(false, false)` return.
        let fallback_pending = self.backend.fallback_parse_pending();
        if let Some(ws) = self.windows.get_mut(&id) {
            let fallback_was = ws.cpu_cache.swap_fallback_pending(fallback_pending);
            let (rearm, invalidate) = fallback_convergence_action(fallback_pending, fallback_was);
            if rearm {
                if invalidate
                    && let Some(
                        PresentTarget::Gpu { window_gpu, .. }
                        | PresentTarget::Virtual { window_gpu },
                    ) = &mut ws.present
                {
                    window_gpu.invalidate_present();
                }
                // Force the re-armed frame past the content early-out. Off-glass
                // the recording loop's next timer tick picks it up.
                ws.last_present = None;
                if let Some(w) = &window {
                    w.request_redraw();
                }
            }
        }
        let _ = window;
    }

    /// Compute this window's tab-strip fingerprint for the current frame (refilling
    /// the reused title buffer the splice will paint from).
    ///
    /// The fingerprint feeds the RepaintKey, so it MUST be computed before the
    /// early-out — a title change has to invalidate it. Single-tab identity is visible,
    /// so the non-blocking, keep-stale title read runs for every non-empty enabled
    /// strip. Strip disabled remains byte-identical to the pre-strip path.
    ///
    /// Refills the window's reused `strip_titles_scratch` IN PLACE and returns
    /// only the fingerprint; the splice reads the same buffer. The buffer stays
    /// resident in the window, so it survives the redraw early-out too. On
    /// steady frames the refill performs no `Vec<String>`/`String` allocation
    /// after warmup: raw-title reads reuse each slot, and Smart-Title
    /// composition is either the clean-title fast path (slot kept verbatim) or
    /// a per-session cache hit `clone_from`d into the same slot — a label only
    /// recomposes (and allocates) when its title or description changed.
    fn redraw_tab_strip_state(&mut self, id: WindowId) -> u64 {
        let tab_count = self.windows.get(&id).map_or(0, |ws| ws.tab_set.len());
        if self.tab_strip_enabled() && tab_count > 0 {
            // Take the reused buffer out, refill it against the live tab titles, then
            // put it back. `tab_titles`-equivalent: empty/missing → "aterm".
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
            let fp = self.tab_strip_fingerprint_from_parts(&titles, &metadata, active);
            if let Some(ws) = self.windows.get_mut(&id) {
                ws.strip_titles_scratch = titles;
                ws.strip_metadata_scratch = metadata;
            }
            fp
        } else {
            0
        }
    }

    /// Refill `titles` in place with one entry per TAB of window `id` — each tab's
    /// FOCUSED pane session title, with an empty title falling back to the
    /// session's shell-reported cwd (`$HOME`-abbreviated, the cwd-as-default-label
    /// rule) and a missing/label-less session to `"aterm"` (matching
    /// [`Self::tab_titles`] BYTE-FOR-BYTE whenever the read wins its try-lock —
    /// see below — but reusing the caller's buffer and its per-slot `String`
    /// allocations instead of allocating fresh). The two fallback chains must
    /// never diverge: the `Wake::Output` drift handler hashes `tab_titles` output
    /// and compares it against the fingerprint of THIS buffer, so a divergent
    /// label would re-request a redraw on every subsequent output chunk.
    ///
    /// LATENCY: each title lives behind its session's Terminal mutex — the same
    /// mutex that session's PTY reader holds for the whole parse of an output
    /// chunk. This runs on EVERY redraw (pre-early-out: the fingerprint feeds the
    /// RepaintKey), so a BLOCKING lock here couples the foreground present to every
    /// background tab's in-flight parse. `try_lock` instead: on contention the slot
    /// KEEPS its previous contents (the buffer is the window-persistent
    /// `strip_titles_scratch`, so it holds the last-read title across frames), and
    /// a freshly-pushed empty slot (brand-new tab) falls back to `"aterm"`.
    /// Staleness is bounded and self-correcting: the `Wake::Output` title-drift
    /// handler epoch-gates background title changes and requests a redraw when the
    /// strip fingerprint drifts, and the fingerprint + painted strip both read this
    /// same buffer, so pixels and RepaintKey never disagree.
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
                    // TOP RUNG (byte-identical twin of `tab_titles`): the
                    // operator's `meta set title` outranks the live OSC title.
                    // A LEAF mutex on the session ctx — never the term lock, and
                    // dropped before the term try-lock below; contended only by
                    // an actual `meta set`, so the per-frame cost is one
                    // uncontended lock. When it hits, the term lock is skipped.
                    let (user_title, authored_description) = {
                        let meta = s.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
                        (
                            meta.presentation_value("title"),
                            meta.presentation_value("description"),
                        )
                    };
                    if let Some(t) = user_title {
                        slot.clear();
                        slot.push_str(&t);
                    } else {
                        // Poisoned ⇒ recover the guard exactly like `term_lock`;
                        // WouldBlock ⇒ keep the stale slot rather than waiting out a
                        // background tab's parser.
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
                        let t = term.title();
                        slot.clear();
                        if !t.is_empty() {
                            slot.push_str(t);
                        } else if let Some(cwd) = term
                            .current_working_directory()
                            .filter(|cwd| !cwd.is_empty())
                        {
                            // Cwd-as-default-label, `~`-abbreviated IN PLACE (push
                            // into the reused slot — no per-frame `String` alloc,
                            // the invariant this refill exists to keep).
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
                    // Compose IN PLACE through the coordinator's per-session
                    // label cache: a clean title with no description is kept
                    // verbatim, and an unchanged (title, description) pair is
                    // `clone_from`d out of the cache into this resident slot —
                    // either way a steady frame does no sanitize/grapheme work
                    // and no fresh allocation. Only an actual title/description
                    // change composes (and allocates) once.
                    self.title_summaries.compose_label_into(
                        Some(view.session),
                        authored_description.as_deref(),
                        self.config.tab_title_format_or_default(),
                        &self.config,
                        " · ",
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

    /// Refill the strip's canonical icon/status/close metadata in stable tab order.
    /// Unlike terminal titles this needs no mutex or stale-value policy: it is copied
    /// directly from the window's authoritative `TabSet` presentation.
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
    }

    /// Ensure a windowed CPU-backend window owns a CPU presentation target. The
    /// old target is dropped before either fallible softbuffer constructor, so a
    /// failure can never strand a dead `PresentTarget::Gpu` behind a CPU backend.
    /// Headless windows and an already-matching target are no-ops.
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
            ws.last_present = None;
        }
        let context = softbuffer::Context::new(window.clone())
            .map_err(|_| metrics::PresentDropReason::CpuAcquire)?;
        let surface = softbuffer::Surface::new(&context, window)
            .map_err(|_| metrics::PresentDropReason::CpuAcquire)?;
        let Some(ws) = self.windows.get_mut(&id) else {
            return Err(metrics::PresentDropReason::TargetMismatch);
        };
        ws.present = Some(PresentTarget::Cpu {
            surface,
            _context: context,
        });
        Ok(())
    }

    /// Recover from a GPU **device loss** — a Windows NVIDIA/AMD driver install or
    /// update, a TDR (2-second GPU hang) reset, or an eGPU unplug — reported by wgpu's
    /// device-lost callback and latched on [`aterm_gpu::GpuRenderer::device_lost`].
    /// Without recovery every subsequent `get_current_texture()` returns `Lost`, so
    /// each window would freeze at its last frame forever. Downgrade the whole app to
    /// the CPU softbuffer backend — the same fail-soft path `attach_os_window` uses
    /// when GPU surface creation fails at launch — so windows keep rendering.
    /// Idempotent: once `use_gpu` is false there is no GPU device left to lose.
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
                let Some(window) = window else {
                    return false;
                };
                request_recovery_redraw(retry, || window.request_redraw());
                true
            },
        )
    }

    /// Exact GPU-loss fallback transaction with the two OS-dependent target
    /// operations injected. Production passes `ensure_cpu_present_target` and
    /// `Window::request_redraw`; Tier-1 passes deterministic stand-ins while
    /// still executing the real CPU-renderer replacement, per-window retry
    /// accounting, and source-outcome classification.
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
        eprintln!(
            "aterm-gui: GPU device lost — downgrading to the CPU renderer so windows keep rendering"
        );
        let cpu = match self
            .backend
            .cpu_renderer_from_admitted(self.font_px, self.theme)
        {
            Ok(cpu) => cpu,
            Err(error) => {
                eprintln!(
                    "aterm-gui: could not rebuild the resident CPU fallback after GPU loss: {error}"
                );
                return GpuRecoveryOutcome::BackendUnavailable;
            }
        };
        self.backend = crate::BackendSlot::Ready(crate::Backend::Cpu(cpu));
        self.use_gpu = false;
        metrics::set_backend_gpu(false);
        self.pin_backend_render_config_core();
        // Rebuild each window's present target as a CPU softbuffer surface over the
        // still-live OS window, dropping the dead GPU swapchain. Mirrors the softbuffer
        // setup in `attach_os_window`'s CPU arm. A window whose softbuffer surface
        // cannot yet be built stays targetless inside the bounded retry scheduler;
        // each due attempt re-enters `ensure_cpu_present_target` rather than retaining
        // a permanently mismatched dead-GPU target or taking the process down.
        let mut pad_scale = 1.0_f64;
        let mut source_outcome = GpuRecoveryOutcome::SourceWithoutTarget;
        let wids: Vec<WindowId> = self.windows.keys().copied().collect();
        for wid in wids {
            let window = self.windows.get(&wid).and_then(|ws| ws.os_window.clone());
            if let Some(window) = &window {
                pad_scale = window.scale_factor();
                // GPU translucency is a native window/layer state, not renderer
                // state. CPU softbuffer is intentionally opaque: remove any blur
                // view, restore opacity/theme fill, and reassert the CPU window
                // colour-space contract before its first fallback frame.
                self.apprt.window_set_vibrancy(
                    window,
                    crate::app_config::BackgroundMaterial::None,
                    false,
                    self.theme.bg,
                );
                self.apprt
                    .window_set_appearance(window, self.window_theme_for_chrome());
            }
            match ensure_target(self, wid) {
                Ok(()) => {
                    let requested = self.windows.get_mut(&wid).is_some_and(|ws| {
                        // Replacing the dead swapchain is a genuine surface
                        // stimulus. Retain an outstanding acknowledgement until
                        // a real CPU present/drop so a suppressed winit request
                        // is re-delivered by the next external stimulus.
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
                    eprintln!(
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
                        // This proactive rebuild did not itself cross another
                        // redraw/present seam. For the initiating window, keep
                        // the one transaction count and refine its disposition;
                        // for siblings, their live per-window retry state is the
                        // diagnostic until an actual redraw attempt fails.
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
        self.backend.set_pad(self.cfg_pad_for_scale(pad_scale));
        // The fresh CPU backend booted with head 0: restore the front window's
        // chrome band too, or every chrome'd frame composes head px short with
        // the grid under the titlebar (adversarial review — every set_pad site
        // needs its set_head sibling). Per-window head is then kept current by
        // apply_window_scale on each redraw.
        let (pad_top, head) = self
            .frontmost_window
            .and_then(|wid| self.windows.get(&wid))
            .map_or((self.cfg_pad_for_scale(pad_scale), 0), |ws| {
                (ws.metrics.pad_top, ws.metrics.head)
            });
        // Restore the tightened TOP pad too (a fresh renderer tracks `pad`); the
        // front window's recorded pad_top keeps the grid origin consistent.
        self.backend.set_pad_top(pad_top);
        self.backend.set_head(head);
        self.warn_font_feature_issues();
        self.sync_chrome_fonts();
        source_outcome
    }

    /// Present the window's filled `input_scratch` into its surface via the active
    /// backend. Returns `Err(reason)` (frame aborted, NO present recorded) on a present
    /// target/backend mismatch, surface acquisition failure, or final CPU-surface
    /// commit failure; otherwise `Ok(causal_work_ns)` after a successful present.
    /// The sample is CPU wall time covering raster/copy or GPU command
    /// encode/queue-submit. It is not a GPU-completion timestamp. Swapchain/buffer
    /// acquire and the final compositor present are excluded because their waits
    /// are intentional pacing rather than work decorative shedding can reduce.
    fn present_input_scratch(
        &mut self,
        id: WindowId,
        invert: bool,
        overlay: Option<OverlayGlow>,
    ) -> Result<u64, metrics::PresentDropReason> {
        // Disjoint borrows: the renderer (`self.backend`) and the target window's
        // present target + input snapshot are SEPARATE fields of `self`, so
        // destructuring lets both be borrowed mutably at once with no aliasing.
        let App {
            backend,
            windows,
            theme,
            ..
        } = self;
        let ws = windows
            .get_mut(&id)
            .ok_or(metrics::PresentDropReason::TargetMismatch)?;
        if backend.is_gpu() {
            // GPU on-glass present: render the offscreen frame (the single source
            // of truth) and BLIT it straight into the swapchain — no Frame, no
            // softbuffer copy, no GPU->CPU readback. The blit shader applies the
            // visual-bell invert. The same offscreen texture is what the
            // snapshot/`image` introspection reads back, so screen == introspection.
            let (input_rows, input_cols) = (ws.input_scratch.rows, ws.input_scratch.cols);
            let (visible_width, visible_height) = backend.frame_size(input_rows, input_cols);
            // The GPU renderer still owns a legacy `2*pad` source texture while
            // the GUI exposes configured-top + base-bottom. Resolve the exact
            // source crop against THIS destination height: when the row-fit
            // remainder is odd, the number of rows cropped above depends on its
            // parity. This keeps GPU glyph/card Y coordinates identical to the
            // CPU present for exact fits, arbitrary resizes, and zoom/reload.
            let destination_height = ws
                .win_px
                .map_or_else(|| visible_height, |size| size.height.max(1) as usize);
            let source_crop_top =
                backend.configure_gpu_visible_y(input_rows, input_cols, destination_height);
            let present_crop = aterm_gpu::PresentCrop {
                source_y: u32::try_from(source_crop_top).unwrap_or(u32::MAX),
                height: u32::try_from(visible_height).unwrap_or(u32::MAX),
            };
            // P3 settings card → raw bytes + device-px rect for the GPU tray quad.
            // Disjoint sub-borrow of `ws.settings_card` (separate field from `ws.present`,
            // taken mutably below). `None` ⇒ the GPU draws nothing (feature off / closed).
            // The gpu crate takes raw bytes, NOT the gui `SettingsCard` type.
            // Modal card FIRST, else the level-up arrow burst, else the transient notice,
            // else the paint-only build/version badge (all share the one tray-quad slot:
            // a modal covers the rest; the burst supersedes the pill; the pill replaces
            // the static badge).
            let tray_arg = ws
                .settings_card
                .as_ref()
                .or(ws.level_up_card.as_ref())
                .or(ws.notice_card.as_ref())
                .or(ws.badge_card.as_ref())
                .map(|c| aterm_gpu::TrayQuad {
                    rgba: c.rgba.as_slice(),
                    pw: c.pw,
                    ph: c.ph,
                    dx: c.dx,
                    dy: c
                        .dy
                        .saturating_add(u32::try_from(source_crop_top).unwrap_or(u32::MAX)),
                });
            // Map the glow → the GPU overlay params (the alphas are the caller's:
            // fixed for the drop target, breathing for the level-up celebration; the
            // GPU derives the border thickness from the framebuffer size to match CPU).
            let gpu_overlay = overlay.map(|g| aterm_gpu::DropOverlay {
                accent: g.accent,
                wash_a: g.wash_a,
                border_a: g.border_a,
            });
            let source_shift = i32::try_from(source_crop_top).ok();
            let effects_shifted = source_shift.is_some_and(|shift| {
                crate::try_shift_window_absolute_effects_y(&mut ws.input_scratch, shift)
            });
            let input = &ws.input_scratch;
            let presented = match (backend.gpu_mut(), ws.present.as_mut()) {
                (
                    Some(gpu),
                    Some(PresentTarget::Gpu {
                        gpu_surface,
                        window_gpu,
                    }),
                ) => gpu
                    .present_input_cropped(
                        window_gpu,
                        gpu_surface,
                        input,
                        invert,
                        gpu_overlay,
                        tray_arg,
                        present_crop,
                    )
                    .map_err(|reason| match reason {
                        aterm_gpu::SurfacePresentFailure::Reconfigured => {
                            metrics::PresentDropReason::GpuReconfigured
                        }
                        aterm_gpu::SurfacePresentFailure::Timeout => {
                            metrics::PresentDropReason::GpuTimeout
                        }
                        aterm_gpu::SurfacePresentFailure::Occluded => {
                            metrics::PresentDropReason::GpuOccluded
                        }
                        aterm_gpu::SurfacePresentFailure::Validation => {
                            metrics::PresentDropReason::GpuValidation
                        }
                    })
                    .map(|()| window_gpu.last_present_work_ns()),
                // HEADLESS PRESENT-REAL: the glass-less recording target — the
                // SAME compose-and-blit seam presented into the persistent
                // virtual texture (tap copy included), no `present()`. Cannot
                // drop (no acquire).
                (Some(gpu), Some(PresentTarget::Virtual { window_gpu })) => {
                    if gpu.present_virtual_cropped(
                        window_gpu,
                        input,
                        invert,
                        gpu_overlay,
                        tray_arg,
                        present_crop,
                        (
                            u32::try_from(visible_width).unwrap_or(u32::MAX),
                            u32::try_from(visible_height).unwrap_or(u32::MAX),
                        ),
                    ) {
                        Ok(window_gpu.last_present_work_ns())
                    } else {
                        Err(metrics::PresentDropReason::Virtual)
                    }
                }
                _ => Err(metrics::PresentDropReason::TargetMismatch),
            };
            if effects_shifted {
                let restored = crate::try_shift_window_absolute_effects_y(
                    &mut ws.input_scratch,
                    -source_shift.expect("successful shift has a delta"),
                );
                debug_assert!(restored, "valid effect shift must be reversible");
            }
            let work_ns = presented?;
            ws.present_retry.on_presented();
            Ok(work_ns)
        } else {
            // CPU present: rasterize via the renderer's damage-tracked cache and
            // take a BORROW of the framebuffer (`render_input_cached`) rather than
            // an owned `Frame` — eliding the per-frame cache→Frame clone — then
            // copy it into the softbuffer surface. The cache→surface copy is
            // DAMAGE-BOUNDED: when the renderer reports a gate hit or a row-level
            // dirty set AND the retained surface provably holds the previous
            // present (`age() == 1`, no frontend chrome either frame), only the
            // dirty row bands are copied and the present carries per-band damage
            // rects — a cursor blink or one-row echo no longer pays a full-window
            // memcpy plus a full-window compositor upload. Every other state
            // takes the always-correct full copy (bytewise the historical path).
            let Some(PresentTarget::Cpu { surface, .. }) = ws.present.as_mut() else {
                return Err(metrics::PresentDropReason::TargetMismatch);
            };
            let Backend::Cpu(r) = backend.ready_mut() else {
                return Err(metrics::PresentDropReason::TargetMismatch);
            };
            // Frontend chrome (bell invert / drop overlay / settings card) is
            // composited into the SURFACE only, never into the renderer cache, so
            // a chrome frame — and the first frame AFTER chrome ends (the erase)
            // — must full-copy. Read the previous frame's record before the
            // render borrows the cache; re-record after the buffer is written.
            // The transient notice + the always-on build/version badge are frontend chrome
            // too (own paint-only tray-quad slots), so they force the full-copy path — a
            // damage-bounded partial present would overwrite their region with un-composited
            // pixels, or skip re-compositing them onto a dirty row.
            let chrome = invert
                || overlay.is_some()
                || ws.settings_card.is_some()
                || ws.level_up_card.is_some()
                || ws.notice_card.is_some()
                || ws.badge_card.is_some();
            let chrome_prev = ws.cpu_cache.presented_chrome();
            // Render into the per-window damage cache and take a BORROW of the
            // framebuffer. `&mut ws.cpu_cache` and `&ws.input_scratch` are disjoint
            // sub-borrows of `ws`; `r` borrows `backend` (a sibling of `windows`),
            // so all three are non-aliasing. The cache is per-window (S5c), so two
            // windows on one CPU `Renderer` keep their damage tracking isolated.
            // The exclusive render borrow is scoped to the dims read; `pixels` (and
            // the damage accessors below) then re-borrow the cache SHARED.
            let raster_started = Instant::now();
            let (fw, raw_fh) = {
                let view = r.render_input_cached(&mut ws.cpu_cache, &ws.input_scratch);
                (view.width().max(1), view.height().max(1))
            };
            // The renderer cache retains its legacy `2*pad` allocation. Expose
            // only configured-top + base-bottom rows; this is a prefix crop on
            // CPU because the renderer already places the grid at `pad_top`.
            let fh = crate::visible_frame_height(raw_fh, ws.metrics.pad, ws.metrics.pad_top);
            let pixels = ws.cpu_cache.frame_pixels();
            // W1 (kill the compositor stretch), mirroring the GPU blit: the
            // softbuffer surface is the RAW window size (`win_px`), the frame
            // lands at the centred band offset, and the `0..cell-1` remainder
            // bands are painted the live terminal background — the compositor never
            // rescales. Headless / pre-attach falls back to the frame dims (the
            // historical exact fit, byte-identical: offset 0, zero band pixels).
            let (dw, dh) = ws.win_px.map_or((fw, fh), |s| {
                (s.width.max(1) as usize, s.height.max(1) as usize)
            });
            let band_bg = present_band_bg(ws.input_scratch.default_bg, theme.bg);
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
            // `buffer_mut` can wait for compositor ownership. Deliberately start
            // the copy timer only AFTER acquisition, just as the GPU timer starts
            // after `get_current_texture`. Acquisition AND final commit are one
            // fallible transaction: either error returns `None` to the caller's
            // dropped-present re-arm path.
            let presented_work_ns = cpu_surface_transaction(surface.buffer_mut(), |mut buf| {
                let copy_started = Instant::now();
                let n = buf.len().min(dw * dh);
                // `age() == 1` is the ONLY state where the retained buffer provably
                // holds the previous present (0 = new/unknown contents: first
                // frame, post-resize, and backends that never report — e.g. macOS
                // CG — simply keep the full path). Any frontend chrome (this frame
                // or the one before, so the erase repaints) also forces the full
                // copy, as does a surface whose length no longer matches the raw
                // window size (a transient mid-resize buffer).
                let damage = ws.cpu_cache.last_damage();
                // A rescued E7 scroll blit shifted the renderer's CACHE rows, but the
                // retained SURFACE (a separate softbuffer) was not shifted — a
                // dirty-band copy would strand the moved rows. The cache already holds
                // the correct full frame, so fall back to the always-safe full copy.
                let full = chrome
                    || chrome_prev
                    || damage == DamageOutcome::Full
                    || matches!(damage, DamageOutcome::Scroll { .. })
                    || buf.age() != 1
                    || buf.len() != dw * dh;
                let commit = if full {
                    // W1 full copy: content placed 1:1 at the centred band offset
                    // (never scaled) + bell invert (content only; the bands are
                    // chrome and never flash — GPU parity) + the remainder bands
                    // painted the live terminal background.
                    aterm_render::place_frame_bands(
                        &mut buf[..n],
                        dw,
                        dh,
                        pixels,
                        fw,
                        fh,
                        invert,
                        band_bg,
                    );
                    for px in buf.iter_mut().skip(n) {
                        *px = 0;
                    }
                    let (ox, oy) = (
                        aterm_render::band_offset(dw, fw),
                        aterm_render::band_offset(dh, fh),
                    );
                    // Inset accent border + faint wash over the just-placed CONTENT frame
                    // (after the bell invert, so it reads as chrome on top; frame-relative
                    // like the GPU shader) — the drag-and-drop drop target OR the level-up
                    // celebration glow. A no-op allocation-free pass; skipped when neither.
                    if let Some(g) = overlay {
                        apply_overlay_at(&mut buf[..n], dw, dh, ox, oy, fw, fh, g);
                    }
                    // P3: composite the frosted Settings card on top (after the drop overlay,
                    // so it reads as the topmost modal), shifted by the frame offset. No-op
                    // when the card is absent. Same compositor the headless `image`/`snapshot`
                    // use (at offset 0 there) ⇒ on-glass == introspection within the frame.
                    // Modal card FIRST, else the level-up arrow burst, else the transient
                    // update notice, else the badge (same priority as the GPU tray slot).
                    if let Some(card) = ws
                        .settings_card
                        .as_ref()
                        .or(ws.level_up_card.as_ref())
                        .or(ws.notice_card.as_ref())
                        .or(ws.badge_card.as_ref())
                    {
                        composite_tray_at(&mut buf[..n], dw, dh, ox, oy, card);
                    }
                    causal_work_ns = causal_work_ns.saturating_add(
                        u64::try_from(copy_started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    );
                    buf.present()
                } else if damage == DamageOutcome::GateHit {
                    // Pixel-identical frame: zero copy. Present a minimal 1×1
                    // damage rect rather than an empty slice — empty damage is
                    // not a well-specified no-op across softbuffer backends, and
                    // over-claiming damage is always safe.
                    let one = NonZeroU32::new(1).unwrap();
                    causal_work_ns = causal_work_ns.saturating_add(
                        u64::try_from(copy_started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    );
                    buf.present_with_damage(&[softbuffer::Rect {
                        x: 0,
                        y: 0,
                        width: one,
                        height: one,
                    }])
                } else {
                    // Rows: copy only the dirty row bands into the BAND-OFFSET
                    // surface and report one damage rect per contiguous band.
                    // `compute_dirty_rows` is the single source of truth for what
                    // the renderer repainted: every overlay (trail / glow / nova /
                    // scene / sparkles) is row-gated onto dirty rows, a clean-row
                    // cursor rewrite is byte-identical, and a padding recolour forces
                    // `Full`. Only reached with NO chrome (chrome ⇒ `full`), so there
                    // is no invert/overlay/card to composite here; the surrounding
                    // bands are constant (age == 1 ⇒ no resize) and stay valid from
                    // the last full present, so only the content columns are copied.
                    let off_x = aterm_render::band_offset(dw, fw);
                    let off_y = aterm_render::band_offset(dh, fh);
                    let x0 = off_x.max(0) as usize;
                    let x1 = (off_x + fw as i64).clamp(0, dw as i64) as usize;
                    let span = x1.saturating_sub(x0);
                    let sx0 = (x0 as i64 - off_x) as usize;
                    let mut rects: Vec<softbuffer::Rect> = Vec::new();
                    for (r0, r1) in dirty_row_runs(ws.cpu_cache.dirty_rows()) {
                        let (fy0, _) = r.row_pixel_band(r0, ws.input_scratch.rows, fh);
                        let (_, fy1) = r.row_pixel_band(r1, ws.input_scratch.rows, fh);
                        let (mut lo, mut hi) = (usize::MAX, 0usize);
                        for fy in fy0..fy1 {
                            let dyi = fy as i64 + off_y;
                            if dyi < 0 || dyi >= dh as i64 {
                                continue; // content row cropped out of the surface
                            }
                            let dy = dyi as usize;
                            if span > 0 {
                                buf[dy * dw + x0..dy * dw + x0 + span]
                                    .copy_from_slice(&pixels[fy * fw + sx0..fy * fw + sx0 + span]);
                            }
                            lo = lo.min(dy);
                            hi = hi.max(dy + 1);
                        }
                        if span > 0
                            && lo < hi
                            && let (Some(width), Some(height)) = (
                                NonZeroU32::new(span as u32),
                                NonZeroU32::new((hi - lo) as u32),
                            )
                        {
                            rects.push(softbuffer::Rect {
                                x: x0 as u32,
                                y: lo as u32,
                                width,
                                height,
                            });
                        }
                    }
                    if rects.is_empty() {
                        // Every band clamped empty (or zero content span): nothing
                        // was copied — present the same minimal 1×1 rect.
                        let one = NonZeroU32::new(1).unwrap();
                        rects.push(softbuffer::Rect {
                            x: 0,
                            y: 0,
                            width: one,
                            height: one,
                        });
                    }
                    causal_work_ns = causal_work_ns.saturating_add(
                        u64::try_from(copy_started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    );
                    buf.present_with_damage(&rects)
                };
                commit.map(|()| causal_work_ns)
            });
            // Record chrome only after a successful commit. An acquire/commit
            // failure leaves this record describing what the surface still holds
            // and routes through the caller's dropped-frame re-arm path.
            // Invalidating on failure is load-bearing: the just-rasterized cache
            // must not gate-hit on retry while glass still contains the older frame.
            if presented_work_ns.is_ok() {
                ws.cpu_cache.set_presented_chrome(chrome);
                ws.present_retry.on_presented();
            } else {
                ws.cpu_cache.invalidate();
            }
            presented_work_ns
        }
    }

    /// Latency self-introspection: window `wid`'s frame is now presented. If an
    /// output burst is pending in a session this window is SHOWING, return how
    /// long the oldest one waited from "content ready" to "presented"
    /// (output->present) — aterm's render-pipeline latency, the slice of
    /// input-to-photon software controls. swap(0) so the next burst's leading edge
    /// restarts the clock; `$ATERM_TRACE_LATENCY` keeps the stderr log, but the
    /// number is always returned for the `metrics` verb regardless.
    ///
    /// Attribution is PER-WINDOW (the touch-to-glass audit's artifact fix): only
    /// stamps from the presenting window's VISIBLE tab book latency — a TUI
    /// streaming in a background window (or a hidden tab here) used to stamp the
    /// old process-global and get booked against whichever window presented
    /// next, inflating `present_p99` with spans no human was watching. Hidden
    /// tabs' stamps are consumed and DISCARDED: their content is not waiting on
    /// this glass, and letting a stamp age across a later tab switch would book
    /// the whole hidden interval as render latency.
    fn present_latency_ns(&self, wid: WindowId) -> u64 {
        let Some(ws) = self.windows.get(&wid) else {
            return 0;
        };
        // Honesty bound (mirrors INPUT_SLICE_CAP_NS's rationale): a genuine
        // output→present pipeline wait is milliseconds; SECONDS means the stamp
        // aged through an interval nobody was watching — a hidden tab revealed
        // by a switch with no interim present, a miniaturized window, a
        // sleep/wake gap. Booking those would inflate max/p99 with exactly the
        // artifact the per-window attribution exists to kill.
        const PRESENT_LATENCY_CAP_NS: u64 = 5_000_000_000;
        let now = self.lat_epoch.elapsed().as_nanos() as u64;
        let mut dt_max = 0u64;
        for tab in ws.tab_set.tabs() {
            let visible = ws.tab_set.active_id() == Some(tab.id);
            for view in tab.root.leaves() {
                let Some(sid) = self
                    .view_store
                    .get(view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                else {
                    continue;
                };
                let Some(sess) = self.pool.get(sid) else {
                    continue;
                };
                if !visible && self.is_visible_session(sid) {
                    // SHARED (Cmd-Shift-O) session hidden HERE but on glass in
                    // another window's active tab: leave the stamp armed for
                    // THAT window's present to book — a swap here would
                    // silently destroy the showing window's measurement.
                    continue;
                }
                let stamp = sess.last_output_ns.swap(0, Ordering::Relaxed);
                if visible && stamp != 0 {
                    let dt = now.saturating_sub(stamp);
                    if dt < PRESENT_LATENCY_CAP_NS {
                        dt_max = dt_max.max(dt);
                    }
                }
            }
        }
        if dt_max != 0 && self.trace_latency {
            eprintln!(
                "aterm-latency output->present: {:.2} ms",
                dt_max as f64 / 1e6
            );
        }
        dt_max
    }

    /// Fallback template cell for a predicted glyph on a still-EMPTY row (a fresh
    /// line under `cat`/`read` with no prompt: `render_row` yields a 0-length row,
    /// so there is no last cell to clone). Built from the theme so the glyph stays
    /// legible. Shared by the single-pane and composed prediction paints.
    fn pred_blank_cell(&self) -> RenderCell {
        let rgb = |v: u32| {
            [
                ((v >> 16) & 0xff) as u8,
                ((v >> 8) & 0xff) as u8,
                (v & 0xff) as u8,
            ]
        };
        RenderCell {
            ch: ' ',
            fg: rgb(self.theme.fg),
            bg: rgb(self.theme.bg),
            wide: false,
            emoji_presentation: false,
            bold: false,
            italic: false,
            underline: aterm_core::terminal::UnderlineStyle::None,
            strikethrough: false,
            overline: false,
            underline_color: None,
        }
    }

    /// SPLIT PANES: compose the active tab's frame from EVERY visible pane and fill
    /// `input_scratch` at window size, ready for the SAME present path the
    /// single-pane redraw uses (CPU/GPU consume `input_scratch` unchanged — no
    /// renderer change). Returns `Some(focused_title)` when a present is needed, or
    /// `None` on the D-1 early-out (nothing visible changed across any pane).
    ///
    /// The combined early-out folds every visible pane's `damage_epoch` (so a
    /// background-pane write in this tab still repaints) plus the focused pane's
    /// blink/invert/cursor-override/selection state. On a repaint it lays out the
    /// panes, locks each in turn, refills `pane_scratch`, and blits its cells into
    /// `input_scratch` at the pane's offset; the FOCUSED pane's cursor is the only
    /// solid cursor (others draw none), and 1-cell dividers fill the gaps.
    #[allow(
        clippy::too_many_arguments,
        reason = "a window's full compose inputs (id/dims/invert/drag-hover/cursor-override/tab-strip); bundling them into a struct only relocates the argument list"
    )]
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
        // Read theme BEFORE borrowing `ws` (fill_divider_grid needs it after the
        // ws borrow is live). Layout + per-pane state come from window `wid`.
        let theme = self.theme;
        // The LUMEN aurora config + cell geometry must also be read before the `ws`
        // borrow (they map the focused cursor cell → grid-interior pixels). The
        // MOTION POLICY (W11) gates its amplitude exactly like the single-pane
        // path: Reduced (config / OS flag / unfocused window) ⇒ intensity 0 —
        // with the same `motion_focus` recording pin as the single-pane path.
        let raw_focused = self.windows.get(&wid)?.focused;
        let focused = self.motion_focus(wid, raw_focused);
        let cursor_body_allowed = self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::CursorBody);
        // Load shed folds in as the SOFT envelope (same seam as the single-pane
        // path in `tick_cursor_fx`): a latch flip fades the glow/trail instead
        // of wiping it. Accessibility/config cuts stay hard via `policy`.
        let mode = self.config.motion_mode();
        let policy = crate::motion::MotionPolicy::resolve(mode, self.system_reduce_motion, focused);
        let shed_env = if mode != crate::motion::MotionMode::Full
            && self.config.load_adaptive_motion_or_default()
        {
            self.shed_envelope(now)
        } else {
            1.0
        };
        let mut glow_cfg = self.glow_config();
        glow_cfg.intensity *= policy.amplitude(crate::motion::MotionEffect::CursorGlow) * shed_env;
        // Cadence-comet motion-trail config (the FOCUSED pane's cursor drives it, in
        // WINDOW coords — like the aurora). Reduced motion forces the whole comet off.
        let mut trail_cfg = self.trail_config();
        trail_cfg.enabled &=
            policy.animate(crate::motion::MotionEffect::CursorGlow) && shed_env > 0.0;
        let sound_gain = trail_sound_gain(
            raw_focused,
            self.config.trail_sounds_or_default()
                && self
                    .serious_mode_policy()
                    .allows(crate::motion::SeriousEffect::TerminalSound)
                // Resize repaint storms drain silently (RESIZE_SOUND_QUIET).
                && !self
                    .windows
                    .get(&wid)
                    .is_some_and(|ws| ws.resize_sound_quiet(std::time::Instant::now())),
            self.config.trail_sound_volume(),
        );
        let (glow_cw, glow_ch) = self.win_cell_size(wid);
        let grid_top = self.win_pad_top(wid) + self.win_head(wid);
        // Predictive-echo config, read before the borrows below — the FOCUSED pane
        // reconciles/paints exactly like the single-pane path. Its empty-row ghost
        // template is the per-terminal `blank` captured under the pane lock below.
        // Cached per config generation (see `predict_mode`), not re-parsed per frame.
        let pmode = self.predict_mode();
        let ws = self.windows.get(&wid)?;
        let tree = &ws.layouts[ws.tabs.active];
        let focus = tree.focus();
        let blink_phase = ws.blink_phase;
        let last_present = ws.last_present;
        let rects = tree.compute_layout(ws.rows, ws.cols);
        // Fold every visible pane's damage epoch into one key term (wrapping add is
        // fine — the early-out only needs the combination to CHANGE on any change).
        let mut damage_epoch: u64 = 0;
        let mut focus_selection =
            SelectionFingerprint::of(&aterm_core::selection::TextSelection::new());
        // The FOCUSED pane's cursor (pane sub-coords) drives both the repaint-key
        // cursor terms (so a pure move repaints in split panes too) and the
        // aurora. `focus_off` is its window-space origin, used to place the wake.
        let mut focus_cur_pos = (0u16, 0u16);
        let mut focus_vis = false;
        let mut focus_style = CursorStyle::default();
        let mut focus_off = (0u16, 0u16);
        // The focused pane's size in cells — with `focus_off`, the pane box the
        // effect streams are clipped to below (light must not cross dividers).
        let mut focus_dims = (0u16, 0u16);
        // Scrolled into history? The effect engines then see NO cursor (the
        // single-pane law: active-grid coords over scrollback rows are lies).
        let mut focus_scrolled = false;
        // The focused pane's live OSC-12 cursor colour: recolours the wake/comet
        // exactly like the single-pane `tick_cursor_fx` (layout parity).
        let mut focus_cursor_rgb: Option<[u8; 3]> = None;
        // The focused pane's ERASE-POOF probe `(row, caret)` in WINDOW coords
        // (`None` ⇒ scrolled back / history shifted — the engine keeps its
        // previous probe). Chars ride `ws.poof_row_buf`, window-column aligned.
        let mut focus_probe: Option<(u16, u16)> = None;
        // PHOSPHOR rain, compose path (split-pane audit): the FOCUSED pane
        // rains — the aurora/comet law ("effects follow focus, clipped to the
        // pane") applied to the ambient effect. These locals snapshot the
        // focused pane's rain inputs under its pass-1 lock, the single-pane
        // LOCK A capture's split twin; the engine block below the loop
        // consumes them unlocked.
        let mut focus_epoch = 0u64;
        let mut focus_content_seq = 0u64;
        let mut focus_cmd_done: Option<(u64, i32)> = None;
        let mut focus_shell_edge = false;
        let mut focus_alt = false;
        let mut focus_d_off = 0usize;
        let mut focus_sel_clone = aterm_core::selection::TextSelection::new();
        let mut pane_default_bg_u32 = 0u32;
        // VI-1 (compose, split-pane audit): the focused pane's on-viewport vi
        // (copy-mode) cursor in PANE-LOCAL coords — v1 wired vi only on the
        // single-pane path, so in a split the vi cursor neither painted nor
        // repainted (navigation was blind). Steers the repaint key, the
        // painted cursor override, and the rain cursor band, exactly like the
        // single-pane `vi_screen`.
        let mut focus_vi_screen: Option<(usize, usize)> = None;
        let mut rain_refresh = false;
        // Suspended until the focused pane proves live: a torn-down focused
        // session (impossible mid-redraw, but the loop tolerates it) must
        // wind the engine down, never tick it on stale inputs.
        let mut rain_suspend = true;
        // Clone each pane's `term` handle OUT of the `&self`/`ws` borrow so the
        // mutating composition loop below can write this window's `input_scratch`/
        // `pane_scratch` freely. Cheap: an `Arc` clone per visible pane. Panes whose
        // session was just torn down (impossible mid-redraw) are skipped.
        let panes: Vec<(pane::PaneRect, Arc<Mutex<Terminal>>)> = rects
            .iter()
            .filter_map(|r| self.pool.get(r.session).map(|s| (*r, s.term.clone())))
            .collect();
        // Effective rain state for the FOCUSED pane's session (its runtime
        // override, else the config bit — per-session semantics: in a split
        // where only one pane enabled rain, rain follows that pane's focus).
        // ENGAGED while on OR while this window still holds an engine that
        // must wind down through the suspended/drain path (the single-pane
        // `rain_cfg` law verbatim; the D-1 zero-cost pin holds — no engine is
        // ever constructed for a never-enabled session).
        let rain_session_on = self.session_rain_enabled(focus);
        let load_shed = self.load_shed_active();
        let rain_cfg = self.rain.filter(|_| {
            rain_session_on
                || self
                    .windows
                    .get(&wid)
                    .is_some_and(|w| w.matrix_rain.is_some())
        });
        for (r, term) in &panes {
            let mut term = term_lock(term);
            // Per-pane damage is window-scoped via the per-window `last_present`
            // (read above); the take_damage below is per-session, but the early-out
            // compares against THIS window's key, so a co-viewer window is not
            // starved (it keeps its own last_present and re-folds the same epochs).
            damage_epoch = damage_epoch.wrapping_add(term.damage_epoch());
            if r.session == focus {
                focus_selection = SelectionFingerprint::of(term.text_selection());
                let cp = term.cursor();
                focus_cur_pos = (cp.row, cp.col);
                focus_vis = term.cursor_visible();
                focus_style = term.cursor_style();
                focus_off = (r.row_off, r.col_off);
                focus_dims = (r.rows, r.cols);
                focus_cursor_rgb = term.cursor_color().map(|c| [c.r, c.g, c.b]);
                // ERASE-POOF probe, split panes: the single-pane LOCK A capture
                // with row/caret reported in WINDOW coords (+ row_off/col_off,
                // matching `win_cur` below) so the engine and geom agree. Only
                // the engine read (`row_cols_into`) runs under the lock; the
                // window-column blank-pad shift is host-only work, applied at
                // the unlocked feed site below (lock diet). Same guards: live
                // bottom + unmoved scrollback fence.
                if let Some(ws) = self.windows.get_mut(&wid) {
                    let d_off = term.grid().display_offset();
                    focus_scrolled = d_off != 0;
                    let sb = term.grid().scrollback_lines();
                    let pane_alt = term.is_alternate_screen();
                    // REPAINT-BLINK edge + context feed — the single-pane LOCK A
                    // detector's twin (same lock, same engines, `now` clock).
                    let blink_epoch = term.repaint_blink_epoch();
                    if ws.blink_reseed {
                        // Tab/pane switch: adopt the NEW terminal's epoch silently —
                        // a cross-terminal mismatch is not a repaint (see sync_window).
                        ws.blink_reseed = false;
                        ws.blink_epoch_seen = blink_epoch;
                    } else if blink_epoch != ws.blink_epoch_seen {
                        ws.blink_epoch_seen = blink_epoch;
                        ws.last_blink_at = Some(now);
                        ws.cursor_glow.note_repaint_blink(now);
                        ws.cursor_trail.note_repaint_blink(now);
                    }
                    ws.cursor_glow.note_context(pane_alt);
                    ws.cursor_trail.note_context(pane_alt);
                    let blink_recent = ws
                        .last_blink_at
                        .is_some_and(|t| now.saturating_duration_since(t) <= BLINK_RECENT_MAX);
                    let pane_probe_ok = !pane_alt || blink_recent;
                    focus_probe = if pane_probe_ok && d_off == 0 && ws.poof_scrollback == Some(sb) {
                        let _fill = term.row_cols_into(cp.row as usize, &mut ws.poof_row_buf);
                        Some((cp.row + r.row_off, cp.col + r.col_off))
                    } else {
                        None
                    };
                    // Scroll translation + fenced-frame probe drop — the
                    // single-pane path's twins (see the LOCK A capture there).
                    let scrolled = ws.poof_scrollback.map_or(0, |p| sb.saturating_sub(p));
                    if scrolled > 0 {
                        let d = (scrolled.min(u16::MAX as usize) as u16).min(r.rows);
                        ws.cursor_glow.note_scroll(d);
                        ws.cursor_trail.note_scroll(d);
                        ws.cursor_glow.drop_row_probe();
                    }
                    ws.poof_scrollback = Some(sb);
                    // VI-1: the pane-local vi cursor (see the declaration) —
                    // `None` when vi is off or scrolled off-viewport.
                    focus_vi_screen = term
                        .vi_is_active()
                        .then(|| term.vi_cursor_point())
                        .and_then(|p| {
                            vi_screen_row(p.line, d_off as i32, usize::from(r.rows)).map(|row| {
                                (
                                    row,
                                    (p.col as usize).min(usize::from(r.cols).saturating_sub(1)),
                                )
                            })
                        });
                    // PHOSPHOR rain input snapshot — the single-pane LOCK A
                    // capture's split twin, all under this SAME pane lock.
                    focus_epoch = term.damage_epoch();
                    focus_content_seq = term.content_seq();
                    focus_cmd_done = term
                        .last_completed_command()
                        .and_then(|m| Some((term.completed_command_seq(), m.exit_code?)));
                    focus_shell_edge = rain_shell_execute_rising_edge(
                        &mut ws.rain_shell_executing,
                        r.session,
                        term.shell_state() == aterm_core::terminal::ShellState::Executing,
                    );
                    focus_alt = pane_alt;
                    focus_d_off = d_off;
                    focus_sel_clone = term.text_selection().clone();
                    // The PANE's live default bg (DECSCNM-folded) — the rain
                    // occupancy eligibility ground, exactly like single-pane.
                    let dbg = if term.modes().reverse_video() {
                        term.default_foreground()
                    } else {
                        term.default_background()
                    };
                    pane_default_bg_u32 = aterm_render::rgb_to_u32([dbg.r, dbg.g, dbg.b]);
                    let rain_suppress_alt =
                        pane_alt && rain_cfg.is_some_and(|c| c.suppress_in_alt_screen);
                    rain_suspend = rain_suppress_alt || load_shed || !rain_session_on;
                    // Hidden-cursor damage band (design §6), folded BEFORE any
                    // take_damage — pane-LOCAL rows, matching the pane-local
                    // geometry the engine ticks at on this path.
                    if rain_cfg.is_some() && !rain_suspend {
                        update_rain_hidden_band(
                            &mut ws.rain_hidden_band,
                            term.grid().damage(),
                            usize::from(r.rows),
                        );
                    }
                    // Tier-A refresh gate + extraction, the LOCK A etiquette:
                    // extract AND consume together under one lock. Pass 2
                    // re-extracts for the blit; a PTY write racing between the
                    // passes leaves occupancy one frame stale at worst (the
                    // epoch key term forces the corrective present).
                    rain_refresh = rain_refresh_needed(
                        rain_cfg.is_some(),
                        rain_suspend,
                        false,
                        d_off,
                        ws.matrix_rain.as_deref(),
                        focus_epoch,
                    );
                    if rain_refresh {
                        term.cell_frame_into(
                            &mut ws.pane_scratch,
                            usize::from(r.rows),
                            usize::from(r.cols),
                        );
                        term.take_damage();
                    }
                }
            }
        }
        // PHOSPHOR rain engine tick, compose path (split-pane audit): the
        // single-pane rain block's split twin, ticking the SAME retained
        // per-window engine at the FOCUSED pane's geometry off the pass-1
        // snapshot. Emission is pane-LOCAL; `translate_rain_into_pane` shifts
        // it into window-content coords and clips it to the pane's interior
        // box (rain never crosses a divider). The fingerprint joins the
        // compose RepaintKey — mixed with the pane origin, so the SAME field
        // at a MOVED pane (divider drag) still presents — and is EXACTLY 0
        // when off/suspended/drained (byte-identical to the pre-rain compose).
        let rain_fp = if let Some(cfg) = rain_cfg {
            let ws = self.windows.get_mut(&wid)?;
            if rain_suspend {
                // Alt-suppression / load-shed / session-off / torn-down focus:
                // the suspended wind-down, exactly like single-pane — weather
                // starves, the drain completes, `is_active` self-disarms.
                if let Some(engine) = ws.matrix_rain.as_mut() {
                    engine.tick_suspended(now);
                }
                // Keep the completion latch BASELINED while suspended (the
                // single-pane law: suspension-era completions absorb silently).
                ws.rain_last_cmd = Some((focus, focus_cmd_done.map_or(0, |(e, _)| e)));
                ws.rain_scratch.clear();
                ws.rain_add_scratch.clear();
                0
            } else {
                // Lazy build on the first enabled tick — toggling rain ON
                // inside a split works exactly like single-pane (zero-cost
                // pin: a never-enabled session never constructs an engine).
                let engine = ws.matrix_rain.get_or_insert_with(|| {
                    Box::new(crate::matrix_rain::MatrixRain::new(
                        crate::rain_config_for_window(cfg, wid),
                    ))
                });
                engine.set_reduced_motion(!policy.animate(crate::motion::MotionEffect::MatrixRain));
                engine.set_visibility(if raw_focused {
                    crate::matrix_rain::RainVisibility::Focused
                } else {
                    crate::matrix_rain::RainVisibility::VisibleUnfocused
                });
                engine.note_activity(focus_content_seq);
                if focus_shell_edge {
                    engine.note_signal(crate::matrix_rain::RainSignal::Execute as u32, 4);
                }
                // EXIT STATUS → weather, keyed (session, seq) — the
                // single-pane block verbatim (a pane-focus switch
                // re-baselines; only a same-session new completion notes).
                {
                    let seq = focus_cmd_done.map_or(0, |(e, _)| e);
                    let key = (focus, seq);
                    if ws.rain_last_cmd != Some(key) {
                        let same_session = ws.rain_last_cmd.is_some_and(|(sid, _)| sid == focus);
                        ws.rain_last_cmd = Some(key);
                        if same_session && let Some((_, code)) = focus_cmd_done {
                            engine.note_exit_status(code != 0);
                        }
                    }
                }
                if rain_refresh && engine.can_emit() {
                    // Pass 1 extracted the focused pane into `pane_scratch`
                    // under its lock at this same epoch; scan those cells
                    // here on host state, no lock held.
                    let needs_grid_rescan = engine.needs_rescan(focus_epoch);
                    let needs_material_sample = engine.needs_material_sample()
                        || (needs_grid_rescan && cfg.output_material);
                    if needs_grid_rescan {
                        engine.rescan_from_cells(
                            &ws.pane_scratch.cells,
                            &ws.pane_scratch.line_sizes,
                            &ws.pane_scratch.images,
                            usize::from(focus_dims.0),
                            usize::from(focus_dims.1),
                            pane_default_bg_u32,
                            focus_epoch,
                        );
                    }
                    if needs_material_sample {
                        engine.sample_material(
                            &ws.pane_scratch.cells,
                            usize::from(focus_dims.0),
                            (focus_vis && !focus_scrolled).then_some(focus_cur_pos),
                            &ws.rain_hidden_band,
                        );
                    }
                }
                let effect_geom = crate::word_decorations::EffectGeom {
                    cell_w: glow_cw as u16,
                    cell_h: glow_ch as u16,
                    rows: focus_dims.0,
                    cols: focus_dims.1,
                };
                // Tier-B live inputs, all pane-LOCAL (cursor, hidden band,
                // selection, scroll/alt gates) — the pane-local twin of the
                // single-pane RainTickInput. In vi copy-mode the band follows
                // the PAINTED vi cursor (the one the user steers), exactly
                // like single-pane.
                let input = crate::matrix_rain::RainTickInput {
                    cursor: focus_vi_screen
                        .map(|(r, c)| (r as u16, c as u16))
                        .or((focus_vis && !focus_scrolled).then_some(focus_cur_pos)),
                    hidden_band: &ws.rain_hidden_band,
                    sel: Some(crate::word_decorations::SelView {
                        sel: &focus_sel_clone,
                        display_offset: focus_d_off as i32,
                    }),
                    display_offset: focus_d_off as i32,
                    is_alt_screen: focus_alt,
                };
                let fp = engine.tick(
                    now,
                    effect_geom,
                    &input,
                    &mut ws.rain_scratch,
                    &mut ws.rain_add_scratch,
                );
                translate_rain_into_pane(
                    &mut ws.rain_scratch,
                    &mut ws.rain_add_scratch,
                    focus_off.0,
                    focus_off.1,
                    focus_dims.0,
                    focus_dims.1,
                    glow_cw as u32,
                    glow_ch as u32,
                );
                if fp == 0 {
                    0
                } else {
                    // Fold the pane origin so a divider drag with an
                    // identical pane-local field still presents. `fp != 0`
                    // stays nonzero (rotate+xor of a nonzero fp with a
                    // <2^32 term cannot produce 0 only probabilistically —
                    // so OR in a sentinel bit to make "live field" exact).
                    (fp.rotate_left(17) ^ (u64::from(focus_off.0) << 16 | u64::from(focus_off.1)))
                        | 1
                }
            }
        } else {
            // Fully off with no lingering engine: nothing runs, nothing is
            // constructed (the D-1 zero-cost pin, compose edition).
            0
        };
        // Fold the LIVE theme ground — the single-pane `tick_cursor_fx` fold's
        // compose twin. glow_config() hardcodes `dark_theme: true` (the resolver
        // has no per-window background), and without this fold a light-theme
        // split ran fire/vapor in dark-theme ADDITIVE mode: additive light
        // cannot darken a white ground, so smoke/steam vanished and the flame
        // body lost its light-theme ink mode — a layout-dependent downgrade.
        // The compose path presents no per-window OSC-11 bg (it resets
        // `default_bg` to UNSET below), so the configured theme ground is the
        // honest input here.
        glow_cfg.dark_theme = aterm_render::theme_is_dark(theme.bg);
        // Cursor WAKE follows the FOCUSED pane's live OSC-12 colour — the
        // single-pane recolour block's compose twin (same rules: an explicit
        // config colour wins; the LASER keeps electric yellow).
        if self.config.cursor_trail_color_u32().is_none()
            && !matches!(glow_cfg.style, crate::cursor_glow::GlowStyle::Laser)
            && let Some(rgb) = focus_cursor_rgb
        {
            let live = aterm_render::rgb_to_u32(rgb);
            glow_cfg.color = live;
            trail_cfg.color = live;
            if self.config.cursor_trail_accent_u32().is_none() {
                let m = |sh: u32| ((((live >> sh) & 0xff) as f32) * 1.5).min(255.0) as u32;
                glow_cfg.accent = (m(16) << 16) | (m(8) << 8) | m(0);
            }
        }
        // Advance the LUMEN aurora off the FOCUSED pane's cursor in WINDOW coords
        // (its pixel quads are window-absolute — no later offset; only the damage
        // row tags shift with the strip, like the single-pane path).
        let (glow_fp, trail_fp, forge_fill) = {
            // Shared window-space derivation — MUST agree with `tick_cursor_fx`.
            let (origin_x, origin_y, win_w, win_h, fx_head) =
                self.effects_origin_win(wid, rows, cols, glow_ch);
            let ws = self.windows.get_mut(&wid)?;
            // M1: the scroll pill is single-pane only (a per-window bar is
            // ambiguous across split viewports); mark it un-painted so the
            // fade servicing never waits on an erase frame here.
            ws.pill_shown = false;
            // M2: the stream fade is single-pane only too (the age map mirrors
            // one window-content view; per-pane maps are a follow-up). The
            // composed path never tints, so mark it un-shown — split frames
            // stay byte-identical to the pre-feature compose.
            ws.fade_shown = false;
            // Scrolled into history ⇒ NO cursor for the effect engines (the
            // single-pane law: the active-grid cursor over scrollback rows
            // would spawn light on unrelated history lines).
            let win_cur = (focus_vis && !focus_scrolled)
                .then_some((focus_cur_pos.0 + focus_off.0, focus_cur_pos.1 + focus_off.1));
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
            // BAR-CURSOR ANCHOR, split-pane path — same rule as the single-pane
            // tick: a thin bar attaches the streak at its own x, not mid-cell.
            glow_cfg.head_dx = if matches!(
                focus_style,
                aterm_core::terminal::CursorStyle::BlinkingBar
                    | aterm_core::terminal::CursorStyle::SteadyBar
            ) {
                0.08
            } else {
                0.5
            };
            // ERASE-POOF probe feed, immediately before the tick — the compose
            // path's mirror of `tick_cursor_fx`'s feed (same engine, same
            // window-coord contract).
            if let Some((prow, pcaret)) = focus_probe {
                // Shift onto window columns with a leading blank pad, so the
                // diff's vanished-span columns emit at the pane's true x
                // (capacity settles after one frame; no steady-state alloc).
                // Deferred here from the pane loop: host-only O(cols) memmove
                // that must not extend the Terminal hold (lock diet). Nothing
                // touches `poof_row_buf` between the locked fill and this feed.
                if focus_off.1 > 0 {
                    let off = usize::from(focus_off.1);
                    let len = ws.poof_row_buf.len();
                    ws.poof_row_buf.resize(len + off, ' ');
                    ws.poof_row_buf.rotate_right(off);
                }
                ws.cursor_glow
                    .observe_row(prow, pcaret, &ws.poof_row_buf, now);
                // STAR-LANDING NEIGHBORS: deliberately NOT wired on the
                // compose path (the pane loop would need two more locked
                // captures + the same column shift). Unprobed neighbors mean
                // the displaced nyan stars take the safe IN-CELL fallback in
                // split panes — never a guess over an adjacent row's glyphs.
            }
            let glow_fp =
                ws.cursor_glow
                    .tick(win_cur, now, &glow_cfg, glow_geom, &mut ws.glow_scratch);
            // Tone resolution — the single-pane seam verbatim (knob off ⇒
            // the neutral identity, stale cache included).
            let tone = if self.config.tone_melody_or_default() {
                ws.tone_tracker.current()
            } else {
                aterm_effects::tone::Tone::Technical
            };
            drain_trail_sound_cues(
                &mut ws.cursor_glow,
                glow_cfg.style,
                cols.min(u16::MAX as usize) as u16,
                TrailSoundPolicy {
                    voice: self.config.trail_sound_voice(),
                    gain: sound_gain,
                    tone,
                    bed: self.config.trail_sound_bed_or_default(),
                },
                |event| self.trail_audio.push(event),
            );
            // The FORGE cursor fill (fire style) — the single-pane seam's compose
            // twin: split panes must keep the warm-metal cursor the owner shipped
            // (v0.31 "the cursor color starts green"), not revert to the raw
            // theme fill while flames still burn around it. Its colour is folded
            // into `glow_fp` by the engine, so a cooling cursor keeps presenting.
            let forge_fill = forge_cursor_fill(cursor_body_allowed, &glow_cfg, || {
                ws.cursor_glow.forge_fill()
            });
            // Cadence-comet trail off the SAME focused-pane cursor (window coords),
            // ignited by the window's typing cadence — the aurora crown's directional
            // body in split panes too. Idle → zero cells → `trail_fp == 0`.
            crate::cursor_trail::ignite(
                &mut trail_cfg,
                ws.typing_cadence.intensity(now),
                ws.typing_cadence.warmth(now),
            );
            let trail_fp = ws
                .cursor_trail
                .tick(win_cur, now, &trail_cfg, &mut ws.trail_scratch);
            (glow_fp, trail_fp, forge_fill)
        };
        // Keep the IME candidate/compose window anchored at the caret (only re-reports
        // to winit when the cursor cell actually moves).
        self.report_ime_cursor_area(wid, focus_cur_pos, focus_off, focus_vis);
        let settings_fp = self.windows.get(&wid).map_or(0, |ws| ws.overlay_fp());
        // Find bar shows in split panes too (its current-match highlight is suppressed,
        // but the bar row + readout still paint) — mirror the single-pane term so an edit
        // that does not move the highlight repaints. `0` when not searching.
        let find_fp = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.search.as_ref())
            .map_or(0, |s| s.fingerprint());
        let relaunch_fp = self.relaunch.as_ref().map_or(0, |n| n.fingerprint());
        let badge_fp = crate::build_badge::fingerprint(self.config.show_build_badge_or_default());
        let notice_fp = self
            .notice
            .as_ref()
            .map_or(0, |n| n.fingerprint(std::time::Instant::now()));
        let level_up_fp = self.level_up.as_ref().map_or(0, |l| l.fingerprint(now));
        let key = RepaintKey {
            damage_epoch,
            grid_top,
            blink_phase,
            invert,
            drag_hover,
            cursor_override,
            // VI-1: while copy-mode steers, ITS cursor is the key's cursor —
            // a vi motion damages no cell, so these terms are what force the
            // post-motion repaint in a split (the single-pane law).
            cursor_row: focus_vi_screen.map_or(focus_cur_pos.0 as usize, |(r, _)| r),
            cursor_col: focus_vi_screen.map_or(focus_cur_pos.1 as usize, |(_, c)| c),
            cursor_visible: focus_vis || focus_vi_screen.is_some(),
            cursor_style: focus_style,
            glow_fp,
            trail_fp,
            // Split-pane compose does not run the sparkle-words scan (single-pane only).
            deco_fp: 0,
            // PHOSPHOR rain: the FOCUSED pane's field fingerprint (pane-origin
            // mixed — see the engine block above); 0 when off/suspended/drained,
            // byte-identical to the pre-rain compose.
            rain_fp,
            selection: focus_selection,
            tab_strip,
            settings_fp,
            find_fp,
            // Split-pane compose paints no scroll pill (single-pane only).
            pill_fp: 0,
            // M1b sub-row scroll is single-pane only: a whole-composite band shift
            // would slide every pane together, so splits stay whole-row (frac 0).
            scroll_frac_px: 0,
            relaunch_fp,
            badge_fp,
            notice_fp,
            level_up_fp,
            // Same appearance term as the single-pane key (see `RepaintKey`).
            system_dark: repaint_system_dark(self.os_appearance),
        };
        // Displayed (or just-erased) predictions bypass the skip exactly like the
        // single-pane path: a ghost paints/erases without perturbing the RepaintKey.
        let recovery_redraw_outstanding = self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.present_retry.recovery_redraw_outstanding);
        if !should_repaint_or_recover(last_present, key, recovery_redraw_outstanding)
            && self
                .windows
                .get(&wid)
                .is_none_or(|ws| !ws.predictor.is_displaying(now) && !ws.pred_shown)
        {
            return None;
        }
        // FOCUSED-PANE clip box for the cursor-effect streams (window-absolute
        // px): the pane's cell rect, extended up into the head band when the
        // pane touches the grid top (the EFFECTS BOX law applied per-pane).
        // Degenerate focus dims (pane vanished mid-redraw) ⇒ `None` ⇒ no clip.
        let pane_clip = (focus_dims.0 > 0 && focus_dims.1 > 0).then(|| {
            let (origin_x, origin_y, _w, _h, fx_head) =
                self.effects_origin_win(wid, rows, cols, glow_ch);
            let (cw, chp) = (glow_cw as u32, glow_ch as u32);
            let x0 = u32::from(origin_x) + u32::from(focus_off.1) * cw;
            let mut y0 = u32::from(origin_y) + u32::from(focus_off.0) * chp;
            if focus_off.0 == 0 {
                // Top pane: its slice of the chrome head band is effect-lawful.
                y0 = y0.saturating_sub(u32::from(fx_head));
            }
            let x1 = u32::from(origin_x) + (u32::from(focus_off.1) + u32::from(focus_dims.1)) * cw;
            let y1 = u32::from(origin_y) + (u32::from(focus_off.0) + u32::from(focus_dims.0)) * chp;
            (
                x0.min(u32::from(u16::MAX)) as u16,
                y0.min(u32::from(u16::MAX)) as u16,
                x1.min(u32::from(u16::MAX)) as u16,
                y1.min(u32::from(u16::MAX)) as u16,
            )
        });
        // Commit to presenting. Re-borrow `ws` mutably now (the immutable borrow
        // above is dropped). Fill the composite: window-size grid of divider cells
        // first, then overlay each pane.
        let ws = self.windows.get_mut(&wid)?;
        fill_divider_grid(&mut ws.input_scratch, rows, cols, theme);
        // FOCUSED-PANE fx clip for the PRESENT-TIME GPU post-fx (split-pane
        // audit): the bloom composite and the heat-shimmer refraction operate
        // on the finished frame AFTER the host-side quad clip above, so their
        // blur/displacement could still paint (and sample) across a divider.
        // Hand them the same pane box; the renderer intersects its pass
        // regions with it. `None` (degenerate focus) = no clip, matching the
        // quad-clip fallback.
        ws.input_scratch.fx_clip = pane_clip;
        // The composed (multi-pane) path doesn't resolve a single per-window live
        // OSC 11 background (which pane's would win is ambiguous), so reset to
        // UNSET: the renderer falls back to the configured theme — byte-identical
        // to the pre-OSC-11 split behaviour, and never a STALE value carried over
        // from a prior single-pane frame in this window's reused `input_scratch`.
        // The CURSOR colour is NOT ambiguous — only the FOCUSED pane draws a
        // solid cursor — so its live OSC-12 value rides exactly like the
        // single-pane path (and matches the wake recolour above); `None` maps to
        // UNSET → the configured theme, byte-identical to before.
        ws.input_scratch.default_bg = aterm_core::render::COLOR_UNSET;
        ws.input_scratch.cursor_color =
            focus_cursor_rgb.map_or(aterm_core::render::COLOR_UNSET, aterm_render::rgb_to_u32);
        // The compose path has no single `cell_frame_into` into `input_scratch`.
        // Start with neutral metadata and replace it from the focused pane's actual
        // extracted snapshot below (not the earlier repaint-key probe, which can race
        // with PTY output before extraction).
        ws.input_scratch.base_y = 0;
        ws.input_scratch.absolute_row_revision = 0;
        let mut focus_title: Arc<str> = Arc::from("");
        let mut painted_pred = false;
        let mut focus_pred_last: Option<(u16, u16)> = None;
        let mut focus_sub_cols = cols;
        for (r, term) in &panes {
            let (sub_rows, sub_cols) = (r.rows as usize, r.cols as usize);
            // TERM-LOCK DIET (the ghostty #13227 shape): the per-pane hold covers
            // ONLY engine work — the grid extract, the damage consume, and the
            // focused pane's reconcile (its closure reads `term.render_row`).
            // The overlay flush + ghost paint mutate host state alone (predictor
            // + `pane_scratch`), so they run after the guard drops and the PTY
            // parse thread's per-batch lock acquisition never queues behind them.
            // `pred_paint`: None = nothing pending; Some(false) = expiry flush
            // only (scrolled into history); Some(true) = paint survivors.
            let (title, pred_paint, blank) = {
                let mut term = term_lock(term);
                term.cell_frame_into(&mut ws.pane_scratch, sub_rows, sub_cols);
                let blank = terminal_blank_cell(&term);
                if r.session == focus {
                    // Stamp the focused pane's absolute-row metadata from the SAME
                    // `cell_frame_into` snapshot as the cells being composited. This
                    // is the split-pane twin of the single-pane frame contract.
                    ws.input_scratch.base_y = ws.pane_scratch.base_y;
                    ws.input_scratch.absolute_row_revision = ws.pane_scratch.absolute_row_revision;
                }
                term.take_damage();
                // Predictive local echo for the FOCUSED pane (mirrors the
                // single-pane path): reconcile pending guesses against THIS pane's
                // grid, then ghost-paint survivors into the pane-local scratch —
                // BEFORE the blit offsets it into window space, so no coordinate
                // math and no bleed past the pane's sub-rect (the ghost painter
                // clips to `sub_cols`; the blit does not).
                let pred_paint = if r.session == focus && pmode == crate::predict::PredictMode::Off
                {
                    // Config flipped to OFF with guesses pending: flush them, or
                    // the stranded past deadline spins the event loop (same fix
                    // as the single-pane path — see there).
                    ws.predictor.reset();
                    None
                } else if r.session == focus && (!ws.predictor.idle() || ws.pred_shown) {
                    // Idle guard — the single-pane path's twin: an idle
                    // predictor skips the reconcile + overlay + ghost paint
                    // (and their per-frame `to_vec`) entirely.
                    ws.predictor.set_mode(pmode);
                    if term.grid().display_offset() != 0 {
                        // Scrolled into history: never paint over the scrollback
                        // view, but still run the expiry flush (see the
                        // single-pane path — without it a stale deadline spins).
                        Some(false)
                    } else {
                        let cp = term.cursor();
                        // No-echo gate (alt screen OR app-owned Kitty composer),
                        // identical to the single-pane reconcile above: a mode flip
                        // flushes the focused pane's in-flight guesses. Read-only
                        // projection.
                        let no_echo =
                            term.is_alternate_screen() || term.kitty_suppresses_predictive_echo();
                        ws.predictor
                            .reconcile(Some((cp.row, cp.col)), no_echo, now, |rr, cc| {
                                term.render_row(rr as usize)
                                    .get(cc as usize)
                                    .map(|cell| cell.ch)
                                    .filter(|ch| *ch != ' ')
                            });
                        Some(true)
                    }
                } else {
                    None
                };
                (term.title_arc(), pred_paint, blank)
            };
            // ---- Pane guard dropped: host-state-only prediction work. The
            // ghosts paint into the pane-local snapshot just extracted, so a
            // PTY write landing after the drop cannot skew them (the snapshot
            // is immutable host memory until the next extract). ----
            match pred_paint {
                Some(false) => {
                    let _ = ws.predictor.overlay(now);
                }
                Some(true) => {
                    let preds = ws.predictor.overlay(now).to_vec();
                    if paint_prediction_ghosts(&mut ws.pane_scratch, &preds, sub_cols, blank) {
                        painted_pred = true;
                        focus_pred_last = preds.last().map(|p| (p.row, p.col));
                        focus_sub_cols = sub_cols;
                    }
                }
                None => {}
            }
            // The cursor (window coords) is drawn SOLID only in the focused
            // pane; other panes contribute none. Pure `pane_scratch` reads —
            // no lock needed.
            let cursor = (r.session == focus && ws.pane_scratch.cursor_visible).then_some((
                ws.pane_scratch.cursor_row,
                ws.pane_scratch.cursor_col,
                ws.pane_scratch.cursor_style,
            ));
            // `pane_scratch` and `input_scratch` are disjoint fields of `ws`.
            blit_pane_into(
                &mut ws.input_scratch,
                &ws.pane_scratch,
                r.row_off as usize,
                r.col_off as usize,
                blank,
            );
            if r.session == focus {
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
            }
        }
        // The LUMEN aurora was already produced in WINDOW coords (clamped to the
        // window grid), so copy it straight in; the tab-strip splice shifts it after.
        ws.input_scratch
            .cursor_glow_add
            .clone_from(&ws.glow_scratch);
        ws.input_scratch.glow_halo.clear();
        ws.input_scratch
            .glow_halo
            .extend_from_slice(ws.cursor_glow.halos());
        ws.input_scratch.fire_patch.clear();
        ws.input_scratch
            .fire_patch
            .extend_from_slice(ws.cursor_glow.patches());
        ws.input_scratch.glow_under.clear();
        ws.input_scratch
            .glow_under
            .extend_from_slice(ws.cursor_glow.under_quads());
        ws.input_scratch.char_fg.clear();
        ws.input_scratch
            .char_fg
            .extend_from_slice(ws.cursor_glow.charred());
        ws.input_scratch.fire_halo.clear();
        ws.input_scratch
            .fire_halo
            .extend_from_slice(ws.cursor_glow.halo_cells());
        // The rainbow cursor is a single-pane treatment (a split frame never
        // inherits a stale rainbow fill), but the fire FORGE fill composes: the
        // fire style's warm-metal cursor is a shipped product law (v0.31), and
        // dropping it here left splits with full flames around a theme-green
        // block. `None` for every other style while focused; a real inactive
        // window instead uses the same neutral white hollow outline as the
        // single-pane path.
        ws.input_scratch.cursor_fill_override = window_cursor_fill(cursor_override, forge_fill);
        // The cadence-comet trail was likewise produced in WINDOW coords off the
        // focused pane's cursor; copy it + its heated colour in (empty when idle / not
        // the comet style → byte-identical to no trail). `fill_divider_grid` cleared
        // any stale single-pane trail, so this is the sole compose producer.
        ws.input_scratch.cursor_trail.clone_from(&ws.trail_scratch);
        ws.input_scratch.cursor_trail_color = trail_cfg.color;
        // PHOSPHOR rain (compose): install the FOCUSED pane's emission — the
        // engine block above already translated it into window-content coords
        // and clipped it to the pane's interior box. `fill_divider_grid`
        // cleared the channels, so this is the sole compose producer; the
        // versioned atlas Arc rides ONLY when quads do (the free_atlas idiom
        // — a rain-free split frame is byte-identical to the pre-rain input).
        // The tab-strip splice below shifts row + pixel y with the grid,
        // exactly like the single-pane path.
        //
        // ACCEPTED one-frame race (post-merge re-audit): these quads were
        // emitted against the PASS-1 occupancy snapshot, while the cells
        // beneath them come from the PASS-2 re-extraction — a PTY write
        // landing between the passes can leave rain over a just-occupied
        // cell for ONE frame. Deliberately NOT gated on snapshot-seq
        // equality: under heavy output the passes race often, so a gate
        // would flicker the whole field off at exactly the moments rain is
        // most alive — worse than a one-frame overlap the semantic-clearance
        // margin already softens. The advanced damage epoch forces the
        // corrective present immediately after.
        ws.input_scratch.rain_quads.clone_from(&ws.rain_scratch);
        ws.input_scratch.rain_add.clone_from(&ws.rain_add_scratch);
        ws.input_scratch.rain_atlas = if ws.rain_scratch.is_empty() {
            None
        } else {
            ws.matrix_rain.as_mut().and_then(|e| e.rain_atlas())
        };
        // CURSOR-EFFECT light must not cross pane dividers (audit: a cursor at
        // a pane edge washed its crown/beam over the divider and the neighbour
        // pane's text; the GPU bloom then blurred it further across). Clip every
        // effect stream to the focused pane's box before the renderer sees it —
        // BOTH backends consume these same streams, so parity holds by
        // construction. Flat quads truncate; the radial-halo / fire-field
        // pixels are pure functions of ABSOLUTE window coordinates + per-quad
        // params (centre/root ride untouched), so a clipped quad renders
        // byte-identical pixels over the surviving area (the FirePatch
        // continuity law — no seams). Cell-anchored streams filter by the
        // pane's cell rect. O(live light), like every per-frame effect cost.
        if let Some((cx0, cy0, cx1, cy1)) = pane_clip {
            let clip = move |x: u16, y: u16, w: u16, h: u16| -> Option<(u16, u16, u16, u16)> {
                let nx0 = x.max(cx0);
                let ny0 = y.max(cy0);
                let nx1 = x.saturating_add(w).min(cx1);
                let ny1 = y.saturating_add(h).min(cy1);
                // LAZY `then`, not `then_some`: a quad wholly outside the box
                // yields `nx1 < nx0`, and `then_some`'s eagerly-evaluated
                // argument underflowed there (debug-build panic on the first
                // fully-clipped quad; discarded wrap in release).
                (nx1 > nx0 && ny1 > ny0).then(|| (nx0, ny0, nx1 - nx0, ny1 - ny0))
            };
            ws.input_scratch
                .cursor_glow_add
                .retain_mut(|q| match clip(q.x, q.y, q.w, q.h) {
                    Some((x, y, w, h)) => {
                        (q.x, q.y, q.w, q.h) = (x, y, w, h);
                        true
                    }
                    None => false,
                });
            ws.input_scratch
                .glow_under
                .retain_mut(|q| match clip(q.x, q.y, q.w, q.h) {
                    Some((x, y, w, h)) => {
                        (q.x, q.y, q.w, q.h) = (x, y, w, h);
                        true
                    }
                    None => false,
                });
            ws.input_scratch
                .glow_halo
                .retain_mut(|q| match clip(q.x, q.y, q.w, q.h) {
                    Some((x, y, w, h)) => {
                        (q.x, q.y, q.w, q.h) = (x, y, w, h);
                        true
                    }
                    None => false,
                });
            ws.input_scratch
                .fire_patch
                .retain_mut(|q| match clip(q.x, q.y, q.w, q.h) {
                    Some((x, y, w, h)) => {
                        (q.x, q.y, q.w, q.h) = (x, y, w, h);
                        true
                    }
                    None => false,
                });
            let (r0, c0) = (usize::from(focus_off.0), usize::from(focus_off.1));
            let (r1, c1) = (
                r0 + usize::from(focus_dims.0),
                c0 + usize::from(focus_dims.1),
            );
            ws.input_scratch.char_fg.retain(|c| {
                (r0..r1).contains(&usize::from(c.row)) && (c0..c1).contains(&usize::from(c.col))
            });
            ws.input_scratch.fire_halo.retain(|c| {
                (r0..r1).contains(&usize::from(c.row)) && (c0..c1).contains(&usize::from(c.col))
            });
            ws.input_scratch
                .cursor_trail
                .retain(|t| (r0..r1).contains(&t.row) && (c0..c1).contains(&t.col));
        }
        // Predicted cursor (mosh-style), placed in WINDOW coords: one past the
        // newest displayed guess in the focused pane, clipped to its sub-rect —
        // mirrors the single-pane advance so type-ahead never visibly trails.
        if let Some((pr, pc)) = focus_pred_last {
            ws.input_scratch.cursor_row = focus_off.0 as usize + pr as usize;
            ws.input_scratch.cursor_col =
                focus_off.1 as usize + (pc as usize + 1).min(focus_sub_cols.saturating_sub(1));
        }
        // VI-1 (compose): paint the vi (copy-mode) cursor INSTEAD of the
        // terminal cursor while steering — the focused pane's vi point mapped
        // through its display offset, offset into WINDOW coords. Runs AFTER
        // the prediction advance so copy-mode navigation wins (there is no
        // type-ahead while navigating) — the single-pane override's twin.
        if let Some((vr, vc)) = focus_vi_screen {
            ws.input_scratch.cursor_row = usize::from(focus_off.0) + vr;
            ws.input_scratch.cursor_col = usize::from(focus_off.1) + vc;
            ws.input_scratch.cursor_visible = true;
        }
        // Record whether THIS present painted a guess, so the early-out repaints
        // the frame that ERASES a ghost (same contract as the single-pane path).
        ws.pred_shown = painted_pred;
        // A composed frame has no single selection (cross-pane selection is
        // deferred); the focused pane's text is selectable only when it fills the
        // window (the single-pane path). Stamp a fresh seq so the cache sees change.
        ws.input_scratch.selection = aterm_core::selection::TextSelection::new();
        ws.input_scratch.snapshot_seq = ws.input_scratch.snapshot_seq.wrapping_add(1);
        ws.stamp_present_decision(key);
        Some(focus_title)
    }

    /// Splice the VISIBLE tab strip into the top `tab_strip_rows` rows of the
    /// just-composed `input_scratch` frame, shifting the terminal content (and the
    /// cursor) DOWN by `tab_strip_rows`. Called from `redraw` after either the
    /// single-pane or composed path filled `input_scratch` at TERMINAL size
    /// (`self.rows × self.cols`); the result is the FULL-window frame
    /// (`(self.rows + tab_strip_rows) × self.cols`) the renderer presents.
    ///
    /// A no-op when the strip is disabled (`tab_strip_rows == 0`) — `input_scratch`
    /// is then the terminal grid exactly as before, so the present + oracle paths are
    /// byte-identical. The strip's laid-out segments are cached in `self.tab_segments`
    /// for click hit-testing. The session grids are NEVER touched — only the composed
    /// `RenderInput` is shifted (so a program's cursor row, reflow, and SIGWINCH
    /// geometry are unchanged).
    pub(crate) fn splice_tab_strip(&mut self, wid: WindowId) {
        if self.tab_strip_rows == 0 {
            return;
        }
        // Cold callers (snapshot / oracle paths) refill the reused title buffer +
        // fingerprint here; the redraw hot path does the same ONCE in
        // `redraw_tab_strip_state` and shares it with the RepaintKey fingerprint. Both
        // funnel into `splice_tab_strip_with`, which reads `strip_titles_scratch`.
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
        let tab_strip = self.tab_strip_fingerprint_from_parts(&titles, &metadata, active);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.strip_titles_scratch = titles;
            ws.strip_metadata_scratch = metadata;
        }
        self.splice_tab_strip_with(wid, tab_strip);
    }

    /// Splice with the strip `tab_strip` fingerprint already computed by the caller
    /// (the redraw path reuses the one it built for the RepaintKey, so each tab's
    /// terminal is locked ONCE per present, not twice). The per-tab labels are read
    /// from the window's reused `strip_titles_scratch` (refilled by the caller), so
    /// no `Vec<String>` is threaded through. E3: when the fingerprint AND column width
    /// match the last build, the painted strip rows are REUSED from `cached_strip_rows`
    /// — the common present (terminal content moved, the strip did not) skips the
    /// `layout_segments` + `paint_strip` rebuild AND never touches the title buffer.
    /// The output is byte-identical either way (the cache is keyed on exactly what the
    /// rows are painted from: fingerprint = count+active+titles, plus `cols`).
    pub(crate) fn splice_tab_strip_with(&mut self, wid: WindowId, tab_strip: u64) {
        let strip = self.tab_strip_rows as usize;
        if strip == 0 || !self.windows.contains_key(&wid) {
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
        // A staged update shows the leading `↻` alert in the strip (even with one tab),
        // so it's folded into the cache key + it forces the strip to appear.
        let show_update = self.relaunch.is_some();
        let cache_key = (tab_strip, cols, show_update);
        let hit = self.windows.get(&wid).is_some_and(|ws| {
            ws.last_strip_fp == Some(cache_key) && ws.cached_strip_rows.len() == strip
        });
        if !hit {
            // Rebuild: lay out the segments + paint the labels onto the LAST strip row
            // (upper rows stay bare chrome). Cache the rows + segments for reuse.
            //
            // SINGLE-TAB IDENTITY: this is title chrome, not merely a switcher. A lone
            // tab therefore keeps its human title, state, close policy, and New Tab
            // action visible on every semantic host. Native apps keep their typed icon;
            // a terminal is deliberately title-only and recovers the icon width.
            let theme = self.theme;
            // Borrow the reused title buffer out of the window (take/restore) so the
            // paint can read it while `ws.cached_strip_rows`/`tab_segments` are written
            // below; a cache HIT skips this block entirely and never disturbs it.
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
            let segments = if tab_count > 0 || show_update {
                tab_bar::layout_segments_with_metadata(
                    cols as u16,
                    tab_count,
                    &metadata,
                    active,
                    show_update,
                )
            } else {
                Vec::new()
            };
            let mut strip_images = Vec::new();
            let mut rows: Vec<Vec<RenderCell>> = Vec::with_capacity(strip);
            for r in 0..strip {
                let mut row = vec![tab_bar::blank_cell(theme); cols];
                if r + 1 == strip && !segments.is_empty() {
                    strip_images = tab_bar::paint_strip_with_metadata(
                        &mut row,
                        &segments,
                        &titles,
                        &metadata,
                        active,
                        theme,
                        self.config.active_tab_color_rgb(),
                    );
                }
                rows.push(row);
            }
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.tab_segments = segments;
                ws.cached_strip_rows = rows;
                ws.cached_strip_images = strip_images;
                ws.last_strip_fp = Some(cache_key);
                ws.strip_titles_scratch = titles;
                ws.strip_metadata_scratch = metadata;
            }
        }
        // Shift the composed frame DOWN by `strip` rows, prepending the (cached)
        // strip rows. The cache is passed by reference — disjoint field borrows of
        // `ws` — so it stays intact for the next present with no per-present clone
        // of the outer Vec (the splice clones only the `strip` rows themselves).
        // W12: THIS window's own cell height (mixed-DPI) — the strip splice must use
        // the window's cell box, not whatever size the shared renderer holds.
        let cell_h = self.win_cell_size(wid).1;
        // The window's grid-interior top (`pad_top + head`): the window-space
        // effect streams' row-tag pin anchors here (their pixels are already
        // absolute), matching the renderer's grid_top after the top-pad tighten.
        let grid_top = self.win_pad_top(wid) + self.win_head(wid);
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
        if let Some(image_row) = ws.input_scratch.images.get_mut(strip - 1) {
            image_row.clone_from(&ws.cached_strip_images);
        }
    }

    /// M1b sub-row scroll — record the terminal-content grid/chrome partition and
    /// the banked sub-row residual on `input_scratch`, so the present translate
    /// (CPU [`aterm_render::scroll_translate`] / the GPU band shift) glides the grid
    /// by the pixel while chrome stays pinned. Called from the compose tail AFTER
    /// [`Self::splice_tab_strip_with`] (the grid has slid down by the chrome rows),
    /// so the terminal band is exactly `[strip, input_scratch.rows)`.
    ///
    /// The residual is presented ONLY under a Full [`crate::motion::MotionPolicy`]
    /// for [`crate::motion::MotionEffect::SmoothScroll`]; Reduced motion (or an
    /// unfocused window) snaps whole-row (frac 0) — the SAME gate the single-pane
    /// [`RepaintKey`] uses, so the key term and the presented value never disagree.
    fn set_scroll_band(&mut self, wid: WindowId) {
        let strip = if self.tab_strip_rows != 0 && self.windows.contains_key(&wid) {
            self.tab_strip_rows as usize
        } else {
            0
        };
        // W12: M1b sub-row scroll banks against THIS window's own cell height
        // (mixed-DPI) — read from the per-window view, not the shared renderer's
        // currently-active size.
        let cell_h = self.win_cell_size(wid).1.max(1) as i32;
        let focused = self.motion_focus(wid, self.windows.get(&wid).is_some_and(|ws| ws.focused));
        let animate = self
            .motion_policy(focused)
            .animate(crate::motion::MotionEffect::SmoothScroll);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        ws.input_scratch.grid_top_row = strip;
        ws.input_scratch.grid_bot_row = ws.input_scratch.rows;
        // Clamp into `(-cell_h, cell_h)` defensively: a stale bank can never
        // over-translate the band past one whole row in EITHER direction (the glide
        // residual is `[0, cell_h)`; the elastic-overscroll bounce is signed and
        // sub-cell). A positive frac shifts the band up, a negative frac down.
        ws.input_scratch.scroll_frac_px = if animate {
            ws.scroll_frac_px.clamp(-(cell_h - 1), cell_h - 1)
        } else {
            0
        };
    }

    /// Paint the Cmd-F FIND BAR over the TOP terminal row (directly below the tab strip;
    /// adaptively floated to the bottom when the current match sits on the top row) while
    /// find mode is active (`ws.search`), so the search is VISIBLE on glass — the live
    /// query, a caret, the match position, and the key hints — not just a window-title
    /// readout + a match highlight (FIND-1). Overwrites the row in place (like [`Self::splice_config_notice`])
    /// and PINS it as chrome: its row is excluded from the M1b sub-row scroll band so the
    /// smooth-scroll pixel translate never slides the bar with the grid (the same treatment
    /// as the prepended tab strip). No-op ⇒ byte-identical frame when no
    /// find is in flight. Its last row is the grid's bottom edge, and the bar is
    /// and is captured WYSIWYG by the `image`/`snapshot` introspection path. The `text`/
    /// `screen` verbs read the engine, not this scratch, so the bar never pollutes the
    /// machine-readable terminal text.
    pub(crate) fn splice_find_bar(&mut self, wid: WindowId) {
        // This frame's base_y (absolute row of the top visible line), as FRAME-CAPTURED
        // with the cells in `cell_frame_into` (single pane) / stamped from the focused pane
        // (compose) — NOT a fresh term-lock read. Reading it off `input_scratch` means the
        // re-anchor uses the base_y of the exact grid being presented, so a PTY write landing
        // between cell extraction and this splice can't drift the tint off its text; it also
        // drops a term-lock acquisition from the present path. Stored match rows are relative
        // to SEARCH-time base_y (SearchState.match_base_y); re-anchoring by
        // `delta = base_y_now − match_base_y` keeps the highlight on its line when output
        // scrolled the grid since the search (mirrors search_apply_current).
        let Some((base_y_now, frame_absolute_row_revision)) = self.windows.get(&wid).map(|ws| {
            (
                ws.input_scratch.base_y,
                ws.input_scratch.absolute_row_revision,
            )
        }) else {
            return;
        };
        // In a SPLIT, matches are keyed to the FOCUSED pane's grid, but the composite tiles
        // panes at pane row/col offsets this splice does not track — so a highlight-all tint
        // would land in the wrong pane. Suppress the tint there; the bar row and the current
        // match's selection (painted by the pane's own renderer) are still correct.
        // Zoom-aware (split-pane audit): a ZOOMED pane renders through the
        // single-pane path in window-content coords, so the tint is correct
        // there and stays on.
        let multi_pane = self
            .active_tree(wid)
            .is_some_and(|t| t.len() > 1 && !t.is_zoomed());
        // The current match's row is FOCUSED-PANE-relative in a split, while
        // the bar placement below reasons in WINDOW terminal-band rows —
        // translate by the focused pane's row offset (split-pane audit: a
        // BOTTOM pane's row-0 match does not sit under a top bar, so floating
        // the bar away was needless; a top pane's offsets keep working).
        let focus_row_off: i64 = if multi_pane {
            self.active_tree(wid)
                .and_then(|t| {
                    let focus = t.focus();
                    let (rows, cols) = self
                        .windows
                        .get(&wid)
                        .map_or((0, 0), |ws| (ws.rows, ws.cols));
                    t.compute_layout(rows, cols)
                        .iter()
                        .find(|r| r.session == focus)
                        .map(|r| i64::from(r.row_off))
                })
                .unwrap_or(0)
        } else {
            0
        };
        let (match_base_y, match_absolute_row_revision) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.search.as_ref())
            .map_or((0, 0), |s| (s.match_base_y, s.match_absolute_row_revision));
        // A protected-footer splice changes absolute rows piecewise. Until the UI
        // recomputes, the cached match coordinates cannot safely be mapped into this
        // frame with a uniform delta. Keep the bar visible, but fail closed for every
        // geometry-dependent use of those cached rows.
        let stale_absolute_rows = frame_absolute_row_revision != match_absolute_row_revision;
        let delta = base_y_now.saturating_sub(match_base_y);
        // Read the find state (bar view + the CURRENT match's terminal-band row + its
        // index + the frame/terminal row counts) before the disjoint-field borrow. The
        // full match set is NOT cloned out here — the highlight loop below borrows
        // `ws.search`'s `matches` in place (a field disjoint from `input_scratch.cells`),
        // avoiding an up-to-~800KB heap copy every presented frame while the bar is open.
        // `None` ⇒ not searching ⇒ no-op.
        let Some((view, cur_term_row, cur_idx, nrows, term_rows)) =
            self.windows.get(&wid).and_then(|ws| {
                ws.search.as_ref().map(|s| {
                    // Terminal-band row of the current match: `sel_row + display_offset`,
                    // re-anchored by `− delta` for output that scrolled in since the search.
                    let offset = i64::from(ws.input_scratch.display_offset).saturating_sub(delta);
                    let cur_term_row = if stale_absolute_rows {
                        None
                    } else {
                        s.matches
                            .get(s.current)
                            .map(|&(r, _, _)| i64::from(r).saturating_add(offset))
                    };
                    (
                        crate::find_bar::FindBarView {
                            query: s.query.clone(),
                            cursor: s.cursor,
                            idx: s.current + 1,
                            total: s.matches.len(),
                            case_sensitive: s.case_sensitive,
                            is_regex: s.is_regex,
                            regex_error: s.regex_error,
                            truncated: s.truncated,
                        },
                        cur_term_row,
                        s.current,
                        ws.input_scratch.cells.len(),
                        ws.rows as usize,
                    )
                })
            })
        else {
            // Not searching this frame ⇒ no bar is drawn, so drop any clickable indicator
            // geometry a PRIOR frame left behind. This is the invariant belt to
            // `sync_window`/`search_exit`: `find_bar_hit` must never outlive the bar, or a
            // click on those cells is swallowed by a no-op toggle (the #1 dead-zone).
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.find_bar_hit = None;
            }
            return;
        };
        let cols = self.windows.get(&wid).map_or(0, |ws| ws.cols as usize);
        if cols == 0 {
            return;
        }
        // The tab strip (when enabled) is already prepended, so the frame is `strip` chrome
        // rows then the `term_rows`-tall terminal band. Placement + highlight work in
        // TERMINAL rows (0..term_rows), mapping to a `cells` frame index by adding `strip`.
        let strip = nrows.saturating_sub(term_rows);
        let Some(term_bottom) = term_rows.checked_sub(1) else {
            return; // zero-row terminal
        };
        // ADAPTIVE PLACEMENT: the panel rides the TOP terminal rows (directly below the
        // tab strip — find lives at the top of the window), but if the CURRENT match
        // (what the user just navigated to) sits under it, a top panel would hide it.
        // When the terminal is tall enough to hold the band twice over, float the panel
        // to the BOTTOM rows instead so the match stays visible; the seam always rides
        // the panel's content-facing edge. The apply path lands scrollback matches just
        // below the band, so this float is the exception — the clamp at the oldest
        // history line — not the common step. A terminal shorter than the band gets a
        // degraded (2-row, then 1-row) panel rather than none.
        let band_rows = crate::find_bar::FIND_BAR_ROWS.min(term_rows);
        let match_under_top_band = cur_term_row
            .map(|r| r + focus_row_off)
            .is_some_and(|r| r >= 0 && (r as usize) < band_rows);
        let bar_row = if term_rows > band_rows && match_under_top_band {
            term_rows - band_rows
        } else {
            0
        };
        let band = bar_row..bar_row + band_rows;
        let frame_bar_row = strip + bar_row;
        let seam_at_top = band.end > term_bottom;
        // Tint off the live OSC-11 background (like splice_notice) so the
        // bar stays WCAG-AA legible on a program-recoloured background.
        let mut theme = self.theme;
        if let Some(ws) = self.windows.get(&wid) {
            let live = ws.input_scratch.default_bg;
            if live != aterm_core::render::COLOR_UNSET {
                theme.bg = live;
            }
        }
        // HIGHLIGHT-ALL colour: the bottom terminal bg tinted halfway toward the theme's
        // selection tone. Every visible match gets it; the CURRENT match keeps the full
        // selection colour (the renderer paints `input_scratch.selection` on top), so it
        // stands out. Blended toward bg (not full selection) so each cell's own fg stays
        // legible without the renderer's selection-fg floor.
        let hi = {
            let bg = crate::settings::u32_rgb(theme.bg);
            let sel = crate::settings::u32_rgb(theme.selection);
            [
                ((u16::from(bg[0]) + u16::from(sel[0])) / 2) as u8,
                ((u16::from(bg[1]) + u16::from(sel[1])) / 2) as u8,
                ((u16::from(bg[2]) + u16::from(sel[2])) / 2) as u8,
            ]
        };
        let painted = crate::find_bar::find_bar_paint(&view, cols, band_rows, theme, seam_at_top);
        let hi_u32 = aterm_render::rgb_to_u32(hi);
        let cell_h = self.win_cell_size(wid).1;
        let offset = self.windows.get(&wid).map_or(delta.saturating_neg(), |ws| {
            i64::from(ws.input_scratch.display_offset).saturating_sub(delta)
        });
        // Results are sorted in selection row/column order. Binary-slice the
        // immutable history before taking the mutable render borrow: a 100k-hit
        // scrollback must cost O(log N + visible), never O(history) per frame.
        let (visible_matches, _match_search_comparisons) = if multi_pane || stale_absolute_rows {
            (0..0, 0)
        } else {
            self.windows
                .get(&wid)
                .and_then(|ws| ws.search.as_ref())
                .map_or((0..0, 0), |search| {
                    visible_match_range(&search.matches, offset, term_rows)
                })
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        #[cfg(test)]
        {
            ws.find_bar_match_work = _match_search_comparisons + visible_matches.len();
        }
        // HIGHLIGHT-ALL: tint the bg of every VISIBLE matched cell AND floor its fg
        // against that tint (WCAG ~4.5:1) so text whose SGR colour is close to the tint
        // stays legible. Matches are in SELECTION rows; the terminal-band row for a match
        // is `sel_row + display_offset − delta` (delta re-anchors for output that scrolled
        // in since the search), and the `cells` frame index is that plus `strip` (the
        // prepended tab-strip chrome rows). Matches scrolled out of the band, and the bar's
        // own row, are skipped. The CURRENT match is skipped for the fg floor — the renderer
        // paints the full selection over it and applies its OWN selection-fg floor. `c0..=c1`
        // are DISPLAY/cell columns (the engine's `ColumnMap` already counts a wide CJK glyph
        // as two), so they index the render grid directly. Suppressed in a split (the tint
        // would land in the wrong pane — see the `multi_pane` note above).
        // Borrow the match set in place — `ws.search` is a field disjoint from the
        // `ws.input_scratch.cells` the loop writes, so the compiler splits the borrow and
        // no per-frame clone is needed. `search` is known-Some here (the early return above
        // covered `None`); if it somehow raced to `None`, there is nothing to tint.
        let matches: &[(i32, u16, u16)] = match ws.search.as_ref() {
            Some(s) => &s.matches,
            None => return,
        };
        for (relative, &(sel_row, c0, c1)) in matches[visible_matches.clone()].iter().enumerate() {
            let i = visible_matches.start + relative;
            let term_row = i64::from(sel_row).saturating_add(offset);
            debug_assert!(term_row >= 0 && (term_row as usize) < term_rows);
            if band.contains(&(term_row as usize)) {
                continue;
            }
            let is_current = i == cur_idx;
            let row = &mut ws.input_scratch.cells[strip + term_row as usize];
            for cell in row
                .iter_mut()
                .take(usize::from(c1) + 1)
                .skip(usize::from(c0))
            {
                cell.bg = hi;
                if !is_current {
                    cell.fg = crate::settings::u32_rgb(aterm_render::floor_selection_fg(
                        aterm_render::rgb_to_u32(cell.fg),
                        hi_u32,
                    ));
                }
                // The CURRENT match's fg is deliberately NOT floored here: the renderer
                // paints the full selection over it and applies its OWN selection-fg floor,
                // so flooring first would DOUBLE-floor and shift the common (settled) case's
                // colour. The one gap this leaves is cosmetic and self-healing: during
                // passive PTY streaming the tint re-anchors per present but the selection
                // (set only by `search_apply_current` on a user action) does not, so for a
                // frame or two the current-match cell can sit un-floored on the tint until
                // the next nav re-anchors the selection. Flooring it unconditionally to close
                // that is a worse trade (a visible colour regression on every settled frame),
                // so it is left to the renderer's selection floor.
            }
        }
        debug_assert_eq!(painted.rows.len(), band_rows);
        let frame_band = frame_bar_row..frame_bar_row + band_rows;
        for (offset_row, built) in painted.rows.into_iter().enumerate() {
            debug_assert_eq!(built.len(), cols);
            let frame_row = frame_bar_row + offset_row;
            ws.input_scratch.cells[frame_row] = built;
            // Clear the parallel per-row arrays so the covered row's terminal content
            // (emoji clusters / combining marks / inline images) can't bleed through, and
            // mark the row single-width — exactly as splice_config_notice does for the
            // rows it overwrites.
            if let Some(c) = ws.input_scratch.clusters.get_mut(frame_row) {
                c.clear();
            }
            if let Some(c) = ws.input_scratch.combining.get_mut(frame_row) {
                c.clear();
            }
            if let Some(im) = ws.input_scratch.images.get_mut(frame_row) {
                im.clear();
            }
            if let Some(ls) = ws.input_scratch.line_sizes.get_mut(frame_row) {
                *ls = aterm_core::grid::LineSize::SingleWidth;
            }
        }
        // If the terminal cursor sits on a row the panel now covers (e.g. ⌘F at a shell
        // prompt pinned to the bottom line), hide it so its block doesn't render on top of
        // the panel — the panel draws its own caret in the field. `cursor_row` is a frame row.
        if frame_band.contains(&ws.input_scratch.cursor_row) {
            ws.input_scratch.cursor_visible = false;
        }
        // The bar hides the cursor — hide the cursor EFFECTS on its row too. The
        // ⌘F-at-a-bottom-prompt case leaves live trail cells (an OPAQUE ember-bed
        // fill), charred-ink recolours, fire contrast halos, and additive light
        // exactly on the row the bar just covered; without this scrub they paint
        // OVER the bar's chrome (the audit's find-bar overlap). Cell streams are
        // frame-row tagged by now (the strip splice shifted them with the grid);
        // the window-space pixel streams carry a re-derived frame-row damage tag
        // and the single-row-band invariant, so dropping by tag removes exactly
        // the bar-band light. Pure per-frame recompute — the next frame's splice
        // re-derives from fresh engine output, so nothing is lost off-bar.
        let band16 = u16::try_from(frame_band.start).unwrap_or(u16::MAX)
            ..u16::try_from(frame_band.end).unwrap_or(u16::MAX);
        let in_band = |row: u16| band16.contains(&row);
        let bar_y0 = i64::try_from(frame_band.start.saturating_mul(cell_h)).unwrap_or(i64::MAX);
        let bar_y1 = i64::try_from(frame_band.end.saturating_mul(cell_h)).unwrap_or(i64::MAX);
        ws.input_scratch
            .cursor_trail
            .retain(|t| !frame_band.contains(&t.row));
        ws.input_scratch.char_fg.retain(|c| !in_band(c.row));
        ws.input_scratch.fire_halo.retain(|c| !in_band(c.row));
        ws.input_scratch.cursor_glow_add.retain(|q| !in_band(q.row));
        ws.input_scratch.glow_under.retain(|q| !in_band(q.row));
        ws.input_scratch.fire_patch.retain(|q| !in_band(q.row));
        ws.input_scratch.glow_halo.retain(|q| !in_band(q.row));
        ws.input_scratch.ink.retain(|c| !in_band(c.row));
        ws.input_scratch.word_decorations.retain(|decoration| {
            // Decorations deliberately jitter by signed `dy` and occupy one
            // cell-height stamp, so a neighbour can spill into the bar even
            // when its row tag differs. Test the real signed pixel extent.
            let y0 = i64::from(decoration.row) * i64::try_from(cell_h).unwrap_or(0)
                + i64::from(decoration.dy);
            let y1 = y0.saturating_add(i64::try_from(cell_h).unwrap_or(0));
            y1 <= bar_y0 || y0 >= bar_y1
        });
        ws.input_scratch.nova_add.retain(|q| !in_band(q.row));
        ws.input_scratch.cat_quads.retain(|q| !in_band(q.row));
        ws.input_scratch.rain_quads.retain(|q| !in_band(q.row));
        ws.input_scratch.rain_add.retain(|q| !in_band(q.row));
        // Free sprites have no row tag: their signed grid-interior pixel extent
        // is authoritative. Chrome owns the whole bar band, so drop any sprite
        // that intersects it (rather than letting a multi-row cat/sparkle paint
        // over the replacement cells).
        ws.input_scratch.free_sprites.retain(|sprite| {
            let y0 = i64::from(sprite.y);
            let y1 = y0.saturating_add(i64::from(sprite.h));
            y1 <= bar_y0 || y0 >= bar_y1
        });
        // PIN the panel out of the sub-row scroll band so the smooth-scroll pixel translate
        // leaves it fixed: at the bottom, shrink `grid_bot_row` (exclusive) past its rows;
        // at the top, raise `grid_top_row` past them. Guarded so the band never
        // inverts, and a 0 band (headless capture, no set_scroll_band) is left untouched.
        if frame_band.end == nrows {
            if ws.input_scratch.grid_bot_row > frame_band.start {
                let top = ws.input_scratch.grid_top_row;
                ws.input_scratch.grid_bot_row = frame_band.start.max(top);
            }
        } else if ws.input_scratch.grid_top_row < frame_band.end {
            let bot = ws.input_scratch.grid_bot_row;
            ws.input_scratch.grid_top_row = frame_band.end.min(bot);
        }
        // Record where the clickable toggle indicators + the editable well landed (their
        // row + column spans + the well's horizontal scroll), derived from the SAME
        // builder inputs as the paint, so the mouse hit-test reads exact geometry instead
        // of re-deriving the adaptive placement.
        ws.find_bar_hit = Some(crate::FindBarHit {
            row: bar_row + painted.field_row,
            case_cols: painted.case_cols,
            regex_cols: painted.regex_cols,
            field_cols: painted.field_cols,
            field_scroll: painted.field_scroll,
            band,
        });
        ws.input_scratch.snapshot_seq = ws.input_scratch.snapshot_seq.wrapping_add(1);
    }

    /// The theme an overlay card on window `wid` tints with. Tracks the live
    /// OSC-11 background so a card over a real terminal tints off the current bg.
    pub(crate) fn overlay_card_theme(&self, wid: WindowId) -> Theme {
        let mut theme = self.theme;
        if let Some(ws) = self.windows.get(&wid) {
            let live = ws.input_scratch.default_bg;
            if live != aterm_core::render::COLOR_UNSET {
                theme.bg = live;
            }
        }
        theme
    }

    /// OVERLAY the modal Settings panel over the TOP `panel_rows` rows of the composed
    /// frame (a no-op — byte-identical — when this window has no open overlay). The
    /// settings panel overwrites existing rows in place so it covers the content without
    /// changing the frame geometry (no swapchain / PTY resize). It rides inside the same
    /// `input_scratch` both renderers consume, so it is byte-parity-correct AND captured
    /// WYSIWYG by the image/snapshot introspection path. Called LAST in the chrome seam
    /// (after the tab strip) so it paints on top.
    pub(crate) fn splice_settings_panel(&mut self, wid: WindowId) {
        // Single source for the overlay height (C2): closed ⇒ 0 ⇒ no-op (byte-identical
        // to no panel); a frame too short for even the title ⇒ 0 too.
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
        let theme = self.overlay_card_theme(wid);
        // Build + stash the frosted DrawPrim card (device px, scale 1.0 — `cell_size`/`pad`
        // are already device px). It composites at present/capture time as a translucent
        // overlay — `App::composite_tray` on CPU, `draw_tray_into_offscreen` on GPU — so the
        // terminal cells are left UNTOUCHED (no cell overwrite). Re-rasterizes only when the
        // model fingerprint changes; the `RepaintKey::settings_fp` term drives the present.
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid) as u32;
        let pad_top = self.win_pad_top(wid) as u32;
        let head = self.win_head(wid) as u32;
        let font_px = self.win_font_px(wid);
        // The App tracks the OS appearance (`sync_app_theme_to_appearance`) and the
        // window's monitor DPI scale; the pure painters read both via PreviewCtx —
        // Settings resolves the window_theme=auto titlebar mock, About sizes its
        // native-pt text. Read before the ws borrow.
        let ctx = crate::settings::PreviewCtx {
            system_dark: repaint_system_dark(self.os_appearance),
            scale: self.windows.get(&wid).map_or(1.0, |ws| ws.scale as f32),
            // The configured trail colour/accent overrides, resolved exactly as
            // `glow_config` resolves them — so the settings demo lane previews
            // the colours the live effect actually renders.
            trail_color: self.config.cursor_trail_color_u32(),
            trail_accent: self.config.cursor_trail_accent_u32(),
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        // The ACTIVE overlay (Settings, About, or Palette — mutually exclusive) supplies the
        // fingerprint + prims through the ONE `OverlayModel` trait; every variant rasterizes
        // into the SAME `settings_card` slot + composite path, so each is captured WYSIWYG
        // identically. The producer is generalized; the SACRED composite path is untouched.
        let fp = ws.overlay_fp();
        // Stale on a MODEL change (fp) OR a GEOMETRY change (resize / zoom / live font
        // edit while open): the fp only hashes state, so `geom_key` hashes EVERY
        // geometry input the layout reads — otherwise a stale-metric raster keeps
        // compositing while the freshly computed mouse hit-test (which always reads
        // the LIVE geometry) resolves clicks against different pixels.
        let geom_key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            cw.hash(&mut h);
            ch.hash(&mut h);
            font_px.to_bits().hash(&mut h);
            cols.hash(&mut h);
            panel_rows.hash(&mut h);
            pad.hash(&mut h);
            pad_top.hash(&mut h);
            head.hash(&mut h); // the dy anchor moves with the chrome headroom
            // Not state, not geometry — but they move pixels (the auto titlebar mock;
            // About's native-pt text sizes), so an OS appearance flip or a monitor
            // DPI change must invalidate the cached raster too.
            ctx.system_dark.hash(&mut h);
            ctx.scale.to_bits().hash(&mut h);
            // The demo lane's configured trail colours: a hot-reload that changes
            // them while the panel is open must re-rasterize the preview card.
            ctx.trail_color.hash(&mut h);
            ctx.trail_accent.hash(&mut h);
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
            let mut tray = overlay.model().tray(&geom, theme, ctx);
            // Rasterize only the card's PAINT BOUNDS (its rect + a margin covering the
            // drop shadow), clamped to the tray. For the top-band Settings/Palette
            // cards (card == the whole tray) this is byte-identical to rasterizing the
            // tray; for the FLOATING About dialog it shrinks the buffer from the whole
            // frame to the dialog's region — every focus/status repaint then allocates,
            // uploads (GPU), and per-present composites (CPU) the dialog, not a
            // full-frame, mostly-transparent canvas.
            const PAINT_MARGIN: f32 = 12.0; // covers the 2-step drop shadow (≤ 8 px)
            let tray_w = (cols * cw) as f32;
            let tray_h = (panel_rows * ch) as f32;
            let (card_x, card_y, card_w, card_h) = tray.card;
            let x0 = (card_x - PAINT_MARGIN).max(0.0).floor();
            let y0 = (card_y - PAINT_MARGIN).max(0.0).floor();
            let x1 = (card_x + card_w + PAINT_MARGIN).min(tray_w).ceil();
            let y1 = (card_y + card_h + PAINT_MARGIN).min(tray_h).ceil();
            if x1 <= x0 || y1 <= y0 {
                // Degenerate card (nothing visible to paint): drop any stale raster.
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
                rgba,
                pw,
                ph,
                dx: pad + x0 as u32,
                dy: pad_top + head + y0 as u32,
                fp,
                geom: geom_key,
            });
        }
        // Hide the terminal cursor so it doesn't show through the modal card.
        ws.input_scratch.cursor_visible = false;
    }

    /// Rasterize the subtle top-right build/version badge into its OWN paint-only
    /// `badge_card` slot (which NEVER gates the mouse — unlike the modal `settings_card`).
    /// No-op ⇒ `badge_card = None` when the `show_build_badge` setting is off
    /// (byte-identical to no badge). Re-rasterizes only when the badge fingerprint OR the
    /// geometry changes; the version is static, so this is a one-time cost that then
    /// composites every present from the cache. Uses the SAME tray rasterizer + composite
    /// path as [`Self::splice_settings_panel`] (so screen == introspection), and the
    /// composite prefers `settings_card` over `badge_card` — an open overlay covers the
    /// badge, and it returns when the overlay closes.
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
        // Tint off the live OSC-11 background exactly as `splice_settings_panel` does.
        let mut theme = self.theme;
        if let Some(ws) = self.windows.get(&wid) {
            let live = ws.input_scratch.default_bg;
            if live != aterm_core::render::COLOR_UNSET {
                theme.bg = live;
            }
        }
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid) as u32;
        let pad_top = self.win_pad_top(wid) as u32;
        let head = self.win_head(wid) as u32;
        let font_px = self.win_font_px(wid);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        // Staleness: the badge fingerprint (version/build) OR any geometry input the pill
        // layout reads. Static in practice ⇒ rasterizes once, then composites from cache.
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
            let mut tray = crate::build_badge::badge_tray(&geom, theme);
            let tray_w = (cols * cw) as f32;
            let (card_x, card_y, card_w, card_h) = tray.card;
            // Rasterize only the pill's bounds (+ a hairline margin), clamped to the tray
            // width — a tiny buffer, not a full-frame canvas.
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
                rgba,
                pw,
                ph,
                dx: pad + x0 as u32,
                dy: pad_top + head + y0 as u32,
                fp,
                geom: geom_key,
            });
        }
    }

    /// Rasterize the GLOBAL transient update notice ([`self.notice`]) into this window's
    /// paint-only `notice_card` (composited with priority OVER `badge_card`, UNDER a modal
    /// `settings_card`). No-op ⇒ `notice_card = None` when no notice is up. Re-rasterizes
    /// when the notice's quantized fade fingerprint OR the geometry changes, so the pill
    /// fades smoothly through the SAME rasterizer + composite path the badge/overlay use
    /// (screen == introspection). Mirrors [`Self::splice_build_badge`].
    pub(crate) fn splice_notice(&mut self, wid: WindowId) {
        let now = std::time::Instant::now();
        let Some(fp) = self.notice.as_ref().map(|n| n.fingerprint(now)) else {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.notice_card = None;
            }
            return;
        };
        let cols = self.windows.get(&wid).map_or(0, |ws| ws.cols as usize);
        if cols == 0 {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.notice_card = None;
            }
            return;
        }
        // Tint off the live OSC-11 background, like the badge/settings splices.
        let mut theme = self.theme;
        let cursor = crate::settings::u32_rgb(self.theme.cursor);
        if let Some(ws) = self.windows.get(&wid) {
            let live = ws.input_scratch.default_bg;
            if live != aterm_core::render::COLOR_UNSET {
                theme.bg = live;
            }
        }
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid) as u32;
        let pad_top = self.win_pad_top(wid) as u32;
        let head = self.win_head(wid) as u32;
        let font_px = self.win_font_px(wid);
        let Some(notice) = self.notice.as_ref() else {
            return;
        };
        let geom = crate::settings::SettingsGeom {
            cw: cw as f32,
            ch: ch as f32,
            font_px,
            cols,
            panel_rows: 0, // unused: the pill self-positions at the top-centre
        };
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
            h.finish()
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if ws
            .notice_card
            .as_ref()
            .is_none_or(|c| c.fp != fp || c.geom != geom_key)
        {
            let mut tray = crate::notice::notice_tray(notice, &geom, theme, cursor, now);
            let tray_w = (cols * cw) as f32;
            let (card_x, card_y, card_w, card_h) = tray.card;
            const PAINT_MARGIN: f32 = 4.0; // covers the soft shadow
            let x0 = (card_x - PAINT_MARGIN).max(0.0).floor();
            let y0 = (card_y - PAINT_MARGIN).max(0.0).floor();
            let x1 = (card_x + card_w + PAINT_MARGIN).min(tray_w).ceil();
            let y1 = (card_y + card_h + PAINT_MARGIN).ceil();
            if x1 <= x0 || y1 <= y0 {
                ws.notice_card = None;
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
            ws.notice_card = Some(crate::SettingsCard {
                rgba,
                pw,
                ph,
                dx: pad + x0 as u32,
                dy: pad_top + head + y0 as u32,
                fp,
                geom: geom_key,
            });
        }
    }

    /// Rasterize the LEVEL-UP rising up-arrow ([`self.level_up`]) into this window's
    /// paint-only `level_up_card` (composited with priority OVER `notice_card`, UNDER a
    /// modal `settings_card`). No-op ⇒ `level_up_card = None` when no celebration is up OR
    /// the arrow has already faded (its alpha reached 0) — the border glow keeps going via
    /// the overlay pass and the "Update ready" pill shows through beneath. Re-rasterizes
    /// each animation step (the arrow moves + fades), through the SAME rasterizer +
    /// composite path the notice/badge use, so screen == introspection. Mirrors
    /// [`Self::splice_notice`].
    pub(crate) fn splice_level_up(&mut self, wid: WindowId) {
        if !self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::LevelUp)
        {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.level_up_card = None;
            }
            return;
        }
        let now = std::time::Instant::now();
        // Only while the arrow is actually visible do we hold a card; once it fades the
        // slot is released so the notice pill can composite through.
        let Some(fp) = self
            .level_up
            .as_ref()
            .filter(|l| l.arrow_alpha(now) > 0.0)
            .map(|l| l.fingerprint(now))
        else {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.level_up_card = None;
            }
            return;
        };
        let (cols, rows) = self
            .windows
            .get(&wid)
            .map_or((0, 0), |ws| (ws.cols as usize, ws.rows as usize));
        if cols == 0 || rows == 0 {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.level_up_card = None;
            }
            return;
        }
        // Accent = the live cursor colour (like the notice's LevelUp flourish + the border
        // glow above), so the arrow and the frame it rises within share one hue.
        let accent = crate::settings::u32_rgb(self.theme.cursor);
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid) as u32;
        let pad_top = self.win_pad_top(wid) as u32;
        let head = self.win_head(wid) as u32;
        let font_px = self.win_font_px(wid);
        let Some(level_up) = self.level_up.as_ref() else {
            return;
        };
        let geom = crate::settings::SettingsGeom {
            cw: cw as f32,
            ch: ch as f32,
            font_px,
            cols,
            panel_rows: rows, // the window height the arrow centres its rise within
        };
        let geom_key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            cw.hash(&mut h);
            ch.hash(&mut h);
            font_px.to_bits().hash(&mut h);
            cols.hash(&mut h);
            rows.hash(&mut h);
            pad.hash(&mut h);
            pad_top.hash(&mut h);
            head.hash(&mut h); // the dy anchor moves with the chrome headroom
            h.finish()
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if ws
            .level_up_card
            .as_ref()
            .is_none_or(|c| c.fp != fp || c.geom != geom_key)
        {
            let mut tray = crate::level_up::arrow_tray(level_up, &geom, accent, now);
            let tray_w = (cols * cw) as f32;
            let (card_x, card_y, card_w, card_h) = tray.card;
            const PAINT_MARGIN: f32 = 4.0; // covers any glyph overhang
            let x0 = (card_x - PAINT_MARGIN).max(0.0).floor();
            let y0 = (card_y - PAINT_MARGIN).max(0.0).floor();
            let x1 = (card_x + card_w + PAINT_MARGIN).min(tray_w).ceil();
            let y1 = (card_y + card_h + PAINT_MARGIN).ceil();
            if x1 <= x0 || y1 <= y0 {
                ws.level_up_card = None;
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
            ws.level_up_card = Some(crate::SettingsCard {
                rgba,
                pw,
                ph,
                dx: pad + x0 as u32,
                dy: pad_top + head + y0 as u32,
                fp,
                geom: geom_key,
            });
        }
    }

    /// Paint the transient config-warning banner (`self.config_notice`) by OVERWRITING
    /// the top rows in place — no geometry change — mirroring [`Self::splice_settings_panel`].
    /// `None` ⇒ no-op (byte-identical frame; no `snapshot_seq` bump). GLOBAL (App-level),
    /// so it draws into every window. Called LAST in `redraw_window`, so it sits on top of
    /// grid + strip + settings; while up it covers the tab strip + the top of the
    /// grid for ~`NOTICE_TTL` (the same trade-off the modal settings panel makes). Unlike
    /// settings it does NOT hide the cursor (a banner isn't modal).
    pub(crate) fn splice_config_notice(&mut self, wid: WindowId) {
        let panel_rows = {
            let Some(notice) = self.config_notice.as_ref() else {
                return; // no banner -> byte-identical frame
            };
            let avail = match self.windows.get(&wid) {
                Some(ws) => ws.input_scratch.cells.len(),
                None => return,
            };
            notice.wanted_rows().min(avail)
        };
        if panel_rows == 0 {
            return;
        }
        let cols = match self.windows.get(&wid) {
            Some(ws) => ws.cols as usize,
            None => return,
        };
        // Tint off the live OSC-11 background (like splice_settings_panel) so the
        // band colors keep the banner text WCAG-AA legible on a recoloured bg.
        let mut theme = self.theme;
        if let Some(ws) = self.windows.get(&wid) {
            let live = ws.input_scratch.default_bg;
            if live != aterm_core::render::COLOR_UNSET {
                theme.bg = live;
            }
        }
        let built = {
            let Some(notice) = self.config_notice.as_ref() else {
                return;
            };
            crate::config_notice::notice_rows(&notice.lines, cols, panel_rows, theme)
        };
        if let Some(ws) = self.windows.get_mut(&wid) {
            for (r, row) in built.into_iter().enumerate() {
                if r >= ws.input_scratch.cells.len() {
                    break;
                }
                debug_assert_eq!(row.len(), cols);
                ws.input_scratch.cells[r] = row;
                // Clear the parallel per-row arrays so the covered rows' terminal
                // content doesn't bleed through, and mark the row single-width.
                if let Some(c) = ws.input_scratch.clusters.get_mut(r) {
                    c.clear();
                }
                if let Some(c) = ws.input_scratch.combining.get_mut(r) {
                    c.clear();
                }
                if let Some(im) = ws.input_scratch.images.get_mut(r) {
                    im.clear();
                }
                if let Some(ls) = ws.input_scratch.line_sizes.get_mut(r) {
                    *ls = aterm_core::grid::LineSize::SingleWidth;
                }
            }
            ws.input_scratch.snapshot_seq = ws.input_scratch.snapshot_seq.wrapping_add(1);
        }
    }

    /// Apply a `(rows, cols)` grid resize to the engine + PTY + GPU swapchain
    /// (the geometry the main thread owns). The CPU softbuffer resizes itself in
    /// `redraw` from the Frame dims. No-op when the geometry is unchanged. Shared
    /// by the window `Resized` path and the control-socket resize (RES-1).
    ///
    /// TABS + PANES: rows/cols are WINDOW-level, so a resize re-lays EVERY tab's
    /// panes of window `wid` and resizes each pane's engine + PTY to ITS sub-rect
    /// (not just the active one) — a background tab/pane kept at the old size would
    /// reflow wrongly the moment it became visible, and its app (vim/htop) would see
    /// a stale `SIGWINCH` geometry. With one pane per tab this is the same single
    /// resize as before (the pane fills the whole window).
    pub(crate) fn apply_term_resize(&mut self, wid: WindowId, rows: u16, cols: u16) -> bool {
        // W12: inline-image pixel sizing is a property of THIS window's grid.
        // The shared renderer may currently be activated to another monitor, so
        // its live cell size is not authority for `wid`.
        let (cw, ch) = self.win_cell_size(wid);
        // Report the real cell pixel metric to THIS window's panes' engines so
        // inline images (iTerm2 OSC 1337 `File=`) sized in pixels/percent land on
        // the right cell footprint. Pushed before the no-op early-return so every
        // session stays in sync with the font in use.
        for id in self.window_terminal_sessions(wid) {
            if let Some(s) = self.pool.get(id) {
                term_lock(&s.term).set_cell_pixel_size(cw as u16, ch as u16);
            }
        }
        // Size THIS window's GPU swapchain to the true window CLIENT area (not the
        // grid-derived pixel size) so the WSI never stretches the terminal. Done
        // BEFORE the unchanged-geometry early-return so the control-echo follow-up
        // `Resized` (which lands here with rows/cols already applied) still corrects
        // the surface to the window's settled size. `present_input` letterboxes the
        // grid-sized offscreen into it.
        self.sync_gpu_surface_size(wid);
        if Some((rows, cols)) == self.windows.get(&wid).map(|ws| (ws.rows, ws.cols)) {
            return false;
        }
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.rows = rows;
            ws.cols = cols;
            // The grid dimensions (and thus the cursor coordinate space predictions are
            // anchored in) just changed — resize, scale-factor, or font-zoom re-grid —
            // so drop any in-flight predictions rather than paint them at stale coords.
            ws.predictor.reset();
            // A COMMITTED resize supersedes any still-armed live-drag coalesce: an
            // out-of-band resize (the control-socket `resize` verb, a scale-factor or
            // config/font re-grid) reaching here must cancel a pending drag-settle so a
            // stale older size can't be re-applied ~RESIZE_THROTTLE later. The throttle's
            // own leading-edge / trailing-flush paths already cleared these, so this is a
            // no-op for them — it only hardens the out-of-band callers.
            ws.pending_resize = None;
            ws.next_resize_settle = None;
        }
        // Resize every pane (of every tab of THIS window) to its computed sub-rect;
        // with no splits each pane fills its whole tab = the full window grid.
        // `resize_panes` records each pane's asciicast + temporal-spine resize event.
        self.resize_panes(wid);
        // The GPU swapchain was already sized to the true window client area above
        // (via `sync_gpu_surface_size`, before the early-return), independent of the
        // grid geometry; the offscreen stays grid-sized and is letterboxed into it in
        // `present_input`.
        true
    }

    /// Configure window `wid`'s GPU swapchain to the RAW window pixels (`win_px`,
    /// recorded at attach + every `Resized`), NOT the grid-derived pixel size. The
    /// swapchain must match the real window so the WSI (DX12 `DXGI_SCALING_STRETCH`
    /// / Vulkan) never bilinearly stretches the terminal at a non-cell-multiple
    /// window size (maximized / snapped / mid-drag); the offscreen stays at the
    /// integer-cell grid size and `GpuRenderer::present_input` letterboxes it into
    /// this surface, filling the sub-cell remainder with the terminal background.
    /// Sourcing the cached `win_px` (not a fresh `inner_size()` query) keeps a
    /// recorder replay deterministic. Headless / pre-attach (`win_px == None`) keeps
    /// the historical frame-size fallback (the padded full-window grid: terminal
    /// rows + tab strip + independent top/base-bottom padding). No-op on the CPU
    /// backend or a window without a live GPU surface; `resize_surface` is
    /// idempotent, so a call with the size already applied does no reconfigure.
    fn sync_gpu_surface_size(&mut self, wid: WindowId) {
        let strip = if self.windows.contains_key(&wid) {
            self.tab_strip_rows as usize
        } else {
            0
        };
        let App {
            backend, windows, ..
        } = self;
        if let Some(ws) = windows.get_mut(&wid)
            && let (Some(gpu), Some(PresentTarget::Gpu { gpu_surface, .. })) =
                (backend.gpu_mut(), ws.present.as_mut())
        {
            let (w_px, h_px) = match ws.win_px {
                Some(size) => (size.width.max(1), size.height.max(1)),
                None => {
                    let win_rows = ws.rows as usize + strip;
                    let (w, h) = gpu.frame_size(win_rows, ws.cols as usize);
                    (w as u32, h as u32)
                }
            };
            gpu.resize_surface(gpu_surface, w_px, h_px);
        }
    }

    /// HEADLESS PRESENT-REAL: the device-pixel size of window `wid`'s full
    /// composed frame with NO glass — the SAME fallback math as
    /// [`Self::sync_gpu_surface_size`]'s `win_px == None` arm (terminal rows +
    /// tab strip for a chromed window, + independent top/base-bottom padding
    /// border), which is exactly the offscreen `encode_frame` produces for the
    /// spliced `input_scratch`. Sizes the `virtual_begin` target + tap so the
    /// first `present_virtual` frame matches the tap's geometry (a mismatch
    /// would finalize the recording on frame one). `None` when the window or
    /// the GPU backend is gone.
    pub(crate) fn headless_frame_px(&mut self, wid: WindowId) -> Option<(u32, u32)> {
        let strip = if self.windows.contains_key(&wid) {
            self.tab_strip_rows as usize
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

    /// W1: reconfigure this window's GPU swapchain to the RAW window pixels
    /// (`win_px`). Called straight from `WindowEvent::Resized` — BEFORE the
    /// throttled grid reflow — so the surface never lags the window and the
    /// compositor never rescales a stale-sized frame. A no-op for the CPU
    /// backend (softbuffer resizes itself at present) and headless.
    pub(crate) fn sync_surface_to_window(&mut self, wid: WindowId) {
        let App {
            backend, windows, ..
        } = self;
        if let Some(ws) = windows.get_mut(&wid)
            && let Some(size) = ws.win_px
            && let (Some(gpu), Some(PresentTarget::Gpu { gpu_surface, .. })) =
                (backend.gpu_mut(), ws.present.as_mut())
        {
            gpu.resize_surface(gpu_surface, size.width.max(1), size.height.max(1));
        }
    }

    /// RES-1: a control-socket `resize` verb landed on the main thread (via
    /// `Wake::Input` carrying an `InputEvent::Resize { echo_to_window: true }`).
    /// Apply the term/PTY/framebuffer resize, then ask the window to match the new
    /// grid pixel size so the on-screen geometry tracks the engine (the window
    /// `Resized` event that follows is a no-op — the grid already matches). Finally
    /// request a redraw so the resized screen is presented. Without this the verb
    /// left `App.rows/cols` + framebuffer stale and sent no Wake, so a follow-up
    /// `image`/`dims` disagreed. The interactive window-resize path uses
    /// [`Self::apply_term_resize`] directly (no `request_inner_size`) so it never
    /// fights an edge-drag.
    pub(crate) fn apply_grid_resize(&mut self, rows: u16, cols: u16) {
        // The control `resize` verb follows the active/front window.
        let Some(wid) = self.frontmost_window else {
            return;
        };
        let changed = self.apply_term_resize(wid, rows, cols);
        if !changed {
            return;
        }
        // Request the FULL visible window size (terminal rows + the tab strip above,
        // `2·pad` horizontally, and `pad_top + pad` vertically) so on-screen
        // geometry tracks the engine.
        // `window_frame_px` folds in the strip AND the pad; with both zero this keeps
        // the original request (byte-identical).
        let size = self.window_frame_px(rows, cols);
        if let Some(w) = self.front().and_then(|ws| ws.os_window.as_ref()) {
            // A best-effort request; the WM may clamp. The engine/PTY geometry is
            // already authoritative regardless of what the window settles on.
            let _ = w.request_inner_size(size);
            w.request_redraw();
        }
    }
}

fn preview_damage_from_compiled(
    compiled: &crate::native_ui::CompiledUi,
) -> Option<crate::native_app::DamageRegion> {
    let mut union: Option<crate::native_ui::LogicalRect> = None;
    for node in &compiled.paint {
        if !matches!(
            node.content,
            crate::native_ui::UiContent::SettingsPreview(_)
        ) {
            continue;
        }
        let rect = node.rect.intersect(node.clip)?.intersect(compiled.bounds)?;
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

/// Per-step input-line budget for the native `aterm-reflow` worker's chunked
/// rewrap ([`drive_reflow_job`]). The worker has a whole thread to burn, so —
/// unlike the wasm modules' `REFLOW_STEP_BUDGET_LINES = 2_000`, which bounds an
/// event-loop task — this budget exists ONLY to bound CANCELLATION LATENCY: the
/// gap between `reflow_cancel` checks. Sizing against the measured per-line cost
/// (aterm-grid's `reflow_step_timing` harness, 2026-07-14, Apple Silicon,
/// release: ~1.4 µs/input-line, stepping adds no overhead): 50_000 lines/step ≈
/// 70 ms between checks — an imperceptible teardown tail, while keeping the
/// step-loop overhead at ~zero for the multi-million-line histories the offload
/// exists for. Two honest caveats inherit from `reflow_step`'s cost contract:
/// the single step that completes a soft-wrapped run longer than the budget
/// rewraps that whole run at once (runs are capped at MAX_LOGICAL_WIDTH cells),
/// and the input-exhausting step also clears the store (O(store blocks)) — both
/// bounded, so the latency bound holds up to those constants.
pub(crate) const REFLOW_WORKER_STEP_LINES: usize = 50_000;

/// Drive a detached scrollback-reflow job to completion in bounded
/// [`reflow_step`](aterm_core::grid::PendingScrollbackReflow::reflow_step)
/// increments of `step_budget` input lines, checking `cancel` BETWEEN steps.
///
/// * `Some(reflowed)` — the completed rewrap, content-IDENTICAL to the one-shot
///   `PendingScrollbackReflow::reflow()` for any budget (aterm-grid's proven
///   `reflow_step_any_schedule_matches_one_shot` property) — pass to
///   `Terminal::finish_resize_offload` exactly as before.
/// * `None` — `cancel` was observed: the job (and the detached history it owns)
///   is DROPPED, the documented bounded-loss semantics of a dying worker, but
///   IMMEDIATE and clean instead of after the full O(history) rewrap. The
///   caller must then run the existing recovery path
///   (`Terminal::abort_resize_offload`) — the same one the worker-panic arm
///   uses — so the grid leaves the detach window bounded instead of wedged.
///
/// The `Acquire` load pairs with `Session::drop`'s `Release` store of
/// `reflow_cancel` (house style of `reader_stop`; the flag guards no data — the
/// job is owned right here — so this is belt-and-suspenders for the signal).
pub(crate) fn drive_reflow_job(
    mut job: aterm_core::grid::PendingScrollbackReflow,
    cancel: &std::sync::atomic::AtomicBool,
    step_budget: usize,
) -> Option<aterm_core::grid::ReflowedScrollback> {
    loop {
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return None; // drops `job`: bounded loss, caller aborts the offload
        }
        match job.reflow_step(step_budget) {
            aterm_core::grid::ReflowStep::InProgress(next) => job = next,
            aterm_core::grid::ReflowStep::Done(reflowed) => return Some(reflowed),
        }
    }
}

#[cfg(test)]
mod native_preview_scheduler_tests {
    use crate::{App, WindowId};

    /// Font size does not currently change a preview's cadence, so a black-box
    /// animation assertion alone cannot distinguish a global-font regression. Pair
    /// the real two-metric scheduler drive with a narrow source-wiring guard on this
    /// one call site: the scheduler must feed the same window-local font authority as
    /// semantic compilation and its paint stamp.
    #[test]
    fn settings_preview_scheduler_uses_the_target_window_font() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.font_px = 8.0;
        app.windows.get_mut(&wid).unwrap().metrics.font_px = 24.0;
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::CursorMotion));
        let (_, view) = app.active_native_view(wid).unwrap();
        let expected = match app.native_runtime.view_state(view) {
            Some(crate::native_app::AppViewState::Settings(state)) => state.preview_animation(
                100,
                app.native_view_motion_cx(wid, view),
                app.win_font_px(wid),
                app.theme,
                app.native_ui_viewport(wid).unwrap(),
            ),
            _ => panic!("Settings view state"),
        };
        assert_eq!(app.win_font_px(wid), 24.0);
        assert_eq!(
            app.active_native_settings_preview(wid, 100)
                .map(|(_, animation)| animation),
            (expected != crate::settings_preview::PreviewAnimation::None).then_some(expected)
        );

        let source = include_str!("app_render.rs");
        let body = source
            .split_once("pub(crate) fn active_native_settings_preview")
            .expect("preview scheduler definition")
            .1
            .split_once("fn append_native_modal_prims")
            .expect("preview scheduler body")
            .0;
        assert!(body.contains("self.win_font_px(wid)"));
        assert!(!body.contains("self.font_px,"));
    }
}

#[cfg(test)]
mod reflow_worker_tests {
    use super::{REFLOW_WORKER_STEP_LINES, drive_reflow_job};
    use aterm_core::grid::ReflowStep;
    use aterm_core::terminal::Terminal;
    use std::sync::atomic::AtomicBool;

    /// A terminal whose bulk history lives in the tiered store (small ring → most
    /// scroll-off spills to tiered, like a real session). Mirrors aterm-core's
    /// `offload_tests::term_with_history`.
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

    /// Full decoded history text of a terminal, for content-identity assertions.
    fn history_fingerprint(t: &Terminal) -> Vec<String> {
        (0..t.grid().scrollback_lines())
            .map(|i| {
                t.grid()
                    .get_history_line(i)
                    .map_or_else(String::new, |l| format!("{l:?}"))
            })
            .collect()
    }

    /// MID-FLIGHT CANCEL (the teardown trigger): a job that has already made
    /// partial progress is abandoned at the next step boundary; the existing
    /// abort path (the SAME recovery the worker-panic arm uses — audit #5's
    /// `abort_resize_offload`) then returns the grid to a bounded, non-wedged
    /// state. Driver-level mirror of aterm-grid's
    /// `dropping_a_half_stepped_job_recovers_via_abort`.
    #[test]
    fn cancelled_mid_flight_job_drops_and_recovers_via_existing_abort() {
        let mut t = term_with_history(24, 80, 800);
        let before = t.grid().scrollback_lines();
        assert!(before > 100, "precondition: deep tiered history ({before})");

        let job = t
            .resize_offloading_scrollback(24, 40)
            .expect("width change with tiered history detaches a job");
        // Progress the job first so the cancel provably lands MID-flight.
        let ReflowStep::InProgress(half) = job.reflow_step(5) else {
            panic!("an 800-line history cannot complete in one 5-line step");
        };
        let cancel = AtomicBool::new(true); // Session::drop raced in
        assert!(
            drive_reflow_job(half, &cancel, 5).is_none(),
            "the worker loop must observe the cancel at the next step boundary"
        );
        // The worker's cancel arm: reuse the existing recovery, not a second one.
        t.abort_resize_offload();
        assert!(
            !t.grid().reflow_offload_in_flight(),
            "abort must close the detach window (no wedge)"
        );
        // Post-abort the grid is ring-only BOUNDED: new scroll-off cannot grow an
        // un-drainable lazy backlog (the leak the abort path exists to prevent).
        let mut buf = Vec::new();
        for i in 0..5_000 {
            buf.extend_from_slice(format!("R{i}\r\n").as_bytes());
        }
        t.process(&buf);
        assert!(
            t.grid().scrollback_lines() < 1_000,
            "post-cancel grid stays bounded, got {}",
            t.grid().scrollback_lines()
        );
    }

    /// A pre-set cancel (teardown before the worker's first step) drops the job
    /// without doing ANY rewrap work — the immediate-teardown fast path.
    #[test]
    fn cancel_before_first_step_does_no_work() {
        let mut t = term_with_history(24, 80, 300);
        let job = t.resize_offloading_scrollback(24, 40).expect("job");
        let cancel = AtomicBool::new(true);
        assert!(drive_reflow_job(job, &cancel, REFLOW_WORKER_STEP_LINES).is_none());
        t.abort_resize_offload();
        assert!(!t.grid().reflow_offload_in_flight());
    }

    /// NO-CANCEL IDENTITY: an uncancelled worker loop produces a result
    /// content-IDENTICAL to the one-shot `reflow()` — chunked at a small budget
    /// here to force many steps (the production 50k budget is the same proven
    /// any-schedule property, aterm-grid's
    /// `reflow_step_any_schedule_matches_one_shot`).
    #[test]
    fn uncancelled_worker_loop_matches_one_shot() {
        let mut a = term_with_history(24, 80, 500);
        let mut b = term_with_history(24, 80, 500);
        let ja = a.resize_offloading_scrollback(24, 40).expect("job a");
        let jb = b.resize_offloading_scrollback(24, 40).expect("job b");

        let cancel = AtomicBool::new(false);
        let ra = drive_reflow_job(ja, &cancel, 7).expect("uncancelled job completes");
        let rb = jb.reflow(); // the old one-shot the worker used to run

        assert_eq!(ra.line_count(), rb.line_count());
        a.finish_resize_offload(ra);
        b.finish_resize_offload(rb);
        assert_eq!(
            history_fingerprint(&a),
            history_fingerprint(&b),
            "worker stepping must be behavior-identical when nothing cancels"
        );
    }

    /// SUPERSEDE, MEASURED (why a superseding resize does NOT cancel): while a
    /// job is in flight a second width-resize detaches nothing (the one-detach
    /// serialization), and the job's completed result STILL re-attaches with its
    /// content intact — only the wrapping is stale, and the very next width
    /// change re-detaches and re-reflows it (the documented self-heal). Cancel +
    /// abort here would instead drop the ENTIRE tiered history and leave the
    /// session permanently ring-only — a routine drag becoming data loss.
    #[test]
    fn superseding_resize_keeps_the_in_flight_history() {
        let mut t = term_with_history(24, 80, 500);
        let before = t.grid().scrollback_lines();
        assert!(before > 100);

        let job = t
            .resize_offloading_scrollback(24, 40)
            .expect("first width change detaches");
        // SUPERSEDE mid-flight: serialized, no second detach, plain bounded resize.
        assert!(
            t.resize_offloading_scrollback(24, 60).is_none(),
            "an in-flight reflow self-throttles a superseding resize to ring-only"
        );
        assert!(t.grid().reflow_offload_in_flight());

        // The worker finishes at the OLD width; the result still re-attaches.
        let cancel = AtomicBool::new(false);
        let reflowed = drive_reflow_job(job, &cancel, REFLOW_WORKER_STEP_LINES).expect("no cancel");
        t.finish_resize_offload(reflowed);
        assert!(!t.grid().reflow_offload_in_flight());
        let after = t.grid().scrollback_lines();
        assert!(
            after > 100,
            "history content survives a superseded reflow (before={before}, after={after})"
        );
        // The stale wrap self-heals: the NEXT width change re-detaches the store.
        assert!(
            t.resize_offloading_scrollback(24, 50).is_some(),
            "next width change re-reflows the width-stale store"
        );
    }
}

#[cfg(test)]
mod strip_title_lock_tests {
    use crate::{App, WindowId, term_lock};

    /// Tab-strip title reads must NEVER block the present on a busy background
    /// tab's Terminal mutex: `refill_strip_titles` try-locks each tab's term and,
    /// on contention, keeps the slot's previous (window-persistent buffer)
    /// contents — with the `"aterm"` fallback only for a fresh, never-filled slot
    /// — then converges to the live title once the lock is free again.
    #[test]
    fn strip_titles_keep_stale_on_contention() {
        let app = App::headless_for_test();
        let wid = WindowId(0);
        // Set the lone session's title through the real OSC 2 path.
        let term = app.pool.get(0).expect("session 0").term.clone();
        term_lock(&term).process(b"\x1b]2;hello\x07");

        // Uncontended: the slot reads the live title.
        let mut titles = Vec::new();
        app.refill_strip_titles(wid, &mut titles);
        assert_eq!(titles, vec!["hello".to_string()]);

        // Contended (a std `Mutex` is non-reentrant, so `try_lock` from the
        // holding thread reports `WouldBlock` — the same signal a mid-parse PTY
        // reader produces): a warm slot KEEPS its stale title, a fresh empty
        // slot falls back to "aterm", and neither read blocks.
        {
            let _busy = term.lock().unwrap();
            app.refill_strip_titles(wid, &mut titles);
            assert_eq!(
                titles,
                vec!["hello".to_string()],
                "warm slot keeps the last-read title under contention"
            );
            let mut fresh = Vec::new();
            app.refill_strip_titles(wid, &mut fresh);
            assert_eq!(
                fresh,
                vec!["aterm".to_string()],
                "fresh slot shows the default under contention"
            );
        }

        // Lock released: the next refill converges to the live title.
        term_lock(&term).process(b"\x1b]2;world\x07");
        app.refill_strip_titles(wid, &mut titles);
        assert_eq!(titles, vec!["world".to_string()]);
    }

    /// Per-frame Smart-Title composition must be served from the coordinator's
    /// label cache: refills over unchanged inputs perform zero compositions
    /// (clean-title fast path or cache hit), and a title or description change
    /// recomposes exactly once.
    #[test]
    fn strip_refill_serves_unchanged_labels_from_the_compose_cache() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        term_lock(&term).process(b"\x1b]2;build shell\x07");

        // No description + presentation-clean title: the fast path keeps the
        // raw slot verbatim and composes nothing at all.
        let mut titles = Vec::new();
        app.refill_strip_titles(wid, &mut titles);
        assert_eq!(titles, vec!["build shell".to_string()]);
        let fast = app.title_summaries.compose_runs();
        app.refill_strip_titles(wid, &mut titles);
        assert_eq!(
            app.title_summaries.compose_runs(),
            fast,
            "a clean title with no description must not compose"
        );

        // A live generated description forces real composition; unchanged
        // inputs must then hit the per-session cache.
        app.config.descriptive_titles = Some(true);
        app.config.title_summary_provider = Some(crate::app_config::TitleSummaryProvider::Builtin);
        app.note_title_activity(0);
        app.title_summaries
            .set_test_activity(0, "Compiling the release build");
        app.refill_strip_titles(wid, &mut titles);
        assert_eq!(
            titles,
            vec!["build shell · Compiling the release build".to_string()]
        );
        let warm = app.title_summaries.compose_runs();
        app.refill_strip_titles(wid, &mut titles);
        app.refill_strip_titles(wid, &mut titles);
        assert_eq!(
            titles,
            vec!["build shell · Compiling the release build".to_string()]
        );
        assert_eq!(
            app.title_summaries.compose_runs(),
            warm,
            "unchanged title+description frames must be cache hits"
        );

        // A title change invalidates and recomposes exactly once...
        term_lock(&term).process(b"\x1b]2;linker\x07");
        app.refill_strip_titles(wid, &mut titles);
        assert_eq!(
            titles,
            vec!["linker · Compiling the release build".to_string()]
        );
        assert_eq!(app.title_summaries.compose_runs(), warm + 1);
        // ...and so does a description change.
        app.title_summaries.set_test_activity(0, "Linking objects");
        app.refill_strip_titles(wid, &mut titles);
        assert_eq!(titles, vec!["linker · Linking objects".to_string()]);
        assert_eq!(app.title_summaries.compose_runs(), warm + 2);
    }

    /// The NATIVE toolbar strip path ([`crate::App::tab_titles`], driven by
    /// `refresh_window_tabs` on every tab mutation — including the tab SWITCH)
    /// obeys the same try-lock keep-stale law. Its old blocking `term_lock` per
    /// tab parked the main thread behind a flooding session's mid-`process()`
    /// reader — tab switching froze until the flooding program quit.
    #[test]
    fn tab_titles_keep_stale_on_contention_and_never_block() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        term_lock(&term).process(b"\x1b]2;hello\x07");

        // Uncontended: the live title, and the keep-stale cache warms.
        assert_eq!(app.tab_titles(wid), vec!["hello".to_string()]);

        {
            let _busy = term.lock().unwrap();
            // Contended with a WARM cache: the last-read title, no block.
            assert_eq!(
                app.tab_titles(wid),
                vec!["hello".to_string()],
                "warm cache keeps the last-read title under contention"
            );
            // Contended with a COLD cache: the presentation fallback, no block.
            if let Some(ws) = app.windows.get_mut(&wid) {
                ws.tab_title_cache.clear();
            }
            assert_eq!(
                app.tab_titles(wid),
                vec!["aterm".to_string()],
                "cold cache shows the fallback under contention"
            );
        }

        // Lock released: converges to the live title again.
        term_lock(&term).process(b"\x1b]2;world\x07");
        assert_eq!(app.tab_titles(wid), vec!["world".to_string()]);
    }
}

#[cfg(test)]
mod find_bar_splice_tests {
    use crate::{App, WindowId, term_lock};

    /// Fill the window's render scratch from the engine exactly as `snapshot()` /
    /// `redraw_window` do first, so the splice under test has a real grid to overwrite.
    fn fill_scratch(app: &mut App, wid: WindowId) -> (usize, usize) {
        let (rows, cols) = {
            let ws = app.windows.get(&wid).unwrap();
            (ws.rows as usize, ws.cols as usize)
        };
        let terminal = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        let ws = app.windows.get_mut(&wid).unwrap();
        let mut term = term_lock(&terminal);
        term.cell_frame_into(&mut ws.input_scratch, rows, cols);
        (rows, cols)
    }

    fn row_text(app: &App, wid: WindowId, idx: usize) -> String {
        let cells = &app.windows.get(&wid).unwrap().input_scratch.cells;
        cells
            .get(idx)
            .map(|row| row.iter().map(|c| c.ch).collect())
            .unwrap_or_default()
    }

    fn top_row_text(app: &App, wid: WindowId) -> String {
        row_text(app, wid, 0)
    }

    /// FRAME text of the find panel's FIELD row (the `Find: ` prompt + the well) —
    /// located through the geometry the splice recorded, so these tests never
    /// hard-code which row inside the band carries the field.
    fn field_row_text(app: &App, wid: WindowId) -> String {
        let hit = app.windows[&wid]
            .find_bar_hit
            .as_ref()
            .expect("the panel recorded its geometry");
        row_text(app, wid, app.tab_strip_rows as usize + hit.row)
    }

    /// The whole find panel band as one string (rows joined) — for content assertions
    /// that don't care which row of the panel a run landed on.
    fn panel_text(app: &App, wid: WindowId) -> String {
        let hit = app.windows[&wid]
            .find_bar_hit
            .as_ref()
            .expect("the panel recorded its geometry");
        let strip = app.tab_strip_rows as usize;
        hit.band
            .clone()
            .map(|row| row_text(app, wid, strip + row))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Seed the open find's query the way a user's typing does — through the one seam
    /// that keeps the caret on a char boundary at the end of the text.
    fn seed_query(app: &mut App, wid: WindowId, query: &str) {
        app.windows
            .get_mut(&wid)
            .unwrap()
            .search
            .as_mut()
            .unwrap()
            .set_query(query.to_string());
    }

    /// End-to-end: the Cmd-F find PANEL becomes VISIBLE on the TOP terminal rows once
    /// find mode is active — the FIND-1 gap (search was wired but only surfaced in the
    /// window title). Uses the REAL search path (`search_recompute` over live content)
    /// so the match count on the panel reflects the engine, and asserts the splice is a
    /// no-op before searching (byte-identical frame). The matches sit BELOW the panel's
    /// band, so this exercises the DEFAULT top placement (no adaptive float).
    #[test]
    fn find_bar_appears_on_top_row_while_searching() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);

        // Two "foo" hits on live row 4 — clear of the rows the panel now owns.
        let term = app.pool.get(0).expect("session 0").term.clone();
        term_lock(&term).process(b"\r\n\r\n\r\n\r\nfoo and foo here\r\n");

        // BEFORE searching: the splice is a no-op — no find bar on the top row.
        fill_scratch(&mut app, wid);
        app.splice_find_bar(wid);
        assert!(
            !top_row_text(&app, wid).contains("Find:"),
            "no find bar before Cmd-F"
        );

        // Enter find mode (the real entry point) and run the real match over live content.
        app.search_enter();
        seed_query(&mut app, wid, "foo");
        app.search_recompute();
        assert_eq!(
            app.windows
                .get(&wid)
                .unwrap()
                .search
                .as_ref()
                .unwrap()
                .matches
                .len(),
            2,
            "engine finds both 'foo' hits"
        );

        // Render + splice: the panel's band owns the TOP rows, its field row shows the
        // query and the 1-based position, and the matches' own row is left uncovered.
        fill_scratch(&mut app, wid);
        app.splice_find_bar(wid);
        let band = app.windows[&wid]
            .find_bar_hit
            .as_ref()
            .expect("panel geometry")
            .band
            .clone();
        assert_eq!(
            band,
            0..crate::find_bar::FIND_BAR_ROWS,
            "the panel owns the top band"
        );
        let row = field_row_text(&app, wid);
        assert!(
            row.contains("Find: "),
            "find panel shows the prompt: {row:?}"
        );
        assert!(row.contains("foo"), "find panel shows the query: {row:?}");
        assert!(
            row.contains("1/2"),
            "find panel shows the match position: {row:?}"
        );
        assert!(
            row_text(&app, wid, 4).contains("foo and foo here"),
            "the match row stays uncovered below the panel"
        );

        // Exiting find mode restores a byte-identical frame (no find bar).
        app.search_exit();
        fill_scratch(&mut app, wid);
        app.splice_find_bar(wid);
        assert!(
            !top_row_text(&app, wid).contains("Find:"),
            "find bar gone after Esc"
        );
    }

    /// A query with no hits paints a `no matches` find bar (not a silent/blank frame),
    /// and the bar is still exactly `cols` wide.
    #[test]
    fn find_bar_shows_no_matches() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        term_lock(&term).process(b"nothing to see\r\n");

        app.search_enter();
        seed_query(&mut app, wid, "zzz");
        app.search_recompute();

        let (_rows, cols) = fill_scratch(&mut app, wid);
        app.splice_find_bar(wid);
        // No matches ⇒ no current match to protect ⇒ the panel sits at its default TOP.
        let band = app.windows[&wid]
            .find_bar_hit
            .as_ref()
            .expect("panel geometry")
            .band
            .clone();
        assert_eq!(band, 0..crate::find_bar::FIND_BAR_ROWS);
        let cells = &app.windows[&wid].input_scratch.cells;
        for row in band {
            assert_eq!(cells[row].len(), cols, "every spliced row stays full width");
        }
        let field = field_row_text(&app, wid);
        assert!(field.contains("zzz"), "{field:?}");
        assert!(field.contains("no matches"), "{field:?}");
    }

    /// A terminal SHORTER than the panel degrades instead of panicking or vanishing:
    /// the band shrinks to the rows available (dropping the pad first, then the hints)
    /// and the field row always survives, still exactly `cols` wide.
    #[test]
    fn find_panel_degrades_on_a_terminal_shorter_than_the_band() {
        for rows in [1u16, 2, 3] {
            let mut app = App::headless_for_test();
            let wid = app.insert_logical_window(crate::stub_session(1), rows, 60);
            app.frontmost_window = Some(wid);
            app.search_enter();
            seed_query(&mut app, wid, "q");
            let (_, cols) = fill_scratch(&mut app, wid);
            app.splice_find_bar(wid);
            let hit = app.windows[&wid]
                .find_bar_hit
                .clone()
                .expect("the panel still paints on a short terminal");
            assert_eq!(
                hit.band,
                0..usize::from(rows).min(crate::find_bar::FIND_BAR_ROWS),
                "band clamps to the rows available (rows={rows})"
            );
            assert!(hit.band.contains(&hit.row), "the field row survives");
            let cells = &app.windows[&wid].input_scratch.cells;
            for row in hit.band.clone() {
                assert_eq!(cells[row].len(), cols, "rows={rows}");
            }
            assert!(
                field_row_text(&app, wid).contains("Find: "),
                "rows={rows} keeps the field"
            );
        }
    }

    /// The real compositor must not walk deep history on every presented frame.
    /// With 100,000 sorted hits and only one terminalful visible, work is bounded
    /// by two binary searches plus the visible rows—not by total history.
    #[test]
    fn find_bar_100k_history_work_is_logarithmic_plus_visible() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.search_enter();
        let (rows, _) = fill_scratch(&mut app, wid);
        let base_y = app.windows[&wid].input_scratch.base_y;
        let display_offset = app.windows[&wid].input_scratch.display_offset;
        assert_eq!(display_offset, 0, "fixture starts at the live viewport");
        {
            let search = app.windows.get_mut(&wid).unwrap().search.as_mut().unwrap();
            search.query = "x".into();
            search.matches = (-99_976..24).map(|row| (row, 0, 0)).collect();
            search.current = 99_999;
            search.match_base_y = base_y;
            assert_eq!(search.matches.len(), 100_000);
        }

        app.splice_find_bar(wid);

        let ws = &app.windows[&wid];
        assert!(
            ws.find_bar_match_work <= rows + 40,
            "two <=17-step lower bounds plus <=rows visible visits; got {} for {rows} rows",
            ws.find_bar_match_work
        );
        assert!(
            ws.find_bar_match_work >= rows,
            "fixture must witness the visible match visits"
        );
    }

    /// When the terminal cursor sits on the top line (⌘F on a fresh screen whose prompt
    /// is at home), the splice hides it so its block doesn't paint over the bar's caret.
    #[test]
    fn find_bar_hides_cursor_on_covered_row() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        // Park the cursor on the top row (CUP home) — the row the bar now owns.
        term_lock(&term).process(b"\x1b[1;1H");
        app.search_enter();
        seed_query(&mut app, wid, "x");
        app.search_recompute();
        let (rows, _cols) = fill_scratch(&mut app, wid);
        // Precondition: the engine put the cursor on the top row (and the terminal is
        // tall enough that the bar's default placement is that same row).
        assert!(rows > 1);
        assert_eq!(app.windows.get(&wid).unwrap().input_scratch.cursor_row, 0);
        app.splice_find_bar(wid);
        assert!(
            !app.windows.get(&wid).unwrap().input_scratch.cursor_visible,
            "cursor hidden while the find bar covers its row"
        );
    }

    #[test]
    fn find_bar_owns_its_entire_effect_band_without_sparkle_or_sprite_bleed() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.search_enter();
        seed_query(&mut app, wid, "absent");
        app.search_recompute();
        fill_scratch(&mut app, wid);
        let (_, ch) = app.win_cell_size(wid);
        let input = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
        input.ink.push(aterm_render::InkCell {
            row: 0,
            col: 0,
            color: [255, 0, 255],
        });
        input.word_decorations.push(aterm_render::WordDecoration {
            // Row 1 jitters one pixel upward into the top bar; row-tag-only
            // scrubbing would miss this neighbour spill.
            row: 1,
            col: 0,
            dx: 0,
            dy: -1,
            glyph: aterm_render::DecoGlyph::Star4,
            blend: aterm_render::DecoBlend::Add,
            color: 0x00ff_00ff,
            alpha: 255,
        });
        input.nova_add.push(aterm_render::GlowQuad {
            row: 0,
            ..Default::default()
        });
        input.cat_quads.push(aterm_render::SpriteQuad {
            row: 0,
            ..Default::default()
        });
        input.rain_quads.push(aterm_render::SpriteQuad {
            row: 0,
            ..Default::default()
        });
        input.rain_add.push(aterm_render::RainHalo {
            row: 0,
            ..Default::default()
        });
        input.free_sprites.push(aterm_core::render::FreeSprite {
            y: 0,
            h: u16::try_from(ch).unwrap_or(u16::MAX),
            w: 1,
            ..Default::default()
        });

        app.splice_find_bar(wid);
        let input = &app.windows[&wid].input_scratch;
        assert!(input.ink.is_empty());
        assert!(input.word_decorations.is_empty());
        assert!(input.nova_add.is_empty());
        assert!(input.cat_quads.is_empty());
        assert!(input.rain_quads.is_empty());
        assert!(input.rain_add.is_empty());
        assert!(input.free_sprites.is_empty());
    }

    #[test]
    fn bottom_floated_find_bar_scrubs_upward_neighbour_decoration_spill() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.search_enter();
        fill_scratch(&mut app, wid);
        let rows = app.windows[&wid].input_scratch.rows;
        let base_y = app.windows[&wid].input_scratch.base_y;
        let band_rows = crate::find_bar::FIND_BAR_ROWS;
        assert!(rows >= band_rows + 2);
        {
            let search = app.windows.get_mut(&wid).unwrap().search.as_mut().unwrap();
            search.query = "x".into();
            search.matches = vec![(0, 0, 0)];
            search.current = 0;
            search.match_base_y = base_y;
        }
        app.windows
            .get_mut(&wid)
            .unwrap()
            .input_scratch
            .word_decorations
            .push(aterm_render::WordDecoration {
                // The row directly ABOVE the floated band, jittering one pixel down into
                // it; row-tag-only scrubbing would miss this neighbour spill.
                row: u16::try_from(rows - band_rows - 1).unwrap_or(u16::MAX),
                col: 0,
                dx: 0,
                dy: 1,
                glyph: aterm_render::DecoGlyph::Star4,
                blend: aterm_render::DecoBlend::Add,
                color: 0x00ff_00ff,
                alpha: 255,
            });
        app.splice_find_bar(wid);
        let ws = &app.windows[&wid];
        let hit = ws.find_bar_hit.as_ref().unwrap();
        assert_eq!(hit.band, rows - band_rows..rows, "panel floated to the bottom");
        assert_eq!(hit.row, rows - band_rows + 1, "field row inside that band");
        assert!(ws.input_scratch.word_decorations.is_empty());
    }

    /// End-to-end: `search_recompute` honors the case + regex toggles through the real
    /// App path, and flags an invalid regex — the machinery the ⌥⌘C/⌥⌘R chords drive.
    #[test]
    fn recompute_honors_case_and_regex_toggles() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        term_lock(&term).process(b"foo Foo FOO bar\r\n");
        app.search_enter();

        let total = |app: &App| {
            app.windows
                .get(&wid)
                .unwrap()
                .search
                .as_ref()
                .unwrap()
                .matches
                .len()
        };
        let set = |app: &mut App, f: &mut dyn FnMut(&mut crate::app_search::SearchState)| {
            f(app.windows.get_mut(&wid).unwrap().search.as_mut().unwrap());
        };

        // Default literal, case-insensitive: `foo` hits foo/Foo/FOO = 3.
        set(&mut app, &mut |s| s.query = "foo".to_string());
        app.search_recompute();
        assert_eq!(total(&app), 3, "case-insensitive foo");

        // ^S → case-sensitive: only the lowercase `foo` = 1.
        set(&mut app, &mut |s| s.case_sensitive = true);
        app.search_recompute();
        assert_eq!(total(&app), 1, "case-sensitive foo");

        // ^R → regex (case-insensitive again): `f.o` matches all three.
        set(&mut app, &mut |s| {
            s.case_sensitive = false;
            s.is_regex = true;
            s.query = "f.o".to_string();
        });
        app.search_recompute();
        assert_eq!(total(&app), 3, "regex f.o");

        // Invalid regex → flagged, zero matches (the bar's "bad regex" state).
        set(&mut app, &mut |s| s.query = "(".to_string());
        app.search_recompute();
        let ws = app.windows.get(&wid).unwrap();
        let s = ws.search.as_ref().unwrap();
        assert!(s.regex_error, "bad regex flagged");
        assert!(s.matches.is_empty(), "bad regex yields no matches");
    }

    /// The highlight-all colour: bottom-terminal bg blended halfway toward the theme's
    /// selection tone (mirrors `splice_find_bar`, default theme + no OSC 11 in headless).
    fn highlight_bg() -> [u8; 3] {
        let t = aterm_render::Theme::default();
        let bg = crate::settings::u32_rgb(t.bg);
        let sel = crate::settings::u32_rgb(t.selection);
        [
            ((u16::from(bg[0]) + u16::from(sel[0])) / 2) as u8,
            ((u16::from(bg[1]) + u16::from(sel[1])) / 2) as u8,
            ((u16::from(bg[2]) + u16::from(sel[2])) / 2) as u8,
        ]
    }

    /// Highlight-all tints the bg of EVERY visible match, not just the current one — the
    /// `display_offset == 0` (live, unscrolled) case where screen row == selection row.
    #[test]
    fn highlight_all_tints_visible_matches() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        // "hit" at cols 3..=5 on live rows 0 and 1.
        term_lock(&term).process(b"aa hit bb\r\ncc hit dd\r\n");
        app.search_enter();
        seed_query(&mut app, wid, "hit");
        app.search_recompute();
        assert_eq!(
            app.windows.get(&wid).unwrap().input_scratch.display_offset,
            0,
            "live matches keep the viewport at the bottom"
        );
        fill_scratch(&mut app, wid);
        app.splice_find_bar(wid);
        let hi = highlight_bg();
        let cells = &app.windows.get(&wid).unwrap().input_scratch.cells;
        for row in [0usize, 1] {
            for (i, cell) in cells[row][3..=5].iter().enumerate() {
                assert_eq!(cell.bg, hi, "match cell ({row},{}) tinted", 3 + i);
            }
            // A non-match cell on the same row keeps its ordinary bg.
            assert_ne!(cells[row][0].bg, hi, "non-match cell ({row},0) untinted");
        }
    }

    /// Highlight-all maps a SCROLLBACK match through `display_offset`: after find scrolls
    /// the top (current) match into view, it lands on screen row 1 — one row BELOW the
    /// top-anchored bar (`search_apply_current`'s below-bar landing) — highlighted; the
    /// offset-sign check (`screen = sel_row + display_offset`).
    #[test]
    fn highlight_all_maps_scrollback_current_below_top_bar() {
        // Pushes matches into SCROLLBACK, so it assumes the default (large) search cap —
        // serialize against the cap-mutation test that lowers the process-global cap.
        let _serial = crate::control::search_cap_test_guard();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let rows = app.windows.get(&wid).unwrap().rows as usize;
        let term = app.pool.get(0).expect("session 0").term.clone();
        // Non-matching filler ABOVE the first hit, so the oldest match (matches[0]) has
        // history above it and can land one row BELOW the top bar rather than clamping at
        // the very top of scrollback. Then more matching lines than the window ⇒ the
        // earliest hits fall into scrollback.
        let mut input = Vec::new();
        for i in 0..3 {
            input.extend_from_slice(format!("topfill {i}\r\n").as_bytes());
        }
        for _ in 0..(rows + 6) {
            input.extend_from_slice(b"hit here\r\n"); // "hit" at cols 0..=2
        }
        term_lock(&term).process(&input);
        app.search_enter();
        seed_query(&mut app, wid, "hit");
        app.search_recompute();
        // current = matches[0] = the oldest hit, which is in scrollback; find scrolled it up.
        let cur_row = app
            .windows
            .get(&wid)
            .unwrap()
            .search
            .as_ref()
            .unwrap()
            .matches[0]
            .0;
        assert!(cur_row < 0, "top match is in scrollback (row {cur_row})");
        // fill_scratch syncs input_scratch.display_offset from the (now-scrolled) terminal,
        // exactly as the real redraw does before the splice.
        fill_scratch(&mut app, wid);
        assert!(
            app.windows.get(&wid).unwrap().input_scratch.display_offset > 0,
            "viewport scrolled up to show the top match"
        );
        app.splice_find_bar(wid);
        // The current (scrollback) match landed on the first row BELOW the panel band,
        // cols 0..=2, highlighted; the panel itself owns the rows above it.
        let hi = highlight_bg();
        let landing = crate::find_bar::FIND_BAR_ROWS;
        let panel = panel_text(&app, wid);
        assert!(panel.contains("hit"), "panel echoes the query: {panel:?}");
        let cells = &app.windows.get(&wid).unwrap().input_scratch.cells;
        for (col, cell) in cells[landing][0..=2].iter().enumerate() {
            assert_eq!(
                cell.bg, hi,
                "scrollback match tinted at screen row {landing} (below the panel), col {col}"
            );
        }
    }

    /// ADAPTIVE PLACEMENT (#7): when the CURRENT match sits on the top row (where the
    /// default top bar would hide it), the bar floats to the BOTTOM row instead — the
    /// match stays visible on the top row, and the seam flips to an overline (facing the
    /// content above), not the usual bottom-edge underline.
    #[test]
    fn find_bar_floats_to_bottom_when_current_match_on_top() {
        use aterm_core::terminal::UnderlineStyle;
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let rows = app.windows.get(&wid).unwrap().rows as usize;
        let term = app.pool.get(0).expect("session 0").term.clone();
        // The ONLY match sits at home — so `matches[0]` (the current one) is on the top
        // live row, exactly where the default bar placement would cover it.
        term_lock(&term).process(b"hit");
        app.search_enter();
        seed_query(&mut app, wid, "hit");
        app.search_recompute();
        // The single match is on the top row (selection row 0, viewport unscrolled).
        let cur = app
            .windows
            .get(&wid)
            .unwrap()
            .search
            .as_ref()
            .unwrap()
            .matches[0]
            .0;
        assert_eq!(cur, 0, "current match is on the top row");

        fill_scratch(&mut app, wid);
        app.splice_find_bar(wid);
        let band_rows = crate::find_bar::FIND_BAR_ROWS;
        let band = app.windows[&wid]
            .find_bar_hit
            .as_ref()
            .expect("panel geometry")
            .band
            .clone();
        assert_eq!(band, rows - band_rows..rows, "panel floated to the bottom");
        // The current match is UNCOVERED on the top row; the panel's field row echoes it.
        let field = field_row_text(&app, wid);
        assert!(
            field.contains("Find: ") && field.contains("hit"),
            "floated panel keeps its field: {field:?}"
        );
        let cells = &app.windows.get(&wid).unwrap().input_scratch.cells;
        let top: String = cells[0].iter().map(|c| c.ch).collect();
        assert!(
            top.contains("hit"),
            "current match still visible on top: {top:?}"
        );
        // The seam is now an overline on the band's TOP row, never a bottom underline.
        assert!(
            cells[band.start].iter().any(|c| c.overline),
            "bottom-floated panel draws an overline seam on its top edge"
        );
        assert!(
            band.clone()
                .all(|row| cells[row].iter().all(|c| c.underline == UnderlineStyle::None)),
            "bottom-floated panel drops the underline seam"
        );
    }

    /// A frame extracted after a non-uniform absolute-row splice must never use
    /// pre-splice cached match rows. The bar remains visible, but its adaptive
    /// placement and highlight-all tint fail closed until search recomputes.
    #[test]
    fn find_bar_suppresses_stale_absolute_row_geometry() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        // Current match at row 0 would normally float the bar to the bottom;
        // a second visible match witnesses whether highlight-all still ran.
        term_lock(&term).process(b"hit\r\nhit");
        app.search_enter();
        seed_query(&mut app, wid, "hit");
        app.search_recompute();
        assert_eq!(
            app.windows[&wid].search.as_ref().unwrap().matches[0].0,
            0,
            "fixture current match is on the top row"
        );

        fill_scratch(&mut app, wid);
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            let search_revision = ws.search.as_ref().unwrap().match_absolute_row_revision;
            assert_eq!(
                ws.input_scratch.absolute_row_revision, search_revision,
                "freshly extracted fixture starts coherent"
            );
            // Model a newer frame extracted after a protected-footer splice,
            // before the UI wake has recomputed the open search.
            ws.input_scratch.absolute_row_revision = search_revision.saturating_add(1);
        }

        app.splice_find_bar(wid);
        let field = field_row_text(&app, wid);
        assert!(
            field.contains("Find: ") && field.contains("hit"),
            "stale geometry does not hide the panel: {field:?}"
        );
        let ws = &app.windows[&wid];
        assert_eq!(
            ws.find_bar_hit.as_ref().map(|hit| hit.band.clone()),
            Some(0..crate::find_bar::FIND_BAR_ROWS),
            "stale current-row geometry cannot float the panel"
        );
        let hi = highlight_bg();
        assert!(
            ws.input_scratch
                .cells
                .iter()
                .flatten()
                .all(|cell| cell.bg != hi),
            "stale cached matches cannot tint a newer frame"
        );
        assert_eq!(
            ws.find_bar_match_work, 0,
            "stale-frame gate skips the cached match walk"
        );
    }

    /// CONTRAST FLOOR (#8): a non-current highlighted match whose fg is close to the
    /// highlight tint is lifted to a legible contrast (WCAG floor), while the CURRENT
    /// match's fg is left untouched — the renderer paints the full selection over it and
    /// applies its own selection-fg floor.
    #[test]
    fn highlight_floors_noncurrent_fg_for_contrast() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        let hi = highlight_bg();
        // Row 0: default-fg "hit" (becomes the CURRENT match). Row 1: "hit" whose fg is set
        // to the tint itself (worst-case 1:1 contrast) via a truecolor SGR — NON-current.
        let sgr = format!("\x1b[38;2;{};{};{}m", hi[0], hi[1], hi[2]);
        let input = format!("hit\r\n{sgr}hit\x1b[0m\r\n");
        term_lock(&term).process(input.as_bytes());
        app.search_enter();
        seed_query(&mut app, wid, "hit");
        app.search_recompute();

        fill_scratch(&mut app, wid);
        // The current match (row 0) fg BEFORE the splice — it must survive untouched.
        let cur_fg_before = app.windows.get(&wid).unwrap().input_scratch.cells[0][0].fg;
        app.splice_find_bar(wid);

        let expected = crate::settings::u32_rgb(aterm_render::floor_selection_fg(
            aterm_render::rgb_to_u32(hi),
            aterm_render::rgb_to_u32(hi),
        ));
        let cells = &app.windows.get(&wid).unwrap().input_scratch.cells;
        // Row 1 (non-current): bg tinted AND fg lifted off the tint to the WCAG floor.
        for (col, cell) in cells[1][0..=2].iter().enumerate() {
            assert_eq!(cell.bg, hi, "non-current match bg tinted (col {col})");
            assert_ne!(
                cell.fg, hi,
                "non-current match fg lifted off the tint (col {col})"
            );
            assert_eq!(
                cell.fg, expected,
                "non-current match fg floored (col {col})"
            );
        }
        // Row 0 (current): fg unchanged — deferred to the renderer's selection floor.
        for (col, cell) in cells[0][0..=2].iter().enumerate() {
            assert_eq!(
                cell.fg, cur_fg_before,
                "current match fg untouched (col {col})"
            );
        }

        // INDEPENDENT direction/contrast check (not just `== floor_selection_fg(...)`,
        // which would only prove the splice calls the same fn the assertion does): a
        // from-scratch WCAG ratio confirms the floored fg genuinely clears the ~4.5:1
        // legibility target against the tint, whereas the raw tint-on-tint fg it replaced
        // sits at the 1:1 worst case. Standard sRGB→linear + relative-luminance formula.
        fn wcag_ratio(a: [u8; 3], b: [u8; 3]) -> f32 {
            fn lin(c: u8) -> f32 {
                let s = f32::from(c) / 255.0;
                if s <= 0.03928 {
                    s / 12.92
                } else {
                    ((s + 0.055) / 1.055).powf(2.4)
                }
            }
            let lum = |c: [u8; 3]| 0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2]);
            let (la, lb) = (lum(a), lum(b));
            let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
            (hi + 0.05) / (lo + 0.05)
        }
        assert!(
            wcag_ratio(hi, hi) < 1.01,
            "sanity: tint-on-tint (the fg the floor replaced) is the 1:1 worst case"
        );
        let achieved = wcag_ratio(expected, hi);
        assert!(
            achieved >= 4.4,
            "floored fg independently clears WCAG ~4.5:1 against the tint (got {achieved:.2}:1)"
        );
    }

    /// STICKY TOGGLES (#10): the match-case / regex modes are remembered across find
    /// sessions — toggling them, closing find, and reopening restores the same modes
    /// rather than snapping back to the literal / case-insensitive defaults.
    #[test]
    fn find_toggles_persist_across_sessions() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.search_enter();
        // Fresh find starts at the defaults.
        {
            let s = app.windows.get(&wid).unwrap().search.as_ref().unwrap();
            assert!(
                !s.case_sensitive && !s.is_regex,
                "defaults: literal, insensitive"
            );
        }
        app.search_toggle_case();
        app.search_toggle_regex();
        // Close and reopen — the sticky app state seeds the new session.
        app.search_exit();
        app.search_enter();
        let s = app.windows.get(&wid).unwrap().search.as_ref().unwrap();
        assert!(s.case_sensitive, "case toggle persisted across sessions");
        assert!(s.is_regex, "regex toggle persisted across sessions");
    }

    /// CLICKABLE TOGGLES (#10): a click on the `Aa` / `.*` indicators — hit-tested against
    /// the geometry `splice_find_bar` recorded — flips match-case / regex (and the sticky
    /// default), while a click off the indicators is not consumed and changes nothing.
    #[test]
    fn clicking_indicators_toggles_via_recorded_geometry() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        term_lock(&term).process(b"click target here\r\n");
        app.search_enter();
        seed_query(&mut app, wid, "target");
        app.search_recompute();
        // The splice records where the indicators landed.
        fill_scratch(&mut app, wid);
        app.splice_find_bar(wid);
        let hit = app
            .windows
            .get(&wid)
            .unwrap()
            .find_bar_hit
            .clone()
            .expect("splice recorded the indicator geometry");
        let row = u16::try_from(hit.row).unwrap();
        let case_col = u16::try_from(hit.case_cols.expect("Aa drawn").start).unwrap();
        let regex_col = u16::try_from(hit.regex_cols.expect(".* drawn").start).unwrap();

        // Click `Aa` → match-case on (state + sticky), and the click is consumed.
        assert!(app.find_bar_click(wid, row, case_col), "Aa click consumed");
        assert!(
            app.windows
                .get(&wid)
                .unwrap()
                .search
                .as_ref()
                .unwrap()
                .case_sensitive
        );
        assert!(app.search_sticky_case, "click updated the sticky default");
        // Click `.*` → regex on.
        assert!(app.find_bar_click(wid, row, regex_col), ".* click consumed");
        assert!(
            app.windows
                .get(&wid)
                .unwrap()
                .search
                .as_ref()
                .unwrap()
                .is_regex
        );
        assert!(app.search_sticky_regex);
        // A click OFF the indicators but ON the panel (col 0, the `Find:` prompt) is
        // CONSUMED — the band is chrome, not selectable terminal output — and flips
        // nothing.
        assert!(
            app.find_bar_click(wid, row, 0),
            "a click on the panel is consumed"
        );
        let s = app.windows.get(&wid).unwrap().search.as_ref().unwrap();
        assert!(
            s.case_sensitive && s.is_regex,
            "off-indicator click changed no toggle"
        );
        // A click OFF the panel entirely falls through to the terminal untouched.
        let below = u16::try_from(hit.band.end).unwrap();
        assert!(
            !app.find_bar_click(wid, below, 0),
            "a click below the panel is not consumed"
        );

        // A click INSIDE the well puts the caret on the character under the pointer —
        // the text-field behaviour, hit-tested through the recorded well geometry.
        let field = hit.field_cols.clone();
        let caret_col = u16::try_from(field.start + 3).unwrap();
        assert!(app.find_bar_click(wid, row, caret_col), "well click consumed");
        let s = app.windows.get(&wid).unwrap().search.as_ref().unwrap();
        assert_eq!(s.query, "target", "clicking the well never edits the text");
        assert_eq!(s.cursor, 3, "caret landed on the clicked character");
        // …on BOTH sides of the caret. The well maps cell → character directly (the
        // block caret costs no cell), so a click to the RIGHT of the caret must not
        // land one character short.
        app.search_edit_in(wid, crate::app_search::SearchEdit::MoveStart);
        let right_of_caret = u16::try_from(field.start + 4).unwrap();
        assert!(app.find_bar_click(wid, row, right_of_caret), "well click consumed");
        assert_eq!(
            app.windows[&wid].search.as_ref().unwrap().cursor,
            4,
            "a click right of the caret lands on the clicked character"
        );
        // Past the end of the text, the caret parks at the end (never mid-nowhere).
        let past = u16::try_from(field.start + 40).unwrap();
        assert!(app.find_bar_click(wid, row, past), "well click consumed");
        assert_eq!(
            app.windows[&wid].search.as_ref().unwrap().cursor,
            "target".len(),
            "a click past the text parks the caret at the end"
        );
    }

    /// STRIP OFFSET (#7 regression): with a tab strip prepended, `splice_find_bar` must
    /// place the bar and every match tint by their FRAME index (`strip + terminal_row`),
    /// not the raw terminal row — otherwise the bar lands one row high and the tints slide
    /// up into the strip. The current match here sits on the TOP terminal row, so this
    /// exercises the FLOATED (bottom) placement: the bar must land on the true frame
    /// bottom while the two matches are tinted at their strip-shifted rows, with the
    /// strip row itself untouched. (The default TOP placement + strip is pinned by
    /// `find_bar_default_top_sits_below_tab_strip`.)
    #[test]
    fn find_bar_splices_below_tab_strip() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        let term = app.pool.get(0).expect("session 0").term.clone();
        // "hit" on terminal rows 0 and 1 (two matches; matches[0] is the current one).
        term_lock(&term).process(b"hit one\r\nhit two\r\n");
        app.search_enter();
        seed_query(&mut app, wid, "hit");
        app.search_recompute();

        let (rows, _) = fill_scratch(&mut app, wid); // input_scratch = `rows` terminal rows
        app.splice_tab_strip_with(wid, 1); // prepend the 1-row strip → `rows + 1` frame rows
        app.splice_find_bar(wid);

        let hi = highlight_bg();
        let band_rows = crate::find_bar::FIND_BAR_ROWS;
        let hit = app.windows[&wid]
            .find_bar_hit
            .clone()
            .expect("panel geometry");
        // Panel band ends on the TRUE frame bottom (`strip + term_bottom = rows`), so in
        // TERMINAL rows it is `rows - band_rows .. rows`, not one row high.
        assert_eq!(hit.band, rows - band_rows..rows, "panel on the frame bottom");
        let field = field_row_text(&app, wid);
        assert!(
            field.contains("Find: ") && field.contains("hit"),
            "the field row rides inside that band: {field:?}"
        );
        let cells = &app.windows.get(&wid).unwrap().input_scratch.cells;
        assert_eq!(cells.len(), rows + 1, "strip added exactly one frame row");
        let above_band: String = cells[rows - band_rows].iter().map(|c| c.ch).collect();
        assert!(
            !above_band.contains("Find:"),
            "panel is NOT one row high (would be the off-by-strip bug): {above_band:?}"
        );
        // Matches from terminal rows 0 and 1 are tinted at FRAME rows strip+0=1 and strip+1=2.
        for (term_row, frame_row) in [(0usize, 1usize), (1, 2)] {
            for (col, cell) in cells[frame_row][0..=2].iter().enumerate() {
                assert_eq!(
                    cell.bg, hi,
                    "match from term row {term_row} tinted at frame row {frame_row}, col {col}"
                );
            }
        }
        // The tab-strip row (frame 0) is the strip's own chrome — never the match tint.
        assert!(
            cells[0].iter().all(|c| c.bg != hi),
            "the tab-strip row is untouched by the highlight tint"
        );
    }

    /// STRIP OFFSET, DEFAULT (top) placement: with a tab strip prepended and the current
    /// match AWAY from the top terminal row, the bar lands at frame row `strip + 0` —
    /// directly BELOW the strip — never overwriting the strip's own chrome row, and the
    /// match tints land at their strip-shifted frame rows.
    #[test]
    fn find_bar_default_top_sits_below_tab_strip() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        let term = app.pool.get(0).expect("session 0").term.clone();
        // "hit" on terminal rows 4 and 5 — clear of the band the panel defaults to.
        term_lock(&term).process(b"\r\n\r\n\r\n\r\nhit one\r\nhit two\r\n");
        app.search_enter();
        seed_query(&mut app, wid, "hit");
        app.search_recompute();

        let (rows, _) = fill_scratch(&mut app, wid);
        app.splice_tab_strip_with(wid, 1);
        app.splice_find_bar(wid);

        let hi = highlight_bg();
        let hit = app.windows[&wid]
            .find_bar_hit
            .clone()
            .expect("panel geometry");
        assert_eq!(
            hit.band,
            0..crate::find_bar::FIND_BAR_ROWS,
            "panel band starts at the first TERMINAL row (below the strip)"
        );
        // Panel directly below the strip: its field row is a FRAME row ≥ strip + 0 = 1.
        let field = field_row_text(&app, wid);
        assert!(
            field.contains("Find: ") && field.contains("hit"),
            "panel directly below the tab strip: {field:?}"
        );
        let cells = &app.windows.get(&wid).unwrap().input_scratch.cells;
        assert_eq!(cells.len(), rows + 1, "strip added exactly one frame row");
        let strip_row: String = cells[0].iter().map(|c| c.ch).collect();
        assert!(
            !strip_row.contains("Find:"),
            "the strip row keeps its own chrome: {strip_row:?}"
        );
        // Matches from terminal rows 4 and 5 are tinted at FRAME rows 5 and 6.
        for (term_row, frame_row) in [(4usize, 5usize), (5, 6)] {
            for (col, cell) in cells[frame_row][0..=2].iter().enumerate() {
                assert_eq!(
                    cell.bg, hi,
                    "match from term row {term_row} tinted at frame row {frame_row}, col {col}"
                );
            }
        }
    }

    /// ⏎ ACCEPT (emacs RET): closing find keeps the viewport where find navigation left
    /// it, keeps the current match SELECTED (ready for ⌘C), and remembers the query for
    /// `^S`/`^R` empty-query recall in the next session.
    #[test]
    fn accept_keeps_viewport_and_remembers_query() {
        // Pushes matches into SCROLLBACK — serialize against the search-cap mutation test.
        let _serial = crate::control::search_cap_test_guard();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let rows = app.windows.get(&wid).unwrap().rows as usize;
        let term = app.pool.get(0).expect("session 0").term.clone();
        let mut input = Vec::new();
        for _ in 0..(rows + 6) {
            input.extend_from_slice(b"hit here\r\n");
        }
        term_lock(&term).process(&input);

        app.search_enter();
        seed_query(&mut app, wid, "hit");
        app.search_recompute();
        // The current (oldest) match is in scrollback: find scrolled the viewport up.
        let offset = term_lock(&term).grid().display_offset();
        assert!(offset > 0, "find navigation scrolled into scrollback");

        app.search_accept();
        assert!(
            app.windows.get(&wid).unwrap().search.is_none(),
            "accept closed find mode"
        );
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            offset,
            "accept left the viewport at the match"
        );
        assert!(
            term_lock(&term).selection_to_string().is_some(),
            "accept kept the match selected"
        );
        assert_eq!(
            app.search_last_query, "hit",
            "accept remembered the query for recall"
        );
        let (accepted_session, _, accepted_row, accepted_start, accepted_end) = app
            .search_last_anchor
            .expect("accept retained the absolute current-match anchor");
        assert_eq!(accepted_session, 0, "anchor is bound to session 0");
        let accepted_anchor = (accepted_row, accepted_start, accepted_end);

        // Standard Cmd-G after Enter closed the bar must reopen the accepted
        // query and continue strictly AFTER that match, not silently no-op or
        // restart on the same first hit.
        app.search_find_again(true);
        let resumed = app.windows[&wid]
            .search
            .as_ref()
            .expect("Find Next reopened the accepted query");
        assert_eq!(resumed.query, "hit");
        let (row, start, end) = resumed.current_match().expect("resumed current match");
        assert!(
            (resumed.match_base_y + i64::from(row), start, end) > accepted_anchor,
            "Find Next resumed strictly after the accepted match"
        );
    }

    #[test]
    fn find_again_never_reuses_an_absolute_anchor_from_another_session() {
        let mut app = App::headless_for_test();
        let first = WindowId(0);
        let first_term = app.pool.get(0).expect("session 0").term.clone();
        term_lock(&first_term).process(b"hit one\r\nhit two");
        app.search_enter();
        app.windows
            .get_mut(&first)
            .unwrap()
            .search
            .as_mut()
            .unwrap()
            .query = "hit".into();
        app.search_recompute();
        app.search_accept();
        assert_eq!(app.search_last_anchor.unwrap().0, 0);

        let second = app.insert_logical_window(crate::stub_session(1), 24, 80);
        app.frontmost_window = Some(second);
        let second_term = app.pool.get(1).expect("session 1").term.clone();
        term_lock(&second_term).process(b"hit alpha\r\nhit beta");
        app.search_find_again(true);
        let search = app.windows[&second]
            .search
            .as_ref()
            .expect("app-sticky query opens in the second session");
        assert_eq!(search.query, "hit");
        assert_eq!(
            search.current, 0,
            "foreign absolute coordinates are ignored; forward search starts at the first local match"
        );
    }

    #[test]
    fn accepted_anchor_keeps_the_revision_its_match_was_computed_in() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let rows = app.windows[&wid].rows;
        let term = app.pool.get(0).expect("session 0").term.clone();
        term_lock(&term).process(b"hit");
        app.search_enter();
        seed_query(&mut app, wid, "hit");
        app.search_recompute();

        let region_bottom = rows - 2;
        term_lock(&term).process(
            format!("\x1b[1;{region_bottom}r\x1b[{region_bottom};1H\r\nX\x1b[r").as_bytes(),
        );
        assert_eq!(term_lock(&term).absolute_row_revision(), 1);
        assert_eq!(
            app.windows[&wid]
                .search
                .as_ref()
                .unwrap()
                .match_absolute_row_revision,
            0,
            "fixture intentionally accepts before the output wake refresh"
        );

        app.search_accept();
        let (_, accepted_revision, ..) = app
            .search_last_anchor
            .expect("accepted query retains its match anchor");
        assert_eq!(
            accepted_revision, 1,
            "accept must refresh stale coordinates and retain the revision actually searched"
        );
    }

    #[test]
    fn active_search_defers_then_recomputes_after_codex_footer_row_splice() {
        let _serial = crate::control::search_cap_test_guard();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let rows = app.windows[&wid].rows;
        let term = app.pool.get(0).expect("session 0").term.clone();

        term_lock(&term).process(format!("\x1b[{rows};1HFOOTERNEEDLE").as_bytes());
        app.search_enter();
        seed_query(&mut app, wid, "FOOTERNEEDLE");
        app.search_recompute();
        assert_eq!(
            app.windows[&wid]
                .search
                .as_ref()
                .unwrap()
                .match_absolute_row_revision,
            0
        );

        // Codex's full-width 1..rows-2 history area leaves two footer rows
        // fixed while inserting one logical row immediately before them. The
        // displaced top row must be WRITTEN — never-written rows scroll
        // history-free (no archival, no splice, no revision bump).
        let region_bottom = rows - 2;
        term_lock(&term).process(
            format!("\x1b[1;1HA\x1b[1;{region_bottom}r\x1b[{region_bottom};1H\r\nX\x1b[r")
                .as_bytes(),
        );
        assert_eq!(term_lock(&term).absolute_row_revision(), 1);

        app.search_refresh_for_output(0);
        let stale = app.windows[&wid]
            .search
            .as_ref()
            .expect("find remains open while invalidated");
        assert!(stale.results_dirty, "streaming output defers the rebuild");
        assert_eq!(stale.match_absolute_row_revision, 0);
        assert!(
            term_lock(&term).selection_to_string().is_none(),
            "a stale protected-footer match is never highlighted"
        );

        app.search_repeat(true);
        let search = app.windows[&wid]
            .search
            .as_ref()
            .expect("repeat refreshes the invalidated find");
        assert!(!search.results_dirty);
        assert_eq!(search.match_absolute_row_revision, 1);
        let (row, _, _) = search
            .current_match()
            .expect("footer match remains searchable");
        assert_eq!(
            search.match_base_y + i64::from(row),
            i64::from(rows),
            "the footer match follows the logical-row insertion"
        );
    }

    /// ⎋/^G CANCEL (emacs C-g): closing find RESTORES the viewport captured at
    /// `search_enter` — even after find navigation scrolled deep into scrollback —
    /// clears the match highlight, and does NOT remember the cancelled query.
    #[test]
    fn cancel_restores_origin_viewport() {
        let _serial = crate::control::search_cap_test_guard();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let rows = app.windows.get(&wid).unwrap().rows as usize;
        let term = app.pool.get(0).expect("session 0").term.clone();
        let mut input = Vec::new();
        for _ in 0..(rows + 8) {
            input.extend_from_slice(b"hit here\r\n");
        }
        term_lock(&term).process(&input);
        // The user was reading 3 lines above the live bottom when they pressed ⌘F.
        term_lock(&term).scroll_display(3);
        assert_eq!(term_lock(&term).grid().display_offset(), 3);

        app.search_enter();
        assert_eq!(
            app.windows
                .get(&wid)
                .unwrap()
                .search
                .as_ref()
                .unwrap()
                .origin_display_offset,
            3,
            "enter captured the origin viewport"
        );
        seed_query(&mut app, wid, "hit");
        app.search_recompute();
        assert_ne!(
            term_lock(&term).grid().display_offset(),
            3,
            "find navigation moved the viewport away from the origin"
        );

        app.search_cancel();
        assert!(
            app.windows.get(&wid).unwrap().search.is_none(),
            "cancel closed find mode"
        );
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            3,
            "cancel restored the origin viewport"
        );
        assert!(
            term_lock(&term).selection_to_string().is_none(),
            "cancel cleared the match highlight"
        );
        assert!(
            app.search_last_query.is_empty(),
            "a cancelled query is not remembered"
        );
    }

    #[test]
    fn cancel_does_not_apply_uniform_origin_delta_across_codex_footer_splice() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let rows = app.windows[&wid].rows;
        let term = app.pool.get(0).expect("session 0").term.clone();

        app.search_enter();
        assert_eq!(
            app.windows[&wid]
                .search
                .as_ref()
                .unwrap()
                .origin_absolute_row_revision,
            0
        );
        // Row 0 must be WRITTEN for the displaced row to archive and splice.
        let region_bottom = rows - 2;
        term_lock(&term).process(
            format!("\x1b[1;1HA\x1b[1;{region_bottom}r\x1b[{region_bottom};1H\r\nX\x1b[r")
                .as_bytes(),
        );
        assert_eq!(term_lock(&term).absolute_row_revision(), 1);
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            0,
            "output at the live tail remains at the live tail"
        );

        app.search_cancel();
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            0,
            "cancel must not turn the piecewise footer insertion into a false one-line origin delta"
        );
    }

    /// `^S`/`^R` on an EMPTY query RECALL the last ACCEPTED query (emacs `C-s C-s`):
    /// forward recall lands on the FIRST match, backward recall on the LAST, a further
    /// `^S` steps normally, and a fresh app with nothing accepted is a no-op.
    #[test]
    fn repeat_recalls_last_accepted_query() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        term_lock(&term).process(b"hit a\r\nhit b\r\nhit c\r\n");

        let state = |app: &App| {
            let s = app.windows.get(&wid).unwrap().search.as_ref().unwrap();
            (s.query.clone(), s.matches.len(), s.current)
        };

        // Nothing accepted yet: ^S on an empty fresh find is a no-op.
        app.search_enter();
        app.search_repeat(true);
        let (q, n, _) = state(&app);
        assert!(q.is_empty() && n == 0, "nothing to recall on a fresh app");

        // Type + accept "hit" — remembered app-sticky.
        seed_query(&mut app, wid, "hit");
        app.search_recompute();
        app.search_accept();

        // Reopen: ^S recalls the query and lands on the FIRST match.
        app.search_enter();
        app.search_repeat(true);
        assert_eq!(state(&app), ("hit".to_string(), 3, 0), "forward recall");
        // A further ^S steps normally (the query is no longer empty).
        app.search_repeat(true);
        assert_eq!(state(&app).2, 1, "second ^S steps to the next match");
        app.search_cancel();

        // Reopen: ^R recalls the query and lands on the LAST match.
        app.search_enter();
        app.search_repeat(false);
        assert_eq!(state(&app), ("hit".to_string(), 3, 2), "backward recall");
    }

    /// FULL DISPATCH (#10): a real left press whose pixel lands on the `Aa` indicator drives
    /// the entire `on_mouse_input` seam — modal misses → strip miss → find-bar pixel gate →
    /// cell map → toggle — flipping match-case (and its sticky default). Proves the find-bar
    /// mouse layer is wired into the live handler, not merely callable in isolation.
    #[test]
    fn find_bar_toggle_fires_through_on_mouse_input() {
        use winit::event::{ElementState, MouseButton};
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        term_lock(&term).process(b"target line here\r\n");
        app.search_enter();
        seed_query(&mut app, wid, "target");
        app.search_recompute();
        fill_scratch(&mut app, wid);
        app.splice_find_bar(wid);
        // Model an ATTACHED native window: every edge keeps 12 px while the top
        // inset is tightened to 4 px and a 3 px chrome band sits above it. The old
        // symmetric-pad hit gate rejected the first 8 real pixels of this bar.
        {
            let metrics = &mut app.windows.get_mut(&wid).unwrap().metrics;
            metrics.pad = 12;
            metrics.pad_top = 4;
            metrics.head = 3;
        }
        let hit = app
            .windows
            .get(&wid)
            .unwrap()
            .find_bar_hit
            .clone()
            .expect("splice recorded the indicator geometry");
        let case = hit.case_cols.expect("Aa drawn");
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid);
        let pad_top = app.win_pad_top(wid);
        let head = app.win_head(wid);
        let frame_row = app.tab_strip_rows as usize + hit.row;
        // Pixel dead-centre of the `Aa` indicator's first cell — inside the bar's band.
        let x = (pad + case.start * cw) as f64 + cw as f64 / 2.0;
        let y = (pad_top + head + frame_row * ch) as f64 + ch as f64 / 2.0;
        app.windows.get_mut(&wid).unwrap().last_cursor_px = (x, y);
        assert!(
            !app.windows
                .get(&wid)
                .unwrap()
                .search
                .as_ref()
                .unwrap()
                .case_sensitive,
            "case-sensitive off before the click"
        );
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        assert!(
            app.windows
                .get(&wid)
                .unwrap()
                .search
                .as_ref()
                .unwrap()
                .case_sensitive,
            "a left press on `Aa` toggled match-case through the full handler"
        );
        assert!(
            app.search_sticky_case,
            "the dispatch updated the sticky default too"
        );
    }

    /// CLAMP GUARD (#10, fix #4): a left press BELOW the grid — in the bottom `pad` border —
    /// that `pixel_to_cell` CLAMPS up onto the bar's (bottom) row must NOT toggle a mode.
    /// The pixel-band gate rejects it even though the clamped cell coincides with an
    /// indicator column, so only a press actually inside the bar's cell band flips a toggle.
    #[test]
    fn find_bar_pixel_gate_rejects_pad_clamp_click() {
        use winit::event::{ElementState, MouseButton};
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        term_lock(&term).process(b"target line here\r\n");
        app.search_enter();
        seed_query(&mut app, wid, "target");
        app.search_recompute();
        fill_scratch(&mut app, wid);
        app.splice_find_bar(wid);
        let hit = app
            .windows
            .get(&wid)
            .unwrap()
            .find_bar_hit
            .clone()
            .expect("splice recorded the indicator geometry");
        let case = hit.case_cols.expect("Aa drawn");
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid);
        let rows = app.windows.get(&wid).unwrap().rows as usize;
        let clamp_row = hit.band.end - 1;
        let frame_row = app.tab_strip_rows as usize + clamp_row;
        // The `Aa` indicator's x, but a y two pixels BELOW the last cell row — in the bottom
        // pad, outside the panel's cell band.
        let x = (pad + case.start * cw) as f64 + cw as f64 / 2.0;
        let y_below = (pad + (frame_row + 1) * ch) as f64 + 2.0;
        // Pre-conditions that make this the dangerous case: the panel floated to the
        // BOTTOM (the current match sits on the top row) and `pixel_to_cell` DOES clamp
        // the below-grid press up into its band — so without the gate the click would be
        // taken as a panel click on the `Aa` column.
        assert_eq!(
            hit.band.end,
            rows,
            "panel floated to the bottom (the clamp target)"
        );
        assert_eq!(
            app.pixel_to_cell(wid, x, y_below).0 as usize,
            clamp_row,
            "the below-grid press clamps into the panel band"
        );
        assert!(
            !app.find_bar_pixel_hit(wid, x, y_below),
            "the pixel gate rejects the pad press the row-clamp would have admitted"
        );
        app.windows.get_mut(&wid).unwrap().last_cursor_px = (x, y_below);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        assert!(
            !app.windows
                .get(&wid)
                .unwrap()
                .search
                .as_ref()
                .unwrap()
                .case_sensitive,
            "a below-grid pad press must NOT toggle match-case"
        );
        assert!(
            !app.search_sticky_case,
            "the sticky default is untouched by the rejected press"
        );
    }

    /// CLAMP GUARD, top-placement twin: with the bar at its DEFAULT top row, a press in
    /// the TOP pad border — above the grid — that `pixel_to_cell` clamps DOWN onto the
    /// bar's row must NOT toggle a mode; the pixel-band gate rejects it.
    #[test]
    fn find_bar_pixel_gate_rejects_top_pad_clamp_click() {
        use winit::event::{ElementState, MouseButton};
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        // Matches below the panel band, so it keeps its default TOP placement.
        term_lock(&term).process(b"\r\n\r\n\r\n\r\ntarget line here\r\n");
        app.search_enter();
        seed_query(&mut app, wid, "target");
        app.search_recompute();
        fill_scratch(&mut app, wid);
        app.splice_find_bar(wid);
        let hit = app
            .windows
            .get(&wid)
            .unwrap()
            .find_bar_hit
            .clone()
            .expect("splice recorded the indicator geometry");
        let case = hit.case_cols.expect("Aa drawn");
        let (cw, _ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid);
        assert_eq!(hit.band.start, 0, "panel sits at its default top placement");
        // The `Aa` indicator's x, but a y ABOVE the first cell row — in the top pad,
        // outside the panel's cell band; `pixel_to_cell` clamps it down into the band.
        let x = (pad + case.start * cw) as f64 + cw as f64 / 2.0;
        let y_above = pad as f64 - 2.0;
        assert_eq!(
            app.pixel_to_cell(wid, x, y_above).0 as usize,
            hit.band.start,
            "the above-grid press clamps into the panel band"
        );
        assert!(
            !app.find_bar_pixel_hit(wid, x, y_above),
            "the pixel gate rejects the pad press the row-clamp would have admitted"
        );
        app.windows.get_mut(&wid).unwrap().last_cursor_px = (x, y_above);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        assert!(
            !app.windows
                .get(&wid)
                .unwrap()
                .search
                .as_ref()
                .unwrap()
                .case_sensitive,
            "an above-grid pad press must NOT toggle match-case"
        );
        assert!(
            !app.search_sticky_case,
            "the sticky default is untouched by the rejected press"
        );
    }

    /// RE-ANCHOR delta≠0 (fix #3 coverage): the marquee re-anchor — mapping stored match
    /// rows by `delta = base_y_now − match_base_y` — was only ever exercised at delta==0
    /// (search immediately followed by splice, no output between). This drives the real
    /// delta>0 path: search, THEN stream output that scrolls the grid while the match stays
    /// on screen, and asserts the highlight tint tracks the row the match ACTUALLY occupies
    /// now — not the (now-different) row it sat on at search time.
    #[test]
    fn splice_reanchors_highlight_after_output_scrolls_in() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let rows = app.windows.get(&wid).unwrap().rows as usize;
        let term = app.pool.get(0).expect("session 0").term.clone();
        // UNIQNEEDLE mid-screen: 10 filler rows above, the needle, then filler filling the
        // rest of the screen (no scrollback yet, viewport at the bottom).
        let mut input = Vec::new();
        for i in 0..10 {
            input.extend_from_slice(format!("above {i}\r\n").as_bytes());
        }
        input.extend_from_slice(b"UNIQNEEDLE\r\n");
        for i in 0..(rows - 12) {
            input.extend_from_slice(format!("below {i}\r\n").as_bytes());
        }
        term_lock(&term).process(&input);
        app.search_enter();
        seed_query(&mut app, wid, "UNIQNEEDLE");
        app.search_recompute();
        let match_base_y = app
            .windows
            .get(&wid)
            .unwrap()
            .search
            .as_ref()
            .unwrap()
            .match_base_y;

        // Stream MORE output AFTER the search — advances base_y (scrolls the grid) while
        // UNIQNEEDLE is still on screen, so the re-anchor delta is strictly positive.
        let mut more = Vec::new();
        for i in 0..5 {
            more.extend_from_slice(format!("stream {i}\r\n").as_bytes());
        }
        term_lock(&term).process(&more);

        fill_scratch(&mut app, wid); // re-extract: input_scratch.base_y has advanced
        let base_y_now = app.windows.get(&wid).unwrap().input_scratch.base_y;
        assert!(
            base_y_now > match_base_y,
            "output scrolled the grid in since the search (delta = {} > 0)",
            base_y_now - match_base_y
        );

        app.splice_find_bar(wid);
        // The (top-anchored) bar echoes `Find: UNIQNEEDLE`, so skip its row when locating
        // the real content match — else the query echo would masquerade as the needle.
        let bar_row = app
            .windows
            .get(&wid)
            .unwrap()
            .find_bar_hit
            .as_ref()
            .map(|h| h.row);
        let hi = highlight_bg();
        let cells = &app.windows.get(&wid).unwrap().input_scratch.cells;
        // Find the frame row that ACTUALLY shows UNIQNEEDLE now, and assert THAT row is
        // tinted. A non-re-anchored tint (stored row, ignoring delta) would paint a
        // different, now-filler row and leave the needle's real row untinted.
        let needle_row = cells
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != bar_row)
            .find(|(_, r)| {
                r.iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .contains("UNIQNEEDLE")
            })
            .map(|(i, _)| i)
            .expect("UNIQNEEDLE still visible after streaming");
        assert!(
            cells[needle_row][0..10].iter().all(|c| c.bg == hi),
            "the re-anchored highlight tints the row UNIQNEEDLE occupies NOW (frame row {needle_row})"
        );
    }

    /// PIXEL-BAND BOUNDARIES + x-GATE (fix #9 coverage): `find_bar_pixel_hit` admits a click
    /// only inside the panel's exact cell band `[top, top + rows*ch)` and inside the grid
    /// columns. Locks the off-by-one edges (first/last device row of the BAND accept; one px
    /// outside either edge rejects) and the right-edge x-gate (a press past the last column
    /// rejects, so `pixel_to_cell`'s column clamp can't land it on an indicator span).
    #[test]
    fn find_bar_pixel_hit_boundaries_and_right_edge() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        term_lock(&term).process(b"boundary target\r\n");
        app.search_enter();
        seed_query(&mut app, wid, "target");
        app.search_recompute();
        fill_scratch(&mut app, wid);
        app.splice_find_bar(wid);
        let hit = app
            .windows
            .get(&wid)
            .unwrap()
            .find_bar_hit
            .clone()
            .expect("bar geometry");
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid);
        let pad_top = app.win_pad_top(wid);
        let head = app.win_head(wid);
        let cols = app.windows.get(&wid).unwrap().cols as usize;
        let frame_row = app.tab_strip_rows as usize + hit.band.start;
        let top = (pad_top + head + frame_row * ch) as f64;
        let height = (hit.band.len() * ch) as f64;
        let x_in = (pad + 5 * cw) as f64 + cw as f64 / 2.0; // a column well inside the grid
        // The exact first and last device rows of the BAND accept.
        assert!(
            app.find_bar_pixel_hit(wid, x_in, top),
            "py == top (first band row) accepts"
        );
        assert!(
            app.find_bar_pixel_hit(wid, x_in, top + height - 1.0),
            "py == top+height-1 (last band row) accepts"
        );
        // One device pixel outside either edge REJECTS.
        assert!(
            !app.find_bar_pixel_hit(wid, x_in, top - 1.0),
            "py == top-1 (row above) rejects"
        );
        assert!(
            !app.find_bar_pixel_hit(wid, x_in, top + height),
            "py == top+height (first row below) rejects"
        );
        // Right-edge x-gate: a press past the last grid column rejects even in-band.
        let x_past = (pad + cols * cw) as f64 + 1.0;
        assert!(
            !app.find_bar_pixel_hit(wid, x_past, top + ch as f64 / 2.0),
            "a press past the right grid edge rejects (no column-clamp false toggle)"
        );
        // Control: a normal in-grid, in-band press still accepts.
        assert!(
            app.find_bar_pixel_hit(wid, x_in, top + ch as f64 / 2.0),
            "an in-band, in-grid press accepts"
        );
    }

    /// MULTI-PANE SUPPRESSION (fix #8 coverage): in a split, matches key to the FOCUSED
    /// pane's grid but the splice tiles panes at offsets it does not track, so a
    /// highlight-all tint would land in the wrong pane. Splitting the active tab must
    /// suppress the tint entirely while the bar row itself is still spliced.
    #[test]
    fn highlight_all_suppressed_in_split() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        term_lock(&term).process(b"hit one\r\nhit two\r\n"); // two live matches
        app.search_enter();
        seed_query(&mut app, wid, "hit");
        app.search_recompute();

        // Split the visible tab so `active_tree(wid).len() > 1` (the multi_pane gate).
        let split = app
            .active_tree_mut(wid)
            .expect("active tree")
            .split_focused(crate::pane::SplitDir::Vertical, 1);
        assert!(split, "the tab split into two panes");
        assert!(
            app.active_tree(wid).is_some_and(|t| t.len() > 1),
            "the window is now multi-pane"
        );

        fill_scratch(&mut app, wid);
        app.splice_find_bar(wid);
        let hi = highlight_bg();
        // The panel is still drawn (it's window chrome), but NO match tint is applied.
        let field = field_row_text(&app, wid);
        assert!(
            field.contains("Find: ") && field.contains("hit"),
            "the find panel is still spliced in a split: {field:?}"
        );
        let cells = &app.windows.get(&wid).unwrap().input_scratch.cells;
        for (r, row) in cells.iter().enumerate() {
            assert!(
                row.iter().all(|c| c.bg != hi),
                "no highlight-all tint anywhere in a split (row {r})"
            );
        }
    }
}

#[cfg(test)]
mod rain_host_tests {
    //! PHOSPHOR host-side wiring pins that live in this file: the hidden-
    //! cursor damage band (design §6) and the tab-strip splice shifting the
    //! rain channels with the grid (the single splice rule).

    use super::{
        prepend_strip_rows, rain_refresh_needed, rain_shell_execute_rising_edge,
        translate_rain_into_pane, update_rain_hidden_band,
    };
    use crate::matrix_rain::{EffectGeom, MatrixRain, RainConfig, RainSignal, RainTickInput};
    use aterm_core::grid::damage::{Damage, DamageTracker};

    fn q(row: u16, x: u16, y: u16, w: u16, h: u16) -> aterm_render::SpriteQuad {
        aterm_render::SpriteQuad {
            row,
            x,
            y,
            w,
            h,
            ax: 0,
            ay: 0,
            aw: 8,
            ah: 16,
            tint: 0x00FF_FFFF,
            alpha: 255,
            flip_x: false,
        }
    }

    fn halo(row: u16, x: u16, y: u16, w: u16, h: u16, cx: u16, cy: u16) -> aterm_render::RainHalo {
        aterm_render::RainHalo {
            row,
            x,
            y,
            w,
            h,
            color: 0x0020_FF40,
            cx,
            cy,
            rx: 12,
            ry: 8,
            mode: aterm_render::HaloMode::Add,
        }
    }

    /// SPLIT-PANE compose (split-pane audit): pane-local rain emission
    /// translates by `(row_off·ch, col_off·cw)` px + `row_off` row tags, and
    /// every surviving quad/halo lies inside the pane's grid-interior box —
    /// rain light can never cross a divider.
    #[test]
    fn translate_rain_into_pane_confines_emission_to_the_pane_box() {
        let (cw, ch) = (8u32, 16u32);
        let (row_off, col_off, rows, cols) = (5u16, 42u16, 10u16, 38u16);
        // A quad at the pane origin, one mid-pane, one hugging the pane's
        // bottom-right cell — all pane-local, all in-bounds.
        let mut quads = vec![
            q(0, 0, 0, 8, 16),
            q(4, 80, 64, 8, 16),
            q(9, (u32::from(cols - 1) * cw) as u16, (9 * ch) as u16, 8, 16),
        ];
        // A halo whose quad pokes past the pane's right edge (emitter spill):
        // it must clip to the box, keeping its centre shift exact. A second
        // halo WHOLLY beyond the pane's right edge must drop WITHOUT panicking
        // (the eager-`then_some` subtraction underflowed there in debug builds
        // — the field-crash regression).
        let mut add = vec![
            halo(0, ((u32::from(cols) - 1) * cw) as u16, 0, 24, 16, 300, 8),
            halo(1, (u32::from(cols) * cw + 40) as u16, 16, 24, 16, 340, 24),
        ];
        translate_rain_into_pane(&mut quads, &mut add, row_off, col_off, rows, cols, cw, ch);
        let x0 = u32::from(col_off) * cw;
        let y0 = u32::from(row_off) * ch;
        let x1 = (u32::from(col_off) + u32::from(cols)) * cw;
        let y1 = (u32::from(row_off) + u32::from(rows)) * ch;
        assert_eq!(quads.len(), 3, "in-bounds quads all survive");
        for quad in &quads {
            assert!(u32::from(quad.x) >= x0 && u32::from(quad.x) + u32::from(quad.w) <= x1);
            assert!(u32::from(quad.y) >= y0 && u32::from(quad.y) + u32::from(quad.h) <= y1);
            assert!(
                (row_off..row_off + rows).contains(&quad.row),
                "row tag shifted"
            );
        }
        assert_eq!(
            quads[0].x, x0 as u16,
            "origin quad lands at the pane origin"
        );
        assert_eq!(quads[0].y, y0 as u16);
        assert_eq!(add.len(), 1, "spilling halo clips, not drops");
        let h0 = &add[0];
        assert!(
            u32::from(h0.x) + u32::from(h0.w) <= x1,
            "halo clipped at the divider"
        );
        assert_eq!(
            u32::from(h0.cx),
            300 + x0,
            "falloff centre shifts with the pane"
        );
        assert_eq!(u32::from(h0.cy), 8 + y0);
    }

    /// Quads that would leave the u16 pixel space (a pane deep in a huge
    /// window) drop whole instead of wrapping.
    #[test]
    fn translate_rain_into_pane_drops_u16_overflow() {
        let (cw, ch) = (8u32, 16u32);
        let mut quads = vec![q(0, 65_000, 0, 40, 16)];
        let mut add = Vec::new();
        // col_off far enough right that 65000 + col_off*8 exceeds u16::MAX.
        translate_rain_into_pane(&mut quads, &mut add, 0, 100, 5, 8200, cw, ch);
        assert!(quads.is_empty(), "overflowing quad drops whole");
    }

    fn shell_execute_edge_model() -> aterm_spec::derive::Model {
        use aterm_spec::ty_model;
        ty_model! {
            ShellExecuteEdge {
                const Buggy = 0;
                var observed = 0;
                var session = 0;
                var executing = 0;
                var pulse = 0;
                var valid_edge = 0;
                action ObserveIdle0 {
                    valid_edge = 0;
                    pulse = 0;
                    observed = 1;
                    session = 0;
                    executing = 0;
                }
                action ObserveExec0 {
                    valid_edge = if observed == 1 && session == 0 && executing == 0 { 1 } else { 0 };
                    pulse = if Buggy == 1 { 1 } else { if observed == 1 && session == 0 && executing == 0 { 1 } else { 0 } };
                    observed = 1;
                    session = 0;
                    executing = 1;
                }
                action ObserveIdle1 {
                    valid_edge = 0;
                    pulse = 0;
                    observed = 1;
                    session = 1;
                    executing = 0;
                }
                action ObserveExec1 {
                    valid_edge = if observed == 1 && session == 1 && executing == 0 { 1 } else { 0 };
                    pulse = if Buggy == 1 { 1 } else { if observed == 1 && session == 1 && executing == 0 { 1 } else { 0 } };
                    observed = 1;
                    session = 1;
                    executing = 1;
                }
                action Reset {
                    observed = 0;
                    session = 0;
                    executing = 0;
                    pulse = 0;
                    valid_edge = 0;
                }
                invariant BooleanBounds: observed <= 1 && session <= 1 && executing <= 1 && pulse <= 1 && valid_edge <= 1;
                invariant PulseExactlyMatchesEdge: pulse == valid_edge;
            }
        }
    }

    #[test]
    fn shell_execute_edge_model_proves_and_catches_level_refresh() {
        let model = shell_execute_edge_model();
        aterm_spec::verify::prove_and_catch_tiered(&model, model.name);
    }

    #[test]
    fn real_shell_execute_edge_conforms_and_long_level_drains() {
        let model = shell_execute_edge_model();
        let mut state = model.init_state();
        let mut last = None;
        let observations = [
            (7, true, "ObserveExec0", false),
            (7, true, "ObserveExec0", false),
            (7, false, "ObserveIdle0", false),
            (7, true, "ObserveExec0", true),
            (7, true, "ObserveExec0", false),
            (9, true, "ObserveExec1", false),
            (9, false, "ObserveIdle1", false),
            (9, true, "ObserveExec1", true),
        ];
        for (session, executing, action, expected) in observations {
            let pulse = rain_shell_execute_rising_edge(&mut last, session, executing);
            assert_eq!(pulse, expected);
            assert!(model.fire(action, &mut state));
            assert_eq!(i64::from(pulse), state["pulse"]);
            assert_eq!(i64::from(last.is_some()), state["observed"]);
            assert_eq!(i64::from(last.expect("observed").1), state["executing"]);
        }

        let mut rain = MatrixRain::new(RainConfig {
            enabled: true,
            output_material: false,
            idle_secs: 2,
            ..RainConfig::default()
        });
        let mut last = Some((7, false));
        assert!(rain_shell_execute_rising_edge(&mut last, 7, true));
        rain.note_signal(RainSignal::Execute as u32, 4);
        let geom = EffectGeom {
            cell_w: 8,
            cell_h: 16,
            rows: 24,
            cols: 80,
        };
        let (mut quads, mut add) = (Vec::new(), Vec::new());
        for _ in 0..240 {
            assert!(
                !rain_shell_execute_rising_edge(&mut last, 7, true),
                "a held OSC execution level emits no repeat pulse"
            );
            rain.advance_ms(83);
            rain.emit(geom, &RainTickInput::default(), &mut quads, &mut add);
        }
        assert!(
            !rain.is_active(),
            "one Execute edge expires and drains despite a held execution level"
        );
    }

    /// A live classic-to-literal reload needs an authoritative material
    /// sample even when occupancy is already current at the same epoch.
    #[test]
    fn literal_reload_refreshes_without_new_grid_damage() {
        let classic = RainConfig {
            enabled: true,
            output_material: false,
            ..RainConfig::default()
        };
        let mut engine = MatrixRain::new(classic);
        engine.rescan_from_cells(&[], &[], &[], 0, 0, classic.default_bg, 42);
        assert!(!engine.needs_rescan(42));
        assert!(!engine.needs_material_sample());
        assert!(!rain_refresh_needed(
            true,
            false,
            false,
            0,
            Some(&engine),
            42
        ));

        engine.set_config(RainConfig {
            output_material: true,
            ..classic
        });
        assert!(engine.needs_material_sample());
        assert!(rain_refresh_needed(
            true,
            false,
            false,
            0,
            Some(&engine),
            42
        ));
        assert!(!rain_refresh_needed(
            true,
            false,
            false,
            1,
            Some(&engine),
            42
        ));
        assert!(!rain_refresh_needed(
            true,
            true,
            false,
            0,
            Some(&engine),
            42
        ));
        assert!(!rain_refresh_needed(
            true,
            false,
            true,
            0,
            Some(&engine),
            42
        ));
        assert!(!rain_refresh_needed(
            false,
            false,
            false,
            0,
            Some(&engine),
            42
        ));
    }

    /// The band ring keeps the last `HIDDEN_CURSOR_BAND_ROWS` damaged rows,
    /// most recent first, bottom-biased within a frame, deduped, capped.
    #[test]
    fn hidden_band_tracks_recent_damage() {
        let mut band: Vec<u16> = Vec::new();
        let mut dmg = Damage::Partial(DamageTracker::new(30));
        dmg.mark_rows(27, 30); // half-open: rows 27, 28, 29
        update_rain_hidden_band(&mut band, &dmg, 30);
        assert_eq!(band, vec![29, 28, 27], "ascending scan ⇒ bottom rows first");

        // A later frame damaging one row promotes it to the ring head.
        let mut dmg = Damage::Partial(DamageTracker::new(30));
        dmg.mark_rows(5, 6);
        update_rain_hidden_band(&mut band, &dmg, 30);
        assert_eq!(band, vec![5, 29, 28, 27]);

        // Re-damaging a resident row DEDUPES (promote, don't duplicate).
        let mut dmg = Damage::Partial(DamageTracker::new(30));
        dmg.mark_rows(28, 29);
        update_rain_hidden_band(&mut band, &dmg, 30);
        assert_eq!(band, vec![28, 5, 29, 27]);

        // The ring caps at HIDDEN_CURSOR_BAND_ROWS (5), evicting the oldest.
        let mut dmg = Damage::Partial(DamageTracker::new(30));
        dmg.mark_rows(10, 13); // rows 10, 11, 12
        update_rain_hidden_band(&mut band, &dmg, 30);
        assert_eq!(band.len(), crate::matrix_rain::HIDDEN_CURSOR_BAND_ROWS);
        assert_eq!(band, vec![12, 11, 10, 28, 5], "oldest rows evicted");
    }

    /// FULL damage (resize / alt-swap / first frame) locates nothing — every
    /// row reads damaged — so the ring is left unchanged rather than flooded.
    #[test]
    fn hidden_band_ignores_full_damage() {
        let mut band: Vec<u16> = vec![7, 3];
        let mut dmg = Damage::Partial(DamageTracker::new(10));
        dmg.mark_full();
        update_rain_hidden_band(&mut band, &dmg, 10);
        assert_eq!(band, vec![7, 3], "full damage leaves the band unchanged");
    }

    /// The tab-strip splice shifts rain quads (row + pixel y, the cat-quad
    /// shape) and rain halos (the nova shape) down with the grid — a quad
    /// emitted in viewport coords stays registered with its cell.
    #[test]
    fn strip_splice_shifts_rain_channels() {
        let mut input = aterm_render::RenderInput::empty();
        input.cells = vec![vec![]; 4];
        input.clusters = vec![vec![]; 4];
        input.combining = vec![vec![]; 4];
        input.images = vec![vec![]; 4];
        input.line_sizes = vec![aterm_core::grid::LineSize::SingleWidth; 4];
        input.rows = 4;
        input.rain_quads.push(aterm_render::SpriteQuad {
            row: 2,
            x: 8,
            y: 32,
            w: 8,
            h: 16,
            ax: 0,
            ay: 0,
            aw: 8,
            ah: 16,
            tint: 0x0028_D75F,
            alpha: 96,
            flip_x: false,
        });
        input.rain_add.push(aterm_render::RainHalo {
            row: 2,
            x: 8,
            y: 32,
            w: 8,
            h: 16,
            color: 0x0010_3010,
            cx: 12,
            cy: 40,
            rx: 6,
            ry: 8,
            // Defaulted `mode: HaloMode::Add` — the historical light.
            ..Default::default()
        });
        let strip = vec![vec![]; 1];
        let mut pool = Vec::new();
        prepend_strip_rows(&mut input, &strip, 16, 0, &mut pool);
        assert_eq!(input.rain_quads[0].row, 3, "row shifted by the strip");
        assert_eq!(input.rain_quads[0].y, 48, "pixel y shifted by strip*cell_h");
        assert_eq!(input.rain_add[0].row, 3);
        assert_eq!(input.rain_add[0].y, 48);
        assert_eq!(
            input.rain_add[0].cy, 56,
            "the halo falloff CENTRE rides the same vertical shift as its quad"
        );
        assert_eq!(
            input.rain_add[0].cx, 12,
            "a vertical splice leaves cx alone"
        );
    }

    /// The tab-strip splice shifts the fire CONTRAST-HALO cells down with the
    /// grid (a GRID stream, the char_fg rule): row tag only — the stream is
    /// cell-anchored and colour-free, so nothing else moves.
    #[test]
    fn strip_splice_shifts_fire_halo_rows() {
        let mut input = aterm_render::RenderInput::empty();
        input.cells = vec![vec![]; 4];
        input.clusters = vec![vec![]; 4];
        input.combining = vec![vec![]; 4];
        input.images = vec![vec![]; 4];
        input.line_sizes = vec![aterm_core::grid::LineSize::SingleWidth; 4];
        input.rows = 4;
        input.fire_halo.push(aterm_render::FireHaloCell {
            row: 2,
            col: 5,
            strength: 200,
        });
        input.char_fg.push(aterm_render::CharFg {
            row: 2,
            col: 5,
            fg: 0x0010_0804,
        });
        let strip = vec![vec![]; 2];
        let mut pool = Vec::new();
        prepend_strip_rows(&mut input, &strip, 16, 0, &mut pool);
        assert_eq!(
            input.fire_halo[0].row, 4,
            "fire_halo row shifted by the strip"
        );
        assert_eq!(input.fire_halo[0].col, 5, "the column never moves");
        assert_eq!(
            input.fire_halo[0].strength, 200,
            "the strength rides untouched"
        );
        assert_eq!(
            input.char_fg[0].row, 4,
            "char_fg shifts identically (the shared GRID-stream rule)"
        );
    }
}

/// VISUAL CAPTURE of the ⌘F find panel: drives the REAL App/splice path, renders the
/// resulting frame through the REAL CPU rasterizer (`aterm_render::Renderer`), and dumps
/// PNGs — so the chrome can be reviewed as PIXELS rather than as row-text asserts. Not a
/// gate: `#[ignore]`d (it needs a system font) and asserted only for "it produced frames".
///
/// ```sh
/// FIND_PANEL_PNG_DIR=/tmp/find cargo test -p aterm-gui --lib \
///     find_panel_visual_capture -- --ignored --nocapture
/// ```
#[cfg(test)]
mod find_panel_visual_tests {
    use crate::app_search::SearchEdit;
    use crate::{App, WindowId, term_lock};

    /// Fill the scratch from the engine and splice the panel, exactly as a real redraw
    /// does, then render + write `name.png`.
    fn capture(app: &mut App, wid: WindowId, dir: &std::path::Path, name: &str) -> bool {
        let (rows, cols) = {
            let ws = &app.windows[&wid];
            (ws.rows as usize, ws.cols as usize)
        };
        let terminal = app.front_terminal(wid).expect("front terminal").term.clone();
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            let mut term = term_lock(&terminal);
            term.cell_frame_into(&mut ws.input_scratch, rows, cols);
        }
        app.splice_find_bar(wid);
        let Some(mut cpu) = aterm_render::Renderer::from_system(20.0, aterm_render::Theme::default())
        else {
            return false; // no system monospace font (headless CI) — skip.
        };
        let frame = cpu.render_input(&app.windows[&wid].input_scratch);
        let mut rgb = Vec::with_capacity(frame.pixels.len() * 3);
        for &p in &frame.pixels {
            rgb.push((p >> 16) as u8);
            rgb.push((p >> 8) as u8);
            rgb.push(p as u8);
        }
        let path = dir.join(format!("{name}.png"));
        let file = std::fs::File::create(&path).expect("create png");
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), frame.width as u32, frame.height as u32);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .expect("png header")
            .write_image_data(&rgb)
            .expect("png data");
        eprintln!("wrote {} ({}x{})", path.display(), frame.width, frame.height);
        true
    }

    fn content(app: &App, lines: &[&str]) {
        let term = app.pool.get(0).expect("session 0").term.clone();
        let mut bytes = Vec::new();
        for line in lines {
            bytes.extend_from_slice(line.as_bytes());
            bytes.extend_from_slice(b"\r\n");
        }
        term_lock(&term).process(&bytes);
    }

    #[test]
    #[ignore = "visual capture: needs a system font; run with --ignored"]
    fn find_panel_visual_capture() {
        let dir = std::env::var("FIND_PANEL_PNG_DIR")
            .map_or_else(|_| std::env::temp_dir().join("find-panel"), std::path::PathBuf::from);
        std::fs::create_dir_all(&dir).expect("output dir");
        let lines = [
            "alpha needle one",
            "beta line two",
            "gamma needle three",
            "delta four",
            "epsilon needle five",
            "zeta seven eight",
        ];

        // 1. Just opened: the empty well shows its placeholder + the full keymap.
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        content(&app, &lines);
        app.search_enter();
        if !capture(&mut app, wid, &dir, "1-empty") {
            eprintln!("no system font — visual capture skipped");
            return;
        }

        // 2. Typed query with live matches: position readout + highlight-all tint.
        for ch in "needle".chars() {
            app.search_edit_in(wid, SearchEdit::Insert(ch.to_string()));
        }
        capture(&mut app, wid, &dir, "2-typed");

        // 3. Caret parked mid-query (^A then ⌥→): the field's edit position on glass.
        app.search_edit_in(wid, SearchEdit::MoveStart);
        app.search_edit_in(wid, SearchEdit::MoveCharRight);
        app.search_edit_in(wid, SearchEdit::MoveCharRight);
        capture(&mut app, wid, &dir, "3-caret-mid");

        // 4. A query far wider than the well: it scrolls to keep the caret in view.
        app.search_edit_in(wid, SearchEdit::KillToEnd);
        app.search_edit_in(wid, SearchEdit::KillToStart);
        app.search_edit_in(
            wid,
            SearchEdit::Insert(
                "a-very-long-query-that-runs-past-the-end-of-the-field-and-keeps-going".into(),
            ),
        );
        capture(&mut app, wid, &dir, "4-overlong");

        // 5. No matches: the honest zero-hit readout.
        app.search_edit_in(wid, SearchEdit::KillToStart);
        app.search_edit_in(wid, SearchEdit::Insert("zzz".into()));
        capture(&mut app, wid, &dir, "5-no-match");

        // 6. A narrow window: the right side degrades before the field is squeezed.
        let narrow = app.insert_logical_window(crate::stub_session(1), 20, 52);
        app.frontmost_window = Some(narrow);
        content(&app, &lines);
        app.search_enter();
        for ch in "needle".chars() {
            app.search_edit_in(narrow, SearchEdit::Insert(ch.to_string()));
        }
        capture(&mut app, narrow, &dir, "6-narrow");
    }
}
