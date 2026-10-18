// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rendering, text and contrast gate models, re-exported from this crate's root. Each
//! constructor is `trust::skip`: it is model data, and the model it returns is what `ty`
//! checks.

use aterm_spec::derive::*;

/// Two native leaves' retained rasters across full-native, split and terminal routes. Binding
/// a new visible plan synchronously drops hidden allocations; painting materializes only
/// visible leaves. `Buggy` prunes only in mixed compose, leaving hidden full-native rasters
/// alive. Tier-1 drives real App plans, native paints and terminal captures.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_leaf_cache_lifetime_model() -> Model {
    aterm_spec::ty_model! {
        NativeLeafCacheLifetime {
            const Buggy = 0;
            var first_visible = 0;
            var second_visible = 0;
            var first_cached = 0;
            var second_cached = 0;
            action BindTerminal {
                first_visible = 0;
                second_visible = 0;
                first_cached = if Buggy == 1 { first_cached } else { 0 };
                second_cached = if Buggy == 1 { second_cached } else { 0 };
            }
            action BindFirst {
                first_visible = 1;
                second_visible = 0;
                second_cached = if Buggy == 1 { second_cached } else { 0 };
            }
            action BindSecond {
                first_visible = 0;
                second_visible = 1;
                first_cached = if Buggy == 1 { first_cached } else { 0 };
            }
            action BindSplit {
                first_visible = 1;
                second_visible = 1;
            }
            action Paint {
                first_cached = if first_visible == 1 { 1 } else { first_cached };
                second_cached = if second_visible == 1 { 1 } else { second_cached };
            }
            invariant OnlyVisibleLeavesRetainRasters:
                first_cached <= first_visible && second_cached <= second_visible;
        }
    }
}

/// A composed cursor frame may read any requested ribbon witness row from the
/// same snapshot as its extracted cells, even if the live PTY advances
/// afterward. A row first requested after a content-scroll translation may
/// still require a later terminal read, admitted only while the captured
/// generation matches. Tier-1 drives both routes in `aterm-gui::app_render`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn composed_witness_generation_model() -> Model {
    aterm_spec::ty_model! {
        ComposedWitnessGeneration {
            const Buggy = 0;
            var changed = 0;
            var captured = 0;
            var admitted = 0;
            action Capture when (captured == 0 && changed == 0) { captured = 1; }
            action Mutate when (changed == 0 && admitted == 0) { changed = 1; }
            action ReadFresh when (admitted == 0 && (changed == 0 || Buggy == 1)) {
                admitted = 1;
            }
            action ReadCaptured when (admitted == 0 && (captured == 1 || Buggy == 1)) {
                admitted = 2;
            }
            invariant CoherentFreshRead:
                admitted == 0 || admitted == 2 || changed == 0;
            invariant CapturedReadOwnsSnapshot:
                admitted == 0 || admitted == 1 || captured == 1;
        }
    }
}

/// One demand-driven native drawable acquisition. `active` includes a queued request that has
/// not entered the OS yet; `ready` includes an unclaimed result. Retirement must follow that
/// request, never wait on the calling UI thread. `AcceptResult` is the pure generation/closed
/// guard, called by the host only with an owned completed result; it does not manufacture
/// drawable ownership. Tier-1 binds the real channel worker and both admission/acceptance
/// helpers in `aterm-gpu/src/metal/acquire_worker.rs`. `Buggy` admits duplicate admission,
/// stale/closed results, early retirement and caller-side waiting.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn metal_drawable_acquire_model() -> Model {
    aterm_spec::ty_model! {
        MetalDrawableAcquire {
            const Buggy = 0;
            const MaxRequests = 2;
            const MaxEpoch = 2;
            var closed = 0;
            var pending = 0;
            var active = 0;
            var ready = 0;
            var epoch = 0;
            var request_epoch = 0;
            var retiring = 0;
            var retired = 0;
            var requests = 0;
            var completed = 0;
            var published = 0;
            var wakes = 0;
            var invalid_accept = 0;
            var main_wait = 0;
            action Request when (
                closed == 0 && requests <= MaxRequests - 1 &&
                (pending == 0 || Buggy == 1)
            ) {
                pending = pending + 1;
                active = active + 1;
                request_epoch = epoch;
                requests = requests + 1;
            }
            action Complete when (active > 0) {
                active = active - 1;
                completed = if Buggy == 3 { completed + 2 } else { completed + 1 };
                ready = if closed == 0 { ready + 1 } else { ready };
                pending = if closed == 0 { pending } else { pending - 1 };
                published = if closed == 0 { published + 1 } else { published };
                wakes = if closed == 0 { wakes + 1 } else { wakes };
            }
            action Take when (closed == 0 && ready > 0) {
                ready = ready - 1;
                pending = if Buggy == 2 { pending } else { pending - 1 };
                published = if Buggy == 4 { published + 1 } else { published };
            }
            action Invalidate when (epoch <= MaxEpoch - 1) {
                epoch = epoch + 1;
            }
            action AcceptResult when (
                Buggy == 1 || (closed == 0 && request_epoch == epoch)
            ) {
                invalid_accept = if closed == 0 && request_epoch == epoch {
                    invalid_accept
                } else { 1 };
            }
            action MainProgress {
                main_wait = if Buggy == 1 && pending > 0 { 1 } else { main_wait };
            }
            action Close when (closed == 0) {
                closed = 1;
                retiring = 1;
                pending = if Buggy == 6 { active + ready } else { active };
                ready = if Buggy == 6 { ready } else { 0 };
                wakes = if Buggy == 5 { wakes + 1 } else { wakes };
            }
            action Retire when (
                retiring == 1 && (active == 0 || Buggy == 1)
            ) {
                retiring = 0;
                retired = 1;
            }
            invariant OneOutstanding: pending <= 1;
            // Every outstanding request is in flight or ready (dial 2 keeps a taken result
            // outstanding); no more completions than requests (dial 3 counts one twice); a
            // completion publishes once (dial 4 re-publishes a taken result); only a
            // publication wakes the caller (dial 5 wakes it on close); close drops the unclaimed
            // result (dial 6 keeps it).
            invariant OutstandingAccounted: pending == active + ready;
            invariant CallsBounded: requests <= MaxRequests && completed <= requests;
            invariant PublicationsBounded: published <= completed;
            invariant OnlyPublicationWakes: wakes == published;
            invariant ClosedHasNoReadyResult: closed == 0 || ready == 0;
            invariant RetirementFollowsAcquisition: retired == 0 || active == 0;
            invariant NoInvalidAcceptance: invalid_accept == 0;
            invariant NoCallerWait: main_wait == 0;
        }
    }
}

