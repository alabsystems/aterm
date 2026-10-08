// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Console attention ownership at completed brain ticks. The host supplies
//! input, selection, content identity and command events; this projection
//! does not model sprite geometry, animation duration or input delivery.

use super::*;

/// Cursor-home placement at emitted frames. Ordinary text remains drawable;
/// a moved caret carries the base home while finite local motion completes,
/// and a wholly protected surface parks the resident. `Arrive` abstracts completion of the real follower, not one
/// animation tick. `RepaintMove` is a program walking its HIDDEN cursor while
/// it repaints — inside the repaint hold, or on a frame it has not finished:
/// that cursor is its write position, so the home stays the last painted
/// caret and nothing travels (`Buggy` chases it). Geometry and its pixel
/// protection are bound separately by the real Terminal/PetBrain walk in
/// `console_cursor_home.rs`; the unfinished-frame case by
/// `pet_holds_through_a_torn_frame.rs`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn console_cursor_home_model() -> Model {
    crate::ty_model! {
        ConsoleCursorHome {
            const Buggy = 0;
            var visible = 1;
            var home = 1;
            var moving = 0;
            var protected = 0;
            var hidden = 0;
            var event = 0;

            action Ink when (protected == 0) {
                visible = if Buggy == 1 { 0 } else { 1 }; event = 1;
            }
            action MoveCaret when (protected == 0 && moving == 0 && hidden == 0) {
                home = 1; moving = 1; visible = 1; event = 2;
            }
            action Follow when (protected == 0 && moving == 1) {
                home = 1; visible = 1; event = 3;
            }
            action Arrive when (protected == 0 && moving == 1) {
                home = if Buggy == 1 { 0 } else { 1 };
                moving = 0; visible = 1; event = 4;
            }
            action ProtectAll when (protected == 0 && moving == 0) {
                protected = 1; visible = 0; home = 0; event = 5;
            }
            action Release when (protected == 1) {
                protected = 0; visible = 1; home = 1; moving = 0; event = 6;
            }
            action HideCursor when (protected == 0 && moving == 0 && hidden == 0) {
                hidden = 1; visible = if Buggy == 1 { 0 } else { 1 }; event = 7;
            }
            action ShowCursor when (protected == 0 && moving == 0 && hidden == 1) {
                hidden = 0; visible = 1; home = 1; event = 8;
            }
            action RepaintMove when (protected == 0 && moving == 0 && hidden == 1) {
                home = if Buggy == 1 { 0 } else { 1 };
                moving = if Buggy == 1 { 1 } else { 0 };
                visible = 1; event = 9;
            }
            invariant OrdinaryTextKeepsBody: if protected == 0 { visible == 1 } else { visible == 0 };
            invariant BaseStaysAtCursor: if protected == 0 { home == 1 } else { home == 0 };
            invariant ArrivesAtCursor: if event == 4 { home == 1 && moving == 0 } else { home <= 1 };
            invariant ProtectionParks: if protected == 1 { visible == 0 && moving == 0 } else { protected == 0 };
            invariant RepaintHoldsHome: if event == 9 { moving == 0 } else { moving <= 1 };
            invariant Bounded: visible <= 1 && home <= 1 && moving <= 1 && protected <= 1 && hidden <= 1 && event <= 9;
        }
    }
}

