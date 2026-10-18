// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    PRELAUNCH_HOLD_MAX, ParkGate, ParkGateFacts, prelaunch_hold_cap, prelaunch_park_admitted,
};
use crate::native_update_auto_intent::{ActivityFacts, ApplyPhase, automatic_park_refusal};
use crate::native_updater_service::ApplyMode;

const MODES: [ApplyMode; 3] = [
    ApplyMode::Automatic,
    ApplyMode::AutomaticPastGrace,
    ApplyMode::Immediate,
];
const PHASES: [ApplyPhase; 4] = [
    ApplyPhase::PreferIdle,
    ApplyPhase::PreferOutputGap,
    ApplyPhase::KeysOnly,
    ApplyPhase::Land,
];

/// A machine that is idle in every sense: every mode parks on these.
fn calm(mode: ApplyMode, phase: ApplyPhase) -> ParkGateFacts {
    ParkGateFacts {
        mode,
        phase,
        activity: ActivityFacts {
            quiet: true,
            hands_off_keys: true,
            output_quiet: true,
            focused: true,
            consent_warmup: false,
            harness_restored_pending: false,
        },
        masters_quiet: true,
        masters_alive: true,
        land_waits: 0,
        land_gate_relaxed: false,
        held_for: std::time::Duration::ZERO,
        recording: false,
    }
}

fn facts_from_bits(mode: ApplyMode, phase: ApplyPhase, bits: u32) -> ParkGateFacts {
    ParkGateFacts {
        mode,
        phase,
        activity: ActivityFacts {
            quiet: bits & 1 != 0,
            hands_off_keys: bits & 2 != 0,
            output_quiet: bits & 4 != 0,
            focused: bits & 8 != 0,
            consent_warmup: bits & 32 != 0,
            harness_restored_pending: bits & 64 != 0,
        },
        masters_quiet: bits & 16 != 0,
        masters_alive: true,
        land_waits: 0,
        land_gate_relaxed: false,
        held_for: std::time::Duration::ZERO,
        recording: false,
    }
}

fn gate(facts: ParkGateFacts) -> ParkGate {
    prelaunch_park_admitted(facts, prelaunch_hold_cap(facts.mode))
}

/// Every combination of the seven boolean facts, for every mode and phase,
/// against the rule written out independently here, so no arm quietly admits a
/// park the ladder refuses or refuses one it owes.
#[test]
fn the_gate_admits_exactly_what_the_ladder_owes() {
    for mode in MODES {
        for phase in PHASES {
            for bits in 0..128u32 {
                let facts = facts_from_bits(mode, phase, bits);
                let a = facts.activity;
                let expected = if !mode.is_automatic() {
                    // The person asked and is waiting for it.
                    true
                } else {
                    facts.masters_quiet
                        && !a.consent_warmup
                        && !a.harness_restored_pending
                        && match phase {
                            ApplyPhase::PreferIdle => a.hands_off_keys && a.quiet,
                            ApplyPhase::PreferOutputGap => {
                                a.hands_off_keys && !(a.focused && !a.output_quiet)
                            }
                            ApplyPhase::KeysOnly => a.hands_off_keys,
                            ApplyPhase::Land => true,
                        }
                };
                assert_eq!(
                    gate(facts) == ParkGate::Park,
                    expected,
                    "{mode:?} {phase:?} with {facts:?} disagreed with the rule"
                );
                // And the park's refusal is the entry's, word for word: one predicate, two
                // instants.
                if mode.is_automatic() && facts.masters_quiet {
                    let entry = automatic_park_refusal(phase, a);
                    match gate(facts) {
                        ParkGate::Park => assert_eq!(entry, None),
                        ParkGate::Wait(reason) => assert_eq!(entry, Some(reason)),
                        ParkGate::StandDown(_) => unreachable!("held_for is zero"),
                    }
                }
            }
        }
    }
}