/// Ligature slicing gate: the shaping-acceptance policy admits exactly the two grid-mappable
/// shape forms. A shape has `n_in` input cells and `n_out` output glyphs, and a config flag
/// `admit` (Cascadia N:1 admission, default off); the gate accepts iff the shape is 1:1
/// (`n_out == n_in`, the Fira/JetBrains spacer form) or, when `admit`, an N:1 collapse
/// (`n_out == 1`, `n_in >= 2`). `ConservativeAccept` holds over the bounded
/// `(n_in, n_out, admit)` space at `Buggy=0`; `Buggy=1` admits a collapse without the flag,
/// drawing a wide glyph the slicing path is not wired for. The real decision is aterm-render's
/// `aterm_render::ligature_shaping::classify_shape` (the seam in `map_shaped_run`); Tier-1 is
/// `tests/ligature_slice.rs::classify_shape_conforms_to_ligature_gate_model`, which replays
/// `Classify` with `accept'` from the real verdict over the whole `0..=8` lattice. The N:1 tile
/// arithmetic is not here (ty has no multiplication): it is the lattice proof in
/// `tests/ligature_slice.rs`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn ligature_gate_model() -> Model {
    // Cap on n_in / n_out — enough to reach 1:1, N:1 collapse (N up to 4), partial
    // collapse (3:2), and expansion (2:3).
    let n = 4;
    // The two grid-mappable predicates over the unprimed (picked) counts.
    let one_to_one = || and_(gt(var("n_in"), int(0)), eq(var("n_out"), var("n_in")));
    let collapse = || and_(eq(var("n_out"), int(1)), gt(var("n_in"), int(1)));
    // accept' = 1:1 ? 1 : (collapse AND (admit OR Buggy)) ? 1 : 0. Buggy weakens the
    // `admit` guard, reproducing the "admit collapse without the flag" defect.
    let admit_or_buggy = gt(add(var("admit"), cst("Buggy")), int(0));
    let accept_expr = if_(
        one_to_one(),
        int(1),
        if_(and_(collapse(), admit_or_buggy), int(1), int(0)),
    );
    Model {
        name: "LigatureGate",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "n_in",
                init: 0,
            },
            StateVar {
                name: "n_out",
                init: 0,
            },
            StateVar {
                name: "admit",
                init: 0,
            },
            StateVar {
                name: "accept",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // Pick a shape: any (n_in, n_out) in 0..=N and any flag setting.
                name: "Pick",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "n_in",
                        expr: in_range(int(0), int(n)),
                    },
                    Update {
                        var: "n_out",
                        expr: in_range(int(0), int(n)),
                    },
                    Update {
                        var: "admit",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "accept",
                        expr: int(0),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                // Classify the picked shape (classify_shape); loop back so the state
                // space is the whole picked lattice with no deadlock.
                name: "Classify",
                guard: Some(eq(var("phase"), int(1))),
                updates: vec![
                    Update {
                        var: "accept",
                        expr: accept_expr,
                    },
                    Update {
                        var: "phase",
                        expr: int(0),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            // An accepted shape is always grid-mappable: 1:1, or an N:1 collapse with the
            // flag. The envelope uses the true `admit`, so a Buggy accept of an unflagged
            // collapse breaks it.
            name: "ConservativeAccept",
            kind: InvariantKind::Claim,
            expr: le(
                var("accept"),
                if_(
                    or_(one_to_one(), and_(collapse(), eq(var("admit"), int(1)))),
                    int(1),
                    int(0),
                ),
            ),
        }],
    }
}

/// HDR reconfigure re-tag: reconfiguring an f16 DX12 surface (resize, a live composite-alpha
/// change, Outdated/Lost recovery) recreates its swapchain and clears the DXGI colour-space tag,
/// and Windows may disable HDR without any such event, so the same decision follows a bounded
/// live validation. The surface stays f16 only when re-establishing scRGB succeeds; otherwise
/// it selects the retained SDR format and switches capture metadata from extended-linear to
/// sRGB before another present.
///
/// Stage 0 is a confirmed f16/scRGB surface. `RetagSucceeds` keeps that pairing; `RetagFails`
/// performs the two-field fallback; `EnterSdrFallback -> UpgradeSucceeds/UpgradeFails` is the
/// same-size HDR-on path from a retained eligible SDR surface. `Buggy=1` enables three negative
/// controls: `BuggyRetagFailureIgnored` keeps f16 and linear capture after a failed re-tag
/// (`ResolvedF16RequiresSuccessfulRetag`); `BuggyUpgradeFailureKeepsF16` stays f16 after a
/// failed upgrade while capture reads SDR (both laws); `BuggyEscapeKeepsLinearCapture` is an SDR
/// escape whose apply lost its metadata half (`CaptureMatchesSurfaceEncoding`). "A failed re-tag
/// falls back on both fields at once" is the two laws together at stage 2; "the retained stage
/// is SDR" is the definition of stage 1, which the renderer derives from `!surf.is_hdr()`.
///
/// Tier-1 is one aterm-gpu test, `src/renderer.rs`'s `hdr_reconfigure_apply_conforms_to_retag_model`,
/// and every action is `#[refines]`-bound to both the planner (`format_plan::hdr_reconfigure_plan`)
/// and the apply that performs it: the test drives the shipping planner and the private
/// `WindowGpu::apply_hdr_reconfigure_plan` / `apply_hdr_surface_upgrade` on a real window and
/// projects with `project_hdr_reconfigure_state`. Of the projection, only `capture_linear` is
/// read back after the shipping apply (so an apply that skips its metadata half lands on no
/// successor); `is_f16` is the planner's format, the real format half living in
/// `GpuRenderer::finish_surface_color_space_recovery` (it needs a real DX12 swapchain) and held
/// by construction: `renderer.rs`'s `LiveSurface` has no configure but the re-tagging one
/// (`the_screen_upgrade_moves_the_layer_before_it_promotes_the_metadata`); `stage` and
/// `retagged` are drive coordinates (`RetagFails` and `EnterSdrFallback` are one apply read at
/// two phases, and the phase and the `if surf.is_hdr()` guard live on the renderer, which
/// `LiveSurface` holds the same way). The test replays the ignore-failure policy as a negative
/// control: a binding that stopped at the planner would stay green if an apply forgot its
/// metadata half, which is what `CaptureMatchesSurfaceEncoding` forbids.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn hdr_reconfigure_retag_model() -> Model {
    aterm_spec::ty_model! {
        HdrReconfigureRetag {
            const Buggy = 0;
            // 0 confirmed HDR, 1 retained eligible SDR, 2 recovery resolved.
            var stage = 0;
            var retagged = 0;
            var is_f16 = 1;
            var capture_linear = 1;

            action RetagSucceeds when (stage == 0) {
                stage = 2;
                retagged = 1;
                is_f16 = 1;
                capture_linear = 1;
            }

            action RetagFails when (stage == 0) {
                stage = 2;
                retagged = 0;
                is_f16 = 0;
                capture_linear = 0;
            }

            action EnterSdrFallback when (stage == 0) {
                stage = 1;
                retagged = 0;
                is_f16 = 0;
                capture_linear = 0;
            }

            action UpgradeSucceeds when (stage == 1) {
                stage = 2;
                retagged = 1;
                is_f16 = 1;
                capture_linear = 1;
            }

            action UpgradeFails when (stage == 1) {
                stage = 2;
                retagged = 0;
                is_f16 = 0;
                capture_linear = 0;
            }

            action BuggyRetagFailureIgnored when (Buggy == 1 && stage == 0) {
                stage = 2;
                retagged = 0;
                is_f16 = 1;
                capture_linear = 1;
            }

            action BuggyUpgradeFailureKeepsF16 when (Buggy == 1 && stage == 1) {
                stage = 2;
                retagged = 0;
                is_f16 = 1;
                capture_linear = 0;
            }

            action BuggyEscapeKeepsLinearCapture when (Buggy == 1 && stage == 0) {
                stage = 1;
                retagged = 0;
                is_f16 = 0;
                capture_linear = 1;
            }

            invariant ResolvedF16RequiresSuccessfulRetag:
                if stage == 2 && is_f16 == 1 {
                    retagged == 1
                } else {
                    retagged <= 1
                };
            invariant CaptureMatchesSurfaceEncoding:
                capture_linear == is_f16;
            bound ValuesBounded:
                stage <= 2 && retagged <= 1 &&
                is_f16 <= 1 && capture_linear <= 1;
        }
    }
}