/// One input sequence is consumed once, reading protects its surface, direct
/// input revokes retained work, and expired or displaced results never replay.
/// Actions include the stimulus and its next tick. `Expire`/`ReleaseSelection`
/// advance beyond the real hold; `StaleComplete` reports a verdict already
/// beyond the real TTL. Tier 1 drives those actual clocks, never edits state.
///
/// `Buggy=1` counts echoes as new input, skips input consumption/revocation,
/// keeps work over selections and new surfaces, and replays expired results.
/// The negative controls in `console_life_conformance.rs` are rejected by the
/// same action relation that accepts shipping PetBrain observations.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn console_life_episode_model() -> Model {
    crate::ty_model! {
        ConsoleLifeEpisode {
            const Buggy = 0;
            const MaxInputs = 3;
            const MaxSurface = 2;
            var seq = 0;
            var consumed = 0;
            // Environment count of actually admitted input events.
            var inputs = 0;
            // 0 rest, 1 input, 2 reading, 3 retained content, 4 result.
            var attention = 0;
            var anchor = 0;
            var selected = 0;
            var result = 0;
            var surface = 0;
            var event = 0;

            action Input when (seq <= MaxInputs - 1) {
                seq = seq + 1;
                inputs = inputs + 1;
                consumed = if Buggy == 1 { consumed } else { seq + 1 };
                attention = if selected == 1 { 2 } else { 1 };
                anchor = if Buggy == 1 { anchor } else { 0 };
                result = if Buggy == 1 { result } else { 0 };
                event = 1;
            }
            // Several native commits between frames coalesce to the newest
            // subject, while the sequence still records every admitted event.
            action BatchInput when (seq <= MaxInputs - 2) {
                seq = seq + 2;
                inputs = inputs + 2;
                consumed = if Buggy == 1 { consumed } else { seq + 2 };
                attention = if selected == 1 { 2 } else { 1 };
                anchor = if Buggy == 1 { anchor } else { 0 };
                result = if Buggy == 1 { result } else { 0 };
                event = 1;
            }
            action Echo when (seq <= MaxInputs) {
                seq = if Buggy == 1 && seq <= MaxInputs - 1 { seq + 1 } else { seq };
                event = 2;
            }
            action Select when (selected == 0) {
                selected = 1;
                attention = if Buggy == 1 { attention } else { 2 };
                anchor = if Buggy == 1 { anchor } else { 0 };
                result = if Buggy == 1 { result } else { 0 };
                event = 3;
            }
            action ReleaseSelection when (selected == 1) {
                selected = 0; attention = 0; anchor = 0; result = 0; event = 4;
            }
            action Work when (selected == 0) {
                attention = 3; anchor = 1; result = 0; event = 5;
            }
            action Complete when (selected == 0 && attention == 3 && anchor == 1) {
                attention = 4; result = 1; event = 6;
            }
            action Expire when (selected == 0 && result == 1 && anchor == 1) {
                attention = if Buggy == 1 { 4 } else { 3 };
                result = if Buggy == 1 { 1 } else { 0 };
                event = 7;
            }
            action StaleComplete when (selected == 0 && attention == 3 && anchor == 1) {
                attention = if Buggy == 1 { 4 } else { 3 };
                result = if Buggy == 1 { 1 } else { 0 };
                event = 8;
            }
            action Surface when (surface <= MaxSurface - 1) {
                surface = surface + 1; selected = 0; consumed = seq;
                attention = if Buggy == 1 { attention } else { 0 };
                anchor = if Buggy == 1 { anchor } else { 0 };
                result = if Buggy == 1 { result } else { 0 };
                event = 9;
            }

            invariant OnlyInputAdvancesSequence: seq == inputs;
            invariant InputIsConsumed: if event == 1 { consumed == seq } else { consumed <= seq };
            invariant InputRevokesWork: if event == 1 { anchor == 0 && result == 0 } else { anchor <= 1 };
            invariant SelectionOwnsAttention:
                if selected == 1 { attention == 2 && anchor == 0 && result == 0 } else { selected == 0 };
            invariant NewSurfaceRetiresWork:
                if event == 9 { attention == 0 && anchor == 0 && result == 0 } else { surface <= MaxSurface };
            invariant ExpiryCannotReplay:
                if event == 7 || event == 8 { result == 0 && attention <= 3 } else { result <= 1 };
            invariant StateBounded:
                seq <= MaxInputs && inputs <= MaxInputs && consumed <= MaxInputs
                    && attention <= 4 && anchor <= 1 && selected <= 1 && result <= 1
                    && surface <= MaxSurface && event <= 9;
        }
    }
}

