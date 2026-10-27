// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Console attention ownership and pet placement models at completed brain ticks.

use aterm_spec::derive::*;

/// Cursor-home placement model at emitted frames.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn console_cursor_home_model() -> Model {
    aterm_spec::ty_model! {
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
            action ProtectAll when (protected == 0 && (moving == 0 || Buggy == 2)) {
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
            bound Bounded: visible <= 1 && home <= 1 && moving <= 1 && protected <= 1 && hidden <= 1 && event <= 9;
        }
    }
}

/// Console attention episode lifecycle model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn console_life_episode_model() -> Model {
    aterm_spec::ty_model! {
        ConsoleLifeEpisode {
            const Buggy = 0;
            const MaxInputs = 3;
            const MaxSurface = 2;
            var seq = 0;
            var consumed = 0;
            var inputs = 0;
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
            bound StateBounded:
                seq <= MaxInputs && inputs <= MaxInputs && consumed <= MaxInputs
                    && attention <= 4 && anchor <= 1 && selected <= 1 && result <= 1
                    && surface <= MaxSurface && event <= 9;
        }
    }
}

/// Cadence custody handoff model when reading takes over an airborne resident.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn console_resident_handoff_model() -> Model {
    aterm_spec::ty_model! {
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
            bound StateBounded:
                legacy <= 1 && resident <= 1 && handoff <= 1 && touch <= 1
                    && blocked <= 1 && wake <= 1 && event <= 8;
        }
    }
}

/// Emitted resident z-order model (`PetFrame::under_ink`); a leg dealt in front
/// (`PetBrain::front_pass`) reaches ink over it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn console_pet_z_order_model() -> Model {
    aterm_spec::ty_model! {
        ConsolePetZOrder {
            const Buggy = 0;
            var drawn = 0;
            var ink = 0;
            var under = 0;
            var prev_under = 0;
            var dwelt = 0;
            var front = 0;

            action Ink when (drawn <= 1) {
                prev_under = under;
                under = if Buggy == 1 && under == 0 && ink == 0 { 0 } else { 1 };
                ink = 1; drawn = 1; dwelt = 0; front = 0;
            }
            action InkOnFrontPass when (drawn <= 1) {
                prev_under = under;
                under = if Buggy == 1 { 1 } else { under };
                ink = 1; drawn = 1; dwelt = 0; front = 1;
            }
            action Clear when (drawn <= 1) {
                prev_under = under;
                under = if Buggy == 1 && ink == 0 { 1 - under } else { under };
                ink = 0; drawn = 1; dwelt = 0; front = 0;
            }
            action ClearDwelt when (drawn == 1 && ink == 0 && under == 1) {
                prev_under = under;
                under = if Buggy == 1 { 1 } else { 0 };
                ink = 0; drawn = 1; dwelt = 1; front = 0;
            }
            action Hide when (drawn <= 1) {
                prev_under = under;
                under = if Buggy == 1 { under } else { 0 };
                ink = 0; drawn = 0; dwelt = 0; front = 0;
            }

            invariant InkStaysInFront:
                if drawn == 1 && ink == 1 && front == 0 { under == 1 } else { under <= 1 };
            invariant FrontPassStaysOver:
                if drawn == 1 && ink == 1 && front == 1 && prev_under == 0 { under == 0 } else { under <= 1 };
            invariant ClearNeverSinks:
                if drawn == 1 && ink == 0 && prev_under == 0 { under == 0 } else { under <= 1 };
            invariant SurfacingWaitsTheDwell:
                if drawn == 1 && prev_under == 1 && under == 0 { dwelt == 1 } else { dwelt <= 1 };
            invariant DwellSurfaces: if dwelt == 1 { under == 0 } else { dwelt == 0 };
            invariant AbsenceReleases: if drawn == 0 { under == 0 } else { drawn == 1 };
        }
    }
}

/// Resident coordinate-seam transition model across a resize, scroll, or clear.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn pet_surface_seam_model() -> Model {
    aterm_spec::ty_model! {
        PetSurfaceSeam {
            const Buggy = 0;
            const PaneCut = 0;
            const Restart = 0;
            const OwnClock = 0;
            const Cull = 0;
            const LeaverCut = 0;
            var drawn = 0;
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
            var refused = 0;
            var dipped = 0;
            var leaver = 0;

            action Draw when (drawn == 0) {
                drawn = 1; lit = 1; hit = 1;
            }
            action Handoff when (drawn == 1 && seam == 0 && leaver == 0) {
                leaver = 1;
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
            action Refuse when (seam == 2 && ticked == 0 && lost == 0 && refused == 0) {
                refused = 1;
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
                } else if seam == 2 && lost == 0 && refused == 1 {
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
                dipped = if seam == 2 && lost == 0 && refused == 1 && (Buggy == 1 || Cull == 1) {
                    1
                } else {
                    0
                };
            }
            action Return when (seam == 2 && ticked == 1 && lost == 0 && returned == 0 && spent == 0) {
                returned = 1; ticked = 0; refused = 0;
                lit = Buggy; hit = Buggy; ghost = 0;
            }
            action Spend when (seam == 2 && ticked == 1 && lost == 0 && spent == 0) {
                spent = 1;
                tail = if returned == 1 && (Buggy == 1 || OwnClock == 1) {
                    1
                } else if leaver == 1 && (Buggy == 1 || LeaverCut == 1) {
                    1
                } else {
                    0
                };
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
                if seam == 2 && ticked == 1 && lost == 0 && refused == 0 {
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
            invariant RefusedGhostHandsOver: dipped == 0;
        }
    }
}