/// Process-global serious-mode master gate. Requested feature bits are kept separate from their
/// effective outputs, so enabling the master silences every decorative/audio lane without
/// destroying the configuration; requests may change while the gate is closed, and disabling
/// serious mode reveals the latest requested vector exactly.
///
/// The seven requested bits are the independently gated families: cursor trail/glow (with its
/// sound), BEL sound, sparkle/cat decorations, matrix rain, transient celebrations, native
/// Settings previews, and GPU bloom/shimmer. The level-up notice is a second projection of the
/// celebration request, so already-shown celebration UI hides too. Static content, cursor
/// blink, selection and the visual bell are outside the model: serious mode keeps them.
/// `Buggy=1` forgets to mute the cursor trail/audio lane.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn serious_mode_model() -> Model {
    aterm_spec::ty_model! {
        SeriousMode {
            const Buggy = 0;
            var serious = 0;
            var want_trail = 1;
            var want_bell = 1;
            var want_sparkle = 1;
            var want_rain = 1;
            var want_celebration = 1;
            var want_preview = 1;
            var want_gpu = 1;
            var trail = 1;
            var bell = 1;
            var sparkle = 1;
            var rain = 1;
            var celebration = 1;
            var notice = 1;
            var preview = 1;
            var gpu = 1;
            action Enable when (serious == 0) {
                serious = 1;
                trail = if Buggy == 1 { want_trail } else { 0 };
                bell = 0;
                sparkle = 0;
                rain = 0;
                celebration = 0;
                notice = 0;
                preview = 0;
                gpu = 0;
            }
            action Disable when (serious == 1) {
                serious = 0;
                trail = want_trail;
                bell = want_bell;
                sparkle = want_sparkle;
                rain = want_rain;
                celebration = want_celebration;
                notice = want_celebration;
                preview = want_preview;
                gpu = want_gpu;
            }
            action ChangeTrail {
                want_trail = 1 - want_trail;
                trail = if serious == 1 { 0 } else { 1 - want_trail };
            }
            action ChangeBell {
                want_bell = 1 - want_bell;
                bell = if serious == 1 { 0 } else { 1 - want_bell };
            }
            action ChangeSparkle {
                want_sparkle = 1 - want_sparkle;
                sparkle = if serious == 1 { 0 } else { 1 - want_sparkle };
            }
            action ChangeRain {
                want_rain = 1 - want_rain;
                rain = if serious == 1 { 0 } else { 1 - want_rain };
            }
            action ChangeCelebration {
                want_celebration = 1 - want_celebration;
                celebration = if serious == 1 { 0 } else { 1 - want_celebration };
                notice = if serious == 1 { 0 } else { 1 - want_celebration };
            }
            action ChangePreview {
                want_preview = 1 - want_preview;
                preview = if serious == 1 { 0 } else { 1 - want_preview };
            }
            action ChangeGpu {
                want_gpu = 1 - want_gpu;
                gpu = if serious == 1 { 0 } else { 1 - want_gpu };
            }
            invariant SeriousSilencesEverything:
                if serious == 1 {
                    trail == 0 && bell == 0 && sparkle == 0 &&
                    rain == 0 && celebration == 0 &&
                    notice == 0 && preview == 0 && gpu == 0
                } else {
                    trail == want_trail && bell == want_bell &&
                    sparkle == want_sparkle && rain == want_rain &&
                    celebration == want_celebration &&
                    notice == want_celebration && preview == want_preview &&
                    gpu == want_gpu
                };
            bound SeriousBitBounded: serious <= 1;
            bound EffectiveBitsBounded:
                trail <= 1 && bell <= 1 && sparkle <= 1 &&
                rain <= 1 && celebration <= 1 &&
                notice <= 1 && preview <= 1 && gpu <= 1;
        }
    }
}

/// Bounded terminal incremental-search navigation: the host-owned Cmd-S/Cmd-R state machine,
/// not the text matching. Opening captures a viewport origin, a completed query publishes an
/// ordered hit set, repeated forward/backward chords move one ordinal with exact wrap, RET
/// accepts the selected hit, streaming output deselects stale results before one bounded
/// refresh+repeat, and cancel restores the captured viewport. `pty_writes` pins that these host
/// chords never reach the PTY. Repeat cost is not claimed: `SearchState::step` is modular
/// arithmetic and no shipping counter records work a bind could read.
///
/// `Buggy=1` has one slip per law: opening leaks a byte to the PTY (`NoPtyLeak`); a forward
/// repeat steps past the final ordinal (`CurrentOrdinalBounded`); output leaves the stale hit
/// selected (`StaleBatchNeverSelected`); cancel leaves the viewport moved
/// (`CancelRestoresOrigin`); accept drops the hit (`AcceptKeepsHit`).
///
/// Tier-1: `aterm-gui/src/app_search.rs::emacs_navigation_conforms_to_derived_transition_model`
/// drives the chords through the real host seam (`terminal_emacs_search_pressed` and the
/// physical release) on a headless App whose PTY is an observer pipe, projects the real search,
/// selection, viewport and PTY bytes after every step, and rejects each slip injected into the
/// same real trace at its own action.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn emacs_search_navigation_model() -> Model {
    aterm_spec::ty_model! {
        EmacsSearchNavigation {
            const Buggy = 0;
            const Cap = 3;
            var active = 0;
            var forward = 1;
            var query = 0;
            var hits = 0;
            var current = 0;
            var origin = 0;
            var viewport = 0;
            var selection = 0;
            var dirty = 0;
            // 0 none/open, 1 cancelled, 2 accepted.
            var last_exit = 0;
            // 1 once any byte has reached the PTY.
            var pty_writes = 0;
            action OpenForward when (active == 0) {
                active = 1;
                forward = 1;
                query = 0;
                hits = 0;
                current = 0;
                origin = viewport;
                selection = 0;
                dirty = 0;
                last_exit = 0;
                pty_writes = if Buggy == 1 { 1 } else { pty_writes };
            }
            action OpenBackward when (active == 0) {
                active = 1;
                forward = 0;
                query = 0;
                hits = 0;
                current = 0;
                origin = viewport;
                selection = 0;
                dirty = 0;
                last_exit = 0;
                pty_writes = if Buggy == 1 { 1 } else { pty_writes };
            }
            action PublishHit when (active == 1 && hits <= Cap - 1) {
                query = 1;
                hits = hits + 1;
                current = if forward == 1 { 0 } else { hits };
                viewport = if forward == 1 { 0 } else { hits };
                selection = 1;
                dirty = 0;
                last_exit = 0;
            }
            action PublishMiss when (active == 1) {
                query = 1;
                hits = 0;
                current = 0;
                selection = 0;
                dirty = 0;
                last_exit = 0;
            }
            action Output when (active == 1 && query == 1) {
                dirty = 1;
                selection = if Buggy == 1 { selection } else { 0 };
            }
            // At Buggy=1 the step off the last ordinal lands one past it (and wraps from
            // there, so the space stays bounded).
            action RepeatForward when (active == 1 && hits > 0 && dirty == 0) {
                forward = 1;
                current = if current <= hits - 2 || (Buggy == 1 && current == hits - 1) {
                    current + 1
                } else {
                    0
                };
                viewport = if current <= hits - 2 || (Buggy == 1 && current == hits - 1) {
                    current + 1
                } else {
                    0
                };
                selection = 1;
                last_exit = 0;
            }
            action RepeatBackward when (active == 1 && hits > 0 && dirty == 0) {
                forward = 0;
                current = if current > 0 { current - 1 } else { hits - 1 };
                viewport = if current > 0 { current - 1 } else { hits - 1 };
                selection = 1;
                last_exit = 0;
            }
            action RefreshRepeatForward when (active == 1 && hits > 0 && dirty == 1) {
                forward = 1;
                current = if current <= hits - 2 { current + 1 } else { 0 };
                viewport = if current <= hits - 2 { current + 1 } else { 0 };
                selection = 1;
                dirty = 0;
            }
            action RefreshRepeatBackward when (active == 1 && hits > 0 && dirty == 1) {
                forward = 0;
                current = if current > 0 { current - 1 } else { hits - 1 };
                viewport = if current > 0 { current - 1 } else { hits - 1 };
                selection = 1;
                dirty = 0;
            }
            action RefreshMiss when (active == 1 && dirty == 1) {
                hits = 0;
                current = 0;
                selection = 0;
                dirty = 0;
            }
            action Cancel when (active == 1) {
                active = 0;
                viewport = if Buggy == 1 { viewport } else { origin };
                selection = 0;
                last_exit = 1;
            }
            action Accept when (active == 1 && dirty == 0) {
                active = 0;
                selection = if hits > 0 && Buggy == 0 { 1 } else { 0 };
                last_exit = 2;
            }
            invariant NoPtyLeak: pty_writes == 0;
            invariant CurrentOrdinalBounded:
                if hits == 0 { current == 0 } else { 0 <= current && current <= hits - 1 };
            bound HitCountBounded: hits <= Cap;
            bound DirectionBounded: forward <= 1;
            bound DirtyBounded: dirty <= 1;
            invariant StaleBatchNeverSelected:
                if dirty == 1 { selection == 0 } else { selection <= 1 };
            invariant CancelRestoresOrigin:
                if last_exit == 1 { viewport == origin && selection == 0 } else { viewport <= Cap };
            invariant AcceptKeepsHit:
                if last_exit == 2 && hits > 0 { selection == 1 } else { selection <= 1 };
        }
    }
}