/// Cadence custody when reading takes over an existing flight. The initial
/// state is a real airborne legacy owner, not PetBrain::default. `Land`
/// abstracts the tick that retires its final flight/landing work; the resident
/// still owes one actual processing tick afterward. A clear existing body can
/// then park, while a no-target decision must consume suppressed affection.
///
/// This model checks wake decisions and ownership, not flight duration,
/// geometry, general perch travel or fairness of the host's future ticks.
/// `Buggy=1` replays premature flight/landing parking, forgotten handoff, and
/// perpetual no-target wake from a stale Land action or unconsumed touch.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn console_resident_handoff_model() -> Model {
    crate::ty_model! {
        ConsoleResidentHandoff {
            const Buggy = 0;
            var legacy = 1;
            var resident = 0;
            var handoff = 0;
            var touch = 0;
            var blocked = 0;
            var wake = 1;
            var event = 0;

            action SelectClear when (resident == 0 && legacy == 1) {
                resident = 1; handoff = 1; blocked = 0;
                wake = if Buggy == 1 { 0 } else { 1 }; event = 1;
            }
            action SelectDense when (resident == 0 && legacy == 1) {
                resident = 1; handoff = 1; blocked = 1;
                wake = if Buggy == 1 { 0 } else { 1 }; event = 2;
            }
            action FlightTick when (resident == 1 && legacy == 1) {
                handoff = 1;
                wake = if Buggy == 1 { 0 } else { 1 }; event = 3;
            }
            action Land when (resident == 1 && legacy == 1) {
                legacy = 0;
                handoff = if Buggy == 1 { 0 } else { 1 };
                wake = if Buggy == 1 { 0 } else { 1 }; event = 4;
            }
            action ResidentTick when (resident == 1 && legacy == 0 && blocked == 0) {
                handoff = 0; touch = 0; wake = 0; event = 5;
            }
            action NoTarget when (resident == 1 && legacy == 0) {
                blocked = 1; handoff = 0;
                touch = if Buggy == 1 { touch } else { 0 };
                wake = if Buggy == 1 { 1 } else { 0 }; event = 6;
            }
            action NotePet when (resident == 1 && legacy == 0 && handoff == 0 && blocked == 0 && touch == 0) {
                touch = 1; wake = 1; event = 7;
            }
            action Quiet when (resident == 1 && legacy == 0 && handoff == 0 && touch == 0) {
                wake = if Buggy == 1 && blocked == 1 { 1 } else { 0 }; event = 8;
            }

            invariant LegacyOwnsCadence: if legacy == 1 { wake == 1 } else { legacy == 0 };
            invariant HandoffOwnsCadence: if handoff == 1 { wake == 1 } else { handoff == 0 };
            invariant LandingOwesResidentTick: if event == 4 { handoff == 1 && wake == 1 } else { handoff <= 1 };
            invariant NoTargetConsumesTouch: if event == 6 { touch == 0 } else { touch <= 1 };
            invariant NoTargetParks: if event == 6 { handoff == 0 && wake == 0 } else { wake <= 1 };
            invariant QuietResidentHasNoWake:
                if resident == 1 && legacy == 0 && handoff == 0 && touch == 0 { wake == 0 } else { wake <= 1 };
            invariant StateBounded:
                legacy <= 1 && resident <= 1 && handoff <= 1 && touch <= 1
                    && blocked <= 1 && wake <= 1 && event <= 8;
        }
    }
}

/// The resident's emitted Z-ORDER (`PetFrame::under_ink`), one action per
/// frame. Ink under the DRAWN body sends it under the glyphs at once; coming
/// back over waits for an unbroken clear run of `Z_DWELL`; a frame that
/// draws nothing releases the hold. `dwelt` is the environment's clock, not
/// a frame count: `ClearDwelt` is a clear frame at or past the dwell of a
/// clear run already under way. Placement, the brain's own hide trick and
/// the visibility hold are not modelled. Tier-1 in aterm-effects'
/// `console_pet_z_order_hold.rs` walks a real PetBrain over a real Terminal
/// through this relation and shows it rejects the shipped per-frame rule.
///
/// `Buggy=1` goes under one frame late (a symmetric hold), strobes on clear
/// frames — sinking with no ink under the body, as the shipped caret-ring
/// rule did, and surfacing on the next one — latches under past the dwell,
/// and carries the hold across a frame that drew nothing.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn console_pet_z_order_model() -> Model {
    crate::ty_model! {
        ConsolePetZOrder {
            const Buggy = 0;
            var drawn = 0;
            var ink = 0;
            var under = 0;
            var prev_under = 0;
            var dwelt = 0;

            action Ink when (drawn <= 1) {
                prev_under = under;
                under = if Buggy == 1 && under == 0 && ink == 0 { 0 } else { 1 };
                ink = 1; drawn = 1; dwelt = 0;
            }
            action Clear when (drawn <= 1) {
                prev_under = under;
                under = if Buggy == 1 && ink == 0 { 1 - under } else { under };
                ink = 0; drawn = 1; dwelt = 0;
            }
            action ClearDwelt when (drawn == 1 && ink == 0 && under == 1) {
                prev_under = under;
                under = if Buggy == 1 { 1 } else { 0 };
                ink = 0; drawn = 1; dwelt = 1;
            }
            action Hide when (drawn <= 1) {
                prev_under = under;
                under = if Buggy == 1 { under } else { 0 };
                ink = 0; drawn = 0; dwelt = 0;
            }

            invariant InkStaysInFront: if drawn == 1 && ink == 1 { under == 1 } else { under <= 1 };
            invariant ClearNeverSinks:
                if drawn == 1 && ink == 0 && prev_under == 0 { under == 0 } else { under <= 1 };
            invariant SurfacingWaitsTheDwell:
                if drawn == 1 && prev_under == 1 && under == 0 { dwelt == 1 } else { dwelt <= 1 };
            invariant DwellSurfaces: if dwelt == 1 { under == 0 } else { dwelt == 0 };
            invariant AbsenceReleases: if drawn == 0 { under == 0 } else { drawn == 1 };
        }
    }
}

