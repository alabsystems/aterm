// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The resize ledger's render verdict: a WRITER/READER contract between the
//! engine's resize journal and the host's ledger.

use super::{Liveness, Model};

/// Whether an application has drawn over content a resize displaced, judged
/// the way `aterm-gui`'s resize ledger judges it (`status render=`, the
/// `resizes` verb, `EVENT <local> render desync-risk`).
///
/// THE INCIDENT (2026-09-28, a live Claude Code session). On the alternate
/// screen, a rows-only shrink with a non-blank bottom row DEMOTES the top row,
/// and the alt grid keeps no history, so it was dropped; the grow that followed
/// appended a blank row instead of revealing it. A 64→63→64 flap therefore
/// left the whole screen one row higher than the app painted it. The app
/// repaints (ED 2) only when its SIGWINCH handler reads a CHANGED size; when
/// both resizes land before the handler runs it reads 64 == 64, skips the
/// repaint, and its diff frames (absolute `CSI r;c H`, no clear) land on the
/// shifted content for good.
///
/// THE ENGINE FIX. The grid's resize undo keeps the rows a shrink takes off
/// (`st`, the demoted rows it could hand back) until anything is drawn; a grow
/// hands them back, so a flap with no output between its halves is an identity
/// (`QuietFlapIsIdentity`). Any output drops the undo: an app that drew at the
/// smaller size has read, or will read, the change and repaint; output that
/// draws nothing (`Output`) leaves a displaced net-zero run the ledger judges.
///
/// THE CONTRACT. The engine is the WRITER: each resize's report names what it
/// did to the rows (`demoted`, `shift()`, and the rows its undo `stashed` or
/// handed back as `restored_top`), and its full-clear and screen-replacement
/// counters say when a displaced screen was repainted from scratch. The ledger
/// is the READER: it groups resizes into RUNS and calls a RISK only a NET-ZERO
/// run — one whose geometry ended where it began, so the app's handler,
/// whenever it ran, read no change and has no reason to repaint. A run that
/// displaces rows stays open through app output inside the run gap (every step
/// here but `Evaluate`, a reader a second later) for a resize that takes the
/// last one back (here, with two geometries, every next resize does; the
/// ledger closes a drawn-on run to any other, so a drag the app follows frame
/// by frame is not one long run): output is no evidence that the handler read
/// the intermediate size, and a handler that did and repaints from scratch
/// shows as the clear that closes the run (`Step` at a changed size); a run
/// that has moved nothing closes on a draw, as before. Past the gap a run
/// closes, unless it displaced rows the app has not drawn over (`silent`):
/// that one stays open for the next resize (`late`), so a grow the undo
/// answers nets it to zero — the rows the grow hands back return to the run
/// whose shrink stashed them (`rd - st`; the ledger's `credit`). A run whose
/// geometry stayed changed is the app's to repaint (its handler reads the
/// change), and the ledger cannot see a repaint that does not clear, so a draw
/// on it is UNVERIFIED (`verdict = 2`), never a risk and never ok. The
/// invariants state the verdict and the `displaced` render against the ground
/// truth (`disp`, `drew`), both ways: a verdict only over displaced content
/// the app drew on (`NoFalseVerdict`), a net-zero one a risk unless a reader
/// saw it short (`RiskIsFlagged`), displaced content drawn on never answered
/// ok (`NoSilentPass`), and `render=displaced` exactly while undrawn content
/// is displaced (`DisplacedIsShown`, `NoFalseDisplaced`).
///
/// (Integrated 2026-09-29 from two designs of 2026-09-28 over one base: the
/// `unverified` verdict and the credit of main's `4aa0cf233`, and the grouping
/// across a draw and past the gap of `fix/resize-ledger-split-flap`. Main's
/// HELD run — `ro = 2`, a run time closed while the undo held its rows — is
/// the grouping's `late` run here: a run holding rows is `silent` and
/// undrawn, so it is never closed by time, and the hold is not a separate
/// state.)
///
/// * `geom`: the grid's rows (0 = the app's full height, 1 = one row fewer).
/// * `app_geom`: the rows the app's frame is laid out for, what its SIGWINCH
///   handler last read.
/// * `disp`: how many rows the grid's content sits above where the app painted
///   it. Engine truth, bounded by `Cap`.
/// * `ro`/`rs`/`rd`/`rw`: the ledger's OPEN run — whether the next resize
///   joins it, the geometry it started from, the rows it displaced so far, and
///   whether the app drew while it displaced rows (`drew` on the ledger's run).
/// * `late`: the open run outlived the run gap (a `silent` run a reader left
///   open for the next resize): a draw now closes it.
/// * `led`: the displacement the ledger booked in closed NET-ZERO runs that
///   moved content up and has not seen healed (a run whose content moved
///   DOWN is not summed: its negative `rd` would cancel an up-moved one's,
///   and it keeps the space bounded); `lr`: it saw the app draw on a closed,
///   unhealed net-zero run that moved content EITHER way — the ledger judges
///   any `displaced != 0` (`Run::judged`), so a drawn down-moved one is a
///   `desync-risk` too, and the open run's verdict reads the same. The rig
///   never forms a down-moved net-zero run (its shrink after a grow that
///   appended trims); the model forms one only through the over-approximated
///   `Trim` after a `Draw`.
/// * `lb`: the ledger booked a closed NET-CHANGED run that displaced rows and
///   has not seen healed; `br`: it saw the app draw on one, which it reports
///   `unverified`.
/// * `blind`: ground truth, not the ledger's: a reader came by (`Evaluate`,
///   past the run gap) while the grid was still at the changed size and its
///   content displaced, so the app has had the time to read the change and
///   owes the repaint; a grow that brings the displacement back to zero
///   ends that episode (a later displacement is one no reader saw). The
///   ledger's named blind spot is a flap whose halves
///   MORE than the run gap separates, with a draw between: two net-changed
///   runs, `unverified` once drawn on, which `cast drift` (a RETURN: a lossy
///   shrink through the resize that brings the size back, however long after)
///   audits instead. A flap inside the gap is never excused by it, whatever
///   was drawn between.
/// * `drew`: the app has drawn a non-clearing frame onto displaced content
///   since its last full repaint.
/// * `st`: demoted rows the engine's resize undo holds for the next grow.
/// * `d0`: `disp` when the open run opened (the identity's reference).
/// * `bot`: what the last grow left on the bottom row, until something draws
///   (0: unknown, either shrink is possible). 1: the blank it appended below
///   the cursor, which the next shrink TRIMS. 2: the full row the undo handed
///   back (a demote left no blank tail), so the next shrink DEMOTES again.
/// * `out`: output (drawn or not) landed inside the open run. It drops the
///   undo without closing the run, which is how a net-zero run can still
///   displace rows and why the ledger still judges one.
/// * `evaluated`/`verdict`/`shown`: the reader's last answer — `verdict` 0 ok,
///   1 `render=desync-risk`, 2 `render=unverified`; `shown` 1 that some run is
///   still `silent` (`render=displaced` when nothing else is said).
/// * `ls`: `Buggy = 3` only — a `silent` run that mutant closed by time.
///
/// `Buggy = 1` is the historical engine AND the historical judge. The engine's
/// grow only appends (the rows a shrink demoted are gone), so a quiet flap
/// leaves the content one row up (`Demote, Grow` breaks
/// `QuietFlapIsIdentity`). The judge calls a render at risk (and displaced)
/// only while the grid's size differs from the size the app last laid out
/// for: it misses that flap (`Demote, Grow, Step, Evaluate`: the sizes agree,
/// the content is one row up and the app drew on it, breaking `RiskIsFlagged`,
/// `NoSilentPass` and `DisplacedIsShown`) and it raises a false alarm on a
/// trim-only shrink (`Trim, Evaluate`: nothing moved, breaking
/// `NoFalseVerdict` and `NoFalseDisplaced`). The ratchet sweeps this one;
/// every claim above falls to it.
///
/// `Buggy = 2` is the ASSUMED REPAINT (the ledger as it first landed,
/// 2026-09-28, never in a release): a net-changed run drawn on was
/// `repainted`, the draw TAKEN as the app's repaint, so it answered ok.
/// `Demote, Draw, Evaluate` (a spinner tick on the moved rows, the size still
/// changed) leaves the screen one row up and drawn on, and that judge says ok:
/// `NoSilentPass` breaks, alone.
///
/// `Buggy = 3` is TIME CLOSING EVERY RUN (neither the late run nor the
/// credit: the grouping before either design). A quiet flap only time split,
/// which the undo made an identity, keeps a `silent` half (`Demote, Evaluate,
/// Grow, Evaluate` breaks `NoFalseDisplaced`: `render=displaced` over an
/// intact screen) and, drawn on, an `unverified` one (`Demote, Evaluate, Grow,
/// Draw, Evaluate` breaks `NoFalseVerdict`). Over the whole space it also
/// breaks `RiskIsFlagged` and `DisplacedIsShown` — four invariants, and only
/// those (`each_named_mutant_breaks_exactly_its_claims_*` assert the set).
///
/// `Buggy = 4` is the design with a net-zero risk downgraded to `unverified`:
/// `Demote, Output, Grow, Step, Evaluate` (a flap output split, the app's
/// handler reading no change and drawing its diff) says `unverified` where the
/// ledger must say `desync-risk` — which `NoSilentPass` accepts (any verdict)
/// and only `RiskIsFlagged` refuses. It breaks `RiskIsFlagged` alone.
///
/// `Buggy = 5` is a DRAW CLOSING EVERY RUN (main's grouping before the
/// integration, credit and all). A flap whose halves a spinner tick splits
/// is two net-changed runs, `unverified` at best (`Demote, Draw, Grow, Step,
/// Evaluate` breaks `RiskIsFlagged`: measured live on 2026-09-28 as
/// `appended=1 restored=0`, ROW00 lost, the ledger not calling the risk), and
/// an earlier restored flap a reader saw excuses no later one (`Demote,
/// Evaluate, Grow, Demote, Draw, Grow, Step, Evaluate`). It breaks
/// `RiskIsFlagged` alone. The Tier-0 test replays `Buggy = 2` to `5` by name
/// and checks each breaks its claims and no other over the whole space.
///
/// THE EPOCH JUDGE (2026-09-29), the ledger's second judge, read from the
/// ENGINE'S DEBT instead of from the shape of a run. The run judge above cannot
/// see a flap an app frame splits (two net-changed runs), and cannot see an
/// undrawn residual at all. The engine counts what it owes
/// (`Terminal::render_debt`, exact however far the journal is overrun) and the
/// ledger judges it as one epoch:
///
/// * `debt`: the engine counter, in rows the content sits above where the
///   program painted it. A shrink that demotes adds one; a grow the undo serves
///   gives back `st`; a changed-size read (the full clear) zeroes it. It is
///   the report sums, not the screen, so `DebtIsTheDisplacement` (`debt == disp`)
///   is the writer/reader bind between the two.
/// * `eg`: the geometry BEFORE the first resize that made the debt (0 while none
///   is owed): where the program's handler last laid the frame out.
/// * `judged`: the presence classifier reads Claude Code, or this session has
///   shown it clears its screen when resized (`Classify`, once).
/// * `ev`: the epoch's word a second later — 0 ok, 1 `desync-risk`, 2
///   `unjudged`: rows are owed AND the geometry is back at `eg` (the handler can
///   only read an unchanged size), accused only when `judged`. While the
///   geometry differs from `eg` the handler is still to read a change and
///   repaints, so nothing is said.
///
/// The claims: a residual at a settled geometry is never answered ok
/// (`EpochFlagsTheResidual`), nothing is said when nothing is owed
/// (`EpochNoFalseAlarm`), an unlearned program is never accused
/// (`OnlyAJudgedProgramIsAccused`: a program that repaints with cursor
/// addressing and no ED 2 is indistinguishable from one that missed), and the
/// counter is the displacement (`DebtIsTheDisplacement`).
///
/// `Buggy = 1` adds the historical epoch to the historical judge: the debt is a
/// counter that no full clear resets, and the word is the size-difference one
/// (`geom` differs from `app_geom`), which ignores the debt and the class.
/// `Buggy = 6` accuses an unlearned program, `7` forgets the residual, `8`
/// answers whenever the geometry is at `eg` whether or not anything is owed;
/// each breaks its one claim and no other.
///
/// ASSUMED, NOT MODELED: the app's changed-size repaint starts with ED 2 (the
/// `Step` action heals). An app that repaints every row WITHOUT clearing is
/// seen by no counter, so a net-zero risk flagged before it stays flagged (a
/// false `desync-risk` the ledger cannot retract), and a net-changed run such
/// an app repaints stays `unverified` — which is why it is not called a risk,
/// and why it is not called ok either. The 250 ms run gap is the host's
/// environment bound: here every step but `Evaluate` lands inside it.
///
/// OVER-APPROXIMATED: whether a shrink demotes or trims is a free choice
/// except right after a grow (`bot`), and every draw may have filled or
/// cleared the bottom row.
///
/// Tier-1: `aterm-gui/src/resize_ledger.rs` drives a real alternate-screen
/// `aterm_core::Terminal` and the real `ResizeLedger` through every schedule
/// of these actions up to a bounded depth, projects the screen, the resize
/// undo, the app stand-in and the ledger's runs onto these variables, and
/// checks each real transition against `action_enabled`, the model's successor
/// and the invariants. Its negative controls replay `Demote, Grow, Step,
/// Evaluate` on the historical engine (the undo dropped between the halves),
/// where the real ledger flags it and the size-difference judge answers ok;
/// the tick-split flap and the time-split restored flap, which the real
/// ledger judges and `Buggy = 5` and `3` miss; and a tick on a still-changed
/// size, where the real ledger says `unverified` and the assumed-repaint judge
/// answers ok.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn resize_render_model() -> Model {
    crate::ty_model! {
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

            // Alt-screen shrink with a non-blank bottom row: the top row is
            // demoted and leaves the screen (the undo keeps it), so everything
            // moves up one row. It joins the open run, or opens one from the
            // current geometry.
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
            // Alt-screen grow: the undo hands its demoted rows back to the top,
            // moving everything back down; the rest is a blank row appended.
            // The rows it hands back are the open run's own (`rd - st`: the
            // ledger's credit to the run whose shrink stashed them). The
            // historical engine (`Buggy = 1`) only appended.
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
            // The app's SIGWINCH handler, then its next frame (output, which
            // drops the engine's resize undo). At a CHANGED size, a full
            // repaint (ED 2), which the ledger reads as a heal of every run:
            // the open run closes and nothing displaced is left. At an
            // unchanged size, a diff frame at absolute positions, with the
            // ledger's run exactly as `Draw` leaves it.
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
            // An ordinary diff frame (a spinner tick), no handler involved.
            // Inside the run gap (`late == 0`), a run that displaces rows
            // stays open and was drawn on (`rw`). A run that moved nothing
            // closes (the app drew on an undisplaced screen: a fresh
            // baseline), as does one past the gap (`late`: the app has had the
            // time to read the size); `Buggy = 5` closes every run on a draw.
            // A net-zero run it closes is booked (`led`) and now drawn on
            // (`lr`); a net-changed one that displaced rows likewise (`lb`,
            // `br`), and a booked one is now drawn on. Any output drops the
            // engine's resize undo.
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
            // Output that draws nothing (a mode set, a query: the first bytes
            // of a SIGWINCH handler): any output drops the engine's resize
            // undo, but the screen's content did not move, so the ledger's open
            // run stays open and the next resize joins it.
            action Output {
                st = 0;
                out = if (ro == 1) { 1 } else { out };
                evaluated = 0;
            }
            // The ledger's verdict a second later, past the run gap, from what
            // it booked and saw drawn: `desync-risk` over a net-zero run drawn
            // on, else `unverified` over a net-changed one, else ok. The time
            // closes the open run (and books it), unless it displaced rows the
            // app has not drawn over (`silent`: it waits for the next resize,
            // however late); `Buggy = 3` closes that one too, leaving it
            // `silent` (`ls`). The time does not drop the engine's undo:
            // nothing was drawn.
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
            invariant Bounded: disp <= Cap && led <= Cap && rd <= Cap && st <= Cap && debt <= Cap;
        }
    }
}

