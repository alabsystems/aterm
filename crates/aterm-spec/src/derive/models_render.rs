// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rendering, text and contrast gate models, re-exported from `crate::derive`. Each
//! constructor is `trust::skip`: it is model data, and the model it returns is what `ty`
//! checks.

use super::*;

/// Two native leaves' retained rasters across full-native, split and terminal routes. Binding
/// a new visible plan synchronously drops hidden allocations; painting materializes only
/// visible leaves. `Buggy` prunes only in mixed compose, leaving hidden full-native rasters
/// alive. Tier-1 drives real App plans, native paints and terminal captures.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_leaf_cache_lifetime_model() -> Model {
    crate::ty_model! {
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
    crate::ty_model! {
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
    crate::ty_model! {
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
                completed = completed + 1;
                ready = if closed == 0 { ready + 1 } else { ready };
                pending = if closed == 0 { pending } else { pending - 1 };
                published = if closed == 0 { published + 1 } else { published };
                wakes = if closed == 0 { wakes + 1 } else { wakes };
            }
            action Take when (closed == 0 && ready > 0) {
                ready = ready - 1;
                pending = pending - 1;
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
                pending = active;
                ready = 0;
            }
            action Retire when (
                retiring == 1 && (active == 0 || Buggy == 1)
            ) {
                retiring = 0;
                retired = 1;
            }
            invariant OneOutstanding: pending <= 1;
            uncaught OutstandingAccounted: pending == active + ready;
            uncaught CallsBounded: requests <= MaxRequests && completed <= requests;
            uncaught PublicationsBounded: published <= completed;
            uncaught OnlyPublicationWakes: wakes == published;
            uncaught ClosedHasNoReadyResult: closed == 0 || ready == 0;
            invariant RetirementFollowsAcquisition: retired == 0 || active == 0;
            invariant NoInvalidAcceptance: invalid_accept == 0;
            invariant NoCallerWait: main_wait == 0;
        }
    }
}