/// THE RESIDENT ACROSS A COORDINATE SEAM (2026-10-01): one seam, and the
/// frame after it, for a pet that was on glass. Three seams, three laws.
///
/// * `Regrid` — the same terminal re-gridded (a resize, a font step, a DPI
///   change, a divider drag, a zoom; `CompanionOwner::rebase_geometry`). Its
///   cells are still that terminal's cells, so the body stays lit through the
///   seam and the frame after it, and does not jump: `moved` is a drawn body
///   more than a cell from where it stood.
/// * `Replace` — another terminal, or the same terminal's other screen
///   (`CompanionOwner::resight_owner`). The boundary itself is dark and owns
///   no hit target (`CursorCompanionOwnerLifecycle`'s law); the frame after
///   crossfades: the replacement is lit at its own station while the old body
///   fades as a `ghost` where it was drawn ON GLASS (`ghost_moved` is a ghost
///   anywhere else, measured in window pixels). `Shift` moves the focused
///   pixel frame to another pane while the old one stays on glass exactly as
///   drawn — a split-pane focus switch: the ghost is spent on its OWN glass,
///   placed at its pane, cropped to it and beneath its ink, so the switch
///   crossfades like any other replacement. `Lose` takes the old glass away
///   before that frame — its cell metrics changed (a font step, a DPI
///   change) or its pane left the glass (closed, zoomed away): no pixels of
///   the old body are left to fade, so no ghost is spent and the replacement
///   fades in alone.
/// * `Blind` — the next observation is incoherent (a stale snapshot, a resize
///   frame). The body on glass is held lit in place; `BlindLong` is a blind
///   run past the visibility hold's bound, which fades it out.
///
/// `Return` is a second `Replace` inside a crossfade, to a surface a ghost of
/// this cat is still fading on — a quick toggle between two panes, two tabs
/// or two screens. The boundary is dark, as every replacement's is; after it
/// the seat the cat comes back to never `restarted`: ONE body there, whose
/// presence never falls below what the user saw — an arriver standing where
/// its own ghost still fades adopts it and continues.
/// `Spend` is the crossfade running out: no ghost leaves glass while its
/// arriver is below full presence (`tail`) — each ghost's envelope runs on
/// the arriver's ramp and reaches zero on the frame the arriver is full.
///
/// `hit` is the petting target, which is the drawn body's pixels: a seam
/// withdraws it until a frame draws a body again.
///
/// `Buggy=1` is the shipped rule before 2026-10-01 — a regrid retired the
/// body (dark at the seam and on the frame after), a replacement's next frame
/// was dark with no crossfade (and a split-pane focus switch's stayed so
/// until this fix: its ghost was dropped with the moved pixel frame), a
/// blind frame was cut — plus the mutants a careless fix makes: a
/// replacement carrying the old body and its hit target across the
/// boundary, a ghost spent on glass that is gone, and a blind hold with no
/// end. `PaneCut=1` alone is the rule shipped between the same-glass
/// crossfade (4bf00833c) and the own-glass ghost: a replacement on the same
/// glass crossfades, a `Shift` drops the ghost with the moved pixel frame and
/// the frame after the switch has no cat. `Restart=1` is the rule shipped
/// until the adoption: the returning surface's arriver restarted at the
/// ramp's floor at its seat — beside its own ghost still fading there (a
/// split pane: two cats, the presence the eye sees dipping before it
/// recovers), or after that ghost was cut at the boundary (the same glass: a
/// tab, a screen). `OwnClock=1` is the ghost life shipped until then: a
/// ghost carried across the second boundary kept its old clock and left
/// glass while the restarted arriver was still coming up. Tier-1 in
/// aterm-effects'
/// `pet_coordinate_seams.rs` drives the real
/// `CompanionOwner` over a real `Terminal` through each seam and replays the
/// shipped seams as the controls.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn pet_surface_seam_model() -> Model {
    crate::ty_model! {
        PetSurfaceSeam {
            const Buggy = 0;
            const PaneCut = 0;
            const Restart = 0;
            const OwnClock = 0;
            var drawn = 0;
            // 0 none, 1 regrid, 2 replace, 3 blind.
            var seam = 0;
            var shifted = 0;
            var lost = 0;
            var ticked = 0;
            var lit = 0;
            var ghost = 0;
            var ghost_moved = 0;
            var moved = 0;
            var hit = 0;
            var long = 0;
            var returned = 0;
            var restarted = 0;
            var spent = 0;
            var tail = 0;

            action Draw when (drawn == 0) {
                drawn = 1; lit = 1; hit = 1;
            }
            action Regrid when (drawn == 1 && seam == 0) {
                seam = 1; hit = 0;
                lit = if Buggy == 1 { 0 } else { lit };
            }
            action Replace when (drawn == 1 && seam == 0) {
                seam = 2;
                lit = Buggy; hit = Buggy;
            }
            action Shift when (seam == 2 && ticked == 0 && shifted == 0) {
                shifted = 1;
            }
            action Lose when (seam == 2 && ticked == 0 && lost == 0) {
                lost = 1;
            }
            action Blind when (drawn == 1 && seam == 0) {
                seam = 3;
            }
            action Tick when (1 <= seam && ticked == 0) {
                ticked = 1;
                lit = if seam == 2 && lost == 1 {
                    Buggy
                } else if Buggy == 1 || (shifted == 1 && PaneCut == 1) {
                    0
                } else {
                    1
                };
                hit = if seam == 2 && lost == 1 {
                    Buggy
                } else if Buggy == 1 || (shifted == 1 && PaneCut == 1) {
                    0
                } else {
                    1
                };
                ghost = if seam == 2 && lost == 0 && shifted == 1 && PaneCut == 1 {
                    0
                } else if seam == 2 && lost == 0 {
                    1 - Buggy
                } else if seam == 2 {
                    Buggy
                } else {
                    0
                };
                ghost_moved = if seam == 2 && lost == 1 { Buggy } else { 0 };
                moved = 0;
                restarted = if returned == 1 && (Buggy == 1 || Restart == 1) { 1 } else { 0 };
            }
            action Return when (seam == 2 && ticked == 1 && lost == 0 && returned == 0 && spent == 0) {
                returned = 1; ticked = 0;
                lit = Buggy; hit = Buggy; ghost = 0;
            }
            action Spend when (seam == 2 && ticked == 1 && lost == 0 && spent == 0) {
                spent = 1;
                tail = if returned == 1 && (Buggy == 1 || OwnClock == 1) { 1 } else { 0 };
            }
            action BlindLong when (seam == 3 && ticked == 1 && long == 0) {
                long = 1;
                lit = Buggy; hit = Buggy;
            }

            invariant RegridKeepsTheBody:
                if seam == 1 { lit == 1 && moved == 0 } else { moved <= 1 };
            invariant SeamWithdrawsTheHitTarget:
                if 1 <= seam && seam <= 2 && ticked == 0 { hit == 0 } else { hit <= 1 };
            invariant ReplaceIsDarkAtTheBoundary:
                if seam == 2 && ticked == 0 { lit == 0 } else { lit <= 1 };
            invariant ReplaceCrossfades:
                if seam == 2 && ticked == 1 && lost == 0 {
                    lit == 1 && ghost == 1
                } else {
                    ghost <= 1
                };
            invariant GhostNeverDisplaced: ghost_moved == 0;
            invariant LostGlassSpendsNoGhost: if lost == 1 { ghost == 0 } else { lost == 0 };
            invariant BlindHoldsInPlace:
                if seam == 3 && ticked == 1 && long == 0 { lit == 1 && moved == 0 } else { long <= 1 };
            invariant BlindnessIsBounded: if long == 1 { lit == 0 } else { long == 0 };
            invariant ReturnContinuesOneBody:
                if returned == 1 && ticked == 1 { restarted == 0 } else { restarted <= 1 };
            invariant GhostLeavesAtFullPresence: tail == 0;
        }
    }
}

#[cfg(test)]
mod cursor_home_tests {
    #[test]
    fn derived_cursor_home_proves_and_catches_ink_disappearance_and_stranded_residency() {
        crate::verify::prove_and_catch_scalar(
            &super::console_cursor_home_model(),
            "cursor-home placement and ordinary-text persistence",
        );
    }
}