/// THE REPAINT NUDGE (G2 spike, 2026-09-28): the host's closed-loop repair of a
/// tab whose application will not repaint by itself.
///
/// An application that repaints only when its SIGWINCH handler READS a changed
/// size can be left on a screen it no longer believes in (a flap whose halves
/// it missed). The one natural, input-free way to make it repaint is the
/// resize protocol itself, done so that its handler reads EACH size: hold the
/// session one row short, wait for the app's answer, release, wait for the
/// second answer. Back-to-back (the adoption pulse) it reads one size and
/// repaints nothing (measured); on a timer it guesses the app's longest
/// blocking read. The driver never guesses: the answer is the evidence, and a
/// missing answer is a bounded wait, not a wedge.
///
/// * `phase`: 0 idle, 1 armed (debt seen; waiting for a quiet moment), 2 short
///   (the hold is on), 3 restoring (the hold is off; the true size is back),
///   4 at rest (healed or given up, said in the log).
/// * `held`: the session's target is one row short. THE safety property: it is
///   only ever set in phase 2, or after the driver was lost (`orphan`) and
///   never for longer than `Cap` ticks (the hard deadline).
/// * `halt`: a standing hold (`hold=1`): nothing is started, a running hold is
///   lifted (`Release`) and the attempt judged by its answers like any other.
/// * `safe`: the moment is quiet (no output burst, no DEC 2026 frame open).
/// * `ack1`/`ack2`: the app answered the short / the true size.
/// * `moved`: a real resize left the engine somewhere the hold did not find
///   it. A flap during the hold (`Flap`) is not one: its passes put the PTY at
///   the engine's size and the engine back where it was, so an app that read
///   neither half saw nothing change (2026-10-02: the driver had read every
///   write by someone else as a real resize and dropped the attempt, its one
///   try spent and nothing said - `DropOnFlap`). It comes back (`Return`) when
///   that resize did not last.
/// * `superseded`: the attempt ended superseded (`Supersede`) and the epoch
///   judge watches its epoch: while the resize stands the app has a size of its
///   own, and once the engine is back where the hold found it the window owes
///   the end (`Report`: the give-up while rows are owed). 2026-10-03: nothing
///   watched it, so a resize that came back after the wait left the tab
///   displaced, its try spent, nothing said - `ReturnUnwatched`.
/// * `lost`: the driver's record was lost (`DriverVanishes`).
/// * `gaveup`: the owner hears of the end (the digest line). Only `debt`
///   decides it, never the answers counted: a spent wait on a tab that owes
///   nothing ends in silence (2026-10-02: two answers that landed before one
///   step read as one, and the line flashed for a repaired tab). The Tier-1
///   binds it to the code's `Act::GaveUp`.
///
/// The environment (`Debt`, `Quiet`, `Busy`, `App1`, `App2`, `AltLeft`,
/// `Flap`, `Resize`, `Return`) is never fair. The driver waits for a quiet
/// moment only `Lim` ticks and for each answer only `Lim` ticks, so it ends
/// with the app silent forever.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn repaint_nudge_model() -> Model {
    crate::ty_model! {
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

            // THE ENVIRONMENT.
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

            // THE DRIVER.
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
            // THE WINDOW. The epoch judge finds a superseded attempt's epoch back
            // where the hold found it: rows still owed are the give-up the same
            // attempt is when the resize comes back inside its wait; none, and
            // the end is silent.
            action Report when (superseded == 1 && moved == 0) {
                superseded = 0;
                gaveup = if (debt == 1) { 1 } else { gaveup };
            }
            action Tick when (1 <= phase && phase <= 3 && tick <= Lim - 1) { tick = tick + 1; }

            // THE RELEASE CANNOT BE LOST. The driver's record is lost while the PTY
            // is short (a panic in the tick, a teardown race): the App's own map of
            // held sessions (`orphan`) still stands, and its hard deadline lets the
            // PTY go whether or not any driver is left to do it.
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
            // BUGGY (the first draft's design): the hold resizes the ENGINE, not just the PTY.
            action HoldTheEngine when (Buggy == 1 && phase == 1 && debt == 1) {
                phase = 2;
                held = 1;
                engine = 1;
                tries = if (tries > MaxTries) { tries } else { tries + 1 };
                tick = 0;
                ack1 = 0;
                ack2 = 0;
            }
            // BUGGY: no hard deadline (a hold nobody lifts stays for as long as the
            // process lives).
            action NoCap when (Buggy == 1 && held == 1 && age == Cap) { age = Cap + 1; }
            // BUGGY: retried without a budget (an app that never answers is nudged
            // forever).
            action RetryForever when (Buggy == 1 && phase == 3 && tick > Lim - 1
                                      && (ack1 == 0 || ack2 == 0) && debt == 1) {
                phase = 1;
                tick = 0;
            }
            // BUGGY (shipped until 2026-10-02): any write by someone else read as
            // a real resize, so a flap the app never saw ended the attempt at
            // once: the epoch's one try spent, the tab still owing, nothing said.
            action DropOnFlap when (Buggy == 1 && phase == 2 && held == 0 && moved == 0) {
                phase = 4;
            }
            // BUGGY (shipped until 2026-10-03): nothing watched a superseded
            // attempt's epoch, so a resize that came back after the wait left the
            // tab displaced, its one try spent, nothing said.
            action ReturnUnwatched when (Buggy == 1 && superseded == 1 && moved == 1) {
                moved = 0;
                superseded = 0;
            }

            invariant HeldOnlyWhileShort: held == 0 || phase == 2 || orphan == 1;
            invariant AHeldPtyIsReleasedByTheDeadline: held == 0 || age <= Cap;
            invariant HealedNeedsBothAnswers: healed == 0 || (ack1 == 1 && ack2 == 1);
            invariant OneTryPerEpoch: tries <= MaxTries;
            invariant NudgeNeverTouchesTheEngine: engine == 0;
            // An attempt that ends on a tab still owing rows says so (a give-up,
            // the owner's digest line), unless the driver itself was lost or a
            // real resize superseded it and the epoch judge still watches its
            // epoch: the resize stands, or it came back and the judge owes the
            // give-up, which `TheNudgeEnds` holds it to (`Report`, weakly fair).
            // A moved engine alone excuses nothing.
            invariant AnUnrepairedEndIsReported: phase <= 3 || debt == 0 || healed == 1
                || gaveup == 1 || superseded == 1 || lost == 1;
        }
    }
}

/// The nudge ends, and its end is said: `[]<>(at rest with nothing owed to the
/// owner \/ nothing to repair)`, under weak fairness of the driver's and the
/// window's own steps and NO fairness for the app. At rest with nothing owed:
/// no superseded attempt whose resize came back is left unreported (`Report`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn repaint_nudge_liveness() -> Liveness {
    use super::{and_, eq, int, or_, var};
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
