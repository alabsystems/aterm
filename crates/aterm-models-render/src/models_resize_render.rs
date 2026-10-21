// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The resize ledger's render verdict: a writer/reader contract between the engine's resize
//! journal and the host's ledger, and the repaint nudge that repairs a displaced tab.

use aterm_spec::derive::{Liveness, Model};

/// Whether an application has drawn over content a resize displaced, judged the way `aterm-gui`'s
/// resize ledger judges it (`status render=`, the `resizes` verb, `EVENT <local> render
/// desync-risk`).
///
/// On the alternate screen a rows-only shrink with a non-blank bottom row demotes the top row,
/// and the alt grid keeps no history, so a grow that only appends leaves a flap's content one
/// row higher than the app painted it. An app repaints (ED 2) only when its SIGWINCH handler
/// reads a changed size, so a flap whose halves both land before the handler runs reads no
/// change, and its diff frames (absolute `CSI r;c H`, no clear) land on shifted content.
///
/// The engine's resize undo keeps the rows a shrink demotes (`st`) until anything is drawn; a
/// grow hands them back, so a flap with no output between its halves is an identity
/// (`QuietFlapIsIdentity`). Any output drops the undo: an app that drew at the smaller size has
/// read, or will read, the change and repaint; output that draws nothing (`Output`) leaves a
/// displaced net-zero run for the ledger to judge.
///
/// The engine is the writer: each resize's report names what it did to the rows (`demoted`,
/// `shift()`, the rows its undo `stashed` or handed back as `restored_top`), and its full-clear
/// and screen-replacement counters say when a displaced screen was repainted from scratch. The
/// ledger is the reader: it groups resizes into runs and calls a risk only for a net-zero run —
/// one whose geometry ended where it began, so the handler, whenever it ran, read no change. A
/// run that displaces rows stays open through app output inside the run gap (every step here
/// but `Evaluate`) for a resize that takes the last one back (with two geometries every next
/// resize does; the ledger closes a drawn-on run to any other, so a drag the app follows frame
/// by frame is not one long run): output is no evidence that the handler read the intermediate
/// size, and a handler that did and repaints from scratch shows as the clear that closes the run
/// (`Step` at a changed size). A run that moved nothing closes on a draw. Past the gap a run
/// closes unless it displaced rows the app has not drawn over (`silent`): that one stays open
/// for the next resize (`late`), so a grow the undo answers nets it to zero — the rows handed
/// back return to the run whose shrink stashed them (`rd - st`; the ledger's `credit`). A run
/// whose geometry stayed changed is the app's to repaint, and the ledger cannot see a repaint
/// that does not clear, so a draw on it is `unverified` (`verdict = 2`), never a risk and never
/// ok. The invariants hold the verdict and `displaced` against the ground truth (`disp`,
/// `drew`) both ways: a verdict only over displaced content the app drew on (`NoFalseVerdict`),
/// a net-zero one a risk unless a reader saw it short (`RiskIsFlagged`), displaced content drawn
/// on never ok (`NoSilentPass`), and `render=displaced` exactly while undrawn content is
/// displaced (`DisplacedIsShown`, `NoFalseDisplaced`).
///
/// * `geom`: the grid's rows (0 = the app's full height, 1 = one row fewer).
/// * `app_geom`: the rows the app's frame is laid out for, what its handler last read.
/// * `disp`: how many rows the content sits above where the app painted it. Engine truth,
///   bounded by `Cap`.
/// * `ro`/`rs`/`rd`/`rw`: the ledger's open run — whether the next resize joins it, the
///   geometry it started from, the rows it displaced so far, and whether the app drew while it
///   displaced rows.
/// * `late`: the open run outlived the run gap (a `silent` run left open for the next resize):
///   a draw now closes it.
/// * `led`: the displacement booked in closed net-zero runs that moved content up and not seen
///   healed (a down-moved run is not summed: its negative `rd` would cancel an up-moved one's,
///   and leaving it out keeps the space bounded); `lr`: the app drew on a closed, unhealed
///   net-zero run that moved content either way — the ledger judges any `displaced != 0`
///   (`Run::judged`), so a drawn down-moved one is a `desync-risk` too. The rig never forms a
///   down-moved net-zero run; the model forms one only through the over-approximated `Trim`
///   after a `Draw`.
/// * `lb`: a closed net-changed run that displaced rows, not seen healed; `br`: the app drew on
///   one, reported `unverified`.
/// * `blind`: ground truth: a reader came by (`Evaluate`) while the grid was still at the
///   changed size with content displaced, so the app has had time to read the change and owes
///   the repaint; a grow that brings the displacement to zero ends that episode. The ledger's
///   named blind spot is a flap whose halves more than the run gap separates, with a draw
///   between: two net-changed runs, `unverified` once drawn on, which `cast drift` audits
///   instead. A flap inside the gap is never excused, whatever was drawn between.
/// * `drew`: the app has drawn a non-clearing frame onto displaced content since its last full
///   repaint.
/// * `st`: demoted rows the engine's resize undo holds for the next grow.
/// * `d0`: `disp` when the open run opened (the identity's reference).
/// * `bot`: what the last grow left on the bottom row, until something draws (0: unknown, either
///   shrink is possible; 1: the blank it appended, which the next shrink trims; 2: the full row
///   the undo handed back, so the next shrink demotes again).
/// * `out`: output (drawn or not) landed inside the open run. It drops the undo without closing
///   the run, which is how a net-zero run can still displace rows.
/// * `evaluated`/`verdict`/`shown`: the reader's last answer — `verdict` 0 ok, 1
///   `render=desync-risk`, 2 `render=unverified`; `shown` 1 that some run is still `silent`
///   (`render=displaced` when nothing else is said).
/// * `ls`: `Buggy = 3` only — a `silent` run that mutant closed by time.
///
/// `Buggy = 1` is an append-only engine with a size-difference judge. The grow only appends, so
/// a quiet flap leaves the content one row up (`Demote, Grow` breaks `QuietFlapIsIdentity`); the
/// judge calls risk (and displaced) only while the grid's size differs from the app's, so it
/// misses that flap (`Demote, Grow, Step, Evaluate` breaks `RiskIsFlagged`, `NoSilentPass`,
/// `DisplacedIsShown`) and false-alarms on a trim-only shrink (`Trim, Evaluate` breaks
/// `NoFalseVerdict`, `NoFalseDisplaced`). The ratchet sweeps this one; every claim falls to it.
///
/// `Buggy = 2` assumes the repaint: a drawn-on net-changed run counts as repainted and answers
/// ok. `Demote, Draw, Evaluate` (a spinner tick on the moved rows, the size still changed)
/// breaks `NoSilentPass` alone.
///
/// `Buggy = 3` lets time close every run (no late run, no credit). A quiet flap only time split,
/// which the undo made an identity, keeps a `silent` half (`Demote, Evaluate, Grow, Evaluate`
/// breaks `NoFalseDisplaced`) and, drawn on, an `unverified` one (`Demote, Evaluate, Grow, Draw,
/// Evaluate` breaks `NoFalseVerdict`); over the whole space it also breaks `RiskIsFlagged` and
/// `DisplacedIsShown`, and only those four (`each_named_mutant_breaks_exactly_its_claims_*`).
///
/// `Buggy = 4` downgrades a net-zero risk to `unverified`: `Demote, Output, Grow, Step,
/// Evaluate` says `unverified` where the ledger must say `desync-risk`, which `NoSilentPass`
/// accepts; it breaks `RiskIsFlagged` alone.
///
/// `Buggy = 5` lets a draw close every run. A flap a spinner tick splits is two net-changed
/// runs, `unverified` at best (`Demote, Draw, Grow, Step, Evaluate`), and an earlier restored
/// flap a reader saw excuses no later one (`Demote, Evaluate, Grow, Demote, Draw, Grow, Step,
/// Evaluate`). It breaks `RiskIsFlagged` alone. The Tier-0 test replays `Buggy = 2` to `5` by
/// name and checks each breaks its claims and no other over the whole space.
///
/// The epoch judge is the ledger's second judge, read from the engine's debt instead of a run's
/// shape: the run judge cannot see a flap an app frame splits (two net-changed runs) nor an
/// undrawn residual. The engine counts what it owes (`Terminal::render_debt`, exact however far
/// the journal is overrun) and the ledger judges it as one epoch:
///
/// * `debt`: the engine counter, in rows the content sits above where the program painted it. A
///   demoting shrink adds one; a grow the undo serves gives back `st`; a changed-size read (the
///   full clear) zeroes it. It is the report sums, not the screen, so `DebtIsTheDisplacement`
///   (`debt == disp`) binds writer to reader.
/// * `eg`: the geometry before the first resize that made the debt (0 while none is owed): where
///   the program's handler last laid the frame out.
/// * `judged`: the presence classifier reads Claude Code, or this session has shown it clears
///   its screen when resized (`Classify`, once).
/// * `ev`: the epoch's word a second later — 0 ok, 1 `desync-risk`, 2 `unjudged`: rows are owed
///   and the geometry is back at `eg` (the handler can only read an unchanged size), accused
///   only when `judged`. While the geometry differs from `eg` the handler has a change to read
///   and repaints, so nothing is said.
///
/// The claims: a residual at a settled geometry is never ok (`EpochFlagsTheResidual`), nothing
/// is said when nothing is owed (`EpochNoFalseAlarm`), an unlearned program is never accused
/// (`OnlyAJudgedProgramIsAccused`: one that repaints with cursor addressing and no ED 2 is
/// indistinguishable from one that missed), and the counter is the displacement
/// (`DebtIsTheDisplacement`). Under `Buggy = 1` the debt is a counter no full clear resets and
/// the word is the size-difference one, ignoring the debt and the class. `Buggy = 6` accuses an
/// unlearned program, `7` forgets the residual, `8` answers whenever the geometry is at `eg`
/// whether or not anything is owed; each breaks its one claim and no other.
///
/// Assumed, not modeled: the app's changed-size repaint starts with ED 2 (the `Step` that
/// heals). An app that repaints every row without clearing is seen by no counter, so a net-zero
/// risk flagged before it stays flagged, and a net-changed run it repaints stays `unverified` —
/// hence neither risk nor ok. Abstracted: the column debt. The engine's debt also owes `cut`
/// (drawn columns a narrowing took off that the grow back left blank, S2R-F3), and a cut-only
/// epoch is judged and nudged as owed rows are. This model and its Tier-1 rig never change the
/// width, the rig projects the rows half only (`RenderDebt::rows`), and unit tests alone check
/// the cut path (`a_column_flap_that_cuts_drawn_cells_owes_them_until_a_full_clear` in
/// aterm-core, `a_column_flap_that_cut_cells_is_judged_until_a_repaint` in aterm-gui). The
/// 250 ms run gap is the host's environment bound: every step but `Evaluate` lands inside it.
/// Over-approximated: whether a shrink demotes or trims is free except right after a grow
/// (`bot`), and every draw may have filled or cleared the bottom row.
///
/// Tier-1: `aterm-gui/src/resize_ledger.rs` drives a real alternate-screen
/// `aterm_core::Terminal` and the real `ResizeLedger` through every schedule up to a bounded
/// depth, projects the screen, the undo, the app stand-in and the ledger's runs onto these
/// variables, and checks each real transition against `action_enabled`, the model's successor
/// and the invariants. Its negative controls: `Demote, Grow, Step, Evaluate` with the undo
/// dropped between the halves, where the real ledger flags it and the size-difference judge
/// answers ok; the tick-split and time-split flaps, which the real ledger judges and `Buggy = 5`
/// and `3` miss; and a tick at a still-changed size, where the real ledger says `unverified` and
/// the assumed-repaint judge answers ok.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn resize_render_model() -> Model {
    aterm_spec::ty_model! {
        ResizeRender {
            const Buggy = 0;
            const Cap = 2;
            var geom = 0;
            var app_geom = 0;
            var disp = 0;
            var ro = 0;
            var rs = 0;
            var rd = 0;
            var rw = 0;
            var late = 0;
            var led = 0;
            var lr = 0;
            var lb = 0;
            var br = 0;
            var blind = 0;
            var drew = 0;
            var evaluated = 0;
            var verdict = 0;
            var shown = 0;
            var ls = 0;
            var st = 0;
            var d0 = 0;
            var bot = 0;
            var out = 0;
            var debt = 0;
            var eg = 0;
            var judged = 0;
            var ev = 0;

            // Alt-screen shrink with a non-blank bottom row: the top row is demoted (the undo
            // keeps it), so everything moves up one row. It joins the open run, or opens one.
            action Demote when (geom == 0 && (bot == 0 || bot == 2) && disp <= Cap - 1) {
                rs = if (ro == 1) { rs } else { geom };
                rd = if (ro == 1) { rd + 1 } else { 1 };
                rw = if (ro == 1) { rw } else { 0 };
                d0 = if (ro == 1) { d0 } else { disp };
                out = if (ro == 1) { out } else { 0 };
                ro = 1;
                geom = 1;
                disp = disp + 1;
                st = st + 1;
                late = 0;
                eg = if (debt <= 0) { geom } else { eg };
                debt = if (debt <= Cap - 1) { debt + 1 } else { debt };
                evaluated = 0;
            }
            // Alt-screen shrink with a blank bottom row: the trailing blank is
            // trimmed and nothing moves.
            action Trim when (geom == 0 && bot <= 1) {
                rs = if (ro == 1) { rs } else { geom };
                rd = if (ro == 1) { rd } else { 0 };
                rw = if (ro == 1) { rw } else { 0 };
                d0 = if (ro == 1) { d0 } else { disp };
                out = if (ro == 1) { out } else { 0 };
                ro = 1;
                geom = 1;
                late = 0;
                eg = if (debt <= 0) { 0 } else { eg };
                evaluated = 0;
            }
            // Alt-screen grow: the undo hands its demoted rows back to the top, moving everything
            // back down; the rest is a blank row appended. The rows handed back are the open
            // run's own (`rd - st`: the ledger's credit). `Buggy = 1` only appends.
            action Grow when (geom == 1) {
                rs = if (ro == 1) { rs } else { geom };
                rd = if (ro == 1) {
                    if (Buggy == 1) { rd } else { rd - st }
                } else {
                    if (Buggy == 1) { 0 } else { 0 - st }
                };
                rw = if (ro == 1) { rw } else { 0 };
                d0 = if (ro == 1) { d0 } else { disp };
                out = if (ro == 1) { out } else { 0 };
                ro = 1;
                geom = 0;
                disp = if (Buggy == 1) { disp } else { disp - st };
                bot = if (Buggy == 1) { 1 } else { if (1 <= st) { 2 } else { 1 } };
                blind = if (Buggy == 1) {
                    if (disp <= 0) { 0 } else { blind }
                } else {
                    if (disp - st <= 0) { 0 } else { blind }
                };
                eg = if (Buggy == 1) {
                    if (debt <= 0) { geom } else { eg }
                } else {
                    if (debt - st <= 0) { 0 } else { if (debt <= 0) { geom } else { eg } }
                };
                debt = if (Buggy == 1) { debt } else { debt - st };
                st = 0;
                late = 0;
                evaluated = 0;
            }
            // The app's SIGWINCH handler, then its next frame (output, which drops the undo). At
            // a changed size, a full repaint (ED 2), which the ledger reads as a heal of every
            // run: the open run closes and nothing displaced is left. At an unchanged size, a
            // diff frame at absolute positions, the ledger's run exactly as `Draw` leaves it.
            action Step {
                app_geom = geom;
                disp = if (geom == app_geom) { disp } else { 0 };
                led = if (geom == app_geom) {
                    if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { led } else {
                        if (ro == 1 && geom == rs && 1 <= rd) { led + rd } else { led }
                    }
                } else {
                    0
                };
                lr = if (geom == app_geom) {
                    if (1 <= led) { 1 } else {
                        if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { lr } else {
                            if (ro == 1 && geom == rs && (1 <= rd || rd <= 0 - 1)) { 1 } else { lr }
                        }
                    }
                } else {
                    0
                };
                lb = if (geom == app_geom) {
                    if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { lb } else {
                        if (ro == 1 && geom + rs == 1 && 1 <= rd) { 1 } else { lb }
                    }
                } else {
                    0
                };
                br = if (geom == app_geom) {
                    if (lb == 1) { 1 } else {
                        if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { br } else {
                            if (ro == 1 && geom + rs == 1 && 1 <= rd) { 1 } else { br }
                        }
                    }
                } else {
                    0
                };
                blind = if (geom == app_geom) { blind } else { 0 };
                drew = if (geom == app_geom) {
                    if (1 <= disp) { 1 } else { drew }
                } else {
                    0
                };
                rw = if (geom == app_geom && ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1))) { 1 } else { 0 };
                out = if (geom == app_geom && ro == 1) { 1 } else { out };
                ro = if (geom == app_geom && ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1))) { ro } else { 0 };
                rs = if (geom == app_geom && ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1))) { rs } else { 0 };
                rd = if (geom == app_geom && ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1))) { rd } else { 0 };
                late = 0;
                ls = 0;
                debt = if (geom == app_geom) { debt } else { if (Buggy == 1) { debt } else { 0 } };
                eg = if (geom == app_geom) { eg } else { 0 };
                st = 0;
                bot = 0;
                evaluated = 0;
            }
            // An ordinary diff frame (a spinner tick), no handler involved. Inside the run gap
            // (`late == 0`) a run that displaces rows stays open and is drawn on (`rw`). A run
            // that moved nothing closes (a fresh baseline), as does one past the gap (`late`: the
            // app has had time to read the size); `Buggy = 5` closes every run on a draw. A
            // closed net-zero run is booked (`led`) and drawn on (`lr`); a net-changed one that
            // displaced rows likewise (`lb`, `br`). Any output drops the engine's resize undo.
            action Draw {
                led = if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { led } else {
                    if (ro == 1 && geom == rs && 1 <= rd) { led + rd } else { led }
                };
                lr = if (1 <= led) { 1 } else {
                    if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { lr } else {
                        if (ro == 1 && geom == rs && (1 <= rd || rd <= 0 - 1)) { 1 } else { lr }
                    }
                };
                lb = if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { lb } else {
                    if (ro == 1 && geom + rs == 1 && 1 <= rd) { 1 } else { lb }
                };
                br = if (lb == 1) { 1 } else {
                    if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { br } else {
                        if (ro == 1 && geom + rs == 1 && 1 <= rd) { 1 } else { br }
                    }
                };
                drew = if (1 <= disp) { 1 } else { drew };
                rw = if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { 1 } else { 0 };
                out = if (ro == 1) { 1 } else { out };
                ro = if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { ro } else { 0 };
                rs = if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { rs } else { 0 };
                rd = if ((Buggy <= 4 || 6 <= Buggy) && late == 0 && (1 <= rd || rd <= 0 - 1)) { rd } else { 0 };
                late = 0;
                ls = 0;
                st = 0;
                bot = 0;
                evaluated = 0;
            }
            // Output that draws nothing (a mode set, a query: the first bytes of a SIGWINCH
            // handler) drops the engine's resize undo, but the content did not move, so the
            // ledger's open run stays open and the next resize joins it.
            action Output {
                st = 0;
                out = if (ro == 1) { 1 } else { out };
                evaluated = 0;
            }
            // The ledger's verdict a second later, past the run gap, from what it booked and saw
            // drawn: `desync-risk` over a drawn-on net-zero run, else `unverified` over a
            // net-changed one, else ok. Time closes and books the open run unless it is `silent`
            // (it waits for the next resize, however late); `Buggy = 3` closes that one too,
            // leaving it `silent` (`ls`). Time does not drop the engine's undo: nothing was drawn.
            action Evaluate {
                evaluated = 1;
                ev = if (Buggy == 1) {
                    if (geom + app_geom == 1) { 1 } else { 0 }
                } else {
                    if (Buggy == 7) {
                        0
                    } else {
                        if (geom == eg && (1 <= debt || Buggy == 8)) {
                            if (judged == 1 || Buggy == 6) { 1 } else { 2 }
                        } else {
                            0
                        }
                    }
                };
                verdict = if (Buggy == 1) {
                    if (geom == app_geom) { 0 } else { 1 }
                } else {
                    if (lr == 1 || (ro == 1 && geom == rs && (1 <= rd || rd <= 0 - 1) && rw == 1)) {
                        if (Buggy == 4) { 2 } else { 1 }
                    } else {
                        if (Buggy == 2) { 0 } else {
                            if (br == 1 || (ro == 1 && geom + rs == 1 && 1 <= rd && rw == 1)) { 2 } else { 0 }
                        }
                    }
                };
                shown = if (Buggy == 1) {
                    if (geom == app_geom) { 0 } else { 1 }
                } else {
                    if (ls == 1 || (ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0)) { 1 } else { 0 }
                };
                ls = if (Buggy == 3 && ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0) { 1 } else { ls };
                lr = if ((Buggy <= 2 || 4 <= Buggy) && ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0) {
                    lr
                } else {
                    if (ro == 1 && geom == rs && (1 <= rd || rd <= 0 - 1) && rw == 1) { 1 } else { lr }
                };
                led = if ((Buggy <= 2 || 4 <= Buggy) && ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0) {
                    led
                } else {
                    if (ro == 1 && geom == rs && 1 <= rd) { led + rd } else { led }
                };
                lb = if ((Buggy <= 2 || 4 <= Buggy) && ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0) {
                    lb
                } else {
                    if (ro == 1 && geom + rs == 1 && 1 <= rd) { 1 } else { lb }
                };
                br = if ((Buggy <= 2 || 4 <= Buggy) && ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0) {
                    br
                } else {
                    if (ro == 1 && geom + rs == 1 && 1 <= rd && rw == 1) { 1 } else { br }
                };
                blind = if (1 <= disp && geom == 1) { 1 } else { blind };
                ro = if ((Buggy <= 2 || 4 <= Buggy) && ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0) { 1 } else { 0 };
                rs = if ((Buggy <= 2 || 4 <= Buggy) && ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0) { rs } else { 0 };
                rd = if ((Buggy <= 2 || 4 <= Buggy) && ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0) { rd } else { 0 };
                late = if ((Buggy <= 2 || 4 <= Buggy) && ro == 1 && (1 <= rd || rd <= 0 - 1) && rw == 0) { 1 } else { 0 };
                rw = 0;
            }

            // The class becomes known: the presence classifier reads Claude Code,
            // or the session clears its screen when resized. Once; the environment.
            action Classify when (judged == 0) {
                judged = 1;
                evaluated = 0;
            }

            // Excused only by the blind spot: a flap inside the run gap is
            // flagged whatever was drawn between its halves.
            invariant RiskIsFlagged: evaluated == 0 || disp <= 0 || drew == 0 || blind == 1 || verdict == 1;
            // Displaced content drawn on is never answered ok.
            invariant NoSilentPass: evaluated == 0 || disp <= 0 || drew == 0 || 1 <= verdict;
            invariant NoFalseVerdict: evaluated == 0 || verdict == 0 || (1 <= disp && drew == 1);
            // `render=displaced` names an undrawn displacement, and only one.
            invariant DisplacedIsShown: evaluated == 0 || disp <= 0 || drew == 1 || shown == 1;
            invariant NoFalseDisplaced: evaluated == 0 || shown == 0 || 1 <= disp;
            // A run that opened at the app's full height and is back there,
            // with no output inside it, left the content exactly where the
            // run found it.
            invariant QuietFlapIsIdentity: ro == 0 || rs == 1 || geom == 1 || out == 1 || disp == d0;
            // The epoch judge. A residual whose geometry is back at the epoch's
            // start (`geom + eg == 1` is "differs": both are 0 or 1) is never
            // answered ok, whether or not the program has drawn.
            invariant EpochFlagsTheResidual: evaluated == 0 || disp <= 0 || geom + eg == 1 || 1 <= ev;
            invariant EpochNoFalseAlarm: evaluated == 0 || ev == 0 || 1 <= disp;
            invariant OnlyAJudgedProgramIsAccused: evaluated == 0 || judged == 1 || ev == 0 || 2 <= ev;
            invariant DebtIsTheDisplacement: debt == disp;
            bound Bounded: disp <= Cap && led <= Cap && rd <= Cap && st <= Cap && debt <= Cap;
        }
    }
}