/// Scroll-glide convergence and policy settlement. Under Full motion an armed wheel glide makes
/// strict progress toward its target on every wake, disarms exactly when it lands, and so wakes
/// a bounded number of times per arm (no perpetual wake). A Full→Reduced edge lands at the
/// target and disarms atomically, so Reduced owns no glide deadline. The real code is
/// aterm-gui's `scroll_motion::Glide` sampling and `App::settle_scroll_motion_at_target`;
/// Tier-1 includes `glide_disarms_in_bounded_wakes`, `glide_converges_monotonically_and_exactly`,
/// `reduced_motion_settle_conforms_to_scroll_glide_model`, and
/// `output_mid_glide_conforms_to_scroll_glide_model` for `Output`.
///
/// Over a 0..N lane (N = 3; `pos`/`target` are rows into history, 0 = live): `Arm` starts a
/// glide at a nondeterministic target, `Retarget` redirects it (a chained notch; the clock and
/// wake counter restart), `Wake` steps one unit toward the target and disarms iff it arrived.
/// `SetReduced` lands and disarms in one transition; `SetFull` re-enables arms. `wakes`
/// saturates at N+1 to keep the space finite.
///
/// `Output` is the machine moving the frame under a glide bound into history: a line of output
/// while the reader is off live renumbers every history row (`display_offset += lines_added`),
/// and the viewport and the glide's target move together (`Glide::shift`, detected through
/// `ScrollGlideState::engine_row`), so the landing is the content the reader aimed at. `aim` is
/// the target as last set by a person and `drift` the machine's rows since; `TargetIsAnchored`
/// is `target == aim + drift` while armed. A glide bound for live (target 0) is a destination,
/// not content — it lands on live whatever streams
/// (`a_glide_bound_for_live_lands_on_live_while_output_streams`) — so `Output` is modelled for
/// history targets only.
///
/// `Buggy = 1`: `SetReduced` keeps the intermediate position and armed deadline
/// (`ReducedSettled` breaks), and `Output` re-pins the viewport but leaves the target behind,
/// scrolling the reader back toward live on every wake (`TargetIsAnchored` breaks).
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn scroll_glide_model() -> Model {
    // One Full-policy wake's step: toward the target by 1.
    let step = || {
        if_(
            gt(var("pos"), var("target")),
            sub(var("pos"), int(1)),
            if_(
                gt(var("target"), var("pos")),
                add(var("pos"), int(1)),
                var("pos"),
            ),
        )
    };
    Model {
        name: "ScrollGlide",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "pos",
                init: 0,
            },
            StateVar {
                name: "target",
                init: 0,
            },
            StateVar {
                name: "armed",
                init: 0,
            },
            StateVar {
                name: "wakes",
                init: 0,
            },
            StateVar {
                name: "reduced",
                init: 0,
            },
            StateVar {
                name: "aim",
                init: 0,
            },
            StateVar {
                name: "drift",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // A wheel notch on an idle viewport: aim anywhere in 0..N and arm the glide.
                name: "Arm",
                guard: Some(and_(le(var("armed"), int(0)), le(var("reduced"), int(0)))),
                updates: vec![
                    Update {
                        var: "target",
                        expr: in_range(int(0), int(3)),
                    },
                    Update {
                        var: "armed",
                        expr: int(1),
                    },
                    Update {
                        var: "wakes",
                        expr: int(0),
                    },
                    Update {
                        var: "aim",
                        expr: int(0),
                    },
                    Update {
                        var: "drift",
                        expr: int(0),
                    },
                ],
            },
            Action {
                // A chained wheel notch mid-glide: redirect the target; the ease clock and the
                // per-arm wake budget restart, and the machine's drift so far is dropped.
                name: "Retarget",
                guard: Some(and_(gt(var("armed"), int(0)), le(var("reduced"), int(0)))),
                updates: vec![
                    Update {
                        var: "target",
                        expr: in_range(int(0), int(3)),
                    },
                    Update {
                        var: "wakes",
                        expr: int(0),
                    },
                    Update {
                        var: "aim",
                        expr: int(0),
                    },
                    Update {
                        var: "drift",
                        expr: int(0),
                    },
                ],
            },
            Action {
                // One line of output while the reader is off live (SCR-1's re-pin,
                // `display_offset += 1`): the glide's start and target shift together with the
                // ease clock untouched (`Glide::shift`), so the remaining distance and wake
                // budget are unchanged. `aim` captures the person's target the first time the
                // machine moves it. Buggy re-pins the viewport but leaves the target behind.
                name: "Output",
                guard: Some(and_(
                    and_(gt(var("armed"), int(0)), le(var("reduced"), int(0))),
                    and_(
                        and_(gt(var("pos"), int(0)), le(var("pos"), int(2))),
                        and_(gt(var("target"), int(0)), le(var("target"), int(2))),
                    ),
                )),
                updates: vec![
                    Update {
                        var: "pos",
                        expr: add(var("pos"), int(1)),
                    },
                    Update {
                        var: "target",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            var("target"),
                            add(var("target"), int(1)),
                        ),
                    },
                    Update {
                        var: "aim",
                        expr: if_(eq(var("drift"), int(0)), var("target"), var("aim")),
                    },
                    Update {
                        var: "drift",
                        expr: add(var("drift"), int(1)),
                    },
                ],
            },
            Action {
                // One Full-policy deadline fires: step toward the target, count the wake
                // (saturating at N+1), and disarm iff the step landed on target.
                name: "Wake",
                guard: Some(and_(gt(var("armed"), int(0)), le(var("reduced"), int(0)))),
                updates: vec![
                    Update {
                        var: "pos",
                        expr: step(),
                    },
                    // Buggy = 2 disarms a row short of the target.
                    Update {
                        var: "armed",
                        expr: if_(
                            or_(
                                eq(step(), var("target")),
                                and_(
                                    eq(cst("Buggy"), int(2)),
                                    and_(
                                        le(sub(step(), var("target")), int(1)),
                                        le(sub(var("target"), step()), int(1)),
                                    ),
                                ),
                            ),
                            int(0),
                            int(1),
                        ),
                    },
                    Update {
                        var: "wakes",
                        expr: if_(
                            le(var("wakes"), int(3)),
                            add(var("wakes"), int(1)),
                            var("wakes"),
                        ),
                    },
                ],
            },
            Action {
                // Full→Reduced: committed code lands and disarms atomically; Buggy keeps the
                // intermediate row and its armed deadline.
                name: "SetReduced",
                guard: Some(le(var("reduced"), int(0))),
                updates: vec![
                    Update {
                        var: "pos",
                        expr: if_(eq(cst("Buggy"), int(1)), var("pos"), var("target")),
                    },
                    Update {
                        var: "armed",
                        expr: if_(eq(cst("Buggy"), int(1)), var("armed"), int(0)),
                    },
                    Update {
                        var: "reduced",
                        expr: int(1),
                    },
                ],
            },
            Action {
                // Restoring Full motion permits a later wheel gesture to arm.
                name: "SetFull",
                guard: Some(gt(var("reduced"), int(0))),
                updates: vec![Update {
                    var: "reduced",
                    expr: int(0),
                }],
            },
        ],
        invariants: vec![
            Invariant {
                // No perpetual wake: one arm's ease is over within N wakes (the farthest
                // target is N away and every Full wake advances).
                name: "BoundedWakes",
                kind: InvariantKind::Claim,
                expr: le(var("wakes"), int(3)),
            },
            Invariant {
                // Disarm happens exactly at the target: armed = 0 => pos = target.
                name: "DisarmedAtTarget",
                kind: InvariantKind::Claim,
                expr: or_(gt(var("armed"), int(0)), eq(var("pos"), var("target"))),
            },
            Invariant {
                // Reduced motion has no retained ease/deadline and rests exactly
                // on the intended target in the same policy transition.
                name: "ReducedSettled",
                kind: InvariantKind::Claim,
                expr: or_(
                    le(var("reduced"), int(0)),
                    and_(le(var("armed"), int(0)), eq(var("pos"), var("target"))),
                ),
            },
            Invariant {
                // Output never takes the reader's aim away: while armed, the target is the
                // person's aim plus every row the machine moved the frame by since.
                name: "TargetIsAnchored",
                kind: InvariantKind::Claim,
                expr: or_(
                    or_(le(var("armed"), int(0)), le(var("drift"), int(0))),
                    eq(var("target"), add(var("aim"), var("drift"))),
                ),
            },
        ],
    }
}