/// Colour-presentation gate: a code point that defaults to text presentation is never resolved
/// to the colour-emoji face. The colour face is gated on the Unicode `Emoji_Presentation`
/// property (or an explicit VS16), never on raw font coverage: U+23FA (⏺) is `Emoji=Yes` but
/// `Emoji_Presentation=No`, so it defaults to text, as in iTerm2 and Ghostty. The real
/// decision is aterm-render's `select_face`; Tier-1 is its exhaustive
/// `select_face_never_colors_text_presentation` test.
///
/// `wants_emoji` = 1 iff the code point has default emoji presentation (or VS16); `color` is
/// the gate's output, recomputed from `wants_emoji` in the same step (face selection is
/// stateless). The two `Want*` actions spread the input. `Buggy = 1` sets `color` regardless (a
/// coverage-only gate), so a default-text code point gets colour and `NoColorForText` breaks.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn presentation_gate_model() -> Model {
    // color' = if (wants_emoji' OR Buggy) then 1 else 0. TLA primed semantics use
    // unprimed vars on the RHS, so substitute the literal value each action is
    // about to assign to `wants_emoji` (1 for WantEmoji, 0 for WantText).
    let color_for = |wants_emoji_next: i64| {
        if_(
            gt(add(int(wants_emoji_next), cst("Buggy")), int(0)),
            int(1),
            int(0),
        )
    };
    Model {
        name: "PresentationGate",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "wants_emoji",
                init: 0,
            },
            StateVar {
                name: "color",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // The code point has default-emoji presentation (or VS16): colour allowed.
                name: "WantEmoji",
                guard: Some(le(var("wants_emoji"), int(0))),
                updates: vec![
                    Update {
                        var: "wants_emoji",
                        expr: int(1),
                    },
                    Update {
                        var: "color",
                        expr: color_for(1),
                    },
                ],
            },
            Action {
                // A default-text code point: colour must be withheld.
                name: "WantText",
                guard: Some(gt(var("wants_emoji"), int(0))),
                updates: vec![
                    Update {
                        var: "wants_emoji",
                        expr: int(0),
                    },
                    Update {
                        var: "color",
                        expr: color_for(0),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            // A colour resolution implies the code point wanted emoji presentation.
            name: "NoColorForText",
            kind: InvariantKind::Claim,
            expr: le(var("color"), var("wants_emoji")),
        }],
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
#[crate::model]
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

/// HDR present gate: when the swapchain is `Rgba16Float`, when the blit linear-decodes, when the
/// >1.0 aurora pass runs. The real decisions are aterm-gpu's `format_plan::hdr_swapchain_wants_f16`
/// (the Attach seam, `GpuRenderer::create_window_surface`; the macOS Metal arm goes through
/// `hdr_swapchain_wants_f16_on_screen`, which only narrows it by the screen's EDR potential, and
/// re-picks an 8-bit window live through the narrower `hdr_screen_upgrade_wants_f16`) and
/// `format_plan::hdr_present_plan` (the Present seam, `present_input`). Tier-1 is aterm-gpu's
/// `tests/hdr_gate.rs`, an exhaustive 2^3 enumeration of both functions chained
/// Attach→Present — a complete proof over finite booleans. The float clamp laws the gate feeds
/// (grid ≤ 1.0, additive ≤ EDR headroom) are checked in `aterm_render::hdr`.
///
/// `Observe` spreads the three inputs (config opt-in, surface capability, aurora presence) and
/// resets the lifecycle; `Attach` picks the format once (`is_f16' = hdr_glow ∧ supports_f16`);
/// `Present` (repeatable) derives the blit decode from the actual format (`blit_hdr' = is_f16`,
/// so live config flips degrade safely) and the aurora pass from all three (`boost' = hdr_glow ∧
/// is_f16 ∧ glow`). `Buggy = 1` picks f16 whenever the surface supports it, ignoring the config,
/// so a default install lands on an EDR swapchain and `SdrInvariance` breaks.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn hdr_present_gate_model() -> Model {
    // is_f16' at Attach: the shipping policy (hdr_glow AND supports_f16), or the
    // Buggy=1 defect (supports_f16 alone — config ignored).
    let attach_pick = if_(
        gt(cst("Buggy"), int(0)),
        var("supports_f16"),
        if_(
            and_(gt(var("hdr_glow"), int(0)), gt(var("supports_f16"), int(0))),
            int(1),
            int(0),
        ),
    );
    Model {
        name: "HdrPresentGate",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "hdr_glow",
                init: 0,
            },
            StateVar {
                name: "supports_f16",
                init: 0,
            },
            StateVar {
                name: "glow",
                init: 0,
            },
            StateVar {
                name: "attached",
                init: 0,
            },
            StateVar {
                name: "is_f16",
                init: 0,
            },
            StateVar {
                name: "blit_hdr",
                init: 0,
            },
            StateVar {
                name: "boost",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // A window lifecycle begins: observe the config opt-in, the
                // surface's f16 capability, and whether aurora quads exist this
                // session (all 2^3 combinations); reset the derived state.
                name: "Observe",
                guard: None,
                updates: vec![
                    Update {
                        var: "hdr_glow",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "supports_f16",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "glow",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "attached",
                        expr: int(0),
                    },
                    Update {
                        var: "is_f16",
                        expr: int(0),
                    },
                    Update {
                        var: "blit_hdr",
                        expr: int(0),
                    },
                    Update {
                        var: "boost",
                        expr: int(0),
                    },
                ],
            },
            Action {
                // Surface attach: pick the swapchain format once
                // (format_plan::hdr_swapchain_wants_f16).
                name: "Attach",
                guard: Some(le(var("attached"), int(0))),
                updates: vec![
                    Update {
                        var: "attached",
                        expr: int(1),
                    },
                    Update {
                        var: "is_f16",
                        expr: attach_pick,
                    },
                ],
            },
            Action {
                // A present (format_plan::hdr_present_plan): the blit decode follows the
                // actual format; the aurora pass needs config, format and glow.
                name: "Present",
                guard: Some(gt(var("attached"), int(0))),
                updates: vec![
                    Update {
                        var: "blit_hdr",
                        expr: var("is_f16"),
                    },
                    Update {
                        var: "boost",
                        expr: if_(
                            and_(
                                gt(var("hdr_glow"), int(0)),
                                and_(gt(var("is_f16"), int(0)), gt(var("glow"), int(0))),
                            ),
                            int(1),
                            int(0),
                        ),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                // hdr_glow off ⇒ no f16 swapchain, no linear decode, no >1.0 aurora pass:
                // hdr_glow = 0 => is_f16 = 0 /\ blit_hdr = 0 /\ boost = 0.
                name: "SdrInvariance",
                kind: InvariantKind::Claim,
                expr: or_(
                    gt(var("hdr_glow"), int(0)),
                    and_(
                        eq(var("is_f16"), int(0)),
                        and_(eq(var("blit_hdr"), int(0)), eq(var("boost"), int(0))),
                    ),
                ),
            },
            Invariant {
                // A >1.0 emission can only land on a float swapchain whose blit
                // decoded to linear: boost <= blit_hdr (and blit_hdr <= is_f16).
                name: "BoostNeedsLinearF16",
                kind: InvariantKind::Uncaught,
                expr: and_(
                    le(var("boost"), var("blit_hdr")),
                    le(var("blit_hdr"), var("is_f16")),
                ),
            },
            Invariant {
                // The EDR format is never picked without surface support —
                // holds even under Buggy=1 (whose violation is config, not caps).
                name: "F16NeedsSupport",
                kind: InvariantKind::Uncaught,
                expr: le(var("is_f16"), var("supports_f16")),
            },
        ],
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
/// Tier-1 is in aterm-gpu, and every action is `#[refines]`-bound to both halves:
/// `tests/hdr_gate.rs` enumerates the planner `hdr_reconfigure_plan` over its whole domain, and
/// `src/renderer.rs`'s `hdr_reconfigure_apply_conforms_to_retag_model` drives the private
/// `WindowGpu::apply_hdr_reconfigure_plan` / `apply_hdr_surface_upgrade` on a real window and
/// projects with `project_hdr_reconfigure_state`. Of the projection, only `capture_linear` is
/// read back after the shipping apply (so an apply that skips its metadata half lands on no
/// successor); `is_f16` is the planner's format, the real format half living in
/// `GpuRenderer::finish_surface_color_space_recovery` (it needs a real DX12 swapchain) and held
/// by `hdr_gate.rs`'s source-region gate
/// `every_live_surface_reconfigure_routes_through_hdr_recovery`; `stage` and `retagged` are
/// drive coordinates (`RetagFails` and `EnterSdrFallback` are one apply read at two phases, and
/// the phase and the `if surf.is_hdr()` guard live on the renderer, held by the same gate). Both
/// halves replay the ignore-failure policy as a negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn hdr_reconfigure_retag_model() -> Model {
    crate::ty_model! {
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

/// Chrome face gate: which face draws a chrome (tray/overlay) glyph. A bold run whose real bold
/// face covers the char takes the bold face; else a char the user's primary covers takes the
/// primary; the embedded DejaVu is strictly a coverage fallback (symbols like ⌘⇧⌃✓ the terminal
/// face lacks). The real decision is aterm-gui's `tray_raster::select_chrome_face`; Tier-1 is
/// that module's exhaustive 2^3 test (`chrome_face_gate_exhaustive`).
///
/// `Observe` spreads all 8 input combinations of `<<bold_run, bold_has, primary_has>>`, then
/// `Resolve` runs the gate once; `pick` is its output (0 = embedded fallback, 1 = primary, 2 =
/// bold), meaningful while `resolved = 1`. `Buggy = 1` pins `pick = 0` (everything DejaVu),
/// violating `EmbeddedOnlyAsCoverageFallback` and `BoldHonoredWhenCovered`.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn chrome_face_gate_model() -> Model {
    // pick' = IF Buggy THEN 0 (always the embedded face) ELSE the policy:
    // bold_run /\ bold_has -> 2; primary_has -> 1; else 0.
    let policy = if_(
        and_(gt(var("bold_run"), int(0)), gt(var("bold_has"), int(0))),
        int(2),
        if_(gt(var("primary_has"), int(0)), int(1), int(0)),
    );
    Model {
        name: "ChromeFaceGate",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "bold_run",
                init: 0,
            },
            StateVar {
                name: "bold_has",
                init: 0,
            },
            StateVar {
                name: "primary_has",
                init: 0,
            },
            StateVar {
                name: "resolved",
                init: 0,
            },
            StateVar {
                name: "pick",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // A fresh glyph: observe its coverage facts (all 2^3) and mark the pick stale.
                name: "Observe",
                guard: None,
                updates: vec![
                    Update {
                        var: "bold_run",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "bold_has",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "primary_has",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "resolved",
                        expr: int(0),
                    },
                    Update {
                        var: "pick",
                        expr: int(0),
                    },
                ],
            },
            Action {
                // Run the gate once over the observed facts.
                name: "Resolve",
                guard: Some(le(var("resolved"), int(0))),
                updates: vec![
                    Update {
                        var: "resolved",
                        expr: int(1),
                    },
                    Update {
                        var: "pick",
                        expr: if_(gt(cst("Buggy"), int(0)), int(0), policy),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                // DejaVu only when neither user face covers the glyph: resolved /\ pick = 0
                // => ~primary_has /\ ~(bold_run /\ bold_has).
                name: "EmbeddedOnlyAsCoverageFallback",
                kind: InvariantKind::Claim,
                expr: or_(
                    le(var("resolved"), int(0)),
                    or_(
                        neq(var("pick"), int(0)),
                        and_(
                            eq(var("primary_has"), int(0)),
                            or_(le(var("bold_run"), int(0)), le(var("bold_has"), int(0))),
                        ),
                    ),
                ),
            },
            Invariant {
                // A covered bold run never downgrades: resolved /\ bold_run /\ bold_has =>
                // pick = 2.
                name: "BoldHonoredWhenCovered",
                kind: InvariantKind::Claim,
                expr: or_(
                    le(var("resolved"), int(0)),
                    or_(
                        le(var("bold_run"), int(0)),
                        or_(le(var("bold_has"), int(0)), eq(var("pick"), int(2))),
                    ),
                ),
            },
        ],
    }
}

/// Motion policy, reduced-motion totality: the twin of
/// `aterm_effects::motion::MotionPolicy::resolve` + `amplitude`. Tier-1 is that module's
/// `reduced_motion_totality`, which enumerates the same 3×2×2 input domain × the governed-effect
/// set over the shipping resolver (a complete proof). The amplitude is one scalar over the whole
/// governed set, so an effect joins or leaves the proof through `MotionEffect::ALL` and the
/// exhaustive `amplitude` match, not an edit here.
///
/// `Observe` picks the three motion facts (`mode ∈ 0..2` = auto|full|reduced, `sys`/`focused`)
/// and marks the decision stale; `Resolve` computes the policy (Full ⟺ focused ∧ (full ∨ (auto
/// ∧ ¬sys))) and the amplitude (`amp ∈ {0,1}`, the integer twin of the f32 every effect reads).
/// `Buggy = 1` ignores the OS reduce-motion flag under auto, so a focused auto window keeps
/// animating under a Reduced policy and `ReducedImpliesZeroAmplitude` breaks.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn motion_policy_model() -> Model {
    // Full ⟺ focused ∧ (mode = full ∨ (mode = auto ∧ ¬sys)).
    let full = and_(
        gt(var("focused"), int(0)),
        or_(
            eq(var("mode"), int(1)),
            and_(eq(var("mode"), int(0)), eq(var("sys"), int(0))),
        ),
    );
    // The committed amplitude equals the policy; Buggy's ignores `sys` under auto
    // (animate ⟺ focused ∧ mode ≠ reduced).
    let buggy_full = and_(gt(var("focused"), int(0)), neq(var("mode"), int(2)));
    Model {
        name: "MotionPolicy",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "mode",
                init: 0,
            },
            StateVar {
                name: "sys",
                init: 0,
            },
            StateVar {
                name: "focused",
                init: 0,
            },
            StateVar {
                name: "resolved",
                init: 0,
            },
            StateVar {
                name: "policy",
                init: 0,
            },
            StateVar {
                name: "amp",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // A frame begins: observe the three motion facts (all 3×2×2) and mark the
                // decision stale until Resolve runs.
                name: "Observe",
                guard: None,
                updates: vec![
                    Update {
                        var: "mode",
                        expr: in_range(int(0), int(2)),
                    },
                    Update {
                        var: "sys",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "focused",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "resolved",
                        expr: int(0),
                    },
                    Update {
                        var: "policy",
                        expr: int(0),
                    },
                    Update {
                        var: "amp",
                        expr: int(0),
                    },
                ],
            },
            Action {
                // Resolve the policy + amplitude once over the observed facts.
                name: "Resolve",
                guard: Some(le(var("resolved"), int(0))),
                updates: vec![
                    Update {
                        var: "resolved",
                        expr: int(1),
                    },
                    Update {
                        var: "policy",
                        expr: if_(full.clone(), int(1), int(0)),
                    },
                    Update {
                        var: "amp",
                        expr: if_(
                            gt(cst("Buggy"), int(0)),
                            if_(buggy_full, int(1), int(0)),
                            if_(full, int(1), int(0)),
                        ),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                // A Reduced policy has exactly zero amplitude.
                name: "ReducedImpliesZeroAmplitude",
                kind: InvariantKind::Claim,
                expr: or_(
                    le(var("resolved"), int(0)),
                    or_(gt(var("policy"), int(0)), eq(var("amp"), int(0))),
                ),
            },
            Invariant {
                // Unfocused demotion (W11b): resolved /\ ~focused => policy = 0.
                name: "UnfocusedIsReduced",
                kind: InvariantKind::Uncaught,
                expr: or_(
                    le(var("resolved"), int(0)),
                    or_(gt(var("focused"), int(0)), eq(var("policy"), int(0))),
                ),
            },
            Invariant {
                // Non-vacuity: a Full policy animates at unit amplitude, so a constant-zero
                // amplitude cannot pass.
                name: "FullImpliesUnitAmplitude",
                kind: InvariantKind::Uncaught,
                expr: or_(
                    le(var("resolved"), int(0)),
                    or_(le(var("policy"), int(0)), eq(var("amp"), int(1))),
                ),
            },
        ],
    }
}

/// Process-global serious-mode master gate. Requested feature bits are kept separate from their
/// effective outputs, so enabling the master silences every decorative/audio lane without
/// destroying the configuration; requests may change while the gate is closed, and disabling
/// serious mode reveals the latest requested vector exactly.
///
/// The eight requested bits are the independently gated families: cursor trail/glow (with its
/// sound), BEL sound, sparkle/cat decorations, matrix rain, stream fade, transient celebrations,
/// native Settings previews, and GPU bloom/shimmer. The level-up notice is a second projection
/// of the celebration request, so already-shown celebration UI hides too. Static content, cursor
/// blink, selection and the visual bell are outside the model: serious mode keeps them.
/// `Buggy=1` forgets to mute the cursor trail/audio lane.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn serious_mode_model() -> Model {
    crate::ty_model! {
        SeriousMode {
            const Buggy = 0;
            var serious = 0;
            var want_trail = 1;
            var want_bell = 1;
            var want_sparkle = 1;
            var want_rain = 1;
            var want_fade = 1;
            var want_celebration = 1;
            var want_preview = 1;
            var want_gpu = 1;
            var trail = 1;
            var bell = 1;
            var sparkle = 1;
            var rain = 1;
            var fade = 1;
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
                fade = 0;
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
                fade = want_fade;
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
            action ChangeFade {
                want_fade = 1 - want_fade;
                fade = if serious == 1 { 0 } else { 1 - want_fade };
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
                    rain == 0 && fade == 0 && celebration == 0 &&
                    notice == 0 && preview == 0 && gpu == 0
                } else {
                    trail == want_trail && bell == want_bell &&
                    sparkle == want_sparkle && rain == want_rain &&
                    fade == want_fade && celebration == want_celebration &&
                    notice == want_celebration && preview == want_preview &&
                    gpu == want_gpu
                };
            bound SeriousBitBounded: serious <= 1;
            bound EffectiveBitsBounded:
                trail <= 1 && bell <= 1 && sparkle <= 1 &&
                rain <= 1 && fade <= 1 && celebration <= 1 &&
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
    crate::ty_model! {
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
#[crate::model]
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
                            gt(cst("Buggy"), int(0)),
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
                    Update {
                        var: "armed",
                        expr: if_(eq(step(), var("target")), int(0), int(1)),
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
                        expr: if_(gt(cst("Buggy"), int(0)), var("pos"), var("target")),
                    },
                    Update {
                        var: "armed",
                        expr: if_(gt(cst("Buggy"), int(0)), var("armed"), int(0)),
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
                kind: InvariantKind::Uncaught,
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

/// Sub-row scroll translate partition: a frame row is shifted by the fractional-pixel residual
/// iff it lies in the terminal-content grid band `[GridTop, GridBot)`; chrome rows (the tab strip
/// before `GridTop`, transient edge bars and split dividers from `GridBot` on) are pinned. The
/// real code is aterm-render's `scroll_translate::translate_grid_band_in_place`, which writes
/// only the band's pixels; Tier-1 is that module's exhaustive `chrome_pixels_are_invariant` plus
/// `tests/scroll_frac_translate.rs::chrome_pixels_are_invariant_under_any_frac`. `Classify`
/// spreads `row` across `0..GridBot+1`, straddling the seam. `Buggy = 1` extends the shift to
/// `GridBot` inclusive, leaking it onto the first bottom-chrome row, and breaks
/// `ShiftOnlyInBand`.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn grid_translate_model() -> Model {
    // Indicator (0/1) that `row` lies in `[GridTop, hi)`: `GridTop <= row < hi`.
    let indicator = |hi: Expr| {
        if_(
            and_(le(cst("GridTop"), var("row")), gt(hi, var("row"))),
            int(1),
            int(0),
        )
    };
    Model {
        name: "GridTranslate",
        // GridTop=1 (row 0 is the tab strip), GridBot=3 (rows 3.. are bottom chrome).
        consts: vec![("Buggy", 0), ("GridTop", 1), ("GridBot", 3)],
        vars: vec![StateVar {
            name: "row",
            init: 0,
        }],
        fn_vars: vec![],
        actions: vec![Action {
            // Enumerate every frame row across the chrome/grid seam (0..GridBot+1).
            name: "Classify",
            guard: None,
            updates: vec![Update {
                var: "row",
                expr: in_range(int(0), add(cst("GridBot"), int(1))),
            }],
        }],
        invariants: vec![Invariant {
            // A shifted row is always in-band; at Buggy=1 row == GridBot shifts but is chrome.
            name: "ShiftOnlyInBand",
            kind: InvariantKind::Claim,
            expr: le(
                indicator(add(cst("GridBot"), cst("Buggy"))),
                indicator(cst("GridBot")),
            ),
        }],
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
#[crate::model]
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

/// Stream-fade bypass gate: streamed output may fade in only when no bypass holds, and every
/// bypass is to instant (exact bytes). The real gate is aterm-gui's `stream_fade::fade_permitted`;
/// Tier-1 is that module's exhaustive 2^5 `fade_gate_exhaustive` (a complete proof) plus the
/// pipeline test `bypass_is_byte_identical`. `Observe` picks the five facts and marks the
/// decision stale; `Resolve` sets `fade` = 1 iff the config is on and no bypass (keystroke echo
/// in flight, alternate screen, scrolled-back viewport, reduced motion) holds. `Buggy = 1`
/// ignores `input_hot`, so a keystroke's echo fades in and reads as added latency, breaking
/// `InstantWhileTyping`.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn stream_fade_gate_model() -> Model {
    // fade' = enabled ∧ ¬input_hot ∧ ¬alt ∧ ¬scrolled ∧ ¬reduced; Buggy drops the input_hot
    // conjunct.
    let hot_clear = or_(le(var("input_hot"), int(0)), gt(cst("Buggy"), int(0)));
    let policy = if_(
        and_(
            gt(var("enabled"), int(0)),
            and_(
                hot_clear,
                and_(
                    le(var("alt_screen"), int(0)),
                    and_(le(var("scrolled_back"), int(0)), le(var("reduced"), int(0))),
                ),
            ),
        ),
        int(1),
        int(0),
    );
    // resolved ∧ <bypass> ⇒ fade = 0, in the or-form the checker consumes.
    let instant_when = |bypass: Expr| {
        or_(
            le(var("resolved"), int(0)),
            or_(bypass, eq(var("fade"), int(0))),
        )
    };
    Model {
        name: "StreamFadeGate",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "enabled",
                init: 0,
            },
            StateVar {
                name: "input_hot",
                init: 0,
            },
            StateVar {
                name: "alt_screen",
                init: 0,
            },
            StateVar {
                name: "scrolled_back",
                init: 0,
            },
            StateVar {
                name: "reduced",
                init: 0,
            },
            StateVar {
                name: "resolved",
                init: 0,
            },
            StateVar {
                name: "fade",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // A frame arrives: observe the five gate facts (all 2^5) and mark the
                // decision stale until Resolve runs.
                name: "Observe",
                guard: None,
                updates: vec![
                    Update {
                        var: "enabled",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "input_hot",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "alt_screen",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "scrolled_back",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "reduced",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "resolved",
                        expr: int(0),
                    },
                    Update {
                        var: "fade",
                        expr: int(0),
                    },
                ],
            },
            Action {
                // Run the gate once over the observed facts.
                name: "Resolve",
                guard: Some(le(var("resolved"), int(0))),
                updates: vec![
                    Update {
                        var: "resolved",
                        expr: int(1),
                    },
                    Update {
                        var: "fade",
                        expr: policy,
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                // A keystroke echo in flight renders instant (Buggy=1 violates this).
                name: "InstantWhileTyping",
                kind: InvariantKind::Claim,
                expr: instant_when(le(var("input_hot"), int(0))),
            },
            Invariant {
                // Full-screen programs never smear: resolved ∧ alt ⇒ fade = 0.
                name: "InstantOnAltScreen",
                kind: InvariantKind::Uncaught,
                expr: instant_when(le(var("alt_screen"), int(0))),
            },
            Invariant {
                // History is settled ink: resolved ∧ scrolled ⇒ fade = 0.
                name: "InstantScrolledBack",
                kind: InvariantKind::Uncaught,
                expr: instant_when(le(var("scrolled_back"), int(0))),
            },
            Invariant {
                // resolved ∧ reduced-motion ⇒ fade = 0.
                name: "InstantUnderReducedMotion",
                kind: InvariantKind::Uncaught,
                expr: instant_when(le(var("reduced"), int(0))),
            },
            Invariant {
                // The master switch: resolved ∧ ¬enabled ⇒ fade = 0.
                name: "InstantWhenDisabled",
                kind: InvariantKind::Uncaught,
                expr: instant_when(gt(var("enabled"), int(0))),
            },
            Invariant {
                // Non-vacuity: with the config on and every bypass clear the fade runs, so
                // a constant-zero gate cannot pass.
                name: "FadesWhenPermitted",
                kind: InvariantKind::Uncaught,
                expr: or_(
                    le(var("resolved"), int(0)),
                    or_(
                        le(var("enabled"), int(0)),
                        or_(
                            gt(var("input_hot"), int(0)),
                            or_(
                                gt(var("alt_screen"), int(0)),
                                or_(
                                    gt(var("scrolled_back"), int(0)),
                                    or_(gt(var("reduced"), int(0)), eq(var("fade"), int(1))),
                                ),
                            ),
                        ),
                    ),
                ),
            },
        ],
    }
}

/// AA edge hardening: an anti-aliased procedural glyph's cell-edge texel is always hard 0/MAX.
/// Diagonals, arcs, Powerline and wedges rasterize at 4×4 subsamples per pixel (`raw ∈ 0..=16`
/// here, standing for the byte), but a fractional texel on the cell boundary composes a visible
/// half-covered seam between adjacent cells (and blends differently on CPU and GPU), so border
/// texels harden: majority coverage (`raw >= 8`) becomes full, the rest empty; interior texels
/// keep their AA value. The real pass is aterm-render's `procedural::Canvas::harden_edges`;
/// Tier-1 is the exhaustive size-lattice test `procedural_aa_edges.rs`. `Observe` picks a texel
/// position (`edge`) and raw coverage, `Filter` emits the byte. `Buggy = 1` skips the pass, so
/// an edge texel keeps fractional coverage and `EdgeTexelsHard` breaks.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn aa_edge_hardening_model() -> Model {
    Model {
        name: "AaEdgeHarden",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "edge",
                init: 0,
            },
            StateVar {
                name: "raw",
                init: 0,
            },
            StateVar {
                name: "out",
                init: 0,
            },
            StateVar {
                name: "pc",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // A fresh texel: position (interior/edge) + raw AA coverage; `out` resets
                // so the invariant holds mid-pipeline.
                name: "Observe",
                guard: Some(eq(var("pc"), int(0))),
                updates: vec![
                    Update {
                        var: "edge",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "raw",
                        expr: in_range(int(0), int(16)),
                    },
                    Update {
                        var: "out",
                        expr: int(0),
                    },
                    Update {
                        var: "pc",
                        expr: int(1),
                    },
                ],
            },
            Action {
                // The hardening pass: edge texels quantize (majority wins);
                // interior texels pass through. Buggy=1 skips the pass.
                name: "Filter",
                guard: Some(eq(var("pc"), int(1))),
                updates: vec![
                    Update {
                        var: "out",
                        expr: if_(
                            and_(eq(cst("Buggy"), int(0)), eq(var("edge"), int(1))),
                            if_(gt(var("raw"), int(7)), int(16), int(0)),
                            var("raw"),
                        ),
                    },
                    Update {
                        var: "pc",
                        expr: int(0),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            // An edge texel is never fractional: out ∈ {0, 16} whenever edge=1.
            name: "EdgeTexelsHard",
            kind: InvariantKind::Claim,
            expr: or_(
                eq(var("edge"), int(0)),
                or_(eq(var("out"), int(0)), eq(var("out"), int(16))),
            ),
        }],
    }
}

/// Shade dither phase: the ░▒▓ dither pattern is a function of absolute framebuffer column
/// parity, so a run of shade cells tiles with a uniform 2-pixel period across every seam.
/// Cell-local parity at an odd cell width (`W = 9`) restarts every cell's dither at local x = 0
/// and doubles the dither line at each seam; each cell's pattern is therefore keyed by the
/// parity of its absolute pixel origin (the phase), carried into the glyph key. The real code
/// is aterm-render's `procedural::shade` + `shade_phase_key`; Tier-1 is `shade_phase.rs`, the
/// same invariant over composed adjacent cells at `cell_w = 9` plus a rendered frame.
///
/// The scanner walks absolute columns across consecutive `W`-wide cells: `parity` is the
/// absolute column parity, `local` the cell-local column, `localpar` its parity, `cellpar` the
/// cell origin's parity (the phase fed to the rasterizer), `lit` the ░-row pattern at this
/// column. `UniformPeriod`: `lit + parity = 1`, so two consecutive columns never repeat.
/// `Buggy = 1` uses phase 0 (cell-local dithering) and breaks it at absolute column 9.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn shade_phase_model() -> Model {
    // Post-step values, all over unprimed state: the step advances to absolute column x+1.
    let boundary = || eq(var("local"), sub(cst("W"), int(1)));
    let next_localpar = || if_(boundary(), int(0), sub(int(1), var("localpar")));
    let next_cellpar = || if_(boundary(), sub(int(1), var("parity")), var("cellpar"));
    // The phase the rasterizer is handed for the (possibly new) cell.
    let phase = || if_(gt(cst("Buggy"), int(0)), int(0), next_cellpar());
    Model {
        name: "ShadePhase",
        consts: vec![("Buggy", 0), ("W", 9)],
        vars: vec![
            StateVar {
                name: "parity",
                init: 0,
            },
            StateVar {
                name: "local",
                init: 0,
            },
            StateVar {
                name: "localpar",
                init: 0,
            },
            StateVar {
                name: "cellpar",
                init: 0,
            },
            // Column 0: local 0 + phase 0 → the ░ row is lit there.
            StateVar {
                name: "lit",
                init: 1,
            },
        ],
        fn_vars: vec![],
        actions: vec![Action {
            // Advance one absolute column, wrapping into the next cell at the W boundary.
            name: "Advance",
            guard: None,
            updates: vec![
                Update {
                    var: "parity",
                    expr: sub(int(1), var("parity")),
                },
                Update {
                    var: "local",
                    expr: if_(boundary(), int(0), add(var("local"), int(1))),
                },
                Update {
                    var: "localpar",
                    expr: next_localpar(),
                },
                Update {
                    var: "cellpar",
                    expr: next_cellpar(),
                },
                Update {
                    // lit' = (local' + phase') is even ⟺ localpar' == phase'.
                    var: "lit",
                    expr: if_(eq(next_localpar(), phase()), int(1), int(0)),
                },
            ],
        }],
        invariants: vec![Invariant {
            // The pattern is the absolute-parity function: lit ⟺ even column.
            name: "UniformPeriod",
            kind: InvariantKind::Claim,
            expr: eq(add(var("lit"), var("parity")), int(1)),
        }],
    }
}

/// Styled-run face: a bold ligature run uses a real bold face (injected `set_bold_font` or a
/// discovered `-Bold` sibling) when one exists, and synthetic dilation of the primary only
/// otherwise. The real policy is `aterm_render::resolve_styled_face` on the ligature-run seam
/// (`Renderer::run_face_pick`, `row_glyph_plan`'s shaping closure, `rasterize`'s MonoGid arm);
/// Tier-1 is aterm-render's `tests/styled_faces.rs`, which enumerates the policy's whole 2^6
/// input space and binds the run routing to real rendered ink.
///
/// `bold`: the run's SGR requests bold; `have_real`: a real bold face is available;
/// `synth_bold`: the run rasterizes as primary + synthetic dilation, recomputed in the same
/// step (face resolution is stateless). Four `Case*` actions cover the input square.
/// `Buggy = 1` ignores availability (`synth_bold = bold`), so `RealBoldNeverDilated` breaks.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn styled_run_face_model() -> Model {
    // synth_bold' for the literal inputs (b, h) each Case action installs:
    // committed policy = b AND NOT h; Buggy = b (availability ignored).
    let synth_for = |b: i64, h: i64| {
        if_(
            gt(cst("Buggy"), int(0)),
            int(b),
            int(if b == 1 && h == 0 { 1 } else { 0 }),
        )
    };
    let case = |name: &'static str, b: i64, h: i64| Action {
        name,
        guard: None,
        updates: vec![
            Update {
                var: "bold",
                expr: int(b),
            },
            Update {
                var: "have_real",
                expr: int(h),
            },
            Update {
                var: "synth_bold",
                expr: synth_for(b, h),
            },
        ],
    };
    Model {
        name: "StyledRunFace",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "bold",
                init: 0,
            },
            StateVar {
                name: "have_real",
                init: 0,
            },
            StateVar {
                name: "synth_bold",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            case("CaseRegular", 0, 0),
            case("CaseRegularHave", 0, 1),
            case("CaseBoldNoFace", 1, 0),
            case("CaseBoldHave", 1, 1),
        ],
        invariants: vec![Invariant {
            // A real bold face present ⇒ the run is never drawn as synthetic dilation.
            name: "RealBoldNeverDilated",
            kind: InvariantKind::Claim,
            expr: le(add(var("synth_bold"), var("have_real")), int(1)),
        }],
    }
}