/// The repaint nudge: the host's closed-loop repair of a tab whose application will not repaint
/// by itself.
///
/// An application that repaints only when its SIGWINCH handler reads a changed size can be left
/// on a screen it no longer believes in (a flap whose halves it missed). The one input-free way
/// to make it repaint is the resize protocol itself, done so its handler reads each size: hold
/// the session one row short, wait for the app's answer, release, wait for the second answer.
/// Back-to-back it reads one size and repaints nothing; on a timer it guesses the app's longest
/// blocking read. The driver never guesses: the answer is the evidence, and a missing answer is
/// a bounded wait, not a wedge.
///
/// * `phase`: 0 idle, 1 armed (debt seen; waiting for a quiet moment), 2 short (the hold is
///   on), 3 restoring (the hold is off; the true size is back), 4 at rest (healed or given up,
///   said in the log).
/// * `held`: the session's target is one row short. The safety property: only ever set in phase
///   2, or after the driver was lost (`orphan`), and never longer than `Cap` ticks (the hard
///   deadline).
/// * `halt`: a standing hold (`hold=1`): nothing is started, a running hold is lifted
///   (`Release`) and the attempt judged by its answers like any other.
/// * `safe`: the moment is quiet (no output burst, no DEC 2026 frame open).
/// * `ack1`/`ack2`: the app answered the short / the true size.
/// * `moved`: a real resize left the engine somewhere the hold did not find it. A flap during
///   the hold (`Flap`) is not one: its passes put the PTY at the engine's size and the engine
///   back where it was, so an app that read neither half saw nothing change (`DropOnFlap` is
///   the defect). It comes back (`Return`) when that resize did not last.
/// * `superseded`: the attempt ended superseded (`Supersede`) and the epoch judge watches its
///   epoch: while the resize stands the app has a size of its own, and once the engine is back
///   where the hold found it the window owes the end (`Report`: the give-up while rows are owed;
///   `ReturnUnwatched` is the defect).
/// * `lost`: the driver's record was lost (`DriverVanishes`).
/// * `gaveup`: the owner hears of the end (the digest line). Only `debt` decides it, never the
///   answers counted: a spent wait on a tab that owes nothing ends in silence. Tier-1 binds it
///   to the code's `Act::GaveUp`.
///
/// The environment (`Debt`, `Quiet`, `Busy`, `App1`, `App2`, `AltLeft`, `Flap`, `Resize`,
/// `Return`) is never fair. The driver waits only `Lim` ticks for a quiet moment and for each
/// answer, so it ends with the app silent forever.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn repaint_nudge_model() -> Model {
    aterm_spec::ty_model! {
        RepaintNudge {
            const Buggy = 0;
            const Lim = 2;
            const MaxTries = 1;
            const Cap = 2;
            var phase = 0;
            var held = 0;
            var engine = 0;
            var debt = 0;
            var safe = 0;
            var ack1 = 0;
            var ack2 = 0;
            var tries = 0;
            var tick = 0;
            var healed = 0;
            var gaveup = 0;
            var halt = 0;
            var age = 0;
            var orphan = 0;
            var moved = 0;
            var superseded = 0;
            var lost = 0;

            // The environment.
            action Debt when (phase == 0 && debt == 0) { debt = 1; }
            action Quiet when (safe == 0) { safe = 1; }
            action Busy when (safe == 1) { safe = 0; }
            action App1 when (phase == 2 && ack1 == 0) { ack1 = 1; }
            action App2 when (phase == 3 && ack2 == 0) { ack2 = 1; }
            // The alternate screen left, or a full repaint landed by itself.
            action AltLeft when (debt == 1) { debt = 0; }
            // A halt (`hold=1`) stands or lifts.
            action Halt when (halt == 0) { halt = 1; }
            action Lift when (halt == 1) { halt = 0; }
            // Someone else sets the PTY while it is held: the passes of a flap
            // (the engine ends where the hold found it) or a cell-box sync.
            action Flap when (phase == 2 && held == 1) { held = 0; }
            // A real resize that lasts: the engine, and the PTY with it, ends
            // somewhere the hold did not find it.
            action Resize when (2 <= phase && phase <= 3 && moved == 0) { held = 0; moved = 1; }
            // That resize did not last: after the attempt it superseded, the
            // engine is back where the hold found it.
            action Return when (superseded == 1 && moved == 1) { moved = 0; }

            // The driver.
            action Arm when (phase == 0 && debt == 1) { phase = 1; tick = 0; }
            // Never blocks on the world: past Lim ticks it goes anyway.
            action Hold when (phase == 1 && halt == 0 && (safe == 1 || tick > Lim - 1)) {
                phase = if (debt == 1) { 2 } else { 4 };
                held = if (debt == 1) { 1 } else { 0 };
                tries = if (debt == 1) { if (tries > MaxTries) { tries } else { tries + 1 } } else { tries };
                tick = 0;
                age = 0;
                ack1 = 0;
                ack2 = 0;
            }
            action Release when (phase == 2 && (ack1 == 1 || tick > Lim - 1 || debt == 0 || halt == 1)) {
                phase = 3;
                held = 0;
                age = 0;
                tick = 0;
            }
            // The PTY is not the one the hold set: the hold is over with no
            // write (the PTY is the layout's), and the answer is waited for as
            // after a release.
            action Forget when (phase == 2 && held == 0) {
                phase = 3;
                age = 0;
                tick = 0;
            }
            action Finish when (phase == 3 && ack1 == 1 && ack2 == 1) {
                phase = 4;
                healed = 1;
                debt = 0;
            }
            action GiveUp when (phase == 3 && tick > Lim - 1 && (ack1 == 0 || ack2 == 0)
                                && (debt == 0 || MaxTries <= tries) && moved == 0) {
                phase = 4;
                gaveup = if (debt == 1) { 1 } else { 0 };
            }
            // Unanswered with the engine moved: the app was given a size it reads
            // as changed, and what it draws for it is the epoch judge's, which
            // watches the epoch.
            action Supersede when (phase == 3 && tick > Lim - 1 && (ack1 == 0 || ack2 == 0)
                                   && moved == 1) {
                phase = 4;
                superseded = 1;
            }
            // The epoch judge finds a superseded attempt's epoch back where the hold found it:
            // rows still owed are the give-up the attempt would be had the resize come back
            // inside its wait; none, and the end is silent.
            action Report when (superseded == 1 && moved == 0) {
                superseded = 0;
                gaveup = if (debt == 1) { 1 } else { gaveup };
            }
            action Tick when (1 <= phase && phase <= 3 && tick <= Lim - 1) { tick = tick + 1; }

            // The release cannot be lost. The driver's record is lost while the PTY is short (a
            // panic in the tick, a teardown race): the App's own map of held sessions (`orphan`)
            // still stands, and its hard deadline lets the PTY go with or without a driver.
            action DriverVanishes when (phase == 2) { phase = 4; orphan = 1; lost = 1; }
            action Age when (held == 1 && age <= Cap - 1) { age = age + 1; }
            action ReleaseByDeadline when (held == 1 && age > Cap - 1) {
                held = 0;
                age = 0;
                orphan = 0;
                phase = if (phase == 2) { 3 } else { phase };
            }

            // BUGGY: an exit path that forgets to lift the hold (a timeout that only
            // stops the machine): the tab is left one row short.
            action LoseTheHold when (Buggy == 1 && phase == 2) { phase = 4; }
            // BUGGY: healed on the first answer (the app has read one size only).
            action HealOnFirstAnswer when (Buggy == 1 && phase == 2 && ack1 == 1) {
                phase = 4;
                held = 0;
                healed = 1;
                debt = 0;
            }
            // BUGGY: the hold resizes the engine, not just the PTY.
            action HoldTheEngine when (Buggy == 1 && phase == 1 && debt == 1) {
                phase = 2;
                held = 1;
                engine = 1;
                tries = if (tries > MaxTries) { tries } else { tries + 1 };
                tick = 0;
                ack1 = 0;
                ack2 = 0;
            }
            // BUGGY: no hard deadline (a hold nobody lifts stays for the process's life).
            action NoCap when (Buggy == 1 && held == 1 && age == Cap) { age = Cap + 1; }
            // BUGGY: retried without a budget (an app that never answers is nudged forever).
            action RetryForever when (Buggy == 1 && phase == 3 && tick > Lim - 1
                                      && (ack1 == 0 || ack2 == 0) && debt == 1) {
                phase = 1;
                tick = 0;
            }
            // BUGGY: any write by someone else read as a real resize, so a flap the app never
            // saw ends the attempt at once: the one try spent, the tab still owing, nothing said.
            action DropOnFlap when (Buggy == 1 && phase == 2 && held == 0 && moved == 0) {
                phase = 4;
            }
            // BUGGY: nothing watches a superseded attempt's epoch, so a resize that comes back
            // after the wait leaves the tab displaced, its one try spent, nothing said.
            action ReturnUnwatched when (Buggy == 1 && superseded == 1 && moved == 1) {
                moved = 0;
                superseded = 0;
            }

            invariant HeldOnlyWhileShort: held == 0 || phase == 2 || orphan == 1;
            invariant AHeldPtyIsReleasedByTheDeadline: held == 0 || age <= Cap;
            invariant HealedNeedsBothAnswers: healed == 0 || (ack1 == 1 && ack2 == 1);
            invariant OneTryPerEpoch: tries <= MaxTries;
            invariant NudgeNeverTouchesTheEngine: engine == 0;
            // An attempt that ends on a tab still owing rows says so (a give-up, the owner's
            // digest line), unless the driver was lost or a real resize superseded it and the
            // epoch judge still watches its epoch: the resize stands, or it came back and the
            // judge owes the give-up (`Report`, weakly fair in `TheNudgeEnds`). A moved engine
            // alone excuses nothing.
            invariant AnUnrepairedEndIsReported: phase <= 3 || debt == 0 || healed == 1
                || gaveup == 1 || superseded == 1 || lost == 1;
        }
    }
}

/// The nudge ends, and its end is said: `[]<>(at rest with nothing owed to the
/// owner \/ nothing to repair)`, under weak fairness of the driver's and the
/// window's own steps and no fairness for the app. At rest with nothing owed:
/// no superseded attempt whose resize came back is left unreported (`Report`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(repaint_nudge_model)]
pub fn repaint_nudge_liveness() -> Liveness {
    use aterm_spec::derive::{and_, eq, int, or_, var};
    Liveness {
        name: "TheNudgeEnds",
        goal: or_(
            and_(
                eq(var("phase"), int(4)),
                or_(eq(var("superseded"), int(0)), eq(var("moved"), int(1))),
            ),
            or_(
                and_(eq(var("phase"), int(0)), eq(var("debt"), int(0))),
                eq(var("halt"), int(1)),
            ),
        ),
        // `Forget` needs none: a hold someone else ended is also let go by
        // `Release` once its wait is spent.
        weak: vec![
            "Arm",
            "Hold",
            "Release",
            "Finish",
            "GiveUp",
            "Supersede",
            "Report",
            "Tick",
        ],
        strong: vec![],
        mutants: vec!["RetryForever"],
    }
}