/// Monotone in `held_for`: past the cap every automatic mode stands down, and
/// nothing later makes it admit. The explicit modes never wait, so have no cap.
#[test]
fn the_hold_cap_stands_down_and_never_admits_again() {
    for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
        assert_eq!(prelaunch_hold_cap(mode), Some(PRELAUNCH_HOLD_MAX));
        for phase in PHASES {
            for bits in 0..128u32 {
                let facts = ParkGateFacts {
                    held_for: PRELAUNCH_HOLD_MAX,
                    ..facts_from_bits(mode, phase, bits)
                };
                assert!(
                    matches!(gate(facts), ParkGate::StandDown(_)),
                    "{mode:?} past the cap must stand down: {facts:?}"
                );
                assert!(
                    matches!(
                        gate(ParkGateFacts {
                            held_for: PRELAUNCH_HOLD_MAX * 2,
                            ..facts
                        }),
                        ParkGate::StandDown(_)
                    ),
                    "and stays stood down"
                );
            }
        }
        // Just inside the cap a calm machine still parks: the cap bounds the
        // wait, it does not shorten it.
        assert_eq!(
            gate(ParkGateFacts {
                held_for: PRELAUNCH_HOLD_MAX - std::time::Duration::from_millis(1),
                ..calm(mode, ApplyPhase::PreferIdle)
            }),
            ParkGate::Park
        );
    }
    let mode = ApplyMode::Immediate;
    assert_eq!(prelaunch_hold_cap(mode), None);
    assert_eq!(
        gate(ParkGateFacts {
            held_for: PRELAUNCH_HOLD_MAX * 10,
            ..calm(mode, ApplyPhase::PreferIdle)
        }),
        ParkGate::Park
    );
}

/// What a re-park buys: a park that missed its budget re-parks on the next rung with the
/// successor holding, climbing the whole freeze ladder; past the last rung it stands down in
/// the activity lane (no physical budget, no latch).
#[test]
fn a_park_miss_buys_the_ladders_next_rung_without_a_relaunch() {
    use super::{
        FreezeSeed, PRELAUNCH_MAX_PARK_MISSES, PRELAUNCH_REPARK_DELAY, ParkMissDisposition,
        handoff_freeze_budget, park_miss_disposition,
    };
    use crate::app_native::{HandoffFailureLane, PhysicalFailureShape};

    let rung =
        |misses: u8| handoff_freeze_budget(ApplyMode::Automatic, misses, FreezeSeed::Default);
    // Every miss short of the last rung re-parks one rung wider.
    for misses in 0..PRELAUNCH_MAX_PARK_MISSES {
        match park_miss_disposition(misses, "a PTY reader missed the deadline".into()) {
            ParkMissDisposition::Repark {
                misses: next,
                reason,
            } => {
                assert_eq!(next, misses + 1, "the re-park is the next rung");
                assert!(rung(next) > rung(misses), "and the next rung is wider");
                assert_eq!(reason, "a PTY reader missed the deadline");
            }
            ParkMissDisposition::StandDown(stand_down) => panic!(
                "miss {misses} of {PRELAUNCH_MAX_PARK_MISSES} must re-park, not stand \
                 down ({})",
                stand_down.detail
            ),
            ParkMissDisposition::RetryMidSequence { .. } => {
                panic!("an ordinary miss is never a free mid-sequence re-park")
            }
        }
    }
    // The last rung missed: the successor is stood down as activity, and the shipping
    // classifier keeps it out of every physical shape.
    let ParkMissDisposition::StandDown(stand_down) =
        park_miss_disposition(PRELAUNCH_MAX_PARK_MISSES, "capture exceeded 250 ms".into())
    else {
        panic!("past the last rung the attempt must stand down");
    };
    assert_eq!(
        stand_down.outcome,
        crate::UpdateHandoffOutcome::ActivityRevoked,
        "a stopwatch the parent missed is a fact about the machine's moment, never \
         about the bytes"
    );
    assert!(stand_down.detail.contains("capture exceeded 250 ms"));
    for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
        let lane = HandoffFailureLane::classify(
            mode,
            stand_down.outcome,
            crate::ChildDeathEvidence::Unobserved,
            false,
        );
        assert_eq!(lane, HandoffFailureLane::ActivityRevoked, "{mode:?}");
        assert_ne!(
            lane,
            HandoffFailureLane::Physical(PhysicalFailureShape::Structural),
            "{mode:?}: the two-attempt structural lifetime is the mutant"
        );
    }
    assert!(
        PRELAUNCH_REPARK_DELAY * u32::from(PRELAUNCH_MAX_PARK_MISSES) < PRELAUNCH_HOLD_MAX,
        "every re-park must fit inside the hold it is spending"
    );
}

