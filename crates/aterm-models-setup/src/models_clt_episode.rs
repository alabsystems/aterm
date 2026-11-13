// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the account's record of setup's administrator-password attempt
//! (`aterm_types::clt_mark`): the claim, the hand to the Mac Setup tab's program, the end, the
//! cut-off, a person's own retry and their lift of a decline, and the window's and a terminal
//! `aterm init`'s decisions of how their attempts ended (`aterm_types::clt_mark::tab_end`,
//! `aterm_types::clt_mark::term_end`).

use aterm_spec::derive::*;

/// CltEpisode: the one record every asker of setup's Mac password shares (the window, which
/// hands its claim to its Mac Setup tab's program, a terminal `aterm init`'s automatic ask, and a
/// person's own retry), the processes that ask ending at any point, aterm's quit, and time
/// passing.
///
/// The record holds one line: none, an asker's claim (`asking`), or how the last attempt ended
/// (`unfinished`, `done`, `declined`). An `asking` line keeps the line it replaced, one deep, so
/// it holds two attempts at once: a person's retry and the attempt it took the episode over
/// from, which may still ask. An automatic claim goes only where the line lets an automatic ask
/// go now: no line, an ended attempt whose pause is over, or a claim whose asker no longer runs
/// (cut off) once its pause is, where the attempt it replaced does not still ask either; a
/// person's own retry claims whatever the line says, keeping the attempt that still asks where
/// the line's own was cut off over it. A claim carries its unfinished tries in a row, their
/// schedule and when the elevation last finished. Each attempt writes its own end in its own
/// place: the line's attempt, where the one it replaced still asks, leaves that one asking over
/// its end; the replaced one's end takes its place under the line's. An attempt that asked
/// nothing gives the line it replaced back, or, where that was dropped, what its claim carried
/// of it. The window hands its claim to its tab's program, the line's or the one it keeps, so the
/// window's restart (an update's handoff, which the program outlives) cuts nothing off and the
/// attempt stays the window's to end. A claim whose asker is gone with no end written reads as
/// an unfinished try, due again by itself as the try it was and adding none, never as a decline:
/// only a person's no is a decline, which holds automatic asks for its long pause and is lifted
/// by their yes. aterm's quit, noted by the close of its last window while the program asks,
/// writes no end for what its hangup ends the program with, and the program's own status (a
/// finish) is still its end. Every pause is read at the reader's clock; `Tick` is enough time
/// passing for every pause.
///
/// Not walked: the window's own Install… (an explicit claim it hands to its program, as a
/// person's retry), a second retry at once, and so three attempts at once, where the record,
/// one deep, drops the oldest, whose end then writes only a finish; the count of declines in a
/// row, which the walk passes in one `Tick`; and of the exits the window's decision
/// reads, only one for each of its ends (a finish, a failure, a stop, a tab closed with no status
/// seen, a program that never ran) and, at a quit, the hangup's three (a signal, no status, the
/// stop's code), the rest bound by `tab_end`'s own test; and an update's carry of an earlier
/// build's live tab (`clt_mark::carry`): until the carry names it, that tab's program asks under
/// a historical `opened` line, a format with no state here, and the carry's own test holds it
/// (written over such a line or none only, the same claim given back after, also from under a
/// person's retry, and every other line left as found).
///
/// Variables: `rec` is the line (0 none, 1 asking, 2 unfinished, 3 done, 4 declined); `held` is
/// whose attempt an asking line holds (1 the window's, 2 a terminal `aterm init`'s automatic one,
/// 3 a person's own), `handed` whether it names the window's tab's program, `live` whether the
/// asker it names runs; `win` is the window's attempt (0 none, 1 the window asks, 2 its tab's
/// program asks, 3 its tab's program asks under no claim the window holds), `term` and `own`
/// the other two (0 none, 1 asking), `quitting` whether aterm's quit is noted; `tries` is the
/// unfinished tries in a row the line keeps, `fin` whether it says the elevation ever finished;
/// `prior`, `pheld`, `phanded`, `plive` the replaced line an asking line keeps; `due`, `pdue` and
/// `fdue` whether the line's, the replaced line's and the last finish's pause is over, `sd` and
/// `psd` whether the schedule of the tries an asking line (and the asking line it replaced)
/// carries is. The rest are what the conformance test reads off the real record's behaviour:
/// `said` (the last reading said declined), `ends` (unfinished ends written since the last
/// finish or decline), `ever` (an attempt finished), `early` (an automatic claim through a held
/// decline), `cutend` (an end written for what aterm's quit ended its tab's program with), and
/// `cutno` (a decline written for a terminal `aterm init` its hangup or a `SIGTERM` stopped).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn clt_episode_model() -> Model {
    aterm_spec::ty_model! {
        CltEpisode {
            const Buggy = 0;

            var rec = 0;
            var held = 0;
            var handed = 0;
            var live = 0;
            var win = 0;
            var term = 0;
            var own = 0;
            var quitting = 0;
            var tries = 0;
            var fin = 0;
            var prior = 0;
            var pheld = 0;
            var phanded = 0;
            var plive = 0;
            var due = 1;
            var pdue = 1;
            var fdue = 1;
            var sd = 1;
            var psd = 1;
            var said = 0;
            var ends = 0;
            var ever = 0;
            var early = 0;
            var cutend = 0;
            var cutno = 0;

            // An automatic claim, the window's or a terminal `aterm init`'s: only where the line
            // lets an automatic ask go now (a decline's passed hold reads as the finish before
            // it, and a cut-off line over an attempt that still asks reads asking), keeping the
            // line it replaces and the tries it carries.
            action ClaimW when (win == 0 && quitting == 0 && (rec == 0 || (due == 1 &&
                (rec > 1 || (live == 0 && ((prior <= 0 || prior > 1) || plive == 0))) &&
                (rec <= 3 || fin == 0 || fdue == 1)))) {
                rec = 1;
                held = 1;
                handed = 0;
                live = 1;
                tries = if (rec == 1 || rec == 2) { tries } else { 0 };
                prior = rec;
                pheld = held;
                phanded = handed;
                plive = live;
                pdue = due;
                psd = sd;
                due = if ((rec == 1 || rec == 2) && tries > 0) { 0 } else { 1 };
                sd = if ((rec == 1 || rec == 2) && tries > 0) { due } else { 1 };
                win = 1;
                said = 0;
            }
            action ClaimT when (term == 0 && (rec == 0 || (due == 1 &&
                (rec > 1 || (live == 0 && ((prior <= 0 || prior > 1) || plive == 0))) &&
                (rec <= 3 || fin == 0 || fdue == 1)))) {
                rec = 1;
                held = 2;
                handed = 0;
                live = 1;
                tries = if (rec == 1 || rec == 2) { tries } else { 0 };
                prior = rec;
                pheld = held;
                phanded = handed;
                plive = live;
                pdue = due;
                psd = sd;
                due = if ((rec == 1 || rec == 2) && tries > 0) { 0 } else { 1 };
                sd = if ((rec == 1 || rec == 2) && tries > 0) { due } else { 1 };
                term = 1;
                said = 0;
            }
            // A person's own retry (a typed `aterm init`): their yes, whatever the line says, a
            // running asker's claim included. Over a cut-off line whose replaced attempt still
            // asks, it keeps that attempt, which the gone one adds nothing to.
            action ExplicitO when (own == 0) {
                rec = 1;
                held = 3;
                handed = 0;
                live = 1;
                tries = if (rec == 1 || rec == 2) { tries } else { 0 };
                prior = if (rec == 1 && live == 0 && prior == 1 && plive == 1) { prior } else {
                    rec
                };
                pheld = if (rec == 1 && live == 0 && prior == 1 && plive == 1) { pheld } else {
                    held
                };
                phanded = if (rec == 1 && live == 0 && prior == 1 && plive == 1) { phanded } else {
                    handed
                };
                plive = if (rec == 1 && live == 0 && prior == 1 && plive == 1) { plive } else {
                    live
                };
                pdue = if (rec == 1 && live == 0 && prior == 1 && plive == 1) { pdue } else {
                    due
                };
                psd = if (rec == 1 && live == 0 && prior == 1 && plive == 1) { psd } else { sd };
                due = if ((rec == 1 || rec == 2) && tries > 0) { 0 } else { 1 };
                sd = if ((rec == 1 || rec == 2) && tries > 0) {
                    if (rec == 1 && live == 0 && prior == 1 && plive == 1) { pdue } else { due }
                } else {
                    1
                };
                own = 1;
                said = 0;
            }
            // The window hands its claim to its tab's program, which asks: the line, or the line
            // it keeps under a person's retry, names the program from now.
            action Hand when (win == 1 && ((rec == 1 && held == 1 && handed == 0) ||
                (prior == 1 && pheld == 1 && phanded == 0))) {
                handed = if (rec == 1 && held == 1) { 1 } else { handed };
                phanded = if (rec == 1 && held == 1) { phanded } else { 1 };
                win = 2;
                said = 0;
            }

            // The window's attempt ends, as the window writes it for its tab's program (its
            // decision, `tab_end`): the program's own status, at a quit too, but a stop of its own
            // (a Ctrl-C, System State's Stop) or its tab closed only while aterm runs on, since
            // a quit's hangup ends the program as such a stop does. The attempt held as the line
            // or kept under it writes its end in its own place, and the other attempt the record
            // holds, where it still asks, asks on over it.
            action DoneW when (win > 0 && win <= 2 && ((rec == 1 && held == 1) || (prior == 1 &&
                pheld == 1))) {
                rec = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    1
                } else {
                    3
                };
                held = if (held == 1) {
                    if (prior == 1 && plive == 1) {
                        pheld
                    } else {
                        0
                    }
                } else if (live == 1) {
                    held
                } else {
                    0
                };
                handed = if (held == 1) {
                    if (prior == 1 && plive == 1) {
                        phanded
                    } else {
                        0
                    }
                } else if (live == 1) {
                    handed
                } else {
                    0
                };
                live = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    1
                } else {
                    0
                };
                tries = 0;
                fin = 1;
                fdue = 0;
                due = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    1
                } else {
                    0
                };
                sd = 1;
                prior = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    3
                } else {
                    0
                };
                pheld = 0;
                phanded = 0;
                plive = 0;
                pdue = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    0
                } else {
                    1
                };
                psd = 1;
                ends = 0;
                ever = 1;
                said = 0;
                win = 0;
            }
            action UnfinishedW when (win > 0 && win <= 2 && ((rec == 1 && held == 1) || (prior == 1
                && pheld == 1)) && tries == 0) {
                rec = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    1
                } else {
                    2
                };
                held = if (held == 1) {
                    if (prior == 1 && plive == 1) {
                        pheld
                    } else {
                        0
                    }
                } else if (live == 1) {
                    held
                } else {
                    0
                };
                handed = if (held == 1) {
                    if (prior == 1 && plive == 1) {
                        phanded
                    } else {
                        0
                    }
                } else if (live == 1) {
                    handed
                } else {
                    0
                };
                live = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    1
                } else {
                    0
                };
                tries = tries + 1;
                due = 0;
                sd = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    0
                } else {
                    1
                };
                prior = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    2
                } else {
                    0
                };
                pheld = 0;
                phanded = 0;
                plive = 0;
                pdue = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    0
                } else {
                    1
                };
                psd = 1;
                ends = ends + 1;
                said = 0;
                win = 0;
            }
            action DeclinedW when (win > 0 && win <= 2 && ((rec == 1 && held == 1) || (prior == 1
                && pheld == 1)) && quitting == 0) {
                rec = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    1
                } else {
                    4
                };
                held = if (held == 1) {
                    if (prior == 1 && plive == 1) {
                        pheld
                    } else {
                        0
                    }
                } else if (live == 1) {
                    held
                } else {
                    0
                };
                handed = if (held == 1) {
                    if (prior == 1 && plive == 1) {
                        phanded
                    } else {
                        0
                    }
                } else if (live == 1) {
                    handed
                } else {
                    0
                };
                live = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    1
                } else {
                    0
                };
                tries = 0;
                due = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    1
                } else {
                    0
                };
                sd = 1;
                prior = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    4
                } else {
                    0
                };
                pheld = 0;
                phanded = 0;
                plive = 0;
                pdue = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    0
                } else {
                    1
                };
                psd = 1;
                ends = 0;
                said = 0;
                win = 0;
            }
            action ClosedW when (win > 0 && win <= 2 && ((rec == 1 && held == 1) || (prior == 1 &&
                pheld == 1)) && quitting == 0) {
                rec = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    1
                } else {
                    4
                };
                held = if (held == 1) {
                    if (prior == 1 && plive == 1) {
                        pheld
                    } else {
                        0
                    }
                } else if (live == 1) {
                    held
                } else {
                    0
                };
                handed = if (held == 1) {
                    if (prior == 1 && plive == 1) {
                        phanded
                    } else {
                        0
                    }
                } else if (live == 1) {
                    handed
                } else {
                    0
                };
                live = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    1
                } else {
                    0
                };
                tries = 0;
                due = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    1
                } else {
                    0
                };
                sd = 1;
                prior = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    4
                } else {
                    0
                };
                pheld = 0;
                phanded = 0;
                plive = 0;
                pdue = if ((held == 1 && prior == 1 && plive == 1) || ((held <= 0 || held > 1) && live == 1)) {
                    0
                } else {
                    1
                };
                psd = 1;
                ends = 0;
                said = 0;
                win = 0;
            }
            action ReleasedW when (win > 0 && win <= 2 && ((rec == 1 && held == 1) || (prior == 1
                && pheld == 1))) {
                rec = if (held == 1) {
                    if (prior > 0) {
                        prior
                    } else if (tries > 0) {
                        2
                    } else if (fin == 1) {
                        3
                    } else {
                        0
                    }
                } else if (live == 1) {
                    1
                } else if (tries > 0) {
                    2
                } else if (fin == 1) {
                    3
                } else {
                    0
                };
                held = if (held == 1) { pheld } else if (live == 1) { held } else { 0 };
                handed = if (held == 1) { phanded } else if (live == 1) { handed } else { 0 };
                live = if (held == 1) { plive } else if (live == 1) { 1 } else { 0 };
                due = if (held == 1) {
                    if (prior > 0) {
                        pdue
                    } else if (tries > 0) {
                        sd
                    } else if (fin == 1) {
                        fdue
                    } else {
                        1
                    }
                } else if (live == 1) {
                    due
                } else if (tries > 0) {
                    psd
                } else if (fin == 1) {
                    fdue
                } else {
                    1
                };
                sd = if (held == 1) {
                    if (prior == 1) {
                        psd
                    } else {
                        1
                    }
                } else if (live == 1 && tries > 0) {
                    psd
                } else {
                    1
                };
                prior = if (held == 1) {
                    0
                } else if (live == 1) {
                    if (tries > 0) {
                        2
                    } else if (fin == 1) {
                        3
                    } else {
                        0
                    }
                } else {
                    0
                };
                pheld = 0;
                phanded = 0;
                plive = 0;
                pdue = if (held == 1) {
                    1
                } else if (live == 1) {
                    if (tries > 0) {
                        psd
                    } else if (fin == 1) {
                        fdue
                    } else {
                        1
                    }
                } else {
                    1
                };
                psd = 1;
                said = 0;
                win = 0;
            }

            // A terminal `aterm init`'s automatic attempt ends, and its process with it, as its
            // run reads its own end (its decision, `term_end`): a finish read back, an unfinished
            // try, a person's key at its terminal (Ctrl-C, Ctrl-\), which is their decline, or a
            // `sudo` that never started.
            action DoneT when (term == 1 && ((rec == 1 && held == 2) || (prior == 1 && pheld ==
                2))) {
                rec = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    1
                } else {
                    3
                };
                held = if (held == 2) {
                    if (prior == 1 && plive == 1) {
                        pheld
                    } else {
                        0
                    }
                } else if (live == 1) {
                    held
                } else {
                    0
                };
                handed = if (held == 2) {
                    if (prior == 1 && plive == 1) {
                        phanded
                    } else {
                        0
                    }
                } else if (live == 1) {
                    handed
                } else {
                    0
                };
                live = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    1
                } else {
                    0
                };
                tries = 0;
                fin = 1;
                fdue = 0;
                due = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    1
                } else {
                    0
                };
                sd = 1;
                prior = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    3
                } else {
                    0
                };
                pheld = 0;
                phanded = 0;
                plive = 0;
                pdue = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    0
                } else {
                    1
                };
                psd = 1;
                ends = 0;
                ever = 1;
                said = 0;
                term = 0;
            }
            action UnfinishedT when (term == 1 && ((rec == 1 && held == 2) || (prior == 1 && pheld
                == 2)) && tries == 0) {
                rec = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    1
                } else {
                    2
                };
                held = if (held == 2) {
                    if (prior == 1 && plive == 1) {
                        pheld
                    } else {
                        0
                    }
                } else if (live == 1) {
                    held
                } else {
                    0
                };
                handed = if (held == 2) {
                    if (prior == 1 && plive == 1) {
                        phanded
                    } else {
                        0
                    }
                } else if (live == 1) {
                    handed
                } else {
                    0
                };
                live = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    1
                } else {
                    0
                };
                tries = tries + 1;
                due = 0;
                sd = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    0
                } else {
                    1
                };
                prior = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    2
                } else {
                    0
                };
                pheld = 0;
                phanded = 0;
                plive = 0;
                pdue = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    0
                } else {
                    1
                };
                psd = 1;
                ends = ends + 1;
                said = 0;
                term = 0;
            }
            // The terminal's hangup (it closed, the aterm it ran in quit, a logout ended its
            // login) or a `SIGTERM` (a logout, launchd, an update's handoff) stops it while it
            // asks: nobody at that terminal said no, so the run writes the unfinished try it
            // can, asked again by itself, never a decline.
            action InterruptedT when (term == 1 && ((rec == 1 && held == 2) || (prior == 1 && pheld
                == 2)) && tries == 0) {
                rec = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    1
                } else {
                    2
                };
                held = if (held == 2) {
                    if (prior == 1 && plive == 1) {
                        pheld
                    } else {
                        0
                    }
                } else if (live == 1) {
                    held
                } else {
                    0
                };
                handed = if (held == 2) {
                    if (prior == 1 && plive == 1) {
                        phanded
                    } else {
                        0
                    }
                } else if (live == 1) {
                    handed
                } else {
                    0
                };
                live = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    1
                } else {
                    0
                };
                tries = tries + 1;
                due = 0;
                sd = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    0
                } else {
                    1
                };
                prior = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    2
                } else {
                    0
                };
                pheld = 0;
                phanded = 0;
                plive = 0;
                pdue = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    0
                } else {
                    1
                };
                psd = 1;
                ends = ends + 1;
                said = 0;
                term = 0;
            }
            action DeclinedT when (term == 1 && ((rec == 1 && held == 2) || (prior == 1 && pheld ==
                2))) {
                rec = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    1
                } else {
                    4
                };
                held = if (held == 2) {
                    if (prior == 1 && plive == 1) {
                        pheld
                    } else {
                        0
                    }
                } else if (live == 1) {
                    held
                } else {
                    0
                };
                handed = if (held == 2) {
                    if (prior == 1 && plive == 1) {
                        phanded
                    } else {
                        0
                    }
                } else if (live == 1) {
                    handed
                } else {
                    0
                };
                live = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    1
                } else {
                    0
                };
                tries = 0;
                due = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    1
                } else {
                    0
                };
                sd = 1;
                prior = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    4
                } else {
                    0
                };
                pheld = 0;
                phanded = 0;
                plive = 0;
                pdue = if ((held == 2 && prior == 1 && plive == 1) || ((held <= 1 || held > 2) && live == 1)) {
                    0
                } else {
                    1
                };
                psd = 1;
                ends = 0;
                said = 0;
                term = 0;
            }
            action ReleasedT when (term == 1 && ((rec == 1 && held == 2) || (prior == 1 && pheld ==
                2))) {
                rec = if (held == 2) {
                    if (prior > 0) {
                        prior
                    } else if (tries > 0) {
                        2
                    } else if (fin == 1) {
                        3
                    } else {
                        0
                    }
                } else if (live == 1) {
                    1
                } else if (tries > 0) {
                    2
                } else if (fin == 1) {
                    3
                } else {
                    0
                };
                held = if (held == 2) { pheld } else if (live == 1) { held } else { 0 };
                handed = if (held == 2) { phanded } else if (live == 1) { handed } else { 0 };
                live = if (held == 2) { plive } else if (live == 1) { 1 } else { 0 };
                due = if (held == 2) {
                    if (prior > 0) {
                        pdue
                    } else if (tries > 0) {
                        sd
                    } else if (fin == 1) {
                        fdue
                    } else {
                        1
                    }
                } else if (live == 1) {
                    due
                } else if (tries > 0) {
                    psd
                } else if (fin == 1) {
                    fdue
                } else {
                    1
                };
                sd = if (held == 2) {
                    if (prior == 1) {
                        psd
                    } else {
                        1
                    }
                } else if (live == 1 && tries > 0) {
                    psd
                } else {
                    1
                };
                prior = if (held == 2) {
                    0
                } else if (live == 1) {
                    if (tries > 0) {
                        2
                    } else if (fin == 1) {
                        3
                    } else {
                        0
                    }
                } else {
                    0
                };
                pheld = 0;
                phanded = 0;
                plive = 0;
                pdue = if (held == 2) {
                    1
                } else if (live == 1) {
                    if (tries > 0) {
                        psd
                    } else if (fin == 1) {
                        fdue
                    } else {
                        1
                    }
                } else {
                    1
                };
                psd = 1;
                said = 0;
                term = 0;
            }

            // A person's own attempt ends, and its process with it: nothing claims over it while
            // it asks, so the line holds it.
            action DoneO when (own == 1 && rec == 1 && held == 3) {
                rec = if ((held == 3 && prior == 1 && plive == 1) || ((held <= 2 || held > 3) && live == 1)) {
                    1
                } else {
                    3
                };
                held = if (held == 3) {
                    if (prior == 1 && plive == 1) {
                        pheld
                    } else {
                        0
                    }
                } else if (live == 1) {
                    held
                } else {
                    0
                };
                handed = if (held == 3) {
                    if (prior == 1 && plive == 1) {
                        phanded
                    } else {
                        0
                    }
                } else if (live == 1) {
                    handed
                } else {
                    0
                };
                live = if ((held == 3 && prior == 1 && plive == 1) || ((held <= 2 || held > 3) && live == 1)) {
                    1
                } else {
                    0
                };
                tries = 0;
                fin = 1;
                fdue = 0;
                due = if ((held == 3 && prior == 1 && plive == 1) || ((held <= 2 || held > 3) && live == 1)) {
                    1
                } else {
                    0
                };
                sd = 1;
                prior = if ((held == 3 && prior == 1 && plive == 1) || ((held <= 2 || held > 3) && live == 1)) {
                    3
                } else {
                    0
                };
                pheld = 0;
                phanded = 0;
                plive = 0;
                pdue = if ((held == 3 && prior == 1 && plive == 1) || ((held <= 2 || held > 3) && live == 1)) {
                    0
                } else {
                    1
                };
                psd = 1;
                ends = 0;
                ever = 1;
                said = 0;
                own = 0;
            }
            action UnfinishedO when (own == 1 && rec == 1 && held == 3 && tries == 0) {
                rec = if ((held == 3 && prior == 1 && plive == 1) || ((held <= 2 || held > 3) && live == 1)) {
                    1
                } else {
                    2
                };
                held = if (held == 3) {
                    if (prior == 1 && plive == 1) {
                        pheld
                    } else {
                        0
                    }
                } else if (live == 1) {
                    held
                } else {
                    0
                };
                handed = if (held == 3) {
                    if (prior == 1 && plive == 1) {
                        phanded
                    } else {
                        0
                    }
                } else if (live == 1) {
                    handed
                } else {
                    0
                };
                live = if ((held == 3 && prior == 1 && plive == 1) || ((held <= 2 || held > 3) && live == 1)) {
                    1
                } else {
                    0
                };
                tries = tries + 1;
                due = 0;
                sd = if ((held == 3 && prior == 1 && plive == 1) || ((held <= 2 || held > 3) && live == 1)) {
                    0
                } else {
                    1
                };
                prior = if ((held == 3 && prior == 1 && plive == 1) || ((held <= 2 || held > 3) && live == 1)) {
                    2
                } else {
                    0
                };
                pheld = 0;
                phanded = 0;
                plive = 0;
                pdue = if ((held == 3 && prior == 1 && plive == 1) || ((held <= 2 || held > 3) && live == 1)) {
                    0
                } else {
                    1
                };
                psd = 1;
                ends = ends + 1;
                said = 0;
                own = 0;
            }
            action DeclinedO when (own == 1 && rec == 1 && held == 3) {
                rec = if ((held == 3 && prior == 1 && plive == 1) || ((held <= 2 || held > 3) && live == 1)) {
                    1
                } else {
                    4
                };
                held = if (held == 3) {
                    if (prior == 1 && plive == 1) {
                        pheld
                    } else {
                        0
                    }
                } else if (live == 1) {
                    held
                } else {
                    0
                };
                handed = if (held == 3) {
                    if (prior == 1 && plive == 1) {
                        phanded
                    } else {
                        0
                    }
                } else if (live == 1) {
                    handed
                } else {
                    0
                };
                live = if ((held == 3 && prior == 1 && plive == 1) || ((held <= 2 || held > 3) && live == 1)) {
                    1
                } else {
                    0
                };
                tries = 0;
                due = if ((held == 3 && prior == 1 && plive == 1) || ((held <= 2 || held > 3) && live == 1)) {
                    1
                } else {
                    0
                };
                sd = 1;
                prior = if ((held == 3 && prior == 1 && plive == 1) || ((held <= 2 || held > 3) && live == 1)) {
                    4
                } else {
                    0
                };
                pheld = 0;
                phanded = 0;
                plive = 0;
                pdue = if ((held == 3 && prior == 1 && plive == 1) || ((held <= 2 || held > 3) && live == 1)) {
                    0
                } else {
                    1
                };
                psd = 1;
                ends = 0;
                said = 0;
                own = 0;
            }
            action ReleasedO when (own == 1 && rec == 1 && held == 3) {
                rec = if (held == 3) {
                    if (prior > 0) {
                        prior
                    } else if (tries > 0) {
                        2
                    } else if (fin == 1) {
                        3
                    } else {
                        0
                    }
                } else if (live == 1) {
                    1
                } else if (tries > 0) {
                    2
                } else if (fin == 1) {
                    3
                } else {
                    0
                };
                held = if (held == 3) { pheld } else if (live == 1) { held } else { 0 };
                handed = if (held == 3) { phanded } else if (live == 1) { handed } else { 0 };
                live = if (held == 3) { plive } else if (live == 1) { 1 } else { 0 };
                due = if (held == 3) {
                    if (prior > 0) {
                        pdue
                    } else if (tries > 0) {
                        sd
                    } else if (fin == 1) {
                        fdue
                    } else {
                        1
                    }
                } else if (live == 1) {
                    due
                } else if (tries > 0) {
                    psd
                } else if (fin == 1) {
                    fdue
                } else {
                    1
                };
                sd = if (held == 3) {
                    if (prior == 1) {
                        psd
                    } else {
                        1
                    }
                } else if (live == 1 && tries > 0) {
                    psd
                } else {
                    1
                };
                prior = if (held == 3) {
                    0
                } else if (live == 1) {
                    if (tries > 0) {
                        2
                    } else if (fin == 1) {
                        3
                    } else {
                        0
                    }
                } else {
                    0
                };
                pheld = 0;
                phanded = 0;
                plive = 0;
                pdue = if (held == 3) {
                    1
                } else if (live == 1) {
                    if (tries > 0) {
                        psd
                    } else if (fin == 1) {
                        fdue
                    } else {
                        1
                    }
                } else {
                    1
                };
                psd = 1;
                said = 0;
                own = 0;
            }

            // The environment. An update's handoff replaces the window's process, and its tab's
            // program, which the next window adopts, asks on.
            action WindowGoes when (quitting == 0) {
                live = if (rec == 1 && held == 1 && handed == 0) { 0 } else { live };
                plive = if (prior == 1 && pheld == 1 && phanded == 0) { 0 } else { plive };
                said = 0;
                win = if (win == 1) { 0 } else { win };
            }
            // The close of aterm's last window notes its quit while the tab's program asks: the
            // end the window reads from now is decided as a quit's.
            action QuitNoted when (win == 2 && quitting == 0) {
                quitting = 1;
                said = 0;
            }
            // aterm ends (a quit, a crash, a logout or a restart of the Mac): the window and its
            // tab's program end with it, with no end written.
            action AtermQuits {
                live = if (rec == 1 && held == 1) { 0 } else { live };
                plive = if (prior == 1 && pheld == 1) { 0 } else { plive };
                said = 0;
                win = 0;
                quitting = 0;
            }
            // The tab's program ends with no end written (killed).
            action ProgramGoes when (win == 2) {
                live = if (rec == 1 && held == 1 && handed == 1) { 0 } else { live };
                plive = if (prior == 1 && pheld == 1 && phanded == 1) { 0 } else { plive };
                said = 0;
                win = 0;
            }
            action TermGoes when (term == 1) {
                live = if (rec == 1 && held == 2) { 0 } else { live };
                plive = if (prior == 1 && pheld == 2) { 0 } else { plive };
                said = 0;
                term = 0;
            }
            action OwnGoes when (own == 1) {
                live = if (rec == 1 && held == 3) { 0 } else { live };
                plive = if (prior == 1 && pheld == 3) { 0 } else { plive };
                said = 0;
                own = 0;
            }
            // Time passes past every pause: the longest, a decline's, included.
            action Tick when (due + pdue + fdue + sd + psd <= 4) {
                due = 1;
                pdue = 1;
                fdue = 1;
                sd = 1;
                psd = 1;
                said = 0;
            }

            // A reading of the record: declined only of a person's decline whose hold is ahead.
            action Read {
                said = if (rec == 4 && due == 0) { 1 } else { 0 };
            }
            // A person's yes to setup (Set up now) lifts their decline: back to the last finish,
            // or to no line.
            action Lift when (rec == 4) {
                rec = if (fin == 1) { 3 } else { 0 };
                due = if (fin == 1) { fdue } else { 1 };
                said = 0;
            }

            // Each mutant fires where one defect would, and no further: a cut-off read as a
            // decline; an automatic claim beside a running asker, or through a held decline; a
            // hand that leaves the window named; a hand of the claim the line keeps refused (the
            // window then holds none, so nobody ends the line once it names the window again);
            // an end of the replaced attempt that writes over the line's own; a person's retry's
            // end that drops the running ask it replaced (the record no longer names it, so an
            // automatic claim goes beside it: the code before this model); a release that
            // counts a try; an end that forgets the elevation finished; the window's decision
            // reading aterm's quit as a decline, or dropping the program's own finish at a quit;
            // and a terminal's decision reading its hangup or a `SIGTERM` as a decline (any stop
            // it caught, as `init_clt::episode_end` had it).
            action CutOffReadsDeclined when (Buggy == 1 && rec == 1 && live == 0 &&
                ((prior <= 0 || prior > 1) || plive == 0)) {
                said = 1;
            }
            action ClaimBesideLive when (Buggy == 1 && term == 0 && rec == 1 && live == 1 &&
                held == 1) {
                held = 2;
                handed = 0;
                prior = 1;
                pheld = held;
                phanded = handed;
                plive = 1;
                pdue = due;
                psd = sd;
                due = if (tries > 0) { 0 } else { 1 };
                sd = if (tries > 0) { due } else { 1 };
                term = 1;
                said = 0;
            }
            action ClaimThroughDecline when (Buggy == 1 && term == 0 && rec == 4 && due == 0) {
                rec = 1;
                held = 2;
                handed = 0;
                live = 1;
                prior = 4;
                pheld = 0;
                phanded = 0;
                plive = 0;
                pdue = 0;
                due = 1;
                term = 1;
                said = 0;
                early = 1;
            }
            action HandLeavesWindow when (Buggy == 1 && win == 1 && rec == 1 && held == 1 &&
                handed == 0) {
                win = 2;
                said = 0;
            }
            action HandDropsClaim when (Buggy == 1 && win == 1 && rec == 1 && held == 3 &&
                prior == 1 && pheld == 1 && phanded == 0) {
                win = 3;
                said = 0;
            }
            action StaleEndWrites when (Buggy == 1 && win > 0 && win <= 2 && rec == 1 &&
                held == 3 && live == 1 && prior == 1 && pheld == 1 && tries == 0) {
                rec = 2;
                held = 0;
                handed = 0;
                live = 0;
                tries = tries + 1;
                prior = 0;
                pheld = 0;
                phanded = 0;
                plive = 0;
                due = 0;
                pdue = 1;
                sd = 1;
                psd = 1;
                said = 0;
                ends = ends + 1;
                win = 0;
            }
            action ReleaseCountsATry when (Buggy == 1 && win > 0 && win <= 2 && rec == 1 &&
                held == 1 && prior == 0 && tries == 0) {
                rec = 2;
                held = 0;
                handed = 0;
                live = 0;
                tries = tries + 1;
                due = 0;
                sd = 1;
                said = 0;
                win = 0;
            }
            action EndForgetsFinish when (Buggy == 1 && win > 0 && win <= 2 && rec == 1 &&
                held == 1 && fin == 1 && tries == 0 && ((prior <= 0 || prior > 1) || plive == 0)) {
                rec = 2;
                held = 0;
                handed = 0;
                live = 0;
                tries = tries + 1;
                fin = 0;
                prior = 0;
                pheld = 0;
                phanded = 0;
                plive = 0;
                due = 0;
                pdue = 1;
                fdue = 1;
                sd = 1;
                psd = 1;
                said = 0;
                ends = ends + 1;
                win = 0;
            }
            action RetryEndDropsAsk when (Buggy == 1 && own == 1 && rec == 1 && held == 3 &&
                prior == 1 && pheld == 1 && plive == 1) {
                rec = 3;
                held = 0;
                handed = 0;
                live = 0;
                tries = 0;
                fin = 1;
                prior = 0;
                pheld = 0;
                phanded = 0;
                plive = 0;
                due = 0;
                pdue = 1;
                fdue = 0;
                sd = 1;
                psd = 1;
                said = 0;
                ends = 0;
                ever = 1;
                own = 0;
            }
            action InterruptionDeclines when (Buggy == 1 && win == 2 && quitting == 1 &&
                rec == 1 && held == 1 && ((prior <= 0 || prior > 1) || plive == 0)) {
                rec = 4;
                held = 0;
                handed = 0;
                live = 0;
                tries = 0;
                prior = 0;
                pheld = 0;
                phanded = 0;
                plive = 0;
                due = 0;
                pdue = 1;
                sd = 1;
                psd = 1;
                said = 0;
                ends = 0;
                win = 0;
                quitting = 0;
                cutend = 1;
            }
            action HangupDeclinesT when (Buggy == 1 && term == 1 && rec == 1 && held == 2 &&
                ((prior <= 0 || prior > 1) || plive == 0)) {
                rec = 4;
                held = 0;
                handed = 0;
                live = 0;
                tries = 0;
                prior = 0;
                pheld = 0;
                phanded = 0;
                plive = 0;
                due = 0;
                pdue = 1;
                sd = 1;
                psd = 1;
                said = 0;
                ends = 0;
                term = 0;
                cutno = 1;
            }
            action QuitDropsFinish when (Buggy == 1 && win == 2 && quitting == 1 && rec == 1 &&
                held == 1 && handed == 1 && fin == 0) {
                live = 0;
                said = 0;
                ever = 1;
                win = 0;
            }

            // A reading says declined only of a person's decline: a claim whose asker is gone
            // with no end written (aterm quit, crashed or was killed, the Mac restarted) reads
            // as an unfinished try, asked again by itself.
            invariant OnlyADeclineReadsDeclined: said == 0 || rec == 4;
            // No automatic asker asks beside another: a claim goes only where no attempt the
            // record holds still asks.
            invariant OneAutomaticAsk: win == 0 || term == 0;
            // A person's decline holds automatic asks for its long pause.
            invariant ADeclineHolds: early == 0;
            // Every attempt that asks is one the record holds, as its line or the line that
            // keeps, so its end is the record's and an automatic claim sees it.
            invariant EveryAskIsNamed: (win == 0 || (rec == 1 && held == 1) ||
                (prior == 1 && pheld == 1)) && (term == 0 || (rec == 1 && held == 2) ||
                (prior == 1 && pheld == 2)) && (own == 0 || (rec == 1 && held == 3) ||
                (prior == 1 && pheld == 3));
            // Every attempt the record holds whose asker runs is one its asker's process will
            // end: nothing reads asking under a line nobody ends.
            invariant EveryNamedAskIsHeld: ((rec <= 0 || rec > 1) || live == 0 ||
                (held == 1 && win > 0 && win <= 2) || (held == 2 && term == 1) ||
                (held == 3 && own == 1)) && ((prior <= 0 || prior > 1) || plive == 0 ||
                (pheld == 1 && win > 0 && win <= 2) || (pheld == 2 && term == 1) ||
                (pheld == 3 && own == 1));
            // The tries in a row are the unfinished ends: an attempt that asked nothing, or one
            // cut off, adds none, and one that asked nothing loses none.
            invariant TriesCountUnfinishedEnds: tries == ends;
            // Once the elevation finished, the record says so whatever comes after it: the
            // program's own finish at a quit, and a release after a person's retry gave the line
            // back, included.
            invariant FinishOutlivesLaterTries: ever <= fin;
            // What aterm's quit ends its tab's program with writes no end: the attempt reads cut
            // off, never declined.
            invariant AnInterruptionWritesNoEnd: cutend == 0;
            // What stops a terminal `aterm init` from outside its terminal (its hangup, a
            // `SIGTERM`) is never a person's decline: the attempt is asked again by itself.
            invariant AHangupNeverDeclines: cutno == 0;
            bound EpisodeBox: tries <= 1 && ends <= 1;
        }
    }
}

/// Every ask the record names ends by itself: `[]<>` the record reads no attempt asking, with
/// each asker's finish weakly fair (once an attempt asks, it ends; the walk's other ends are
/// ways it may end too). An asker's attempt is always one the record holds and one its process
/// ends, so the record never reads asking for good. A hand of the claim a person's retry keeps
/// that the record refuses (`HandDropsClaim`, the code before this model) leaves the window
/// holding none: once the retry ends, the line names the window, which nobody ends, and reads
/// asking for the window's whole life, holding every automatic ask.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(clt_episode_model)]
pub fn clt_episode_liveness() -> Liveness {
    Liveness {
        name: "EveryAskEnds",
        goal: or_(
            or_(le(var("rec"), int(0)), gt(var("rec"), int(1))),
            and_(
                eq(var("live"), int(0)),
                or_(
                    or_(le(var("prior"), int(0)), gt(var("prior"), int(1))),
                    eq(var("plive"), int(0)),
                ),
            ),
        ),
        weak: vec!["DoneW", "DoneT", "DoneO"],
        strong: vec![],
        mutants: vec!["HandDropsClaim"],
    }
}