/// Fallback-chain precedence: the config keys (`fallback_fonts` / `symbol_font` / `emoji_font`)
/// strictly outrank built-in discovery, which always exists (the built-in list is non-empty).
/// The real law is `aterm_render::fallback_chain_order`, used by the `set_config_fallback_fonts`
/// / `set_config_symbol_font` / `set_config_emoji_font` builders, whose loaders take the first
/// existing path; Tier-1 is aterm-render's `tests/styled_faces.rs` presence-lattice test, which
/// binds the real function's first-element class to `winner` for every presence combination.
///
/// `winner` is the class of the chain's first candidate (1 = config, 2 = discovery),
/// recomputed from `cfg_present` in the same step. `Buggy = 1` consults discovery first, so a
/// built-in face shadows the user's choice and `ConfigOutranksDiscovery` breaks.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn fallback_precedence_model() -> Model {
    // winner' for the literal input c: committed = config > discovery;
    // Buggy = discovery first (always present, so it always heads the chain).
    let winner_for = |c: i64| {
        let committed = if c == 1 { 1 } else { 2 };
        if_(gt(cst("Buggy"), int(0)), int(2), int(committed))
    };
    let case = |name: &'static str, c: i64| Action {
        name,
        guard: None,
        updates: vec![
            Update {
                var: "cfg_present",
                expr: int(c),
            },
            Update {
                var: "winner",
                expr: winner_for(c),
            },
        ],
    };
    Model {
        name: "FallbackPrecedence",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "cfg_present",
                init: 0,
            },
            // Init matches CaseNone (discovery wins when nothing outranks it).
            StateVar {
                name: "winner",
                init: 2,
            },
        ],
        fn_vars: vec![],
        actions: vec![case("CaseNone", 0), case("CaseCfg", 1)],
        invariants: vec![Invariant {
            // An explicit config entry always heads the chain.
            name: "ConfigOutranksDiscovery",
            kind: InvariantKind::Claim,
            expr: or_(le(var("cfg_present"), int(0)), eq(var("winner"), int(1))),
        }],
    }
}

