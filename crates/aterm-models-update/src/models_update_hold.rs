// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The update hold: one per Mac, every hold expires, and every way one ends.

use aterm_spec::derive::{Liveness, Model, eq, int, or_, var};

/// One Mac's update record (`aterm_update_core::update_record`) as the updater's writers see it.
///
/// Builds are classes: 0 is the build running before the newest offer, 1 the offered build
/// the record first sees, 2 a newer one (`Newest`). `offered` only rises here. A hold is `hb`
/// (0 when the slot is empty) with its escalation count `tries`, its scheduled span `span`, the
/// part of it still ahead `left`, and `lifts`, how many times peer evidence cut this lineage
/// short (0 or 1; the code's `peer_lifted`). Durations are classes of the real seconds, each
/// monotone in them: 0 none, 1 up to an hour, 2 up to six hours, 3 under a day, 4 a day
/// (`HoldMax`, the code's `HOLD_MAX`). `Tick` is the clock crossing one class boundary of the
/// time left. `trial` is the build under a boot trial and `tw` 1 when a window launched it.
/// `pb`/`sup` are one probe of the gate, `holds(build, sha, now)`: `pb` 1 or 2 asks about
/// that build's artifact, 3 about an artifact the hold does not name (another digest), and
/// `sup` is the answer; every other action clears them. `fails` and `peak` are ghosts: the
/// failures this lineage has had and the span its schedule asked for at the latest one, which
/// the stored `span` never falls below. A person's retry (`PersonRetry`) and a peer's evidence
/// (`PeerProven`) cut a live hold short and keep its lineage, so a repeat still escalates.
/// `KeepUnproven` is a trial settled keeping its build without proving it (a headless start, or
/// the refusal bound), which leaves the hold slot alone; `Observe` is the check lane finding the
/// offered build running with no trial of it seen to settle, which proves nothing.
///
/// Schedules, by the escalation count `t` the failure takes the lineage to: a crash loop or
/// an unproven trial `1, 2, 4` (`2, 4` when a window launched it), a stage failure
/// `1, 1, 2, 4`; a repeat never shortens the span. A window-start failure is not this slot: the
/// code keeps it beside the hold (`UpdateRecord::start_failure`), where it holds only the
/// windowless lanes, so no action here writes or ends it.
///
/// Not modelled: an artifact here is its build, while the code names one by build and
/// SHA-256, so a rebuilt artifact of a held build starts its own lineage there; a withdrawn
/// release lowering the offer (the code keeps the hold above it, which still expires); a hold
/// stamped ahead of the clock (the code admits a build whose hold reaches further than
/// `HOLD_MAX` ahead of now); the `Expired` ending, which only writes `last` and so is a
/// stutter here; a window's re-trial of a build kept unproven, whose revert restores the
/// predecessor (the code arms it through the same `arm_trial` as `Stage`, and its revert
/// escalates the kept lineage as `Revert` does here). `Rearm` is no code path: it is the negative control of `HoldBounded` and of
/// `RetryWithin`, a hold set past `HoldMax` again before it lapses.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_hold_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateHold {
            const Buggy = 0;
            const HoldMax = 4;
            const MaxTries = 4;
            const Newest = 2;
            var offered = 1;
            var running = 0;
            var proven = 1;
            var trial = 0;
            var tw = 0;
            var hb = 0;
            var tries = 0;
            var fails = 0;
            var span = 0;
            var peak = 0;
            var left = 0;
            var lifts = 0;
            var pb = 0;
            var sup = 0;

            action Stage when (
                trial == 0 && offered > running && (offered > hb || hb > offered || left == 0)
            ) {
                trial = offered;
                tw = 0;
                pb = 0;
                sup = 0;
            }
            action WindowLaunch when (trial > 0 && tw == 0) {
                tw = 1;
                pb = 0;
                sup = 0;
            }
            action Confirm when (trial > 0) {
                running = trial;
                proven = 1;
                trial = 0;
                tw = 0;
                hb = if Buggy == 4 { hb } else { if hb > 0 && trial + 1 > hb { 0 } else { hb } };
                tries = if Buggy == 4 { tries } else { if hb > 0 && trial + 1 > hb { 0 } else { tries } };
                fails = if Buggy == 4 { fails } else { if hb > 0 && trial + 1 > hb { 0 } else { fails } };
                span = if Buggy == 4 { span } else { if hb > 0 && trial + 1 > hb { 0 } else { span } };
                peak = if Buggy == 4 { peak } else { if hb > 0 && trial + 1 > hb { 0 } else { peak } };
                left = if Buggy == 4 { left } else { if hb > 0 && trial + 1 > hb { 0 } else { left } };
                lifts = if Buggy == 4 { lifts } else { if hb > 0 && trial + 1 > hb { 0 } else { lifts } };
                pb = 0;
                sup = 0;
            }
            action Revert when (trial > 0) {
                hb = if offered > trial { hb } else { trial };
                tries = if offered > trial {
                    tries
                } else {
                    if hb == trial {
                        if Buggy == 5 {
                            1
                        } else {
                            if tries + 1 <= MaxTries { tries + 1 } else { MaxTries }
                        }
                    } else {
                        1
                    }
                };
                fails = if offered > trial {
                    fails
                } else {
                    if hb == trial {
                        if fails + 1 <= MaxTries { fails + 1 } else { MaxTries }
                    } else {
                        1
                    }
                };
                span = if offered > trial {
                    span
                } else {
                    if hb == trial {
                        if Buggy == 5 {
                            if tw == 1 { if 2 <= span { span } else { 2 } } else { if 1 <= span { span } else { 1 } }
                        } else {
                            if tw == 1 {
                                if tries + 1 <= 1 { if 2 <= span { span } else { 2 } } else { 4 }
                            } else {
                                if tries + 1 <= 1 {
                                    if 1 <= span { span } else { 1 }
                                } else {
                                    if tries + 1 <= 2 { if 2 <= span { span } else { 2 } } else { 4 }
                                }
                            }
                        }
                    } else {
                        if tw == 1 { 2 } else { 1 }
                    }
                };
                left = if offered > trial {
                    left
                } else {
                    if hb == trial {
                        if Buggy == 5 {
                            if tw == 1 { if 2 <= span { span } else { 2 } } else { if 1 <= span { span } else { 1 } }
                        } else {
                            if tw == 1 {
                                if tries + 1 <= 1 { if 2 <= span { span } else { 2 } } else { 4 }
                            } else {
                                if tries + 1 <= 1 {
                                    if 1 <= span { span } else { 1 }
                                } else {
                                    if tries + 1 <= 2 { if 2 <= span { span } else { 2 } } else { 4 }
                                }
                            }
                        }
                    } else {
                        if tw == 1 { 2 } else { 1 }
                    }
                };
                peak = if offered > trial {
                    peak
                } else {
                    if hb == trial {
                        if tw == 1 {
                            if fails + 1 <= 1 { 2 } else { 4 }
                        } else {
                            if fails + 1 <= 1 {
                                1
                            } else {
                                if fails + 1 <= 2 { 2 } else { 4 }
                            }
                        }
                    } else {
                        if tw == 1 { 2 } else { 1 }
                    }
                };
                lifts = if offered > trial || hb == trial { lifts } else { 0 };
                trial = 0;
                tw = 0;
                pb = 0;
                sup = 0;
            }
            action Unproven when (trial > 0) {
                running = trial;
                proven = 0;
                hb = if offered > trial { hb } else { trial };
                tries = if offered > trial {
                    tries
                } else {
                    if hb == trial { if tries + 1 <= MaxTries { tries + 1 } else { MaxTries } } else { 1 }
                };
                fails = if offered > trial {
                    fails
                } else {
                    if hb == trial { if fails + 1 <= MaxTries { fails + 1 } else { MaxTries } } else { 1 }
                };
                span = if offered > trial {
                    span
                } else {
                    if hb == trial {
                        if tw == 1 {
                            if tries + 1 <= 1 { if 2 <= span { span } else { 2 } } else { 4 }
                        } else {
                            if tries + 1 <= 1 {
                                if 1 <= span { span } else { 1 }
                            } else {
                                if tries + 1 <= 2 { if 2 <= span { span } else { 2 } } else { 4 }
                            }
                        }
                    } else {
                        if tw == 1 { 2 } else { 1 }
                    }
                };
                left = if offered > trial {
                    left
                } else {
                    if hb == trial {
                        if tw == 1 {
                            if tries + 1 <= 1 { if 2 <= span { span } else { 2 } } else { 4 }
                        } else {
                            if tries + 1 <= 1 {
                                if 1 <= span { span } else { 1 }
                            } else {
                                if tries + 1 <= 2 { if 2 <= span { span } else { 2 } } else { 4 }
                            }
                        }
                    } else {
                        if tw == 1 { 2 } else { 1 }
                    }
                };
                peak = if offered > trial {
                    peak
                } else {
                    if hb == trial {
                        if tw == 1 {
                            if fails + 1 <= 1 { 2 } else { 4 }
                        } else {
                            if fails + 1 <= 1 {
                                1
                            } else {
                                if fails + 1 <= 2 { 2 } else { 4 }
                            }
                        }
                    } else {
                        if tw == 1 { 2 } else { 1 }
                    }
                };
                lifts = if offered > trial || hb == trial { lifts } else { 0 };
                trial = 0;
                tw = 0;
                pb = 0;
                sup = 0;
            }
            action StageFail when (
                trial == 0 && offered > running && (offered > hb || hb > offered || left == 0)
            ) {
                hb = offered;
                tries = if hb == offered { if tries + 1 <= MaxTries { tries + 1 } else { MaxTries } } else { 1 };
                fails = if hb == offered { if fails + 1 <= MaxTries { fails + 1 } else { MaxTries } } else { 1 };
                span = if hb == offered {
                    if tries + 1 <= 2 {
                        if 1 <= span { span } else { 1 }
                    } else {
                        if tries + 1 <= 3 { if 2 <= span { span } else { 2 } } else { 4 }
                    }
                } else {
                    1
                };
                left = if hb == offered {
                    if tries + 1 <= 2 {
                        if 1 <= span { span } else { 1 }
                    } else {
                        if tries + 1 <= 3 { if 2 <= span { span } else { 2 } } else { 4 }
                    }
                } else {
                    1
                };
                peak = if hb == offered {
                    if fails + 1 <= 2 {
                        1
                    } else {
                        if fails + 1 <= 3 { 2 } else { 4 }
                    }
                } else {
                    1
                };
                lifts = if hb == offered { lifts } else { 0 };
                pb = 0;
                sup = 0;
            }
            action OfferNewer when (Newest > offered) {
                offered = Newest;
                hb = if Buggy == 3 { hb } else { if Newest > hb { 0 } else { hb } };
                tries = if Buggy == 3 { tries } else { if Newest > hb { 0 } else { tries } };
                fails = if Buggy == 3 { fails } else { if Newest > hb { 0 } else { fails } };
                span = if Buggy == 3 { span } else { if Newest > hb { 0 } else { span } };
                peak = if Buggy == 3 { peak } else { if Newest > hb { 0 } else { peak } };
                left = if Buggy == 3 { left } else { if Newest > hb { 0 } else { left } };
                lifts = if Buggy == 3 { lifts } else { if Newest > hb { 0 } else { lifts } };
                pb = 0;
                sup = 0;
            }
            action PeerProven when (hb > 0 && left > 0 && (lifts == 0 || Buggy == 6)) {
                left = 0;
                lifts = if lifts + 1 <= 2 { lifts + 1 } else { 2 };
                pb = 0;
                sup = 0;
            }
            action PersonRetry when (hb > 0 && left > 0) {
                left = 0;
                tries = if Buggy == 7 { 0 } else { tries };
                pb = 0;
                sup = 0;
            }
            action KeepUnproven when (trial > 0) {
                running = trial;
                proven = 0;
                trial = 0;
                tw = 0;
                pb = 0;
                sup = 0;
            }
            action Observe when (trial == 0 && offered > running) {
                running = offered;
                proven = 0;
                pb = 0;
                sup = 0;
            }
            action Tick when (hb > 0 && left > 0) {
                left = left - 1;
                pb = 0;
                sup = 0;
            }

            action Rearm when (Buggy == 1 && hb > 0 && left > 0) {
                left = HoldMax + 1;
                pb = 0;
                sup = 0;
            }
            action Probe {
                pb = offered;
                sup = if hb == offered && (left > 0 || Buggy == 2) { 1 } else { 0 };
            }
            action ProbeOther {
                pb = 3;
                sup = 0;
            }

            invariant HoldBounded: span <= HoldMax && left <= span;
            invariant SuppressIffLive:
                pb == 0
                    || (sup == 1 && pb == hb && left > 0)
                    || (sup == 0 && (pb > hb || hb > pb || left == 0));
            invariant NewerEndsHold: hb == 0 || offered <= hb;
            invariant StartEndsHold: proven == 0 || hb == 0 || hb > running;
            invariant RetryEscalates: tries == fails && peak <= span;
            invariant PeerLiftOnce: lifts <= 1;
            invariant OneHold:
                (hb > 0 && tries > 0)
                    || (hb == 0 && tries == 0 && fails == 0 && span == 0 && peak == 0 && left == 0 && lifts == 0);
            bound Builds: offered <= Newest && running <= Newest && trial <= Newest && hb <= Newest;
            bound Counts: tries <= MaxTries && fails <= MaxTries && lifts <= 2;
        }
    }
}

/// [`native_update_hold_model`]'s progress claim: every hold lapses, so the held build is
/// tried again by itself (`Stage` is enabled again once nothing newer replaced it), within the
/// `HoldMax` its span is bounded by. The mutant is the defect this model was written for, a
/// hold whose time never runs out.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(native_update_hold_model)]
pub fn native_update_hold_liveness() -> Liveness {
    Liveness {
        name: "RetryWithin",
        goal: or_(eq(var("hb"), int(0)), eq(var("left"), int(0))),
        weak: vec!["Tick"],
        strong: vec![],
        mutants: vec!["Rearm"],
    }
}
