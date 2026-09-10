// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Console attention ownership at completed brain ticks. The host supplies
//! input, selection, content identity and command events; this projection
//! does not model sprite geometry, animation duration or input delivery.

use super::*;

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