/// Text-blend gate: the linear-corrected mode remaps glyph coverage with
/// `a_corr = clamp((blend_l - bg_l) / (fg_l - bg_l), 0, 1)`, a division by the fg/bg luminance
/// gap, so the remap is legal only when (1) the mode is corrected (`Linear` stays byte-identical
/// to the physical blend), (2) the coverage texel is interior (`cov ∉ {0, 255}`, the
/// endpoint-exact early returns), and (3) the gap is non-degenerate
/// (`|fg_l - bg_l| >= TEXT_BLEND_EPS`). The real gate is `aterm_render::correction_applies`;
/// Tier-1 is `aterm-render/tests/text_blending.rs`, which enumerates its whole
/// `bool × u8 × bool` domain and binds it to the byte-level `blend_text` (output differs from
/// plain linear only when the gate is open). The float remap laws need `*`/`pow` and live in
/// the Tier-1 sweeps. `Buggy = 1` ignores `degenerate`, running the remap on a near-zero gap,
/// and breaks `CorrectionGated`.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn text_blend_gate_model() -> Model {
    // `Pick` chooses a texel context (mode × interior × degeneracy); `Fire` computes the gate
    // from the settled (unprimed) context, avoiding primed values on the right-hand side.
    let gate = if_(
        and_(
            and_(gt(var("corrected"), int(0)), gt(var("interior"), int(0))),
            // Buggy=1 drops the degeneracy conjunct (the unguarded divide).
            or_(le(var("degenerate"), int(0)), gt(cst("Buggy"), int(0))),
        ),
        int(1),
        int(0),
    );
    Model {
        name: "TextBlendGate",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "corrected",
                init: 0,
            },
            StateVar {
                name: "interior",
                init: 0,
            },
            StateVar {
                name: "degenerate",
                init: 0,
            },
            StateVar {
                name: "applies",
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
                        var: "corrected",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "interior",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "degenerate",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "applies",
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
                        var: "applies",
                        expr: gate,
                    },
                    Update {
                        var: "phase",
                        expr: int(0),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            // The remap runs only in corrected mode, on an interior texel, with a
            // non-degenerate gap: applies <= corrected ∧ applies <= interior ∧
            // applies + degenerate <= 1.
            name: "CorrectionGated",
            kind: InvariantKind::Claim,
            expr: and_(
                and_(
                    le(var("applies"), var("corrected")),
                    le(var("applies"), var("interior")),
                ),
                le(add(var("applies"), var("degenerate")), int(1)),
            ),
        }],
    }
}