/// Decoration band containment: a decoration band never leaves its cell. The band
/// `(top, thickness)` is clamped in a fixed order — thickness into `[1, cell_h]` first, then top
/// into `[0, cell_h − thickness]` — which guarantees `top + thickness <= cell_h` for any raw
/// input. The real clamp is `aterm_render::deco::clamp_band`, behind every decoration emitter
/// (`underline_rects_into`, `strike_rect`, `overline_rect`). Tier-1:
/// `aterm-render/tests/deco_lines.rs::resolved_bands_conform_to_the_deco_band_containment_model`
/// drives the shipping `resolve_deco_metrics` over the whole lattice (the underline through its
/// config adjust hatches, the strike through its OS/2 table) and requires each settled `(t, y)`
/// to be the model's one successor; `resolved_bands_always_inside_the_cell` and
/// `decoration_writes_stay_within_the_run_band` carry the law past the lattice and through the
/// real emitters. The resolver's per-em scaling needs `mul` and is checked only there.
///
/// `Buggy = 1` has one slip per law: clamping the top against `cell_h` instead of `cell_h − t`
/// lets a low thick band spill into the row below (`Contained`), and dropping the thickness
/// floor settles a zero request at `t = 0`, an underline that draws nothing (`ThicknessInCell`).
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn deco_band_containment_model() -> Model {
    // clamp(raw_t, 1, cell_h): raw_t is >= 0 on the lattice, so `< 1` is `== 0`.
    // Buggy = the floor dropped: a zero request stays zero.
    let clamp_t = || {
        if_(
            le(var("raw_t"), int(0)),
            if_(gt(cst("Buggy"), int(0)), int(0), int(1)),
            if_(gt(var("raw_t"), var("cell_h")), var("cell_h"), var("raw_t")),
        )
    };
    // The upper bound for the top: cell_h - t (room for the whole stroke); Buggy = cell_h.
    let y_bound = || {
        if_(
            gt(cst("Buggy"), int(0)),
            var("cell_h"),
            sub(var("cell_h"), var("t")),
        )
    };
    // clamp(raw_top, 0, y_bound): raw_top >= 0, so only the upper clamp bites.
    let clamp_y = || if_(gt(var("raw_top"), y_bound()), y_bound(), var("raw_top"));
    let settled_implies = |body: Expr| or_(neq(var("phase"), int(5)), body);
    Model {
        name: "DecoBandContainment",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "cell_h",
                init: 1,
            },
            StateVar {
                name: "raw_t",
                init: 0,
            },
            StateVar {
                name: "raw_top",
                init: 0,
            },
            StateVar { name: "t", init: 0 },
            StateVar { name: "y", init: 0 },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "PickCell",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "cell_h",
                        expr: in_range(int(1), int(5)),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                // Raw thickness may be 0 or exceed the cell; the clamp tames either.
                name: "PickThick",
                guard: Some(eq(var("phase"), int(1))),
                updates: vec![
                    Update {
                        var: "raw_t",
                        expr: in_range(int(0), int(7)),
                    },
                    Update {
                        var: "phase",
                        expr: int(2),
                    },
                ],
            },
            Action {
                // Raw top may point past the cell — the settle must pull it back.
                name: "PickTop",
                guard: Some(eq(var("phase"), int(2))),
                updates: vec![
                    Update {
                        var: "raw_top",
                        expr: in_range(int(0), int(7)),
                    },
                    Update {
                        var: "phase",
                        expr: int(3),
                    },
                ],
            },
            Action {
                // Thickness first, into [1, cell_h].
                name: "SettleThick",
                guard: Some(eq(var("phase"), int(3))),
                updates: vec![
                    Update {
                        var: "t",
                        expr: clamp_t(),
                    },
                    Update {
                        var: "phase",
                        expr: int(4),
                    },
                ],
            },
            Action {
                // Top second, against the thickness-aware bound.
                name: "SettleTop",
                guard: Some(eq(var("phase"), int(4))),
                updates: vec![
                    Update {
                        var: "y",
                        expr: clamp_y(),
                    },
                    Update {
                        var: "phase",
                        expr: int(5),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                // The settled band never leaves the cell bottom; Buggy=1's cell_h bound lets
                // a low thick band spill (y + t > cell_h).
                name: "Contained",
                kind: InvariantKind::Claim,
                expr: settled_implies(le(add(var("y"), var("t")), var("cell_h"))),
            },
            Invariant {
                // The stroke is a visible, in-cell height; Buggy=1's dropped floor settles a
                // zero request at t = 0, a line that draws nothing.
                name: "ThicknessInCell",
                kind: InvariantKind::Claim,
                expr: or_(
                    neq(var("phase"), int(5)),
                    and_(le(int(1), var("t")), le(var("t"), var("cell_h"))),
                ),
            },
        ],
    }
}