/// A hung-up master is never parked over: the gate waits in every automatic phase with its own
/// reason, while a person's apply parks at once. The wait is unbounded here because exited
/// `--hold` panes never reach the gate (`App::handoff_live_sessions` leaves them out, pinned by
/// `an_exited_held_pane_is_not_handed_and_does_not_hold_the_park`).
#[test]
fn a_dead_master_holds_every_automatic_phase_with_its_own_reason() {
    for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
        for phase in PHASES {
            for land_waits in [0, super::PRELAUNCH_LAND_MAX_WAITS, u8::MAX] {
                assert!(
                    matches!(
                        gate(ParkGateFacts {
                            masters_alive: false,
                            land_waits,
                            ..calm(mode, phase)
                        }),
                        ParkGate::Wait(_)
                    ),
                    "{mode:?} {phase:?} after {land_waits} waits"
                );
            }
        }
    }
    let mode = ApplyMode::Immediate;
    assert_eq!(
        gate(ParkGateFacts {
            masters_alive: false,
            ..calm(mode, ApplyPhase::PreferIdle)
        }),
        ParkGate::Park,
        "{mode:?}: the person asked; Commit's own liveness check answers them"
    );
}

/// `park_quiet_gate_at_land = "relaxed"` in the successor's signed policy brings
/// the `Land` bound forward to the first wait and relaxes nothing else: queued
/// output still holds earlier phases, a dead master holds every phase, and the
/// hold cap still stands the successor down.
#[test]
fn a_relaxed_land_policy_parks_over_queued_output_at_land_and_nowhere_else() {
    for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
        for phase in PHASES {
            let relaxed = gate(ParkGateFacts {
                masters_quiet: false,
                land_gate_relaxed: true,
                ..calm(mode, phase)
            });
            if phase == ApplyPhase::Land {
                assert_eq!(relaxed, ParkGate::Park, "{mode:?}: parks at the first run");
            } else {
                assert!(
                    matches!(relaxed, ParkGate::Wait(_)),
                    "{mode:?} {phase:?}: an earlier phase still prefers a clean instant"
                );
            }
            assert!(
                matches!(
                    gate(ParkGateFacts {
                        masters_quiet: false,
                        masters_alive: false,
                        land_gate_relaxed: true,
                        ..calm(mode, phase)
                    }),
                    ParkGate::Wait(_)
                ),
                "{mode:?} {phase:?}: never over a dead master"
            );
        }
        assert!(matches!(
            gate(ParkGateFacts {
                masters_quiet: false,
                land_gate_relaxed: true,
                held_for: PRELAUNCH_HOLD_MAX,
                ..calm(mode, ApplyPhase::Land)
            }),
            ParkGate::StandDown(_)
        ));
    }
}

/// Unread bytes on a master hold every automatic phase, and `Land` only for
/// `PRELAUNCH_LAND_MAX_WAITS` consecutive waits: a producer faster than the parser keeps
/// `POLLIN` set, and the successor replays kernel-queued bytes after Commit.
#[test]
fn unconsumed_master_output_holds_until_the_bound_has_waited_long_enough() {
    for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
        for phase in PHASES {
            for land_waits in [0, super::PRELAUNCH_LAND_MAX_WAITS - 1] {
                assert!(
                    matches!(
                        gate(ParkGateFacts {
                            masters_quiet: false,
                            land_waits,
                            ..calm(mode, phase)
                        }),
                        ParkGate::Wait(_)
                    ),
                    "{mode:?} {phase:?} after {land_waits} waits"
                );
            }
            let waited = gate(ParkGateFacts {
                masters_quiet: false,
                land_waits: super::PRELAUNCH_LAND_MAX_WAITS,
                ..calm(mode, phase)
            });
            if phase == ApplyPhase::Land {
                assert_eq!(
                    waited,
                    ParkGate::Park,
                    "{mode:?}: the bound parks over a master that never goes quiet"
                );
            } else {
                assert!(
                    matches!(waited, ParkGate::Wait(_)),
                    "{mode:?} {phase:?}: the count relaxes Land only"
                );
            }
        }
    }
    // The hold cap still wins over everything: a successor held past it
    // stands down, however many waits it has banked.
    assert!(matches!(
        gate(ParkGateFacts {
            masters_quiet: false,
            land_waits: super::PRELAUNCH_LAND_MAX_WAITS,
            land_gate_relaxed: false,
            held_for: PRELAUNCH_HOLD_MAX,
            ..calm(ApplyMode::AutomaticPastGrace, ApplyPhase::Land)
        }),
        ParkGate::StandDown(_)
    ));
}