/// Minimum-contrast floor: `aterm_render::floor_fg_contrast` delivers the requested ratio
/// whenever the background admits it, and the best achievable contrast otherwise. Tier-1 is
/// `aterm-render/tests/contrast_floor.rs`, which checks the same bound against an independent
/// WCAG oracle over grayscale and a dense RGB lattice (the ratio/luminance arithmetic is not
/// expressible in ty).
///
/// Contrast lattice `0..=4` (0 ≈ 1:1 … 4 ≈ 21:1). `Pick` chooses the incoming fg's contrast
/// `c_fg`, the poles' achievable contrasts `cap_black`/`cap_white` against the same bg, and the
/// requested ratio `r`; `Fire` computes the result `c_out` as the shipping code does: early exit
/// when `c_fg >= r`, else a step search toward the chosen pole, reaching `r` iff that pole's cap
/// admits it and landing on the pole otherwise. `FloorDelivers`: `c_out >= min(r,
/// max(cap_black, cap_white))`. `Buggy = 1` chooses the pole by luminance midpoint
/// (`L(bg) > 0.5`), which for mid-luminance backgrounds picks the weaker pole, so `r` between
/// the weak and strong caps breaks the bound.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn contrast_floor_model() -> Model {
    let strong = || {
        if_(
            gt(var("cap_black"), var("cap_white")),
            var("cap_black"),
            var("cap_white"),
        )
    };
    let weak = || {
        if_(
            gt(var("cap_black"), var("cap_white")),
            var("cap_white"),
            var("cap_black"),
        )
    };
    // The pole the fallback search walks toward: the contrast argmax; under Buggy the
    // midpoint rule's worst case, the weaker pole.
    let chased = || if_(gt(cst("Buggy"), int(0)), weak(), strong());
    // c_out: the early exit keeps c_fg; else the search returns the first candidate meeting
    // r (reachable iff the chased pole admits r, since the final step is the pole), else the
    // pole itself.
    let c_out = if_(
        le(var("r"), var("c_fg")),
        var("c_fg"),
        if_(le(var("r"), chased()), var("r"), chased()),
    );
    // min(r, strong) — what the floor promises.
    let bound = if_(le(var("r"), strong()), var("r"), strong());
    Model {
        name: "ContrastFloor",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "c_fg",
                init: 0,
            },
            StateVar {
                name: "cap_black",
                init: 0,
            },
            StateVar {
                name: "cap_white",
                init: 0,
            },
            StateVar { name: "r", init: 0 },
            StateVar {
                name: "c_out",
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
                        var: "c_fg",
                        expr: in_range(int(0), int(4)),
                    },
                    Update {
                        var: "cap_black",
                        expr: in_range(int(0), int(4)),
                    },
                    Update {
                        var: "cap_white",
                        expr: in_range(int(0), int(4)),
                    },
                    Update {
                        var: "r",
                        expr: in_range(int(0), int(4)),
                    },
                    Update {
                        var: "c_out",
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
                        var: "c_out",
                        expr: c_out,
                    },
                    Update {
                        var: "phase",
                        expr: int(0),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            // Picked-but-unfired states (phase = 1) carry a stale c_out and are
            // exempt; every settled state satisfies the delivery bound.
            name: "FloorDelivers",
            kind: InvariantKind::Claim,
            expr: or_(gt(var("phase"), int(0)), le(bound, var("c_out"))),
        }],
    }
}