/// Vibrancy legibility: translucent glass (`background_opacity < 1.0`) raises the effective
/// per-cell minimum-contrast floor to WCAG AA (4.5:1), so glass never makes text illegible. The
/// real resolver is `aterm_render::vibrancy::effective_minimum_contrast`, applied at blit time by
/// `aterm_render::Renderer::effective_minimum_contrast` (so native, CPU-web and GPU-web share
/// it); Tier-1 is `crates/aterm-render/tests/vibrancy_contrast.rs`, which checks the
/// `NeverIllegible` bound over the opacity × configured-contrast grid and drives this model's
/// `Fire` step against the real resolver point for point.
///
/// Contrast lattice `0..=6`, with WCAG 4.5:1 at `Floor = 3`. `Pick` chooses `translucent` and
/// the configured floor `base`; `Fire` resolves `max(base, Floor)` when translucent, else
/// `base`. `NeverIllegible`: `translucent ⇒ effective >= Floor`, however low
/// `minimum_contrast` is set. `Buggy = 1` drops the auto-floor, so default
/// `minimum_contrast` on glass gets sub-4.5:1 text.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn vibrancy_contrast_model() -> Model {
    // max(base, Floor): the auto-engaged legibility floor for translucent glass.
    let maxf = || if_(gt(var("base"), cst("Floor")), var("base"), cst("Floor"));
    // The effective floor from the settled (unprimed) scene: translucent windows floor up to
    // WCAG AA; opaque windows keep the configured value. Buggy=1 returns the raw base.
    let effective = if_(
        eq(var("translucent"), int(1)),
        if_(eq(cst("Buggy"), int(0)), maxf(), var("base")),
        var("base"),
    );
    Model {
        name: "VibrancyContrast",
        consts: vec![("Buggy", 0), ("Floor", 3)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "translucent",
                init: 0,
            },
            StateVar {
                name: "base",
                init: 0,
            },
            StateVar {
                name: "effective",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Pick",
                guard: Some(le(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "translucent",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "base",
                        expr: in_range(int(0), int(6)),
                    },
                    Update {
                        var: "effective",
                        expr: int(0),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                name: "Fire",
                guard: Some(gt(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "effective",
                        expr: effective,
                    },
                    Update {
                        var: "phase",
                        expr: int(0),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            // Picked-but-unfired states (phase = 1) carry a stale effective and
            // are exempt; every settled translucent scene clears the floor.
            name: "NeverIllegible",
            kind: InvariantKind::Claim,
            expr: or_(
                gt(var("phase"), int(0)),
                or_(
                    eq(var("translucent"), int(0)),
                    le(cst("Floor"), var("effective")),
                ),
            ),
        }],
    }
}

/// Window fit and padding absorption: the swapchain is sized to the raw window pixels, and the
/// maximal grid (`cols = floor((w - 2*pad)/cell)`) plus per-edge pads absorbing the
/// `0..cell-1` remainder must tile the window exactly (`pad_lo + cols*cell + pad_hi == w`),
/// keep the configured pad floor, and split the remainder near-evenly
/// (`pad_lo <= pad_hi <= pad_lo + 1`), so the compositor never rescales the frame and the bands
/// never lean to one side. The real policy is `aterm_render::pad_split`; Tier-1 is
/// `aterm-render/tests/pad_absorption.rs`, which drives this model's interpreter against it
/// point for point over the whole lattice, plus a direct lattice-exhaustive check.
///
/// With no `*`/`/` in ty, `FitColumn` builds `cols*cell` by repeated addition (maximality is the
/// `Settle` guard) and `rem/2` is a case split (`rem <= 3` here). `Buggy = 1` puts the whole
/// remainder on the trailing edge, so a `rem >= 2` window breaks `NearEvenSplit`.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn pad_absorption_model() -> Model {
    // usable == w - 2*pad; rem == usable - acc. All unprimed (evaluated at the
    // pre-state of the one Settle step that consumes them).
    let usable = || sub(sub(var("w"), var("pad")), var("pad"));
    let rem = || sub(usable(), var("acc"));
    // rem/2 without division: 0 for rem in {0,1}, 1 for {2,3} (cell <= 4 on
    // this lattice, so rem <= 3).
    let half = || if_(le(rem(), int(1)), int(0), int(1));
    // Buggy=1: the lopsided split — the leading pad keeps none of the remainder.
    let lo_share = || if_(eq(cst("Buggy"), int(1)), int(0), half());
    let dial = |k: i64| eq(cst("Buggy"), int(k));
    // Invariants hold vacuously until the split settles (phase 4).
    let settled_implies = |body: Expr| or_(neq(var("phase"), int(4)), body);
    Model {
        name: "PadAbsorption",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "cell",
                init: 1,
            },
            StateVar {
                name: "pad",
                init: 0,
            },
            StateVar { name: "w", init: 0 },
            StateVar {
                name: "acc",
                init: 0,
            },
            StateVar {
                name: "pad_lo",
                init: 0,
            },
            StateVar {
                name: "pad_hi",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "PickCell",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "cell",
                        expr: in_range(int(1), int(4)),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                name: "PickPad",
                guard: Some(eq(var("phase"), int(1))),
                updates: vec![
                    Update {
                        var: "pad",
                        expr: in_range(int(0), int(2)),
                    },
                    Update {
                        var: "phase",
                        expr: int(2),
                    },
                ],
            },
            Action {
                // Every in-domain window: w >= 2*pad + cell (one padded cell fits), up to the
                // exhaustive cap.
                name: "PickWin",
                guard: Some(eq(var("phase"), int(2))),
                updates: vec![
                    Update {
                        var: "w",
                        expr: in_range(add(add(var("pad"), var("pad")), var("cell")), int(14)),
                    },
                    Update {
                        var: "phase",
                        expr: int(3),
                    },
                ],
            },
            Action {
                // cols*cell by repeated addition: fit one more column while it fits.
                name: "FitColumn",
                guard: Some(and_(
                    eq(var("phase"), int(3)),
                    le(add(var("acc"), var("cell")), usable()),
                )),
                updates: vec![Update {
                    var: "acc",
                    expr: add(var("acc"), var("cell")),
                }],
            },
            Action {
                // Maximality is the guard: one more column would overflow the usable extent.
                // Absorb the remainder into the two pads.
                name: "Settle",
                // Buggy = 4 settles while another column still fits.
                guard: Some(and_(
                    eq(var("phase"), int(3)),
                    or_(gt(add(var("acc"), var("cell")), usable()), dial(4)),
                )),
                updates: vec![
                    // Buggy = 3 forgets the configured pad on the leading edge; Buggy = 2 drops
                    // the trailing edge's share of the remainder.
                    Update {
                        var: "pad_lo",
                        expr: if_(dial(3), lo_share(), add(var("pad"), lo_share())),
                    },
                    Update {
                        var: "pad_hi",
                        expr: if_(dial(2), var("pad"), add(var("pad"), sub(rem(), lo_share()))),
                    },
                    Update {
                        var: "phase",
                        expr: int(4),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                // pad_lo + cols*cell + pad_hi == w: no pixel scaled or dropped.
                name: "ExactCover",
                kind: InvariantKind::Claim,
                expr: settled_implies(eq(
                    add(add(var("pad_lo"), var("acc")), var("pad_hi")),
                    var("w"),
                )),
            },
            Invariant {
                // Both pads keep the configured floor.
                name: "PadsFloor",
                kind: InvariantKind::Claim,
                expr: settled_implies(and_(
                    le(var("pad"), var("pad_lo")),
                    le(var("pad"), var("pad_hi")),
                )),
            },
            Invariant {
                // |pad_hi - pad_lo| <= 1, leaning to the trailing edge.
                name: "NearEvenSplit",
                kind: InvariantKind::Claim,
                expr: settled_implies(and_(
                    le(var("pad_lo"), var("pad_hi")),
                    le(var("pad_hi"), add(var("pad_lo"), int(1))),
                )),
            },
            Invariant {
                // The settled grid is maximal (restated as a state property).
                name: "Maximal",
                kind: InvariantKind::Claim,
                expr: settled_implies(gt(
                    add(var("acc"), var("cell")),
                    sub(sub(var("w"), var("pad")), var("pad")),
                )),
            },
        ],
    }
}

/// Raw-renderer asymmetric top-padding redistribution and cache identity.
///
/// This is the renderer's raw transport allocation, before a frontend crop: tightening only the
/// raw top edge leaves the allocation unchanged, so the raw bottom absorbs `pad - pad_top` and
/// `pad_top + raw_pad_bottom == 2*pad` ([`visible_pad_crop_model`] proves the GUI-visible
/// `visible_bottom == pad`). The requested top is clamped to the base pad. Raw dimensions and
/// content can stay byte-identical while the grid Y origin moves, so CPU/GPU cache reuse must
/// key the effective `grid_top`, not dimensions alone.
///
/// The lattice abstracts grid height away (it cancels from the cover equation) but keeps the
/// head band in `grid_top`. `Buggy=1` slips, one per law:
/// * the initial top accepted unclamped while the bottom stays at the symmetric pad
///   (`ExactVerticalPadCover`, `TopPadIsBounded`);
/// * the changed top clamped only where reported — `pad_top()` clamps, `set_pad_top` stores the
///   raw request, `grid_top()` reads the field — so the origin and bottom band follow the
///   unclamped request (`GridOriginTracksTopAndHead`);
/// * or the changed top taken unclamped everywhere with the bottom at the symmetric pad
///   (`ExactVerticalPadCover`, `TopPadIsBounded`);
/// * a cache gate keyed on dimensions/content only, so a moved origin hits a stale frame
///   (`LayoutChangeForcesFullRepaint`);
/// * a gate comparing a size the stored frame never has, so every present falls back to a full
///   repaint — a scissored present's `offscreen_holds_prev` sized unpadded against a padded
///   offscreen is always false at `pad > 0` (`IdenticalLayoutMayReuseCache`).
///
/// The two gate faults are alternatives, as are the two changed-top faults, so `Buggy=1` picks
/// one of each per layout in `PickLayout` (`gate_fault`, `top_fault`; pinned to 0 at `Buggy=0`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn asymmetric_pad_layout_model() -> Model {
    let clamp_initial = || {
        if_(
            le(var("initial_request"), var("pad")),
            var("initial_request"),
            var("pad"),
        )
    };
    let clamp_changed = || {
        if_(
            le(var("changed_request"), var("pad")),
            var("changed_request"),
            var("pad"),
        )
    };
    let initial_top = || {
        if_(
            eq(cst("Buggy"), int(1)),
            var("initial_request"),
            clamp_initial(),
        )
    };
    let changed_top = || {
        if_(
            eq(cst("Buggy"), int(1)),
            var("changed_request"),
            clamp_changed(),
        )
    };
    // The runtime path's own non-absorbing slip (`top_fault == 1`): the raw
    // request is the top everywhere, and the bottom stays at the old pad.
    let non_absorbing = || and_(eq(cst("Buggy"), int(1)), eq(var("top_fault"), int(1)));
    let initial_bottom = || {
        if_(
            eq(cst("Buggy"), int(1)),
            var("pad"),
            sub(add(var("pad"), var("pad")), clamp_initial()),
        )
    };
    let layouts_match = || eq(var("cached_grid_top"), var("grid_top"));
    let settled_layout = || gt(var("phase"), int(1));
    Model {
        name: "AsymmetricPadLayout",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "pad",
                init: 1,
            },
            StateVar {
                name: "head",
                init: 0,
            },
            StateVar {
                name: "initial_request",
                init: 0,
            },
            StateVar {
                name: "changed_request",
                init: 0,
            },
            StateVar {
                name: "pad_top",
                init: 0,
            },
            StateVar {
                name: "pad_bottom",
                init: 0,
            },
            StateVar {
                name: "grid_top",
                init: 0,
            },
            StateVar {
                name: "cached_grid_top",
                init: 0,
            },
            StateVar {
                name: "cache_valid",
                init: 0,
            },
            StateVar {
                name: "cache_hit",
                init: 0,
            },
            StateVar {
                name: "full_repaint",
                init: 0,
            },
            // Which gate fault a `Buggy=1` layout carries: 0 the
            // dimension-only key, 1 the always-false padded-size gate. Always 0
            // at `Buggy=0`, where no fault exists to choose.
            StateVar {
                name: "gate_fault",
                init: 0,
            },
            // Which changed-top fault a `Buggy=1` layout carries: 0 the
            // getter-only clamp, 1 the non-absorbing runtime bottom. Always 0
            // at `Buggy=0`.
            StateVar {
                name: "top_fault",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "PickLayout",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "gate_fault",
                        expr: in_range(int(0), cst("Buggy")),
                    },
                    Update {
                        var: "top_fault",
                        expr: in_range(int(0), cst("Buggy")),
                    },
                    Update {
                        var: "pad",
                        expr: in_range(int(1), int(3)),
                    },
                    Update {
                        var: "head",
                        expr: in_range(int(0), int(2)),
                    },
                    Update {
                        var: "initial_request",
                        expr: in_range(int(0), int(4)),
                    },
                    Update {
                        var: "changed_request",
                        expr: in_range(int(0), int(4)),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                name: "ApplyInitialTop",
                guard: Some(eq(var("phase"), int(1))),
                updates: vec![
                    Update {
                        var: "pad_top",
                        expr: initial_top(),
                    },
                    Update {
                        var: "pad_bottom",
                        expr: initial_bottom(),
                    },
                    Update {
                        var: "grid_top",
                        expr: add(var("head"), initial_top()),
                    },
                    Update {
                        var: "phase",
                        expr: int(2),
                    },
                ],
            },
            Action {
                name: "PrimeLayoutCache",
                guard: Some(eq(var("phase"), int(2))),
                updates: vec![
                    Update {
                        var: "cached_grid_top",
                        expr: var("grid_top"),
                    },
                    Update {
                        var: "cache_valid",
                        expr: int(1),
                    },
                    Update {
                        var: "cache_hit",
                        expr: int(0),
                    },
                    Update {
                        var: "full_repaint",
                        expr: int(0),
                    },
                    Update {
                        var: "phase",
                        expr: int(3),
                    },
                ],
            },
            Action {
                name: "ApplyChangedTop",
                guard: Some(eq(var("phase"), int(3))),
                updates: vec![
                    // The getter-only clamp reports a clamped top while Buggy's origin and
                    // bottom band read the raw request (`changed_top`); the non-absorbing slip
                    // reports the raw request and leaves the bottom at the old pad.
                    Update {
                        var: "pad_top",
                        expr: if_(non_absorbing(), var("changed_request"), clamp_changed()),
                    },
                    Update {
                        var: "pad_bottom",
                        expr: if_(
                            non_absorbing(),
                            var("pad"),
                            sub(add(var("pad"), var("pad")), changed_top()),
                        ),
                    },
                    Update {
                        var: "grid_top",
                        expr: add(var("head"), changed_top()),
                    },
                    Update {
                        var: "phase",
                        expr: int(4),
                    },
                ],
            },
            Action {
                name: "RenderWithLayoutCache",
                guard: Some(and_(
                    eq(var("phase"), int(4)),
                    eq(var("cache_valid"), int(1)),
                )),
                updates: vec![
                    // Buggy's gate ignores the origin: the dimension-only key
                    // always hits, the padded-size gate never does.
                    Update {
                        var: "cache_hit",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            if_(eq(var("gate_fault"), int(1)), int(0), int(1)),
                            if_(layouts_match(), int(1), int(0)),
                        ),
                    },
                    Update {
                        var: "full_repaint",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            if_(eq(var("gate_fault"), int(1)), int(1), int(0)),
                            if_(layouts_match(), int(0), int(1)),
                        ),
                    },
                    Update {
                        var: "phase",
                        expr: int(5),
                    },
                ],
            },
        ],
        // No bottom-band window law: once settled it is `ExactVerticalPadCover` with
        // `TopPadIsBounded`, and before that `pad_bottom` has no writer. No total-cache-decision
        // law either: only `RenderWithLayoutCache` (the one entry to phase 5) writes a 1 to
        // `cache_hit` or `full_repaint`, always as complements, so outside phase 5 both are 0
        // and inside it exactly one is 1.
        invariants: vec![
            Invariant {
                name: "ExactVerticalPadCover",
                kind: InvariantKind::Claim,
                expr: if_(
                    settled_layout(),
                    eq(
                        add(var("pad_top"), var("pad_bottom")),
                        add(var("pad"), var("pad")),
                    ),
                    le(
                        add(var("pad_top"), var("pad_bottom")),
                        add(var("pad"), var("pad")),
                    ),
                ),
            },
            Invariant {
                name: "TopPadIsBounded",
                kind: InvariantKind::Claim,
                expr: if_(
                    settled_layout(),
                    le(var("pad_top"), var("pad")),
                    eq(var("pad_top"), int(0)),
                ),
            },
            Invariant {
                name: "GridOriginTracksTopAndHead",
                kind: InvariantKind::Claim,
                expr: if_(
                    settled_layout(),
                    eq(var("grid_top"), add(var("head"), var("pad_top"))),
                    eq(var("grid_top"), int(0)),
                ),
            },
            Invariant {
                name: "LayoutChangeForcesFullRepaint",
                kind: InvariantKind::Claim,
                expr: if_(
                    and_(
                        eq(var("phase"), int(5)),
                        neq(var("cached_grid_top"), var("grid_top")),
                    ),
                    and_(
                        eq(var("cache_hit"), int(0)),
                        eq(var("full_repaint"), int(1)),
                    ),
                    le(var("cache_hit"), int(1)),
                ),
            },
            Invariant {
                name: "IdenticalLayoutMayReuseCache",
                kind: InvariantKind::Claim,
                expr: if_(
                    and_(eq(var("phase"), int(5)), layouts_match()),
                    and_(
                        eq(var("cache_hit"), int(1)),
                        eq(var("full_repaint"), int(0)),
                    ),
                    le(var("full_repaint"), int(1)),
                ),
            },
        ],
    }
}