/// Vibrancy legibility: translucent glass (`background_opacity < 1.0`) raises the effective
/// per-cell minimum-contrast floor to WCAG AA (4.5:1), so glass never makes text illegible. The
/// real resolver is `aterm_gui::app_config::Config::effective_minimum_contrast`; Tier-1 is
/// aterm-gui's exhaustive `vibrancy_contrast_guarantee` over the opacity × configured-contrast
/// grid, asserting the same `NeverIllegible` bound.
///
/// Contrast lattice `0..=6`, with WCAG 4.5:1 at `Floor = 3`. `Pick` chooses `translucent` and
/// the configured floor `base`; `Fire` resolves `max(base, Floor)` when translucent, else
/// `base`. `NeverIllegible`: `translucent ⇒ effective >= Floor`, however low
/// `minimum_contrast` is set. `Buggy = 1` drops the auto-floor, so default
/// `minimum_contrast` on glass gets sub-4.5:1 text.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
#[crate::model]
pub fn pad_absorption_model() -> Model {
    // usable == w - 2*pad; rem == usable - acc. All unprimed (evaluated at the
    // pre-state of the one Settle step that consumes them).
    let usable = || sub(sub(var("w"), var("pad")), var("pad"));
    let rem = || sub(usable(), var("acc"));
    // rem/2 without division: 0 for rem in {0,1}, 1 for {2,3} (cell <= 4 on
    // this lattice, so rem <= 3).
    let half = || if_(le(rem(), int(1)), int(0), int(1));
    // Buggy=1: the lopsided split — the leading pad keeps none of the remainder.
    let lo_share = || if_(gt(cst("Buggy"), int(0)), int(0), half());
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
                guard: Some(and_(
                    eq(var("phase"), int(3)),
                    gt(add(var("acc"), var("cell")), usable()),
                )),
                updates: vec![
                    Update {
                        var: "pad_lo",
                        expr: add(var("pad"), lo_share()),
                    },
                    Update {
                        var: "pad_hi",
                        expr: add(var("pad"), sub(rem(), lo_share())),
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
                kind: InvariantKind::Uncaught,
                expr: settled_implies(eq(
                    add(add(var("pad_lo"), var("acc")), var("pad_hi")),
                    var("w"),
                )),
            },
            Invariant {
                // Both pads keep the configured floor.
                name: "PadsFloor",
                kind: InvariantKind::Uncaught,
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
                kind: InvariantKind::Uncaught,
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
#[crate::model]
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
#[crate::model]
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
#[crate::model]
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
    let report_bearing = if_(gt(cst("Buggy"), int(0)), rounded(), var("bearing"));
    let report_rem = if_(gt(cst("Buggy"), int(0)), int(0), var("rem"));
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
                name: "Report",
                guard: Some(and_(eq(var("phase"), int(2)), rem_in_unit())),
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
                kind: InvariantKind::Uncaught,
                expr: or_(neq(var("phase"), int(3)), rem_in_unit()),
            },
        ],
    }
}