/// GUI-visible crop from the renderer's conserved raw padding allocation.
///
/// The raw renderer frame is `grid + head + 2*pad`, with a tightened top and an expanded raw
/// bottom (the allocation [`asymmetric_pad_layout_model`]'s `ExactVerticalPadCover` proves;
/// `Crop` takes it as input). The GUI removes exactly `pad - pad_top` source rows, so its
/// exposed frame is `grid + head + pad_top + pad`: the clamped top is preserved and the visible
/// bottom is always the base pad. `Buggy=1` exposes the uncropped raw frame/bottom and breaks
/// the clamp seam `clamp_visible_pad_top` (`VisibleTopMatchesRendererTop`). Dropping the clamp
/// at one GUI call site alone is masked (`Backend::set_pad_top` clamps before
/// `GpuBackend::set_visible_pad_top` clamps again), so the reachable slip is the shared helper
/// returning the request: the visible authority keeps it while the renderer's own
/// `set_pad_top` (`.min(self.pad)`) still clamps the drawn top, and the two edges disagree
/// whenever the request exceeds the pad. `pad_top` is the renderer-drawn top, `visible_pad_top`
/// the reported one. The drawn top's bound is not this machine's law: `Crop` writes it clamped
/// at every `Buggy`, and in the code it is the renderer's own clamp,
/// [`asymmetric_pad_layout_model`]'s `TopPadIsBounded`.
///
/// Tier-1 observes the two edges separately — `pad_top` from a real `aterm_render::Renderer`,
/// `visible_pad_top` from the seam — and its negative control replays the seam's unclamped
/// answer through the real renderer clamp, GPU crop offset and frame crop: the projection is
/// exactly the `Buggy=1` `Crop` successor.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn visible_pad_crop_model() -> Model {
    let resolved_top = || if_(le(var("request"), var("pad")), var("request"), var("pad"));
    let raw_height = || add(add(var("grid"), var("head")), add(var("pad"), var("pad")));
    let visible_height = || {
        add(
            add(var("grid"), var("head")),
            add(resolved_top(), var("pad")),
        )
    };
    let settled = || eq(var("phase"), int(2));
    Model {
        name: "VisiblePadCrop",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "pad",
                init: 0,
            },
            StateVar {
                name: "request",
                init: 0,
            },
            StateVar {
                name: "grid",
                init: 1,
            },
            StateVar {
                name: "head",
                init: 0,
            },
            StateVar {
                name: "pad_top",
                init: 0,
            },
            StateVar {
                name: "raw_pad_bottom",
                init: 0,
            },
            StateVar {
                name: "raw_height",
                init: 0,
            },
            StateVar {
                name: "visible_pad_top",
                init: 0,
            },
            StateVar {
                name: "visible_pad_bottom",
                init: 0,
            },
            StateVar {
                name: "visible_height",
                init: 0,
            },
            StateVar {
                name: "crop_total",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "ChooseGeometry",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "pad",
                        expr: in_range(int(0), int(3)),
                    },
                    Update {
                        var: "request",
                        expr: in_range(int(0), int(4)),
                    },
                    Update {
                        var: "grid",
                        expr: in_range(int(1), int(4)),
                    },
                    Update {
                        var: "head",
                        expr: in_range(int(0), int(2)),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                name: "Crop",
                guard: Some(eq(var("phase"), int(1))),
                updates: vec![
                    Update {
                        var: "pad_top",
                        expr: resolved_top(),
                    },
                    Update {
                        var: "raw_pad_bottom",
                        expr: sub(add(var("pad"), var("pad")), resolved_top()),
                    },
                    Update {
                        var: "raw_height",
                        expr: raw_height(),
                    },
                    Update {
                        var: "visible_pad_top",
                        expr: if_(eq(cst("Buggy"), int(1)), var("request"), resolved_top()),
                    },
                    Update {
                        var: "visible_pad_bottom",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            sub(add(var("pad"), var("pad")), resolved_top()),
                            var("pad"),
                        ),
                    },
                    Update {
                        var: "visible_height",
                        expr: if_(eq(cst("Buggy"), int(1)), raw_height(), visible_height()),
                    },
                    Update {
                        var: "crop_total",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            int(0),
                            sub(var("pad"), resolved_top()),
                        ),
                    },
                    Update {
                        var: "phase",
                        expr: int(2),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                // The GUI reports the top the renderer draws at.
                name: "VisibleTopMatchesRendererTop",
                kind: InvariantKind::Claim,
                expr: if_(
                    settled(),
                    eq(var("visible_pad_top"), var("pad_top")),
                    eq(var("pad_top"), int(0)),
                ),
            },
            Invariant {
                name: "VisibleBottomIsBasePad",
                kind: InvariantKind::Claim,
                expr: if_(
                    settled(),
                    eq(var("visible_pad_bottom"), var("pad")),
                    eq(var("visible_pad_bottom"), int(0)),
                ),
            },
            Invariant {
                name: "VisibleHeightUsesIndependentEdges",
                kind: InvariantKind::Claim,
                expr: if_(
                    settled(),
                    eq(var("visible_height"), visible_height()),
                    eq(var("visible_height"), int(0)),
                ),
            },
            Invariant {
                name: "CropDeletesOnlyRemovedTop",
                kind: InvariantKind::Claim,
                expr: if_(
                    settled(),
                    and_(
                        eq(var("crop_total"), sub(var("pad"), var("pad_top"))),
                        eq(
                            var("raw_height"),
                            add(var("visible_height"), var("crop_total")),
                        ),
                    ),
                    eq(var("crop_total"), int(0)),
                ),
            },
        ],
    }
}

/// CT fractional bearing: CoreText sub-pixel placement splits the padded ink-box origin `b`
/// into an integer bearing `floor(b)` plus a retained in-bitmap phase `b - floor(b) ∈ [0, 1)`;
/// pinning the ink box to an integer origin and reporting `round(b)` puts every glyph up to
/// 0.5 px off its designed position. The real policy is `aterm_render::ct_pen_and_bearing`
/// (seam `CtFont::rasterize`); Tier-1 is `aterm-render/tests/ct_fractional_bearing.rs`, which
/// sweeps the pure fn over a dense dyadic lattice and drives this model's interpreter against
/// it point for point, plus the FFI binding test in aterm-render's lib tests.
///
/// With no `*`/`/` in ty's Expr, the model computes the floor operationally in eighth-px fixed
/// point: after `PickB` (`b ∈ [-24, 24]` eighths, ±3 px) and `Latch` (`rem = b`), `StepDown`/
/// `StepUp` move whole pixels between `rem` and `bearing` until `rem ∈ [0, 8)`, and `Report`
/// publishes. `Decompose`: `bearing + rem == b` at every step; `PhaseInUnit`: the reported
/// phase is in `[0, 1)`. `Buggy = 1` reports the rounded bearing with phase 0, so any `b` off
/// the whole-px grid breaks `Decompose`.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn ct_frac_bearing_model() -> Model {
    // Buggy's report: round-to-nearest px (+8 iff rem >= 4), phase discarded; the committed
    // report keeps both.
    let rounded = || {
        if_(
            gt(var("rem"), int(3)),
            add(var("bearing"), int(8)),
            var("bearing"),
        )
    };
    let report_bearing = if_(eq(cst("Buggy"), int(1)), rounded(), var("bearing"));
    let report_rem = if_(eq(cst("Buggy"), int(1)), int(0), var("rem"));
    // rem in [0, 8): the whole-px moves are exhausted, phase is sub-px.
    let rem_in_unit = || and_(le(int(0), var("rem")), le(var("rem"), int(7)));
    Model {
        name: "CtFracBearing",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar { name: "b", init: 0 },
            StateVar {
                name: "rem",
                init: 0,
            },
            StateVar {
                name: "bearing",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // Every designed bearing on the lattice, negatives included
                // (a left-side bearing can be negative).
                name: "PickB",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "b",
                        expr: in_range(int(-24), int(24)),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                // Start the decomposition: the whole origin is still phase.
                name: "Latch",
                guard: Some(eq(var("phase"), int(1))),
                updates: vec![
                    Update {
                        var: "rem",
                        expr: var("b"),
                    },
                    Update {
                        var: "phase",
                        expr: int(2),
                    },
                ],
            },
            Action {
                // Move one whole px of positive origin into the bearing.
                name: "StepDown",
                guard: Some(and_(eq(var("phase"), int(2)), gt(var("rem"), int(7)))),
                updates: vec![
                    Update {
                        var: "rem",
                        expr: sub(var("rem"), int(8)),
                    },
                    Update {
                        var: "bearing",
                        expr: add(var("bearing"), int(8)),
                    },
                ],
            },
            Action {
                // Move one whole px of negative origin into the bearing
                // (floor, not truncation: -0.5px becomes bearing -1 + phase 0.5).
                name: "StepUp",
                guard: Some(and_(eq(var("phase"), int(2)), gt(int(0), var("rem")))),
                updates: vec![
                    Update {
                        var: "rem",
                        expr: add(var("rem"), int(8)),
                    },
                    Update {
                        var: "bearing",
                        expr: sub(var("bearing"), int(8)),
                    },
                ],
            },
            Action {
                // Publish the placement the raster/blit will use.
                // Buggy = 2 reports before the whole-px moves are exhausted.
                name: "Report",
                guard: Some(and_(
                    eq(var("phase"), int(2)),
                    or_(rem_in_unit(), eq(cst("Buggy"), int(2))),
                )),
                updates: vec![
                    Update {
                        var: "bearing",
                        expr: report_bearing,
                    },
                    Update {
                        var: "rem",
                        expr: report_rem,
                    },
                    Update {
                        var: "phase",
                        expr: int(3),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                // Exact reconstruction: integer bearing + retained phase is the designed
                // position at every step after Latch.
                name: "Decompose",
                kind: InvariantKind::Claim,
                expr: or_(
                    le(var("phase"), int(1)),
                    eq(add(var("bearing"), var("rem")), var("b")),
                ),
            },
            Invariant {
                // The reported sub-px phase is in [0, 1) — scaled: rem in [0, 8).
                name: "PhaseInUnit",
                kind: InvariantKind::Claim,
                expr: or_(neq(var("phase"), int(3)), rem_in_unit()),
            },
        ],
    }
}