/// Cursor cut-out clip: every block-cursor cut-out paint is clipped to the cursor rect's x-span
/// `[w0, w1)`, so the visible slice is the intersection `[max(g0, w0), min(g0+gw, w1))` of the
/// glyph extent with the window. An unclipped re-blit paints cell bg over a ligature's
/// lead-cell ink or erases a wide CJK glyph's other half. The real seam is
/// `aterm_render::clip_span` (every cut-out blit) and `glyph_quad`'s x-clip on the GPU; Tier-1
/// is `aterm-render/tests/cursor_ink.rs`, whose lattice tests check this law on the shipping
/// fns and whose pixel sweep requires the complement of the cursor rect to be byte-identical to
/// the no-cursor frame.
///
/// After `PickGlyph` → `PickWinLo` → `PickWinHi` (every extent × window on the lattice), `Slice`
/// computes the intersection with `min`/`max` as `if/else`. The left/right remainders tile the
/// extent by construction, so the laws are `SlicesOrdered` (`g0 <= m0 <= m1 <= g0+gw`) and
/// `CutoutInsideWindow` (a non-empty slice never exits the window). `Buggy = 1` slices the
/// whole extent, so any strictly clipping window breaks `CutoutInsideWindow`.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn cursor_cutout_clip_model() -> Model {
    let min = |a: Expr, b: Expr| if_(le(a.clone(), b.clone()), a, b);
    let max = |a: Expr, b: Expr| if_(le(a.clone(), b.clone()), b, a);
    let end = || add(var("g0"), var("gw"));
    // Committed slice: clamp the window edge into the glyph extent. Buggy: the whole extent,
    // window ignored.
    let clamped = |edge: &'static str| max(var("g0"), min(var(edge), end()));
    let buggy = || gt(cst("Buggy"), int(0));
    let settled_implies = |body: Expr| or_(neq(var("phase"), int(4)), body);
    Model {
        name: "CursorCutoutClip",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "g0",
                init: 0,
            },
            StateVar {
                name: "gw",
                init: 0,
            },
            StateVar {
                name: "w0",
                init: 0,
            },
            StateVar {
                name: "w1",
                init: 0,
            },
            StateVar {
                name: "m0",
                init: 0,
            },
            StateVar {
                name: "m1",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // Every glyph extent on the lattice: origin and width range over
                // enough values that windows can hit the interior, an edge, or miss.
                name: "PickGlyph",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "g0",
                        expr: in_range(int(0), int(3)),
                    },
                    Update {
                        var: "gw",
                        expr: in_range(int(0), int(4)),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                name: "PickWinLo",
                guard: Some(eq(var("phase"), int(1))),
                updates: vec![
                    Update {
                        var: "w0",
                        expr: in_range(int(0), int(6)),
                    },
                    Update {
                        var: "phase",
                        expr: int(2),
                    },
                ],
            },
            Action {
                // w1 >= w0: a well-formed [w0, w1) window (the callers pass
                // x0 .. x0 + cur_w with cur_w >= 0).
                name: "PickWinHi",
                guard: Some(eq(var("phase"), int(2))),
                updates: vec![
                    Update {
                        var: "w1",
                        expr: in_range(var("w0"), int(6)),
                    },
                    Update {
                        var: "phase",
                        expr: int(3),
                    },
                ],
            },
            Action {
                // The clip: middle slice = extent ∩ window (committed), or the
                // whole unclipped extent (Buggy).
                name: "Slice",
                guard: Some(eq(var("phase"), int(3))),
                updates: vec![
                    Update {
                        var: "m0",
                        expr: if_(buggy(), var("g0"), clamped("w0")),
                    },
                    Update {
                        var: "m1",
                        expr: if_(buggy(), end(), clamped("w1")),
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
                // g0 <= m0 <= m1 <= g0+gw: the slice stays inside the glyph, so
                // the left/right remainders [g0,m0) / [m1,g0+gw) tile around it.
                name: "SlicesOrdered",
                kind: InvariantKind::Uncaught,
                expr: settled_implies(and_(
                    le(var("g0"), var("m0")),
                    and_(le(var("m0"), var("m1")), le(var("m1"), end())),
                )),
            },
            Invariant {
                // No bleed: a non-empty cut-out slice never exits the window.
                name: "CutoutInsideWindow",
                kind: InvariantKind::Claim,
                expr: settled_implies(or_(
                    le(var("m1"), var("m0")),
                    and_(le(var("w0"), var("m0")), le(var("m1"), var("w1"))),
                )),
            },
        ],
    }
}
