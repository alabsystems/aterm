// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The session log (`docs/DESIGN-session-log-2026-10-15.md` §9): the
//! per-session ring with its open/close discipline and level-aware eviction,
//! a reader's cursor over it, the turn tracker over screen verdict words, and
//! the disk journal's reader view. Tier-1 bound in aterm-slog
//! (`tests/conformance_ring.rs`, `tests/conformance_cursor.rs`,
//! `tests/conformance_turn.rs`, `tests/conformance_journal.rs`), which drive
//! the shipping `Log`, `TurnTracker` and `journal::view_of` and project them onto these
//! variables; no per-method `#[refines]` anchors (aterm-slog links no
//! aterm-spec outside its tests).

use aterm_spec::derive::*;

/// SESSION LOG RING (phase 2A): one session's bounded ring of records,
/// evicting level-aware (§5.1), over the open path of one run, its turn and
/// the turn's step. The ring is abstracted to how many live records it holds
/// in each eviction class (`aterm_slog::evict_class`), which is what the
/// policy reads; positions within a class are the unit tests'.
///
/// Variables: `seq` the last seq assigned, `lost` the records evicted (or
/// shed by a carry), `first` the first seq the log accounts for, `holes` the
/// seqs it never held; the update: `ep`
/// the epoch (0 before it, 1 after), `drawn` whether the manifest was drawn
/// and `ceil` the seq it carried (the last plus the overlap's `Reserve`), `hi`
/// the last seq the outgoing process assigned, `hseq` the successor's first;
/// `run`/`turn`/`step` the open path, `sorph` whether the open step is an
/// orphan hanging from the run (§2.3), `oold` whether that orphan's open
/// predates the newest turn close (its records are then history once it
/// closes), `tclose` the closes written for the current turn node, `tn` the
/// turns opened; the fold, kept outside the ring: `fturn` its open turn,
/// `fturns` its turn total. Live records by class: `prun`/`pturn`/`pstep`
/// the open path's own open records (pinned); `dturn` current detail under
/// the open turn, `dout` current detail outside a turn (on the run, the
/// session or an orphan step, newer than the newest close beneath it);
/// `tdet` closed detail under the newest closed turn, `dold` all other
/// closed detail; `ocl` the open records of closed runs and turns, `cls`
/// the closes of runs and turns, `tcl` whether the newest closed turn's
/// close is live. `nw` is the class of the record appended last, which no
/// batch takes (0 pinned, 1 `dturn`, 2 `dout`, 3 the newest turn's close, 4
/// another close).
///
/// An append that takes the ring past `Cap` arms a batch (`ev = 1`); the
/// batch evicts one record a step, in policy order, until the ring is at
/// `Low` (`aterm_slog::low_water`) or nothing but the pinned records and the
/// newest is left (`EvictDone`). The order: closed detail (`dold`, `tdet`),
/// closed opens (`ocl`), closes while they pass half the ring
/// (`aterm_slog::closes_over_half`), current detail, the remaining closes.
/// `Mark` lands on the deepest open node (a turn's when one is open);
/// `MarkOutside` hangs from the run or the session whatever is open (an
/// agent's verdict mark, a person's input burst, a gap). `CloseTurnCascade`
/// is a turn closed over its open step (two records: the
/// step `out=gone`, then the turn), `CloseRunCascade` a run closed over its
/// open turn and step, innermost first. A turn closes without touching an
/// orphan step, which hangs from the run.
///
/// An update that carries no records: `Draw` takes the manifest's count
/// (`SessionRecord::log_seq`), the outgoing process goes on appending, a
/// Commit past the count stands down (`StandDown`, a lossless retry), and
/// `HandoffUncarried` starts the successor's log at `ceil + 1` with its `gap
/// why=handoff` (the open path, the ring and the fold start over). An update
/// that carries the log: `HandoffCarried` arms the carry's shed (`cv = 1`),
/// admitted only when its pinned path and newest record fit `CarryCap`,
/// which takes records in the same policy order down to that bound, each
/// counted lost (`aterm_slog::Log::carry`'s cut), and `CarryDone` continues
/// the same log at `ceil + 1` with its `mark - handoff`, the seqs between
/// (the overlap the outgoing process may still have used) never held
/// (`holes`); the open path, the fold and `first` cross unchanged. `cpath`
/// is the open path at the carry (plus one) and `moved` whether the
/// successor opened or closed anything since. The successor then appends at
/// most `After` records, which keeps the box small. `Crash` is a crash
/// adoption (a shell the PTY keeper held): a new seq space from 1, `gap
/// why=crash` first, at an epoch above the last one (the keeper tag carries
/// it); `eplast` is the epoch before the adoption, `adoptions` whether one
/// happened.
///
/// `Buggy = 1` adds dead mutants for the laws: an append that arms no batch
/// (`Bounded`), a batch that takes the open turn's open record
/// (`OpenPathRetained`), one that takes the newest turn's close while its
/// detail lives (`RollupOutlivesDetail`), an eviction nobody counts
/// (`NoSilentLoss`), a turn closed with its step left open and a step
/// attached to a closed turn (`WellNested`), a second close of a closed turn
/// (`CloseOnce`), a fold recomputed from a ring that evicted a turn's
/// records (`ProjectionIndependentOfEviction`), an update that restarts
/// seqs where the outgoing process had used them (`SeqNeverReused`), a
/// carried update that drops the open path (`CarryKeepsOpenPath`), a carry
/// that sheds a record without counting it (`NoSilentLoss`), and a crash
/// adoption that keeps the epoch (`EpochGrows`). Tier-1:
/// aterm-slog's `slog_ring_conforms_to_derived_model`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn session_log_ring_model() -> Model {
    aterm_spec::ty_model! {
        SessionLogRing {
            const Cap = 3;
            const Low = 2;
            const MaxSeq = 6;
            const Reserve = 1;
            const After = 1;
            const CarryCap = 2;
            const Buggy = 0;
            var seq = 0;
            var lost = 0;
            var first = 1;
            var hi = 0;
            var ceil = 0;
            var drawn = 0;
            var ep = 0;
            var holes = 0;
            var hseq = 0;
            var cv = 0;
            var cpath = 0;
            var moved = 0;
            var eplast = 0;
            var adoptions = 0;
            var run = 0;
            var turn = 0;
            var step = 0;
            var sorph = 0;
            var oold = 0;
            var tclose = 0;
            var tn = 0;
            var fturns = 0;
            var fturn = 0;
            var prun = 0;
            var pturn = 0;
            var pstep = 0;
            var dturn = 0;
            var dout = 0;
            var tdet = 0;
            var dold = 0;
            var ocl = 0;
            var cls = 0;
            var tcl = 0;
            var nw = 0;
            var ev = 0;

            action OpenRun when (ev == 0 && run == 0 && seq + 1 <= (if ep == 0 { MaxSeq } else { hseq + After })) {
                moved = 1;
                run = 1;
                prun = 1;
                seq = seq + 1;
                nw = 0;
                ev = if (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) + 1 > Cap { 1 } else { 0 };
            }
            action OpenTurn when (ev == 0 && run == 1 && turn == 0 && seq + 1 <= (if ep == 0 { MaxSeq } else { hseq + After })) {
                moved = 1;
                turn = 1;
                pturn = 1;
                tclose = 0;
                tn = tn + 1;
                fturns = fturns + 1;
                fturn = 1;
                seq = seq + 1;
                nw = 0;
                ev = if (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) + 1 > Cap { 1 } else { 0 };
            }
            action OpenStep when (ev == 0 && turn == 1 && step == 0 && seq + 1 <= (if ep == 0 { MaxSeq } else { hseq + After })) {
                moved = 1;
                step = 1;
                sorph = 0;
                pstep = 1;
                seq = seq + 1;
                nw = 0;
                ev = if (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) + 1 > Cap { 1 } else { 0 };
            }
            action OpenOrphanStep when (ev == 0 && run == 1 && turn == 0 && step == 0 && seq + 1 <= (if ep == 0 { MaxSeq } else { hseq + After })) {
                moved = 1;
                step = 1;
                sorph = 1;
                oold = 0;
                pstep = 1;
                seq = seq + 1;
                nw = 0;
                ev = if (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) + 1 > Cap { 1 } else { 0 };
            }
            action CloseStep when (ev == 0 && step == 1 && seq + 1 <= (if ep == 0 { MaxSeq } else { hseq + After })) {
                moved = 1;
                step = 0;
                sorph = 0;
                oold = 0;
                pstep = 0;
                dturn = if sorph == 0 { dturn + 2 } else { dturn };
                dout = if sorph == 1 { dout + 2 - oold } else { dout };
                dold = if sorph == 1 { dold + oold } else { dold };
                seq = seq + 1;
                nw = if sorph == 0 { 1 } else { 2 };
                ev = if (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) + 1 > Cap { 1 } else { 0 };
            }
            action CloseTurn when (ev == 0 && turn == 1 && (step == 0 || sorph == 1) && seq + 1 <= (if ep == 0 { MaxSeq } else { hseq + After })) {
                moved = 1;
                turn = 0;
                pturn = 0;
                tclose = tclose + 1;
                fturn = 0;
                ocl = ocl + 1;
                cls = cls + 1;
                tcl = 1;
                dold = dold + tdet + dout;
                tdet = dturn;
                dturn = 0;
                dout = 0;
                oold = step;
                seq = seq + 1;
                nw = 3;
                ev = if (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) + 1 > Cap { 1 } else { 0 };
            }
            action CloseTurnCascade when (ev == 0 && turn == 1 && step == 1 && sorph == 0 && seq + 2 <= (if ep == 0 { MaxSeq } else { hseq + After })) {
                moved = 1;
                step = 0;
                pstep = 0;
                turn = 0;
                pturn = 0;
                tclose = tclose + 1;
                fturn = 0;
                ocl = ocl + 1;
                cls = cls + 1;
                tcl = 1;
                dold = dold + tdet + dout;
                tdet = dturn + 2;
                dturn = 0;
                dout = 0;
                seq = seq + 2;
                nw = 3;
                ev = if (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) + 2 > Cap { 1 } else { 0 };
            }
            action CloseRun when (ev == 0 && run == 1 && turn == 0 && step == 0 && seq + 1 <= (if ep == 0 { MaxSeq } else { hseq + After })) {
                moved = 1;
                run = 0;
                prun = 0;
                ocl = ocl + 1;
                cls = cls + 1;
                dold = dold + dout;
                dout = 0;
                seq = seq + 1;
                nw = 4;
                ev = if (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) + 1 > Cap { 1 } else { 0 };
            }
            action CloseRunCascade when (ev == 0 && run == 1 && 1 <= turn + step && seq + 1 + turn + step <= (if ep == 0 { MaxSeq } else { hseq + After })) {
                moved = 1;
                run = 0;
                prun = 0;
                turn = 0;
                pturn = 0;
                step = 0;
                pstep = 0;
                sorph = 0;
                oold = 0;
                fturn = 0;
                tclose = tclose + turn;
                ocl = ocl + 1 + turn;
                cls = cls + 1 + turn;
                tcl = if turn == 1 { 1 } else { tcl };
                tdet = if turn == 1 { dturn + step + step - sorph - sorph } else { tdet };
                dold = if turn == 1 { dold + tdet + dout + sorph + sorph } else { dold + dout + step + step };
                dturn = 0;
                dout = 0;
                seq = seq + 1 + turn + step;
                nw = 4;
                ev = if (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) + 1 + turn + step > Cap { 1 } else { 0 };
            }
            action Mark when (ev == 0 && seq + 1 <= (if ep == 0 { MaxSeq } else { hseq + After })) {
                dturn = if turn == 1 { dturn + 1 } else { dturn };
                dout = if turn == 0 { dout + 1 } else { dout };
                seq = seq + 1;
                nw = if turn == 1 { 1 } else { 2 };
                ev = if (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) + 1 > Cap { 1 } else { 0 };
            }
            action MarkOutside when (ev == 0 && seq + 1 <= (if ep == 0 { MaxSeq } else { hseq + After })) {
                dout = dout + 1;
                seq = seq + 1;
                nw = 2;
                ev = if (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) + 1 > Cap { 1 } else { 0 };
            }
            action EvictOldDetail when (ev == 1 && (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) > (if cv == 1 { CarryCap } else { Low }) && dold > 0) {
                dold = dold - 1;
                lost = lost + 1;
            }
            action EvictTurnDetail when (ev == 1 && (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) > (if cv == 1 { CarryCap } else { Low }) && tdet > 0) {
                tdet = tdet - 1;
                lost = lost + 1;
            }
            action EvictClosedOpen when (ev == 1 && (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) > (if cv == 1 { CarryCap } else { Low }) && dold + tdet <= 0 && ocl > 0) {
                ocl = ocl - 1;
                lost = lost + 1;
            }
            action EvictClose when (ev == 1 && (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) > (if cv == 1 { CarryCap } else { Low }) && dold + tdet <= 0 && ocl <= 0 && ((nw <= 3 && cls > tcl) || (nw == 4 && cls > tcl + 1)) && (cls + cls > Cap || (if (((nw == 1 && dturn > 1) || ((nw <= 0 || nw > 1) && dturn > 0)) || ((nw == 2 && dout > 1) || ((nw <= 1 || nw > 2) && dout > 0))) { 1 } else { 0 }) == 0)) {
                cls = cls - 1;
                lost = lost + 1;
            }
            action EvictTurnClose when (ev == 1 && (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) > (if cv == 1 { CarryCap } else { Low }) && dold + tdet <= 0 && ocl <= 0 && (tcl == 1 && (nw <= 2 || nw == 4)) && (cls + cls > Cap || (if (((nw == 1 && dturn > 1) || ((nw <= 0 || nw > 1) && dturn > 0)) || ((nw == 2 && dout > 1) || ((nw <= 1 || nw > 2) && dout > 0))) { 1 } else { 0 }) == 0)) {
                cls = cls - 1;
                tcl = 0;
                lost = lost + 1;
            }
            action EvictCurrent when (ev == 1 && (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) > (if cv == 1 { CarryCap } else { Low }) && dold + tdet <= 0 && ocl <= 0 && (if cls + cls > Cap && (((nw <= 3 && cls > tcl) || (nw == 4 && cls > tcl + 1)) || (tcl == 1 && (nw <= 2 || nw == 4))) { 1 } else { 0 }) == 0 && ((nw == 1 && dturn > 1) || ((nw <= 0 || nw > 1) && dturn > 0))) {
                dturn = dturn - 1;
                lost = lost + 1;
            }
            action EvictCurrentOut when (ev == 1 && (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) > (if cv == 1 { CarryCap } else { Low }) && dold + tdet <= 0 && ocl <= 0 && (if cls + cls > Cap && (((nw <= 3 && cls > tcl) || (nw == 4 && cls > tcl + 1)) || (tcl == 1 && (nw <= 2 || nw == 4))) { 1 } else { 0 }) == 0 && ((nw == 2 && dout > 1) || ((nw <= 1 || nw > 2) && dout > 0))) {
                dout = dout - 1;
                lost = lost + 1;
            }
            action EvictDone when (ev == 1 && cv == 0 && ((prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) <= Low || (dold + tdet <= 0 && ocl <= 0 && (if (((nw <= 3 && cls > tcl) || (nw == 4 && cls > tcl + 1)) || (tcl == 1 && (nw <= 2 || nw == 4))) { 1 } else { 0 }) == 0 && (if (((nw == 1 && dturn > 1) || ((nw <= 0 || nw > 1) && dturn > 0)) || ((nw == 2 && dout > 1) || ((nw <= 1 || nw > 2) && dout > 0))) { 1 } else { 0 }) == 0))) {
                ev = 0;
            }
            action CarryDone when (ev == 1 && cv == 1 && ((prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) <= CarryCap || (dold + tdet <= 0 && ocl <= 0 && (if (((nw <= 3 && cls > tcl) || (nw == 4 && cls > tcl + 1)) || (tcl == 1 && (nw <= 2 || nw == 4))) { 1 } else { 0 }) == 0 && (if (((nw == 1 && dturn > 1) || ((nw <= 0 || nw > 1) && dturn > 0)) || ((nw == 2 && dout > 1) || ((nw <= 1 || nw > 2) && dout > 0))) { 1 } else { 0 }) == 0))) {
                hi = seq;
                holes = holes + ceil - seq;
                seq = ceil + 1;
                hseq = ceil + 1;
                dout = dout + 1;
                nw = 2;
                eplast = ep;
                adoptions = 1;
                ep = 1;
                drawn = 0;
                ceil = 0;
                cv = 0;
                moved = 0;
                ev = if (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) + 1 > Cap { 1 } else { 0 };
            }

            action Draw when (ev == 0 && drawn == 0 && ep == 0) {
                ceil = seq + Reserve;
                drawn = 1;
            }
            action StandDown when (drawn == 1 && seq > ceil) {
                ceil = 0;
                drawn = 0;
            }
            action HandoffCarried when (ev == 0 && drawn == 1 && ep == 0 && seq <= ceil && prun + pturn + pstep + (if nw > 0 { 1 } else { 0 }) <= CarryCap) {
                ev = 1;
                cv = 1;
                cpath = 1 + run + turn + turn + step + step + step + step;
            }
            action HandoffUncarried when (ev == 0 && drawn == 1 && ep == 0 && seq <= ceil) {
                hi = seq;
                lost = 0;
                run = 0;
                turn = 0;
                step = 0;
                sorph = 0;
                oold = 0;
                tclose = 0;
                tn = 0;
                fturns = 0;
                fturn = 0;
                prun = 0;
                pturn = 0;
                pstep = 0;
                dturn = 0;
                dout = 1;
                tdet = 0;
                dold = 0;
                ocl = 0;
                cls = 0;
                tcl = 0;
                nw = 2;
                drawn = 0;
                ceil = 0;
                eplast = ep;
                adoptions = 1;
                ep = 1;
                seq = ceil + 1;
                first = ceil + 1;
                hseq = ceil + 1;
                holes = 0;
            }
            action Crash when (ev == 0 && ep == 0 && seq > 0) {
                hi = 0;
                lost = 0;
                run = 0;
                turn = 0;
                step = 0;
                sorph = 0;
                oold = 0;
                tclose = 0;
                tn = 0;
                fturns = 0;
                fturn = 0;
                prun = 0;
                pturn = 0;
                pstep = 0;
                dturn = 0;
                dout = 1;
                tdet = 0;
                dold = 0;
                ocl = 0;
                cls = 0;
                tcl = 0;
                nw = 2;
                drawn = 0;
                ceil = 0;
                seq = 1;
                first = 1;
                hseq = 1;
                holes = 0;
                eplast = ep;
                adoptions = 1;
                ep = 1;
            }
            action AppendNoEvict when (Buggy == 1 && ev == 0 && (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) + 1 > Cap && seq + 1 <= (if ep == 0 { MaxSeq } else { hseq + After })) {
                dout = dout + 1;
                seq = seq + 1;
                nw = 2;
            }
            action EvictOpenRecord when (Buggy == 1 && ev == 1 && pturn == 1) {
                pturn = 0;
                lost = lost + 1;
            }
            action EvictCloseBeforeDetail when (Buggy == 1 && ev == 1 && tcl == 1 && tdet > 0) {
                cls = cls - 1;
                tcl = 0;
                lost = lost + 1;
            }
            action EvictUncounted when (Buggy == 1 && ev == 1 && dold > 0) {
                dold = dold - 1;
            }
            action CloseParentLeaveChild when (Buggy == 1 && ev == 0 && turn == 1 && step == 1 && sorph == 0 && seq + 1 <= (if ep == 0 { MaxSeq } else { hseq + After })) {
                turn = 0;
                pturn = 0;
                tclose = tclose + 1;
                fturn = 0;
                ocl = ocl + 1;
                cls = cls + 1;
                tcl = 1;
                dold = dold + tdet + dout;
                tdet = dturn;
                dturn = 0;
                dout = 0;
                seq = seq + 1;
                nw = 3;
                ev = if (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) + 1 > Cap { 1 } else { 0 };
            }
            action StepUnderClosedTurn when (Buggy == 1 && ev == 0 && run == 1 && turn == 0 && step == 0 && tclose == 1 && seq + 1 <= (if ep == 0 { MaxSeq } else { hseq + After })) {
                step = 1;
                sorph = 0;
                pstep = 1;
                seq = seq + 1;
                nw = 0;
                ev = if (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) + 1 > Cap { 1 } else { 0 };
            }
            action CloseTwice when (Buggy == 1 && ev == 0 && turn == 0 && tclose == 1 && seq + 1 <= (if ep == 0 { MaxSeq } else { hseq + After })) {
                tclose = 2;
                cls = cls + 1;
                seq = seq + 1;
                nw = 4;
                ev = if (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) + 1 > Cap { 1 } else { 0 };
            }
            action HandoffReuse when (Buggy == 1 && ev == 0 && drawn == 1 && ep == 0) {
                hi = seq;
                lost = 0;
                run = 0;
                turn = 0;
                step = 0;
                sorph = 0;
                oold = 0;
                tclose = 0;
                tn = 0;
                fturns = 0;
                fturn = 0;
                prun = 0;
                pturn = 0;
                pstep = 0;
                dturn = 0;
                dout = 1;
                tdet = 0;
                dold = 0;
                ocl = 0;
                cls = 0;
                tcl = 0;
                nw = 2;
                drawn = 0;
                ceil = 0;
                eplast = ep;
                adoptions = 1;
                ep = 1;
                seq = 1;
                first = 1;
                hseq = 1;
                holes = 0;
            }
            action HandoffForgetsOpen when (Buggy == 1 && ev == 0 && drawn == 1 && ep == 0 && seq <= ceil && run == 1 && prun + pturn + pstep + (if nw > 0 { 1 } else { 0 }) <= CarryCap) {
                cpath = 1 + run + turn + turn + step + step + step + step;
                moved = 0;
                lost = lost + prun + pturn + pstep;
                run = 0;
                turn = 0;
                step = 0;
                sorph = 0;
                oold = 0;
                prun = 0;
                pturn = 0;
                pstep = 0;
                fturn = 0;
                hi = seq;
                holes = holes + ceil - seq;
                seq = ceil + 1;
                hseq = ceil + 1;
                dout = dout + 1;
                nw = 2;
                eplast = ep;
                adoptions = 1;
                ep = 1;
                drawn = 0;
                ceil = 0;
            }
            action CarryShedUncounted when (Buggy == 1 && ev == 1 && cv == 1 && dold > 0) {
                dold = dold - 1;
            }
            action CrashSameEpoch when (Buggy == 1 && ev == 0 && ep == 0 && seq > 0) {
                hi = 0;
                lost = 0;
                run = 0;
                turn = 0;
                step = 0;
                sorph = 0;
                oold = 0;
                tclose = 0;
                tn = 0;
                fturns = 0;
                fturn = 0;
                prun = 0;
                pturn = 0;
                pstep = 0;
                dturn = 0;
                dout = 1;
                tdet = 0;
                dold = 0;
                ocl = 0;
                cls = 0;
                tcl = 0;
                nw = 2;
                drawn = 0;
                ceil = 0;
                seq = 1;
                first = 1;
                hseq = 1;
                holes = 0;
                eplast = ep;
                adoptions = 1;
                ep = 0;
            }
            action RefoldFromRing when (Buggy == 1 && lost > 0 && fturns > 0) {
                fturns = fturns - 1;
            }

            invariant Bounded: ev == 1 || (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) <= Cap || (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) <= (prun + pturn + pstep) + 1;
            invariant OpenPathRetained: prun == run && pturn == turn && pstep == step;
            invariant RollupOutlivesDetail: tcl == 1 || tdet <= 0;
            invariant NoSilentLoss: lost + (prun + pturn + pstep + dturn + dout + tdet + dold + ocl + cls) + first + holes == seq + 1;
            invariant WellNested: turn <= run && step <= run && sorph <= step && (sorph == 1 || step <= turn);
            invariant CloseOnce: tclose <= 1;
            invariant ProjectionIndependentOfEviction: fturn == turn && fturns == tn;
            invariant SeqNeverReused: hi <= seq;
            invariant EpochGrows: adoptions == 0 || ep > eplast;
            invariant CarryKeepsOpenPath: cpath == 0 || moved == 1 || cpath == 1 + run + turn + turn + step + step + step + step;
            bound StateBounded: seq <= MaxSeq + Reserve + After + 1 && first <= MaxSeq + Reserve + 1 && hseq <= MaxSeq + Reserve + 1 && holes <= Reserve && cpath <= 8 && moved <= 1 && cv <= 1 && eplast <= 1 && adoptions <= 1 && ceil <= MaxSeq + Reserve && hi <= MaxSeq + Reserve && ep <= 1 && lost <= MaxSeq + Reserve + After + 1 && tn <= MaxSeq && dturn <= MaxSeq + Reserve + After + 1 && dout <= MaxSeq + Reserve + After + 1 && tdet <= MaxSeq + Reserve + After + 1 && dold <= MaxSeq + Reserve + After + 1 && ocl <= MaxSeq + Reserve + After + 1 && cls <= MaxSeq + Reserve + After + 1 && nw <= 4 && tclose <= 2;

        }
    }
}

/// SESSION LOG CURSOR (phase 2A): what a reader pulling through
/// `aterm_slog::Log::read` is told, over four seqs, now that level-aware
/// eviction takes records from the middle of the window and a carried update
/// leaves seqs the log never held (`Log::skip_to`).
///
/// Variables: `s1`..`s4` each seq's standing (0 not yet assigned, 1 live,
/// 2 evicted, 3 a hole: never held, 4 written in an update's overlap), `seq`
/// the seqs assigned; the reader's
/// cursor `cur` (the last seq it covered: `since=` is exclusive), the records
/// it was handed `got`, the losses it was told `rlost`, and `told` whether a
/// read told it of a hole (`gap why=handoff`). `Evict<i>` takes any live
/// record, whatever its place. `ReadTo<k>` is a read cut at the live record
/// `k` (`max` reached there), `ReadAll` one that reaches the head; each counts
/// what it covered.
///
/// A carried update's overlap: `Export` takes the carry and fences the
/// outgoing log's readers at its head (`fence`, `aterm_slog::Log::fence_reads`;
/// the fenced records are not evicted meanwhile);
/// `OverlapAppend` is a record the outgoing process writes after (seq
/// standing 4), which the successor never holds and may contradict (a turn
/// it closes again, a step it never closes); `Commit` hands over (those seqs
/// become holes: the successor continues the carry, `ep = 1`), `StandDown`
/// lifts the fence and keeps them. `ov` counts overlap records a reader was
/// handed: the law `ReaderNeverSeesOverlap` is the reader's view of
/// `CloseOnce` across an update.
///
/// `Buggy = 1` adds one dead mutant per law: a read that leaves evicted seqs
/// untold (`NoSilentLoss`), one that counts a hole as a loss
/// (`HoleNeverCountsAsLoss`), one that crosses a hole without saying so
/// (`HandoffSaid`), and one that reads past the fence (`ReaderNeverSeesOverlap`).
/// Tier-1: aterm-slog's `slog_cursor_conforms_to_derived_model`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn session_log_cursor_model() -> Model {
    aterm_spec::ty_model! {
        SessionLogCursor {
            const Buggy = 0;
            var s1 = 0;
            var s2 = 0;
            var s3 = 0;
            var s4 = 0;
            var seq = 0;
            var cur = 0;
            var got = 0;
            var rlost = 0;
            var told = 0;
            var fence = 0;
            var ov = 0;
            var ep = 0;

            action Append when (fence == 0 && seq <= 3) {
                s1 = if seq == 0 { 1 } else { s1 };
                s2 = if seq == 1 { 1 } else { s2 };
                s3 = if seq == 2 { 1 } else { s3 };
                s4 = if seq == 3 { 1 } else { s4 };
                seq = seq + 1;
            }
            action Hole when (fence == 0 && seq <= 3) {
                s1 = if seq == 0 { 3 } else { s1 };
                s2 = if seq == 1 { 3 } else { s2 };
                s3 = if seq == 2 { 3 } else { s3 };
                s4 = if seq == 3 { 3 } else { s4 };
                seq = seq + 1;
            }
            action Evict1 when (s1 == 1 && (fence == 0 || 1 > fence)) {
                s1 = 2;
            }
            action Evict2 when (s2 == 1 && (fence == 0 || 2 > fence)) {
                s2 = 2;
            }
            action Evict3 when (s3 == 1 && (fence == 0 || 3 > fence)) {
                s3 = 2;
            }
            action Evict4 when (s4 == 1 && (fence == 0 || 4 > fence)) {
                s4 = 2;
            }
            action Export when (fence == 0 && ep == 0 && seq > 0) {
                fence = seq;
            }
            action OverlapAppend when (fence > 0 && seq <= 3) {
                s1 = if seq == 0 { 4 } else { s1 };
                s2 = if seq == 1 { 4 } else { s2 };
                s3 = if seq == 2 { 4 } else { s3 };
                s4 = if seq == 3 { 4 } else { s4 };
                seq = seq + 1;
            }
            action Commit when (fence > 0) {
                s1 = if s1 == 4 { 3 } else { s1 };
                s2 = if s2 == 4 { 3 } else { s2 };
                s3 = if s3 == 4 { 3 } else { s3 };
                s4 = if s4 == 4 { 3 } else { s4 };
                fence = 0;
                ep = 1;
            }
            action StandDown when (fence > 0) {
                s1 = if s1 == 4 { 1 } else { s1 };
                s2 = if s2 == 4 { 1 } else { s2 };
                s3 = if s3 == 4 { 1 } else { s3 };
                s4 = if s4 == 4 { 1 } else { s4 };
                fence = 0;
            }
            action ReadTo1 when (s1 == 1 && 1 > cur && (fence == 0 || 1 <= fence)) {
                got = got + ((if 1 > cur && s1 == 1 { 1 } else { 0 }));
                rlost = rlost + ((if 1 > cur && s1 == 2 { 1 } else { 0 }));
                told = if ((if 1 > cur && s1 == 3 { 1 } else { 0 })) > 0 { 1 } else { told };
                ov = ov;
                cur = 1;
            }
            action ReadTo2 when (s2 == 1 && 2 > cur && (fence == 0 || 2 <= fence)) {
                got = got + ((if 1 > cur && s1 == 1 { 1 } else { 0 }) + (if 2 > cur && s2 == 1 { 1 } else { 0 }));
                rlost = rlost + ((if 1 > cur && s1 == 2 { 1 } else { 0 }) + (if 2 > cur && s2 == 2 { 1 } else { 0 }));
                told = if ((if 1 > cur && s1 == 3 { 1 } else { 0 }) + (if 2 > cur && s2 == 3 { 1 } else { 0 })) > 0 { 1 } else { told };
                ov = ov;
                cur = 2;
            }
            action ReadTo3 when (s3 == 1 && 3 > cur && (fence == 0 || 3 <= fence)) {
                got = got + ((if 1 > cur && s1 == 1 { 1 } else { 0 }) + (if 2 > cur && s2 == 1 { 1 } else { 0 }) + (if 3 > cur && s3 == 1 { 1 } else { 0 }));
                rlost = rlost + ((if 1 > cur && s1 == 2 { 1 } else { 0 }) + (if 2 > cur && s2 == 2 { 1 } else { 0 }) + (if 3 > cur && s3 == 2 { 1 } else { 0 }));
                told = if ((if 1 > cur && s1 == 3 { 1 } else { 0 }) + (if 2 > cur && s2 == 3 { 1 } else { 0 }) + (if 3 > cur && s3 == 3 { 1 } else { 0 })) > 0 { 1 } else { told };
                ov = ov;
                cur = 3;
            }
            action ReadTo4 when (s4 == 1 && 4 > cur && (fence == 0 || 4 <= fence)) {
                got = got + ((if 1 > cur && s1 == 1 { 1 } else { 0 }) + (if 2 > cur && s2 == 1 { 1 } else { 0 }) + (if 3 > cur && s3 == 1 { 1 } else { 0 }) + (if 4 > cur && s4 == 1 { 1 } else { 0 }));
                rlost = rlost + ((if 1 > cur && s1 == 2 { 1 } else { 0 }) + (if 2 > cur && s2 == 2 { 1 } else { 0 }) + (if 3 > cur && s3 == 2 { 1 } else { 0 }) + (if 4 > cur && s4 == 2 { 1 } else { 0 }));
                told = if ((if 1 > cur && s1 == 3 { 1 } else { 0 }) + (if 2 > cur && s2 == 3 { 1 } else { 0 }) + (if 3 > cur && s3 == 3 { 1 } else { 0 }) + (if 4 > cur && s4 == 3 { 1 } else { 0 })) > 0 { 1 } else { told };
                ov = ov;
                cur = 4;
            }
            action ReadAll when ((if fence > 0 { fence } else { seq }) > cur) {
                got = got + ((if 1 > cur && s1 == 1 && 1 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 2 > cur && s2 == 1 && 2 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 3 > cur && s3 == 1 && 3 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 4 > cur && s4 == 1 && 4 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }));
                rlost = rlost + ((if 1 > cur && s1 == 2 && 1 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 2 > cur && s2 == 2 && 2 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 3 > cur && s3 == 2 && 3 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 4 > cur && s4 == 2 && 4 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }));
                told = if ((if 1 > cur && s1 == 3 && 1 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 2 > cur && s2 == 3 && 2 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 3 > cur && s3 == 3 && 3 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 4 > cur && s4 == 3 && 4 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 })) > 0 { 1 } else { told };
                ov = ov;
                cur = (if fence > 0 { fence } else { seq });
            }
            action ReadSkipsEvicted when (Buggy == 1 && (if fence > 0 { fence } else { seq }) > cur) {
                got = got + ((if 1 > cur && s1 == 1 && 1 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 2 > cur && s2 == 1 && 2 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 3 > cur && s3 == 1 && 3 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 4 > cur && s4 == 1 && 4 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }));
                rlost = rlost;
                told = if ((if 1 > cur && s1 == 3 && 1 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 2 > cur && s2 == 3 && 2 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 3 > cur && s3 == 3 && 3 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 4 > cur && s4 == 3 && 4 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 })) > 0 { 1 } else { told };
                ov = ov;
                cur = (if fence > 0 { fence } else { seq });
            }
            action ReadCountsHole when (Buggy == 1 && (if fence > 0 { fence } else { seq }) > cur) {
                got = got + ((if 1 > cur && s1 == 1 && 1 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 2 > cur && s2 == 1 && 2 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 3 > cur && s3 == 1 && 3 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 4 > cur && s4 == 1 && 4 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }));
                rlost = rlost + ((if 1 > cur && s1 == 2 && 1 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 2 > cur && s2 == 2 && 2 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 3 > cur && s3 == 2 && 3 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 4 > cur && s4 == 2 && 4 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 })) + ((if 1 > cur && s1 == 3 && 1 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 2 > cur && s2 == 3 && 2 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 3 > cur && s3 == 3 && 3 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 4 > cur && s4 == 3 && 4 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }));
                told = if ((if 1 > cur && s1 == 3 && 1 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 2 > cur && s2 == 3 && 2 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 3 > cur && s3 == 3 && 3 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 4 > cur && s4 == 3 && 4 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 })) > 0 { 1 } else { told };
                ov = ov;
                cur = (if fence > 0 { fence } else { seq });
            }
            action ReadIgnoresHole when (Buggy == 1 && (if fence > 0 { fence } else { seq }) > cur) {
                got = got + ((if 1 > cur && s1 == 1 && 1 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 2 > cur && s2 == 1 && 2 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 3 > cur && s3 == 1 && 3 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 4 > cur && s4 == 1 && 4 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }));
                rlost = rlost + ((if 1 > cur && s1 == 2 && 1 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 2 > cur && s2 == 2 && 2 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 3 > cur && s3 == 2 && 3 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }) + (if 4 > cur && s4 == 2 && 4 <= (if fence > 0 { fence } else { seq }) { 1 } else { 0 }));
                told = told;
                ov = ov;
                cur = (if fence > 0 { fence } else { seq });
            }
            action ReadPastFence when (Buggy == 1 && fence > 0 && seq > cur) {
                got = got + ((if 1 > cur && s1 == 1 { 1 } else { 0 }) + (if 2 > cur && s2 == 1 { 1 } else { 0 }) + (if 3 > cur && s3 == 1 { 1 } else { 0 }) + (if 4 > cur && s4 == 1 { 1 } else { 0 })) + ((if 1 > cur && s1 == 4 { 1 } else { 0 }) + (if 2 > cur && s2 == 4 { 1 } else { 0 }) + (if 3 > cur && s3 == 4 { 1 } else { 0 }) + (if 4 > cur && s4 == 4 { 1 } else { 0 }));
                rlost = rlost + ((if 1 > cur && s1 == 2 { 1 } else { 0 }) + (if 2 > cur && s2 == 2 { 1 } else { 0 }) + (if 3 > cur && s3 == 2 { 1 } else { 0 }) + (if 4 > cur && s4 == 2 { 1 } else { 0 }));
                told = if ((if 1 > cur && s1 == 3 { 1 } else { 0 }) + (if 2 > cur && s2 == 3 { 1 } else { 0 }) + (if 3 > cur && s3 == 3 { 1 } else { 0 }) + (if 4 > cur && s4 == 3 { 1 } else { 0 })) > 0 { 1 } else { told };
                ov = ov + ((if 1 > cur && s1 == 4 { 1 } else { 0 }) + (if 2 > cur && s2 == 4 { 1 } else { 0 }) + (if 3 > cur && s3 == 4 { 1 } else { 0 }) + (if 4 > cur && s4 == 4 { 1 } else { 0 }));
                cur = seq;
            }

            invariant NoSilentLoss: cur <= got + rlost + ((if 1 <= cur && s1 == 3 { 1 } else { 0 }) + (if 2 <= cur && s2 == 3 { 1 } else { 0 }) + (if 3 <= cur && s3 == 3 { 1 } else { 0 }) + (if 4 <= cur && s4 == 3 { 1 } else { 0 }));
            invariant HoleNeverCountsAsLoss: rlost <= ((if 1 <= cur && s1 == 2 { 1 } else { 0 }) + (if 2 <= cur && s2 == 2 { 1 } else { 0 }) + (if 3 <= cur && s3 == 2 { 1 } else { 0 }) + (if 4 <= cur && s4 == 2 { 1 } else { 0 }));
            invariant HandoffSaid: ((if 1 <= cur && s1 == 3 { 1 } else { 0 }) + (if 2 <= cur && s2 == 3 { 1 } else { 0 }) + (if 3 <= cur && s3 == 3 { 1 } else { 0 }) + (if 4 <= cur && s4 == 3 { 1 } else { 0 })) <= 0 || told == 1;
            invariant ReaderNeverSeesOverlap: ov <= 0;
            bound StateBounded: seq <= 4 && cur <= 4 && got <= 4 && rlost <= 4 && told <= 1 && ov <= 4 && fence <= 4 && ep <= 1 && s1 <= 4 && s2 <= 4 && s3 <= 4 && s4 <= 4;

        }
    }
}

/// TURN TRACKER: at most one open turn per agent run, driven by the screen
/// verdict (design §2.4).
///
/// `word` is the current verdict: 0 `unknown`, 1 `busy`, 2 `idle`, 3
/// `prompt` with the question dialog, 4 `prompt` with an approval box, 5
/// `wall:<kind>`, 6 `wall:unresponsive`, 7 `survey`, 8 `question` (asked in
/// prose), 9 `-`. `turn` is the open turn and `extra` a second one under the
/// same run; `step` the screen step held open (0 none, 1 approval, 2
/// question, 3 wall, 4 stall, 5 survey) and `sturn` whether it hangs from
/// the turn (a wall or stall seen with no turn hangs from the run). `pend`
/// is an armed settle and `ticks` its elapsed half-seconds (`Settle` = 1 s);
/// `Tick` is time passing and `Rest` the host's wake after the settle (a
/// wake before it changes nothing; a late one is applied by the next
/// verdict first). `CtlTurn` is a `ctl turn` exchange arriving while the
/// agent rests, before the settle: the exchange it starts is the next turn,
/// so the resting one closes at once (`cw = 11`). `cw` is the word that
/// closed the last turn (10 none yet, 11 a ctl exchange, 12 the transcript)
/// and `cticks` how long its resting word had held.
///
/// The transcript (phase 3B, rulings S2 and A6) adds two actions through
/// `TurnTracker::transcript`: `TxClose`, the vendor turn's end closing the
/// open turn when no screen step is open under it (`cw = 12`; a wall on
/// the run stays), and `TxRefute`, the wall judge retracting the screen's
/// wall, which disarms its settle and latches (`rf`) until the screen's
/// word moves (every `See*`). `wg` is the GHOST of a wall's need taken by a
/// transcript close.
///
/// WHO DISMISSED A BOX (phase 5, ruling T1). `bk` is the input bound to the
/// open approval or question step (`step` 1 or 2): 0 none, 1 a control or
/// harness input (who it came from and its `act=` decide `out=`, which the
/// conformance binding reads off the real close), 2 a person's, 3 one
/// accepted more than `STEP_ACT_MS` before now (`Age`: it dismisses
/// nothing). `pd` is the one input in flight: 1 admitted under the open step
/// (`AdmitCtl` or `AdmitHarness` while a step is open; `Accept` binds it), 2 admitted with none
/// open (the lead: the first approval or question step to open inside
/// `STEP_INPUT_LEAD_MS` with no close between is the step it was admitted
/// under, so such an open turns 2 into 1), 3 accepted on the lead (the next
/// such open binds it), 4 dead: its step closed before its acceptance, or
/// its lead passed (`LeadOut`); its acceptance binds nothing. `SeeNextBox` is
/// a box of another word replacing the open one (one `observe` closes box 1
/// and opens box 2; boxes of one word are one step). A close consumes the
/// binding; `Person` binds the open step at once (`pp`: the step's latest
/// input is a person's). With no approval or question step open, a person's
/// input leads as any input does (`PersonLeads`: `pl`, a person's input
/// accepted on the lead, which the next such open binds as the person's,
/// `bk = 2`, or `PersonLeadOut` drops once the lead passed); it is the latest
/// input, so it takes the lead from an act accepted on it (`pd` 3 to 0), and
/// an act accepted on the lead after it takes the lead back (`Accept` from
/// `pd = 2`).
///
/// WHO SENT AN ACT (phase 6, 6A-5). Two acting actors beside the person:
/// `ctl` and the harness, which the real tracker tells apart on the bound
/// input (`by=ctl` closes `out=passed`, an unproven author; `by=harness`
/// reads its `act=`). `pa` is the sender of the input in flight or on the
/// lead (0 none, 1 ctl, 2 harness), set by `AdmitCtl`/`AdmitHarness` and
/// cleared when the input leaves (`pd` to 0); `ba` is the actor the binding
/// names when `bk = 1`, and `bs` the GHOST of who sent the bound act. A
/// binding names the actor who sent it (`ActorKeepsItsAct`), and a
/// harness act is never read as ctl's (`HarnessActNotCtl`): the law is
/// split by direction so each is the one law a swap from its side breaks.
/// At `Buggy = 1` only the harness admits (`AdmitCtl` is off): no law of
/// that family reads who sent an input, and a second sender would double
/// its walk (80 319 states to 177 652) past the interpreter's bound.
///
/// Ghosts: `gn`, an input bound to a step that was
/// neither open at its admission nor the lead's first; `ab`, a binding still
/// held after it attributed a close; `ga`, a close read
/// `approved|declined|answered` with no fresh, proven, acted input bound to
/// it.
///
/// `Buggy = 1` adds one dead mutant per law: a second turn opened without
/// superseding the first (`AtMostOneOpenTurn`), `unknown` read as an end
/// (`UnknownNeverClosesATurn`), `idle` closing the turn before the settle —
/// the 220 ms mid-answer idle Codex shows (`IdleClosesOnlyAfterSettle`), the
/// question dialog closing its turn (`DialogStaysInTurn`), a step opened
/// under a turn that has closed (`StepsNestInTheirTurn`), the input
/// stall closing the turn (`StallNeverClosesATurn`), a turn end closing the
/// turn with the wall under it (`tx_end` without its screen-step test:
/// `WallNeedOutlivesTxClose`), and a refutation that leaves the wall's
/// settle armed (`tx_wall` without its disarm: `RefutedWallStaysShut`).
/// `Buggy = 2` is T1's family, apart so neither multiplies the other's
/// space: an acceptance after box 1 closed that binds box 2
/// (`ActBindsNextBox`: `ActNeverBindsNextBox`), a close past `STEP_ACT_MS`
/// that reads approved (`AttributeStaleInput`: `ApprovalByObservedInput`), a
/// person's input that leaves the harness's act bound (`ActOverPerson`:
/// `PersonSupersedesAct`), a person's input on the lead that leaves an act
/// waiting on it to bind the next box (`ActLeadOverPerson`, the same law: a
/// person's lead and an act's never stand together), a close that keeps
/// its binding for the next box (`ActTwice`: `AttributesOnce`) and a
/// binding that names the other acting actor (`ActorSwap`: from a ctl act
/// `ActorKeepsItsAct`, from a harness act `HarnessActNotCtl`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn turn_tracker_model() -> Model {
    aterm_spec::ty_model! {
        TurnTracker {
            const Settle = 2;
            const Buggy = 0;
            var turn = 0;
            var extra = 0;
            var step = 0;
            var sturn = 0;
            var word = 0;
            var pend = 0;
            var ticks = 0;
            var cw = 10;
            var cticks = 0;
            var rf = 0;
            var wg = 0;
            var bk = 0;
            var pd = 0;
            var pp = 0;
            var ab = 0;
            var gn = 0;
            var ga = 0;
            var pl = 0;
            var pa = 0;
            var ba = 0;
            var bs = 0;

            action SeeBusy {
                bk = if (step == 1 || step == 2) { 0 } else { bk };
                ba = if (step == 1 || step == 2) { 0 } else { ba };
                bs = if (step == 1 || step == 2) { 0 } else { bs };
                pp = if (step == 1 || step == 2) { 0 } else { pp };
                ab = if (step == 1 || step == 2) { 0 } else { ab };
                pd = if (step == 1 || step == 2) && pd == 1 { 4 } else { pd };
                rf = 0;
                turn = 1;
                step = 0;
                sturn = 0;
                pend = 0;
                ticks = 0;
                word = 1;
            }
            action SeeDialog {
                bk = if step == 1 { 0 } else { if (1 > step || step > 2) && pd == 3 { 1 } else { if (1 > step || step > 2) && pl == 1 { 2 } else { bk } } };
                ba = if step == 1 { 0 } else { if (1 > step || step > 2) && pd == 3 { pa } else { if (1 > step || step > 2) && pl == 1 { 0 } else { ba } } };
                bs = if step == 1 { 0 } else { if (1 > step || step > 2) && pd == 3 { pa } else { if (1 > step || step > 2) && pl == 1 { 0 } else { bs } } };
                pp = if step == 1 || ((1 > step || step > 2) && pd == 3) { 0 } else { if (1 > step || step > 2) && pl == 1 { 1 } else { pp } };
                ab = if step == 1 || ((1 > step || step > 2) && (pd == 3 || pl == 1)) { 0 } else { ab };
                pa = if (1 > step || step > 2) && pd == 3 { 0 } else { pa };
                pd = if step == 1 && pd == 1 { 4 } else { if (1 > step || step > 2) && pd == 2 { 1 } else { if (1 > step || step > 2) && pd == 3 { 0 } else { pd } } };
                pl = 0;
                rf = 0;
                turn = 1;
                step = 2;
                sturn = 1;
                pend = 0;
                ticks = 0;
                word = 3;
            }
            action SeeBox {
                bk = if step == 2 { 0 } else { if (1 > step || step > 2) && pd == 3 { 1 } else { if (1 > step || step > 2) && pl == 1 { 2 } else { bk } } };
                ba = if step == 2 { 0 } else { if (1 > step || step > 2) && pd == 3 { pa } else { if (1 > step || step > 2) && pl == 1 { 0 } else { ba } } };
                bs = if step == 2 { 0 } else { if (1 > step || step > 2) && pd == 3 { pa } else { if (1 > step || step > 2) && pl == 1 { 0 } else { bs } } };
                pp = if step == 2 || ((1 > step || step > 2) && pd == 3) { 0 } else { if (1 > step || step > 2) && pl == 1 { 1 } else { pp } };
                ab = if step == 2 || ((1 > step || step > 2) && (pd == 3 || pl == 1)) { 0 } else { ab };
                pa = if (1 > step || step > 2) && pd == 3 { 0 } else { pa };
                pd = if step == 2 && pd == 1 { 4 } else { if (1 > step || step > 2) && pd == 2 { 1 } else { if (1 > step || step > 2) && pd == 3 { 0 } else { pd } } };
                pl = 0;
                rf = 0;
                turn = 1;
                step = 1;
                sturn = 1;
                pend = 0;
                ticks = 0;
                word = 4;
            }
            action SeeNextBox when (step == 1) {
                bk = 0;
                ba = 0;
                bs = 0;
                pp = 0;
                ab = 0;
                pd = if pd == 1 { 4 } else { pd };
                rf = 0;
                turn = 1;
                step = 1;
                sturn = 1;
                pend = 0;
                ticks = 0;
                word = 4;
            }
            action SeeWall {
                bk = if (step == 1 || step == 2) { 0 } else { bk };
                ba = if (step == 1 || step == 2) { 0 } else { ba };
                bs = if (step == 1 || step == 2) { 0 } else { bs };
                pp = if (step == 1 || step == 2) { 0 } else { pp };
                ab = if (step == 1 || step == 2) { 0 } else { ab };
                pd = if (step == 1 || step == 2) && pd == 1 { 4 } else { pd };
                rf = 0;
                step = 3;
                sturn = turn;
                pend = turn;
                ticks = 0;
                word = 5;
            }
            action SeeStall {
                bk = if (step == 1 || step == 2) { 0 } else { bk };
                ba = if (step == 1 || step == 2) { 0 } else { ba };
                bs = if (step == 1 || step == 2) { 0 } else { bs };
                pp = if (step == 1 || step == 2) { 0 } else { pp };
                ab = if (step == 1 || step == 2) { 0 } else { ab };
                pd = if (step == 1 || step == 2) && pd == 1 { 4 } else { pd };
                rf = 0;
                step = 4;
                sturn = turn;
                pend = 0;
                ticks = 0;
                word = 6;
            }
            action SeeSurvey {
                bk = if (step == 1 || step == 2) { 0 } else { bk };
                ba = if (step == 1 || step == 2) { 0 } else { ba };
                bs = if (step == 1 || step == 2) { 0 } else { bs };
                pp = if (step == 1 || step == 2) { 0 } else { pp };
                ab = if (step == 1 || step == 2) { 0 } else { ab };
                pd = if (step == 1 || step == 2) && pd == 1 { 4 } else { pd };
                rf = 0;
                step = if turn == 1 { 5 } else { 0 };
                sturn = turn;
                pend = turn;
                ticks = 0;
                word = 7;
            }
            action SeeIdle {
                bk = if (step == 1 || step == 2) { 0 } else { bk };
                ba = if (step == 1 || step == 2) { 0 } else { ba };
                bs = if (step == 1 || step == 2) { 0 } else { bs };
                pp = if (step == 1 || step == 2) { 0 } else { pp };
                ab = if (step == 1 || step == 2) { 0 } else { ab };
                pd = if (step == 1 || step == 2) && pd == 1 { 4 } else { pd };
                rf = 0;
                step = 0;
                sturn = 0;
                pend = turn;
                ticks = 0;
                word = 2;
            }
            action SeeQuestion {
                bk = if (step == 1 || step == 2) { 0 } else { bk };
                ba = if (step == 1 || step == 2) { 0 } else { ba };
                bs = if (step == 1 || step == 2) { 0 } else { bs };
                pp = if (step == 1 || step == 2) { 0 } else { pp };
                ab = if (step == 1 || step == 2) { 0 } else { ab };
                pd = if (step == 1 || step == 2) && pd == 1 { 4 } else { pd };
                rf = 0;
                step = 0;
                sturn = 0;
                pend = turn;
                ticks = 0;
                word = 8;
            }
            action SeeGone {
                bk = if (step == 1 || step == 2) { 0 } else { bk };
                ba = if (step == 1 || step == 2) { 0 } else { ba };
                bs = if (step == 1 || step == 2) { 0 } else { bs };
                pp = if (step == 1 || step == 2) { 0 } else { pp };
                ab = if (step == 1 || step == 2) { 0 } else { ab };
                pd = if (step == 1 || step == 2) && pd == 1 { 4 } else { pd };
                rf = 0;
                cw = if turn == 1 { 9 } else { cw };
                cticks = if turn == 1 { 0 } else { cticks };
                turn = 0;
                step = 0;
                sturn = 0;
                pend = 0;
                ticks = 0;
                word = 9;
            }
            action SeeUnknown {
                rf = 0;
                pend = 0;
                ticks = 0;
                word = 0;
            }
            action Tick when (pend == 1 && ticks <= Settle - 1) {
                ticks = ticks + 1;
            }
            action Rest when (pend == 1 && Settle <= ticks) {
                turn = 0;
                cw = word;
                cticks = ticks;
                pend = 0;
                ticks = 0;
                step = if word == 5 { 3 } else { 0 };
                sturn = 0;
            }
            action CtlTurn when (pend == 1 && ticks <= Settle - 1) {
                turn = 0;
                cw = 11;
                cticks = ticks;
                pend = 0;
                ticks = 0;
                step = if word == 5 { 3 } else { 0 };
                sturn = 0;
            }
            action TxClose when (turn == 1 && (step == 0 || sturn == 0)) {
                turn = 0;
                cw = 12;
                cticks = 0;
                pend = 0;
                ticks = 0;
                sturn = 0;
            }
            action TxRefute when (step == 3) {
                step = 0;
                sturn = 0;
                pend = 0;
                ticks = 0;
                rf = 1;
            }

            action AdmitCtl when (pd == 0 && (1 > Buggy || Buggy > 1)) {
                pa = 1;
                pd = if (step == 1 || step == 2) { 1 } else { 2 };
            }
            action AdmitHarness when (pd == 0) {
                pa = 2;
                pd = if (step == 1 || step == 2) { 1 } else { 2 };
            }
            action Accept when (pd == 1 || pd == 2 || pd == 4) {
                bk = if pd == 1 { 1 } else { bk };
                ba = if pd == 1 { pa } else { ba };
                bs = if pd == 1 { pa } else { bs };
                pp = if pd == 1 { 0 } else { pp };
                ab = if pd == 1 { 0 } else { ab };
                pl = if pd == 2 { 0 } else { pl };
                pa = if pd == 2 { pa } else { 0 };
                pd = if pd == 2 { 3 } else { 0 };
            }
            action Person when (step == 1 || step == 2) {
                bk = 2;
                ba = 0;
                bs = 0;
                pp = 1;
                ab = 0;
            }
            action PersonLeads when (1 > step || step > 2) {
                pl = 1;
                pa = if pd == 3 { 0 } else { pa };
                pd = if pd == 3 { 0 } else { pd };
            }
            action Age when (bk == 1 || bk == 2) {
                bk = 3;
                ba = 0;
                bs = 0;
                pp = 0;
            }
            action LeadOut when (pd == 2 || pd == 3) {
                pa = if pd == 2 { pa } else { 0 };
                pd = if pd == 2 { 4 } else { 0 };
            }
            action PersonLeadOut when (pl == 1) {
                pl = 0;
            }

            action SecondTurn when (Buggy == 1 && turn == 1 && extra == 0) {
                extra = 1;
            }
            action UnknownCloses when (Buggy == 1 && turn == 1 && word > 0) {
                turn = 0;
                cw = 0;
                cticks = 0;
                step = 0;
                sturn = 0;
                pend = 0;
                ticks = 0;
                word = 0;
            }
            action EarlyRest when (Buggy == 1 && pend == 1 && word == 2 && Settle > ticks) {
                turn = 0;
                cw = 2;
                cticks = ticks;
                pend = 0;
                ticks = 0;
                step = 0;
                sturn = 0;
            }
            action DialogCloses when (Buggy == 1 && turn == 1 && step == 0) {
                turn = 0;
                cw = 3;
                cticks = 0;
                step = 0;
                sturn = 0;
                pend = 0;
                ticks = 0;
                word = 3;
            }
            action StepAfterClose when (Buggy == 1 && turn == 0 && step == 0 && 10 > cw) {
                step = 1;
                sturn = 1;
            }
            action StallCloses when (Buggy == 1 && turn == 1 && step == 0) {
                turn = 0;
                cw = 6;
                cticks = 0;
                step = 4;
                sturn = 0;
                pend = 0;
                ticks = 0;
                word = 6;
            }
            action TxCloseTakesWall when (Buggy == 1 && turn == 1 && step == 3 && sturn == 1) {
                turn = 0;
                cw = 12;
                cticks = 0;
                step = 0;
                sturn = 0;
                pend = 0;
                ticks = 0;
                wg = 1;
            }
            action RefuteKeepsSettle when (Buggy == 1 && step == 3 && pend == 1) {
                step = 0;
                sturn = 0;
                rf = 1;
            }

            action ActBindsNextBox when (Buggy == 2 && pd == 4 && step == 1) {
                bk = 1;
                ba = pa;
                bs = pa;
                pp = 0;
                ab = 0;
                pa = 0;
                pd = 0;
                gn = 1;
            }
            action AttributeStaleInput when (Buggy == 2 && bk == 3 && step == 1) {
                bk = 0;
                ba = 0;
                bs = 0;
                pp = 0;
                ab = 0;
                ga = 1;
                pd = if pd == 1 { 4 } else { pd };
                rf = 0;
                turn = 1;
                step = 0;
                sturn = 0;
                pend = 0;
                ticks = 0;
                word = 1;
            }
            action ActOverPerson when (Buggy == 2 && bk == 1 && (step == 1 || step == 2)) {
                pp = 1;
            }
            action ActLeadOverPerson when (Buggy == 2 && (1 > step || step > 2) && pd == 3) {
                pl = 1;
            }
            action ActTwice when (Buggy == 2 && step == 1 && ab == 0 && (bk == 1 || bk == 2)) {
                ab = 1;
                pd = if pd == 1 { 4 } else { pd };
                rf = 0;
                turn = 1;
                step = 1;
                sturn = 1;
                pend = 0;
                ticks = 0;
                word = 4;
            }
            action ActorSwap when (Buggy == 2 && bk == 1) {
                ba = 3 - ba;
            }

            invariant AtMostOneOpenTurn: turn + extra <= 1;
            invariant UnknownNeverClosesATurn: cw > 0;
            invariant IdleClosesOnlyAfterSettle: 2 > cw || cw > 2 || Settle <= cticks;
            invariant DialogStaysInTurn: 3 > word || word > 3 || turn == 1;
            invariant StepsNestInTheirTurn: sturn <= turn && sturn <= step;
            invariant StallNeverClosesATurn: 6 > cw || cw > 6;
            invariant WallNeedOutlivesTxClose: wg == 0;
            invariant RefutedWallStaysShut: rf == 0 || ((3 > step || step > 3) && pend == 0);
            invariant ActNeverBindsNextBox: gn == 0;
            invariant AttributesOnce: ab == 0;
            invariant ApprovalByObservedInput: ga == 0;
            invariant PersonSupersedesAct: (bk == 2 || pp == 0) && (pl == 0 || 3 > pd || pd > 3);
            invariant ActorKeepsItsAct: 1 > bk || bk > 1 || bs == 2 || ba == bs;
            invariant HarnessActNotCtl: 1 > bk || bk > 1 || 2 > bs || bs > 2 || ba == 2;
        }
    }
}

/// SESSION LOG JOURNAL VIEW (phase 2B): what a `log @<sid>` answered from the
/// disk journal shows, over three seqs of one session's space: the session's
/// open (seq 1), a run's under it (2) and a turn's under the run (3). The
/// producer appends them (`Append`: `lg` of them in the log) and the writer,
/// behind it, handles them in seq order: each is journaled (`Write`) or said
/// lost by a gap frame (`Lose`: the ring evicted it before the writer reached
/// it; never one a snap restated, an open node's record, which the ring
/// pins), and any journaled line may be journaled again (`Again<i>`: an
/// update's overlap, a stopped writer's successor, a chain restated). `Snap`
/// is a snap the writer writes at its cursor `nxt`, behind the log or not (a
/// new segment, a rollback's restatement), restating the open path as of
/// that head; `sb` remembers the log's head at a snap written behind it.
/// `Hand` is an update's `#handed` for the session, written once the drain
/// reached the fence (`hd`); a later record, gap frame or snap voids it, and
/// `sd` says a snap did (an update that stood down). `Crash` is the chain's
/// writer dying with no `#fin` (its lock free), with whatever backlog it had.
/// `Read` is the reader's view of the journal as it stands: `v<i>` how many
/// times seq `i` is shown, `vlost` the losses it says, `vs`/`vr`/`vt` whether
/// the session, run and turn read open, `cs`/`cr`/`ct` the place of each
/// one's synthetic close among the view's closes (0: none), which the reader
/// derives at read time for a dead chain that did not hand the session on
/// (ruling R1), and `ca` the `after=` of its `gap why=crash` (0: none). Any
/// change to the journal makes an earlier view stale (`rd = 0`, the view's
/// variables cleared).
///
/// Laws (ruling R12): `ViewAtMostOnce` (each `(sid, space, seq)` once in any
/// reply), `DeadViewClosed` (every tree in a dead chain's view is closed,
/// unless its last line for the session hands it on: a rolled-back update's
/// is closed), `ChildClosesFirst` (closes innermost first: ORDER, not counts;
/// no child closes after its parent), `LossSaid` (every loss the journal
/// knows is said as a gap) and `CrashAfterHead` (the crash gap says
/// `after=` the last seq the journal accounted for, never a seq a snap
/// restated past it, nor the last one it holds a record of).
///
/// `Buggy = 1` adds one dead mutant per law: a read that shows a line once
/// per time it was journaled (`ViewAtMostOnce`), one that leaves a dead
/// chain's tree open (`DeadViewClosed`), one that keeps a `#handed` a later
/// snap voided (`DeadViewClosed`), one that closes the parent first
/// (`ChildClosesFirst`), one that numbers what it holds densely and says no
/// loss (`LossSaid`), and the writer and reader before phase 2B's fixes, a
/// snap written behind restating the open path past its head and a reader
/// counting it toward the head (`CrashAfterHead`). Tier-1: aterm-slog's
/// `slog_journal_view_conforms_to_derived_model`, through the real
/// `journal::take`, `journal::take_frames` and `journal::view_of`, with a
/// negative control per mutant.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn session_log_journal_model() -> Model {
    aterm_spec::ty_model! {
        SessionLogJournal {
            const Buggy = 0;
            var nxt = 0;
            var lg = 0;
            var j1 = 0;
            var j2 = 0;
            var j3 = 0;
            var l2 = 0;
            var l3 = 0;
            var dead = 0;
            var hd = 0;
            var sd = 0;
            var sb = 0;
            var rd = 0;
            var v1 = 0;
            var v2 = 0;
            var v3 = 0;
            var vlost = 0;
            var vs = 0;
            var vr = 0;
            var vt = 0;
            var cs = 0;
            var cr = 0;
            var ct = 0;
            var ca = 0;

            action Append when (dead == 0 && 3 > lg) {
                lg = lg + 1;
            }
            action Write when (dead == 0 && lg > nxt) {
                j1 = if nxt == 0 { 1 } else { j1 };
                j2 = if nxt == 1 { 1 } else { j2 };
                j3 = if nxt == 2 { 1 } else { j3 };
                nxt = nxt + 1;
                hd = 0;
                sd = 0;
                rd = 0; v1 = 0; v2 = 0; v3 = 0; vlost = 0; vs = 0; vr = 0; vt = 0; cs = 0; cr = 0; ct = 0; ca = 0;
            }
            action Lose when (dead == 0 && nxt > 0 && lg > nxt && sb <= nxt) {
                l2 = if nxt == 1 { 1 } else { l2 };
                l3 = if nxt == 2 { 1 } else { l3 };
                nxt = nxt + 1;
                hd = 0;
                sd = 0;
                rd = 0; v1 = 0; v2 = 0; v3 = 0; vlost = 0; vs = 0; vr = 0; vt = 0; cs = 0; cr = 0; ct = 0; ca = 0;
            }
            action Again1 when (dead == 0 && j1 == 1) {
                j1 = 2;
                hd = 0;
                sd = 0;
                rd = 0; v1 = 0; v2 = 0; v3 = 0; vlost = 0; vs = 0; vr = 0; vt = 0; cs = 0; cr = 0; ct = 0; ca = 0;
            }
            action Again2 when (dead == 0 && j2 == 1) {
                j2 = 2;
                hd = 0;
                sd = 0;
                rd = 0; v1 = 0; v2 = 0; v3 = 0; vlost = 0; vs = 0; vr = 0; vt = 0; cs = 0; cr = 0; ct = 0; ca = 0;
            }
            action Again3 when (dead == 0 && j3 == 1) {
                j3 = 2;
                hd = 0;
                sd = 0;
                rd = 0; v1 = 0; v2 = 0; v3 = 0; vlost = 0; vs = 0; vr = 0; vt = 0; cs = 0; cr = 0; ct = 0; ca = 0;
            }
            action Snap when (dead == 0 && nxt > 0 && l2 + l3 == 0) {
                sd = if hd == 1 { 1 } else { sd };
                hd = 0;
                sb = if lg > nxt { lg } else { sb };
                rd = 0; v1 = 0; v2 = 0; v3 = 0; vlost = 0; vs = 0; vr = 0; vt = 0; cs = 0; cr = 0; ct = 0; ca = 0;
            }
            action Hand when (dead == 0 && nxt > 0 && lg == nxt && hd == 0) {
                hd = 1;
                rd = 0; v1 = 0; v2 = 0; v3 = 0; vlost = 0; vs = 0; vr = 0; vt = 0; cs = 0; cr = 0; ct = 0; ca = 0;
            }
            action Crash when (dead == 0 && nxt > 0) {
                dead = 1;
                rd = 0; v1 = 0; v2 = 0; v3 = 0; vlost = 0; vs = 0; vr = 0; vt = 0; cs = 0; cr = 0; ct = 0; ca = 0;
            }
            action Read when (nxt > 0) {
                rd = 1;
                v1 = if j1 > 0 { 1 } else { 0 };
                v2 = if j2 > 0 { 1 } else { 0 };
                v3 = if j3 > 0 { 1 } else { 0 };
                vlost = l2 + l3;
                vs = if dead == 1 && hd == 0 { 0 } else { 1 };
                vr = if dead == 1 && hd == 0 { 0 } else { if j2 > 0 { 1 } else { 0 } };
                vt = if dead == 1 && hd == 0 { 0 } else { if j3 > 0 { 1 } else { 0 } };
                ct = if dead == 1 && hd == 0 && j3 > 0 { 1 } else { 0 };
                cr = if dead == 1 && hd == 0 && j2 > 0 { 1 + (if j3 > 0 { 1 } else { 0 }) } else { 0 };
                cs = if dead == 1 && hd == 0 { 1 + (if j3 > 0 { 1 } else { 0 }) + (if j2 > 0 { 1 } else { 0 }) } else { 0 };
                ca = if dead == 1 && hd == 0 { nxt } else { 0 };
            }
            action ReadNoDedupe when (Buggy == 1 && nxt > 0) {
                rd = 1;
                v1 = j1;
                v2 = j2;
                v3 = j3;
                vlost = l2 + l3;
                vs = if dead == 1 && hd == 0 { 0 } else { 1 };
                vr = if dead == 1 && hd == 0 { 0 } else { if j2 > 0 { 1 } else { 0 } };
                vt = if dead == 1 && hd == 0 { 0 } else { if j3 > 0 { 1 } else { 0 } };
                ct = if dead == 1 && hd == 0 && j3 > 0 { 1 } else { 0 };
                cr = if dead == 1 && hd == 0 && j2 > 0 { 1 + (if j3 > 0 { 1 } else { 0 }) } else { 0 };
                cs = if dead == 1 && hd == 0 { 1 + (if j3 > 0 { 1 } else { 0 }) + (if j2 > 0 { 1 } else { 0 }) } else { 0 };
                ca = if dead == 1 && hd == 0 { nxt } else { 0 };
            }
            action ReadNoCrashClose when (Buggy == 1 && nxt > 0 && dead == 1 && hd == 0) {
                rd = 1;
                v1 = if j1 > 0 { 1 } else { 0 };
                v2 = if j2 > 0 { 1 } else { 0 };
                v3 = if j3 > 0 { 1 } else { 0 };
                vlost = l2 + l3;
                vs = 1;
                vr = if j2 > 0 { 1 } else { 0 };
                vt = if j3 > 0 { 1 } else { 0 };
                ct = 0;
                cr = 0;
                cs = 0;
                ca = 0;
            }
            action ReadHandedSticky when (Buggy == 1 && nxt > 0 && dead == 1 && hd == 0 && sd == 1) {
                rd = 1;
                v1 = if j1 > 0 { 1 } else { 0 };
                v2 = if j2 > 0 { 1 } else { 0 };
                v3 = if j3 > 0 { 1 } else { 0 };
                vlost = l2 + l3;
                vs = 1;
                vr = if j2 > 0 { 1 } else { 0 };
                vt = if j3 > 0 { 1 } else { 0 };
                ct = 0;
                cr = 0;
                cs = 0;
                ca = 0;
            }
            action ReadParentFirst when (Buggy == 1 && nxt > 0 && dead == 1 && hd == 0) {
                rd = 1;
                v1 = if j1 > 0 { 1 } else { 0 };
                v2 = if j2 > 0 { 1 } else { 0 };
                v3 = if j3 > 0 { 1 } else { 0 };
                vlost = l2 + l3;
                vs = 0;
                vr = 0;
                vt = 0;
                cs = 1;
                cr = if j2 > 0 { 2 } else { 0 };
                ct = if j3 > 0 { 2 + (if j2 > 0 { 1 } else { 0 }) } else { 0 };
                ca = nxt;
            }
            action ReadHidesLoss when (Buggy == 1 && nxt > 0) {
                rd = 1;
                v1 = if j1 > 0 { 1 } else { 0 };
                v2 = if j2 > 0 { 1 } else { 0 };
                v3 = if j3 > 0 { 1 } else { 0 };
                vlost = 0;
                vs = if dead == 1 && hd == 0 { 0 } else { 1 };
                vr = if dead == 1 && hd == 0 { 0 } else { if j2 > 0 { 1 } else { 0 } };
                vt = if dead == 1 && hd == 0 { 0 } else { if j3 > 0 { 1 } else { 0 } };
                ct = if dead == 1 && hd == 0 && j3 > 0 { 1 } else { 0 };
                cr = if dead == 1 && hd == 0 && j2 > 0 { 1 + (if j3 > 0 { 1 } else { 0 }) } else { 0 };
                cs = if dead == 1 && hd == 0 { 1 + (if j3 > 0 { 1 } else { 0 }) + (if j2 > 0 { 1 } else { 0 }) } else { 0 };
                ca = if dead == 1 && hd == 0 { nxt } else { 0 };
            }
            action ReadAfterRestated when (Buggy == 1 && nxt > 0 && dead == 1 && hd == 0 && sb > nxt) {
                rd = 1;
                v1 = if j1 > 0 { 1 } else { 0 };
                v2 = if j2 > 0 { 1 } else { 0 };
                v3 = if j3 > 0 { 1 } else { 0 };
                vlost = l2 + l3;
                vs = 0;
                vr = 0;
                vt = 0;
                ct = if j3 > 0 { 1 } else { 0 };
                cr = if j2 > 0 { 1 + (if j3 > 0 { 1 } else { 0 }) } else { 0 };
                cs = 1 + (if j3 > 0 { 1 } else { 0 }) + (if j2 > 0 { 1 } else { 0 });
                ca = sb;
            }

            invariant ViewAtMostOnce: v1 <= 1 && v2 <= 1 && v3 <= 1;
            invariant DeadViewClosed: rd == 0 || dead == 0 || hd == 1 || (vs == 0 && vr == 0 && vt == 0);
            invariant ChildClosesFirst: (ct == 0 || cr == 0 || cr > ct) && (cr == 0 || cs == 0 || cs > cr) && (ct == 0 || cs == 0 || cs > ct);
            invariant LossSaid: rd == 0 || l2 + l3 <= vlost;
            invariant CrashAfterHead: rd == 0 || ca == 0 || ca == nxt;
            bound StateBounded: nxt <= 3 && lg <= 3 && j1 <= 2 && j2 <= 2 && j3 <= 2 && l2 <= 1 && l3 <= 1 && dead <= 1 && hd <= 1 && sd <= 1 && sb <= 3 && rd <= 1 && v1 <= 2 && v2 <= 2 && v3 <= 2 && vlost <= 3 && vs <= 1 && vr <= 1 && vt <= 1 && cs <= 3 && cr <= 3 && ct <= 3 && ca <= 3;
        }
    }
}

/// SESSION LOG MARK QUOTA (phase 3A, design §7.4; ruling R4): the claims a
/// consumer writes back (`mark`, `src=mark`) hold at most a quarter of a
/// ring's capacity, and the quota keeps that by taking only marks, the
/// oldest first, never the record just appended and never one the readers'
/// fence holds during an update's overlap.
///
/// Variables: `seq` the last seq assigned, `lost` the records the log
/// counts lost; live records split by whether the fence holds them: `uo`/`fo`
/// observations, `um`/`fm` marks; `fence` whether readers are fenced; `mq`
/// the quota in records (2, or 1 once the byte budget is lowered); `nwm`
/// whether the newest record is a mark and `nwf` whether it is fenced (no
/// eviction takes it); `qobs` the observations the quota took and `fgone` the
/// fenced records anything took (both ghosts).
///
/// `AppendMark` is `aterm_slog::Log::enforce_mark_quota` inline: it takes as
/// many older unfenced marks as the live marks pass `mq` by (at most all of
/// them). `Lift` (the fence lifted) and `LowerBudget` run the quota again.
/// `EvictObs`/`EvictMark` are the level-aware batch, whose order the ring
/// model holds: here it may take any unfenced record but the newest.
///
/// Laws: `MarkQuota`: the marks pass the quota only while the fence holds
/// some, and then by the newest mark alone; `NoSilentLoss`: every seq is
/// live or counted lost; `QuotaTakesOnlyMarks`; `FenceHolds` (R4).
///
/// `Buggy = 1` adds one dead mutant per law: a mark appended without the
/// quota (`MarkQuota`), a quota that takes an observation too
/// (`QuotaTakesOnlyMarks`), one whose taking is uncounted (`NoSilentLoss`),
/// and one that takes a fenced mark (`FenceHolds`). Tier-1: aterm-slog's
/// `slog_mark_quota_conforms_to_derived_model`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn session_log_mark_quota_model() -> Model {
    aterm_spec::ty_model! {
        SessionLogMarkQuota {
            const Buggy = 0;
            const MaxSeq = 7;
            var seq = 0;
            var lost = 0;
            var uo = 0;
            var fo = 0;
            var um = 0;
            var fm = 0;
            var fence = 0;
            var mq = 2;
            var nwm = 0;
            var nwf = 0;
            var qobs = 0;
            var fgone = 0;

            action AppendObs when (MaxSeq > seq) {
                uo = uo + 1;
                seq = seq + 1;
                nwm = 0;
                nwf = 0;
            }
            action AppendMark when (MaxSeq > seq) {
                lost = lost + (if fm + um + 1 > mq { if um > fm + um + 1 - mq { fm + um + 1 - mq } else { um } } else { 0 });
                um = um + 1 - (if fm + um + 1 > mq { if um > fm + um + 1 - mq { fm + um + 1 - mq } else { um } } else { 0 });
                seq = seq + 1;
                nwm = 1;
                nwf = 0;
            }
            action Fence when (fence == 0) {
                fo = fo + uo;
                fm = fm + um;
                uo = 0;
                um = 0;
                fence = 1;
                nwf = if seq > 0 { 1 } else { 0 };
            }
            action Lift when (fence == 1) {
                lost = lost + (if fm + um > mq { if fm + um - nwm > fm + um - mq { fm + um - mq } else { fm + um - nwm } } else { 0 });
                um = fm + um - (if fm + um > mq { if fm + um - nwm > fm + um - mq { fm + um - mq } else { fm + um - nwm } } else { 0 });
                uo = uo + fo;
                fo = 0;
                fm = 0;
                fence = 0;
                nwf = 0;
            }
            action LowerBudget when (mq == 2) {
                lost = lost + (if fm + um > 1 { if um - (if nwm == 1 && nwf == 0 { 1 } else { 0 }) > fm + um - 1 { fm + um - 1 } else { um - (if nwm == 1 && nwf == 0 { 1 } else { 0 }) } } else { 0 });
                um = um - (if fm + um > 1 { if um - (if nwm == 1 && nwf == 0 { 1 } else { 0 }) > fm + um - 1 { fm + um - 1 } else { um - (if nwm == 1 && nwf == 0 { 1 } else { 0 }) } } else { 0 });
                mq = 1;
            }
            action EvictObs when (uo > (if nwm == 0 && nwf == 0 { 1 } else { 0 })) {
                uo = uo - 1;
                lost = lost + 1;
            }
            action EvictMark when (um > (if nwm == 1 && nwf == 0 { 1 } else { 0 })) {
                um = um - 1;
                lost = lost + 1;
            }
            action AppendMarkUnquotaed when (Buggy == 1 && MaxSeq > seq) {
                um = um + 1;
                seq = seq + 1;
                nwm = 1;
                nwf = 0;
            }
            action QuotaTakesObs when (Buggy == 1 && MaxSeq > seq && uo > 0) {
                lost = lost + 1 + (if fm + um + 1 > mq { if um > fm + um + 1 - mq { fm + um + 1 - mq } else { um } } else { 0 });
                um = um + 1 - (if fm + um + 1 > mq { if um > fm + um + 1 - mq { fm + um + 1 - mq } else { um } } else { 0 });
                uo = uo - 1;
                qobs = qobs + 1;
                seq = seq + 1;
                nwm = 1;
                nwf = 0;
            }
            action QuotaUncounted when (Buggy == 1 && MaxSeq > seq) {
                um = um + 1 - (if fm + um + 1 > mq { if um > fm + um + 1 - mq { fm + um + 1 - mq } else { um } } else { 0 });
                seq = seq + 1;
                nwm = 1;
                nwf = 0;
            }
            action QuotaTakesFenced when (Buggy == 1 && MaxSeq > seq && fm > 0) {
                fm = fm - 1;
                fgone = fgone + 1;
                lost = lost + 1;
                um = um + 1;
                seq = seq + 1;
                nwm = 1;
                nwf = 0;
            }

            invariant MarkQuota: fm + um <= mq || um <= 1;
            invariant NoSilentLoss: lost + uo + fo + um + fm == seq;
            invariant QuotaTakesOnlyMarks: qobs == 0;
            invariant FenceHolds: fgone == 0;
            bound StateBounded: seq <= MaxSeq && lost <= MaxSeq && uo <= MaxSeq && fo <= MaxSeq && um <= MaxSeq && fm <= MaxSeq && fence <= 1 && mq <= 2 && nwm <= 1 && nwf <= 1 && qobs <= MaxSeq && fgone <= MaxSeq;
        }
    }
}

/// SESSION LOG PROGRAM SHARE (phase 6, 6F-3; T6, ruling U17): the program
/// marks (OSC 7501's reports) hanging from one node hold at most its share of
/// the ring, and the share keeps that by taking only that node's program
/// marks, the oldest first, never the record just appended and never one the
/// readers' fence holds, each counted lost: a flapping program spends its own
/// share, never another node's nor the facts beside it.
///
/// Variables: `seq` the last seq assigned, `lost` the records the log counts
/// lost; live records split by whether the fence holds them: `uo`/`fo` other
/// records (a mail mark, a bell), `ua`/`fa` the program marks of node `a` (a
/// run), `ub`/`fb` those of node `b` (the session, or another run); `fence`
/// whether readers are fenced; `nwp` which node's program mark the newest
/// record is (0 none, 1 `a`, 2 `b`) and `nwf` whether it is fenced (no
/// eviction takes it); `qoth` the records the share took that were not the
/// over-share node's program marks, and `fgone` the fenced records anything
/// took (both ghosts). `Ps` is the share (64 at a session's bound; 2 here,
/// a 64-record ring's).
///
/// `AppendA`/`AppendB` are `aterm_slog::Log::enforce_program_share` inline:
/// they take as many of the node's older unfenced program marks as its live
/// ones pass `Ps` by (at most all of them). `Lift` (the fence lifted) runs it
/// again for both nodes. `EvictOther`/`EvictA`/`EvictB` are the level-aware
/// batch, whose order the ring model holds: here it may take any unfenced
/// record but the newest.
///
/// Laws: `ProgramShare`: a node's program marks pass the share only while
/// the fence holds some, and then by the newest alone; `NoSilentLoss`: every
/// seq is live or counted lost; `ShareTakesOnlyProgram`: the share takes the
/// over-share node's program marks and nothing else; `FenceHolds` (R4).
///
/// `Buggy = 1` adds one dead mutant per law: a program mark appended
/// without the share (`ProgramShare`), a share that evicts any record, an
/// observation (`ProgramShareEvictsAny`) or the other node's program mark
/// (`ShareTakesOtherNode`), both `ShareTakesOnlyProgram`'s, one whose taking
/// is uncounted (`NoSilentLoss`), and one that takes a fenced program mark
/// (`FenceHolds`). Tier-1: aterm-slog's
/// `slog_program_share_conforms_to_derived_model`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn session_log_program_share_model() -> Model {
    aterm_spec::ty_model! {
        SessionLogProgramShare {
            const Buggy = 0;
            const MaxSeq = 7;
            const Ps = 2;
            var seq = 0;
            var lost = 0;
            var uo = 0;
            var fo = 0;
            var ua = 0;
            var fa = 0;
            var ub = 0;
            var fb = 0;
            var fence = 0;
            var nwp = 0;
            var nwf = 0;
            var qoth = 0;
            var fgone = 0;

            action AppendOther when (MaxSeq > seq) {
                uo = uo + 1;
                seq = seq + 1;
                nwp = 0;
                nwf = 0;
            }
            action AppendA when (MaxSeq > seq) {
                lost = lost + (if fa + ua + 1 > Ps { if ua > fa + ua + 1 - Ps { fa + ua + 1 - Ps } else { ua } } else { 0 });
                ua = ua + 1 - (if fa + ua + 1 > Ps { if ua > fa + ua + 1 - Ps { fa + ua + 1 - Ps } else { ua } } else { 0 });
                seq = seq + 1;
                nwp = 1;
                nwf = 0;
            }
            action AppendB when (MaxSeq > seq) {
                lost = lost + (if fb + ub + 1 > Ps { if ub > fb + ub + 1 - Ps { fb + ub + 1 - Ps } else { ub } } else { 0 });
                ub = ub + 1 - (if fb + ub + 1 > Ps { if ub > fb + ub + 1 - Ps { fb + ub + 1 - Ps } else { ub } } else { 0 });
                seq = seq + 1;
                nwp = 2;
                nwf = 0;
            }
            action Fence when (fence == 0) {
                fo = fo + uo;
                fa = fa + ua;
                fb = fb + ub;
                uo = 0;
                ua = 0;
                ub = 0;
                fence = 1;
                nwf = if seq > 0 { 1 } else { 0 };
            }
            action Lift when (fence == 1) {
                lost = lost + (if fa + ua > Ps { fa + ua - Ps } else { 0 }) + (if fb + ub > Ps { fb + ub - Ps } else { 0 });
                ua = if fa + ua > Ps { Ps } else { fa + ua };
                ub = if fb + ub > Ps { Ps } else { fb + ub };
                uo = uo + fo;
                fo = 0;
                fa = 0;
                fb = 0;
                fence = 0;
                nwf = 0;
            }
            action EvictOther when (uo > (if nwp == 0 && nwf == 0 { 1 } else { 0 })) {
                uo = uo - 1;
                lost = lost + 1;
            }
            action EvictA when (ua > (if nwp == 1 && nwf == 0 { 1 } else { 0 })) {
                ua = ua - 1;
                lost = lost + 1;
            }
            action EvictB when (ub > (if nwp == 2 && nwf == 0 { 1 } else { 0 })) {
                ub = ub - 1;
                lost = lost + 1;
            }
            action AppendAUnshared when (Buggy == 1 && MaxSeq > seq) {
                ua = ua + 1;
                seq = seq + 1;
                nwp = 1;
                nwf = 0;
            }
            action ProgramShareEvictsAny when (Buggy == 1 && MaxSeq > seq && uo > 0) {
                lost = lost + 1 + (if fa + ua + 1 > Ps { if ua > fa + ua + 1 - Ps { fa + ua + 1 - Ps } else { ua } } else { 0 });
                ua = ua + 1 - (if fa + ua + 1 > Ps { if ua > fa + ua + 1 - Ps { fa + ua + 1 - Ps } else { ua } } else { 0 });
                uo = uo - 1;
                qoth = qoth + 1;
                seq = seq + 1;
                nwp = 1;
                nwf = 0;
            }
            action ShareTakesOtherNode when (Buggy == 1 && MaxSeq > seq && ub > 0) {
                lost = lost + 1 + (if fa + ua + 1 > Ps { if ua > fa + ua + 1 - Ps { fa + ua + 1 - Ps } else { ua } } else { 0 });
                ua = ua + 1 - (if fa + ua + 1 > Ps { if ua > fa + ua + 1 - Ps { fa + ua + 1 - Ps } else { ua } } else { 0 });
                ub = ub - 1;
                qoth = qoth + 1;
                seq = seq + 1;
                nwp = 1;
                nwf = 0;
            }
            action ShareUncounted when (Buggy == 1 && MaxSeq > seq) {
                ua = ua + 1 - (if fa + ua + 1 > Ps { if ua > fa + ua + 1 - Ps { fa + ua + 1 - Ps } else { ua } } else { 0 });
                seq = seq + 1;
                nwp = 1;
                nwf = 0;
            }
            action ShareTakesFenced when (Buggy == 1 && MaxSeq > seq && fa > 0) {
                fa = fa - 1;
                fgone = fgone + 1;
                lost = lost + 1;
                ua = ua + 1;
                seq = seq + 1;
                nwp = 1;
                nwf = 0;
            }

            invariant ProgramShare: (fa + ua <= Ps || ua <= 1) && (fb + ub <= Ps || ub <= 1);
            invariant NoSilentLoss: lost + uo + fo + ua + fa + ub + fb == seq;
            invariant ShareTakesOnlyProgram: qoth == 0;
            invariant FenceHolds: fgone == 0;
            bound StateBounded: seq <= MaxSeq && lost <= MaxSeq && uo <= MaxSeq && fo <= MaxSeq && ua <= MaxSeq && fa <= MaxSeq && ub <= MaxSeq && fb <= MaxSeq && fence <= 1 && nwp <= 2 && nwf <= 1 && qoth <= MaxSeq && fgone <= MaxSeq;
        }
    }
}

/// SESSION LOG CLAIMS (phase 3A, design §3.7 and §7.3; rulings R13 and the
/// plan's correction 5, and the fixer's review): the claim half of the fold.
/// A claim is an attributed opinion: it never lifts an observed need, a need
/// it raises ranks below every observed one and stays its raiser's, the label
/// `brief` and the clause show is only ever a trusted author's and a proven
/// one's first, and no other author's verdict displaces the trusted one, nor
/// a `ctl` verdict the proven (the harness's): `ctl` is every in-session
/// client, the supervised agent among them.
///
/// Variables: `obs` an observed need open (a failed run's) and `live` its
/// ghost — raised, and not since closed by observation or by a non-claim
/// clear; `hn`, `cn`, `en` a claim's need open, raised by the harness, by a
/// `ctl` client, by an edge; `hlive` the harness's ghost — raised, and not
/// since cleared by the harness; `top` the most urgent need (0 none, 1 the
/// observed one, 2 a claim's); `lab` the newest label of any author and `tl`
/// the one shown (0 none, 1 a trusted author's, 2 another's), `lby` whose
/// the trusted slot holds (0 none, 1 the harness's, 2 a `ctl` client's),
/// `hl` a proven label held and `sh` the one shown is it; `vany` the newest
/// verdict (as `lab`), `tv` a trusted verdict held and `hadt` its ghost,
/// `vby` whose (as `lby`), `hv` a proven verdict held and `hadh` its ghost.
///
/// Actions are what `aterm_slog::Fold::apply` does with the records:
/// `RaiseObserved` a run closed `fail`, `ObserveLifts` a later run of its
/// kind closed `ok`, `HarnessNeed`/`CtlNeed`/`EdgeNeed` a `need` mark of each
/// author (the same word: each its own need), `HarnessClear`/`CtlClear`/
/// `EdgeClear` a claim's `clear` (`src=mark`) of each, `OwnerClear` a
/// non-claim `clear` from `ctl` (the 2A rule, src=meta), and
/// `HarnessLabel`/`CtlLabel`/`OtherLabel` and
/// `HarnessVerdict`/`CtlVerdict`/`OtherVerdict` the claim marks.
///
/// `Buggy = 1` adds one dead mutant per law, two for the raiser's: the 2A
/// rule for a claim's clear (`ClaimNeverOverridesObservation`), a claim ranked
/// by arrival (`ClaimsRankLast`), another author's label shown
/// (`LabelOnlyFromTrusted`), another author's verdict taking the trusted slot
/// (`TrustedVerdictStands`), a `ctl` verdict taking the proven slot
/// (`ProvenVerdictStands`), a `ctl` label shown over the harness's
/// (`ProvenLabelFirst`), and an edge's raise of the same word taking the
/// harness's need over or a `ctl` clear lifting it (`RaiserKeepsItsNeed`).
/// Tier-1: aterm-slog's `fold_claims_conform_to_derived_model`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn session_log_claims_model() -> Model {
    aterm_spec::ty_model! {
        SessionLogClaims {
            const Buggy = 0;
            var obs = 0;
            var live = 0;
            var hn = 0;
            var cn = 0;
            var en = 0;
            var hlive = 0;
            var top = 0;
            var lab = 0;
            var tl = 0;
            var lby = 0;
            var hl = 0;
            var sh = 0;
            var vany = 0;
            var tv = 0;
            var hadt = 0;
            var vby = 0;
            var hv = 0;
            var hadh = 0;

            action RaiseObserved when (obs == 0) {
                obs = 1;
                live = 1;
                top = 1;
            }
            action ObserveLifts when (obs == 1) {
                obs = 0;
                live = 0;
                top = if hn + cn + en > 0 { 2 } else { 0 };
            }
            action HarnessNeed when (hn == 0) {
                hn = 1;
                hlive = 1;
                top = if obs == 1 { 1 } else { 2 };
            }
            action CtlNeed when (cn == 0) {
                cn = 1;
                top = if obs == 1 { 1 } else { 2 };
            }
            action EdgeNeed when (en == 0) {
                en = 1;
                top = if obs == 1 { 1 } else { 2 };
            }
            action HarnessClear when (hn == 1) {
                hn = 0;
                hlive = 0;
                top = if obs == 1 { 1 } else { if cn + en > 0 { 2 } else { 0 } };
            }
            action CtlClear when (cn == 1 || en == 1) {
                cn = 0;
                en = 0;
                top = if obs == 1 { 1 } else { if hn == 1 { 2 } else { 0 } };
            }
            action EdgeClear when (en == 1) {
                en = 0;
                top = if obs == 1 { 1 } else { if hn + cn > 0 { 2 } else { 0 } };
            }
            action OwnerClear when (obs == 1 || cn == 1 || en == 1) {
                obs = 0;
                live = 0;
                cn = 0;
                en = 0;
                top = if hn == 1 { 2 } else { 0 };
            }
            action HarnessLabel when (lab == 0 || lab == 2 || tl == 0 || tl == 2 || lby == 0 || lby == 2 || hl == 0 || sh == 0) {
                lab = 1;
                tl = 1;
                lby = 1;
                hl = 1;
                sh = 1;
            }
            action CtlLabel when (lab == 0 || lab == 2 || tl == 0 || tl == 2 || lby <= 1) {
                lab = 1;
                tl = 1;
                lby = 2;
            }
            action OtherLabel when (lab <= 1) {
                lab = 2;
            }
            action HarnessVerdict when (vany == 0 || vany == 2 || tv == 0 || vby == 0 || vby == 2 || hv == 0) {
                vany = 1;
                tv = 1;
                hadt = 1;
                vby = 1;
                hv = 1;
                hadh = 1;
            }
            action CtlVerdict when (vany == 0 || vany == 2 || tv == 0 || vby <= 1) {
                vany = 1;
                tv = 1;
                hadt = 1;
                vby = 2;
            }
            action OtherVerdict when (vany <= 1) {
                vany = 2;
            }
            action ClaimClearLiftsObserved when (Buggy == 1 && (cn == 1 || en == 1) && obs == 1) {
                cn = 0;
                en = 0;
                obs = 0;
                top = if hn == 1 { 2 } else { 0 };
            }
            action ClaimRankedByArrival when (Buggy == 1 && cn == 0 && obs == 1) {
                cn = 1;
                top = 2;
            }
            action OtherLabelShown when (Buggy == 1 && tl <= 1) {
                lab = 2;
                tl = 2;
            }
            action OtherVerdictDisplaces when (Buggy == 1 && tv == 1) {
                vany = 2;
                tv = 0;
                vby = 0;
            }
            action CtlDisplacesHarness when (Buggy == 1 && hv == 1) {
                vany = 1;
                tv = 1;
                hadt = 1;
                vby = 2;
                hv = 0;
            }
            action CtlLabelShownOverHarness when (Buggy == 1 && hl == 1 && sh == 1) {
                lab = 1;
                tl = 1;
                lby = 2;
                sh = 0;
            }
            action ReraiseTakesOver when (Buggy == 1 && hn == 1) {
                hn = 0;
                en = 1;
                top = if obs == 1 { 1 } else { 2 };
            }
            action CtlClearLiftsHarness when (Buggy == 1 && hn == 1) {
                hn = 0;
                cn = 0;
                en = 0;
                top = if obs == 1 { 1 } else { 0 };
            }

            invariant ClaimNeverOverridesObservation: live <= obs;
            invariant ClaimsRankLast: obs == 0 || top == 1;
            invariant LabelOnlyFromTrusted: tl <= 1;
            invariant TrustedVerdictStands: hadt == 0 || tv == 1;
            invariant ProvenVerdictStands: hadh == 0 || hv == 1;
            invariant ProvenLabelFirst: sh == hl;
            invariant RaiserKeepsItsNeed: hlive <= hn;
            bound StateBounded: obs <= 1 && live <= 1 && hn <= 1 && cn <= 1 && en <= 1 && hlive <= 1 && top <= 2 && lab <= 2 && tl <= 2 && lby <= 2 && hl <= 1 && sh <= 1 && vany <= 2 && tv <= 1 && hadt <= 1 && vby <= 2 && hv <= 1 && hadh <= 1;
        }
    }
}

/// SESSION LOG FOLD (phase 3B, plan §4.2 as amended by A31 and ruling S13):
/// what `aterm_slog::Fold::apply` and the clause make of one agent run,
/// its one turn, one need-bearing step of that turn (a wall the screen saw),
/// one claimed need and one label, as records arrive and time passes.
///
/// Variables: `r` the run and `t` the turn (0 none, 1 open, 2 closed); `ow`
/// the outcome the fold holds for the closed turn (0 while it is open, 1
/// `done`, 2 `wall:*`, 3 `gone`) and `oc` the rank of the record that set it
/// (`Conf::rank`: 3 strong, 4 exact); `gw`/`gc` the GHOST of the outcome
/// rulings S9 and A5 give over every close and amend of the turn: the close's
/// own, unless an EXACT amend (the transcript, on a measured build) refines
/// it, before the close or after; a strong amend never does; `nd`, `nw`,
/// `ng` the session totals' `done`, `walled` and `other` buckets; `s` the
/// step (0 none, 1 open, 2 closed), `nc` the rank of its need in the fold (0
/// when none) and `sg` the GHOST of the best rank that need was given (its
/// open's, or an exact `confirmed=1`'s); `cn` a claimed need open (a `need`
/// mark's, which raises no clause row); `top` the most urgent need (0 none,
/// 1 the step's, 2 the claim's); `lab` a label held (abstract, A31: what
/// `shown_label` shows while it lives) and `lx` that it expired; `cl` the
/// clause's row (`ClauseRow`: 0 none, 1 need, 2 label, 3 step, turn or run);
/// `dup` that a replayed record changed the fold.
///
/// Actions, each a real record or a real read: `OpenRun`, `OpenTurn`;
/// `CloseDone` the screen's strong close; `AmendOutExact` an exact `out=`
/// amend (open turn or closed); `AmendOutStrong` a strong one; `OpenStep` a
/// strong wall step (raises its need), `ConfirmStep` an exact `confirmed=1`
/// amend of it, `RetractStep` its close; `CloseRun` the run's close (which
/// cascades its open turn and step `out=gone`); `MarkNeed` a claim's need;
/// `Label` a label mark; `Expire` time passing the label's TTL; `Tick` time
/// passing (the clause and hints read at a later `now_wall` over an
/// unchanged fold); `Replay` a record at or below `as_of` applied again.
///
/// `Buggy = 1` adds dead mutants, each naming the real call it breaks
/// (S13): `ReplayApplies` (`Fold::apply` without its `as_of` check:
/// `Idempotent`); `StrongAmendRefines` (`on_amend` through `confidence_law`
/// in place of `amend_refines_out`, a strong amend overruling the screen's
/// close: the fold half of S9's `PartialNeverOverrules`) and
/// `LowerAmendWins` (an amend's `out=` written past the law) and
/// `ConfirmNoUpgrade` (`need_takes_amend` answering no:
/// `ConfidenceMonotone`); `RebucketWithoutDecrement` (`turn_out_rebucket`
/// that only adds: `TotalsMatchClosedTurns`); `RetractKeepsNeed`
/// (`on_close` keeping a closed step's need: `NeedLivesWithItsStep`);
/// `TickRaisesNeed` (`clause` at a later `now_wall` naming a need the fold
/// does not hold: `SilenceRaisesNoNeed`); `CloseRunLeavesTurn` (`on_close`
/// without its child walk: `OpenPathWellNested`); `ExpiredLabelShown`
/// (`shown_label` without its `until` test) and `LabelHidesNeed`
/// (`clause_of` with the label row above the need row: `ClauseNeedFirst`).
/// The claims laws (`ClaimsRankLast`, `LabelOnlyFromTrusted`,
/// `ProvenLabelFirst`) are `SessionLogClaims`' and not restated. Tier-1:
/// aterm-slog's `fold_conforms_to_derived_model`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn session_log_fold_model() -> Model {
    aterm_spec::ty_model! {
        SessionLogFold {
            const Buggy = 0;
            var r = 0;
            var t = 0;
            var ow = 0;
            var oc = 0;
            var gw = 0;
            var gc = 0;
            var nd = 0;
            var nw = 0;
            var ng = 0;
            var s = 0;
            var nc = 0;
            var sg = 0;
            var cn = 0;
            var top = 0;
            var lab = 0;
            var lx = 0;
            var cl = 0;
            var dup = 0;

            action OpenRun when (r == 0) {
                r = 1;
                cl = if top == 1 { 1 } else { if lab == 1 && lx == 0 { 2 } else { 3 } };
            }
            action OpenTurn when (r == 1 && t == 0) {
                t = 1;
            }
            action CloseDone when (t == 1) {
                t = 2;
                ow = if gc == 4 { gw } else { 1 };
                oc = if gc == 4 { 4 } else { 3 };
                gw = if gc == 4 { gw } else { 1 };
                gc = if gc == 4 { 4 } else { 3 };
                nd = if gc == 4 { 0 } else { 1 };
                nw = if gc == 4 { 1 } else { 0 };
                s = if s == 1 { 2 } else { s };
                nc = 0;
                top = if cn == 1 { 2 } else { 0 };
                cl = if lab == 1 && lx == 0 { 2 } else { if r == 1 { 3 } else { 0 } };
            }
            action AmendOutExact when (1 <= t && gc <= 3) {
                gw = 2;
                gc = 4;
                ow = if t == 2 { 2 } else { 0 };
                oc = if t == 2 { 4 } else { 0 };
                nd = 0;
                ng = 0;
                nw = if t == 2 { 1 } else { 0 };
            }
            action AmendOutStrong when (1 <= t) {
                t = t;
            }
            action OpenStep when (t == 1 && s == 0) {
                s = 1;
                nc = 3;
                sg = 3;
                top = 1;
                cl = 1;
            }
            action ConfirmStep when (s == 1 && nc == 3) {
                nc = 4;
                sg = 4;
            }
            action RetractStep when (s == 1) {
                s = 2;
                nc = 0;
                top = if cn == 1 { 2 } else { 0 };
                cl = if lab == 1 && lx == 0 { 2 } else { if r == 1 { 3 } else { 0 } };
            }
            action CloseRun when (r == 1) {
                r = 2;
                t = if t == 1 { 2 } else { t };
                ow = if t == 1 { if gc == 4 { gw } else { 3 } } else { ow };
                oc = if t == 1 { if gc == 4 { 4 } else { 3 } } else { oc };
                gw = if t == 1 { if gc == 4 { gw } else { 3 } } else { gw };
                gc = if t == 1 { if gc == 4 { 4 } else { 3 } } else { gc };
                nw = if t == 1 { if gc == 4 { 1 } else { 0 } } else { nw };
                ng = if t == 1 { if gc == 4 { 0 } else { 1 } } else { ng };
                s = if s == 1 { 2 } else { s };
                nc = 0;
                top = if cn == 1 { 2 } else { 0 };
                cl = if lab == 1 && lx == 0 { 2 } else { 0 };
            }
            action MarkNeed when (cn == 0) {
                cn = 1;
                top = if s == 1 { 1 } else { 2 };
            }
            action Label when (lab == 0 || lx == 1) {
                lab = 1;
                lx = 0;
                cl = if top == 1 { 1 } else { 2 };
            }
            action Expire when (lab == 1 && lx == 0) {
                lx = 1;
                cl = if top == 1 { 1 } else { if r == 1 { 3 } else { 0 } };
            }
            action Tick {
                t = t;
            }
            action Replay {
                dup = dup;
            }

            action ReplayApplies when (Buggy == 1 && t == 2 && dup == 0) {
                dup = 1;
            }
            action StrongAmendRefines when (Buggy == 1 && t == 2 && oc == 3) {
                ow = 2;
                nd = 0;
                ng = 0;
                nw = 1;
            }
            action LowerAmendWins when (Buggy == 1 && t == 2 && oc == 4) {
                oc = 3;
            }
            action ConfirmNoUpgrade when (Buggy == 1 && s == 1 && nc == 3) {
                sg = 4;
            }
            action RebucketWithoutDecrement when (Buggy == 1 && t == 2 && gc == 3) {
                gw = 2;
                gc = 4;
                ow = 2;
                oc = 4;
                nw = 1;
            }
            action RetractKeepsNeed when (Buggy == 1 && s == 1) {
                s = 2;
            }
            action TickRaisesNeed when (Buggy == 1 && r == 1 && (top == 0 || top == 2)) {
                cl = 1;
            }
            action CloseRunLeavesTurn when (Buggy == 1 && r == 1 && t == 1) {
                r = 2;
            }
            action ExpiredLabelShown when (Buggy == 1 && lab == 1 && lx == 0 && cl == 2) {
                lx = 1;
            }
            action LabelHidesNeed when (Buggy == 1 && top == 1 && (lab == 0 || lx == 1)) {
                lab = 1;
                lx = 0;
                cl = 2;
            }

            invariant Idempotent: dup == 0;
            invariant ConfidenceMonotone: (t <= 1 || (ow == gw && oc == gc)) && (s == 0 || s == 2 || nc == sg);
            invariant TotalsMatchClosedTurns: nd + nw + ng == (if t == 2 { 1 } else { 0 }) && nw == (if t == 2 && ow == 2 { 1 } else { 0 }) && nd == (if t == 2 && ow == 1 { 1 } else { 0 });
            invariant NeedLivesWithItsStep: (s == 1 && nc > 0) || ((s == 0 || s == 2) && nc == 0);
            invariant SilenceRaisesNoNeed: cl == 0 || cl > 1 || top == 1;
            invariant OpenPathWellNested: (t == 0 || t == 2 || r == 1) && (s == 0 || s == 2 || t == 1);
            invariant ClauseNeedFirst: (top == 0 || top == 2 || cl == 1) && (cl == 0 || cl == 1 || cl == 3 || (lab == 1 && lx == 0));
            bound StateBounded: r <= 2 && t <= 2 && ow <= 3 && oc <= 4 && gw <= 3 && gc <= 4 && nd <= 2 && nw <= 2 && ng <= 2 && s <= 2 && nc <= 4 && sg <= 4 && cn <= 1 && top <= 2 && lab <= 1 && lx <= 1 && cl <= 3 && dup <= 1;
        }
    }
}

/// TRANSCRIPT JOIN (phase 3B, plan §4.3 as amended by rulings S1, S2, S4,
/// S9 and S13): what `aterm_slog::TurnTracker::transcript` makes of one
/// agent run's vendor turns while the screen drives the same tracker.
///
/// Variables: `wm` the join holds a watermark (bound to the file); `pt` the
/// source is `partial` (an unmeasured build) rather than `on`; `lt` the open
/// turn (0 none, 1 open with no `vref`, 2 open with one); `old` that it
/// opened outside a prompt's attribution window (5 s before now); `lc` that
/// the newest closed turn could take a late prompt (no `vref`, opened within
/// the window); `lv` that it could take one dated near its own open (no
/// `vref`, this run's: `lc` without the window's clause, for a prompt row
/// dated in the past); `vm` what the running vendor turn maps to (0 none, 1 the open
/// turn, 2 a closed turn, 3 nothing: a partial build with no turn to attach)
/// and `ve` that its end (a turn end, or Esc's interrupt) was read; `vo` that
/// the prompt of a vendor turn mapped to nothing is older than the window;
/// `ss` the screen's step under the open turn
/// (0 none, 1 a box, 2 a wall), `wr` a wall on the run, `pend` the wall's
/// settle armed, `wd` the screen's word resting (idle: a settle armed on any
/// open turn), `rf` the refuted latch, `ts` the transcript steps open (tool
/// and subagent, up to two here).
///
/// `g` is ONE GHOST, the first wrong record the real calls appended (0:
/// none; one variable, not eight, so the mutants' space stays small): 1 a
/// second open turn; 2 a `confirmed=` or `vref=` the screen wrote; 3 a row
/// at or below the watermark applied again; 4 a turn the transcript opened
/// dated before the run's open or the adoption (S1); 5 a prompt attached to
/// a turn opened more than 5 s from it, either side (S13); 6 a transcript
/// close that took a screen box with it (S4); 7 one that took the screen's
/// wall, its need gone (S2); 8 a partial record that closed a turn or
/// screen node or changed a held outcome (S9); 9 a prompt that named any
/// turn but the nearer one that could take it (the newest closed turn,
/// when the prompt is dated nearer its open than the open turn's).
///
/// Actions: the screen's `ScreenBusy` (opens a turn, ends any step; a
/// vendor turn still running on the turn the screen closed continues into
/// it, `cont=`; one mapped to nothing whose prompt came within the window
/// is the new turn's, `vref=`), `ScreenBox`, `ScreenWall` (on the turn, armed; else on the
/// run), `ScreenRest` (the idle settle's close: the cascade takes the
/// transcript steps), `WallRest` (the wall's settle: it continues on the
/// run), `Age` (time passes, no settle armed); the source's `Bind` (a first
/// read or a new file: seeded at its end, the old vendor turn forgotten),
/// `Lapse` (a re-bind of the same file), `AdoptWithoutWatermark` (a
/// successor adopted a carry with no `tx_` tokens), `Carry` (one with them),
/// `SetPartial`, `SetOn`; the rows' `PromptAttach` (the open turn, in the
/// window, takes `vref=`), `PromptShare` (a turn held by a screen step, or
/// any on a partial build, is shared), `PromptSupersede` (any other open
/// turn closes `out=superseded` and the vendor turn opens its own),
/// `PromptLate` (the newest closed turn takes it), `PromptUnmapped`,
/// `PromptOpens`; a prompt row dated in the past while a turn is open:
/// `PromptOld` (older than the open turn by more than the window: an
/// earlier turn's, read late, it maps to nothing) and `PromptLateNear`
/// (dated nearer the newest closed turn's open, which takes it though a
/// turn is open); `ToolUse` (under the open turn its vendor turn maps, else
/// counted), `ToolResult`, `TxEndCloses` (a measured build's turn end with
/// no screen step under the turn), `TxEndHeld` (any other: amends only),
/// `Interrupt` (Esc's row: the vendor turn ended, no turn end follows),
/// `Refute`, `Confirm`, `Compact` (an instant step: nothing projected);
/// `Replay` (a row at or below the watermark) and `OldRow` (one past it,
/// older than the floor): nothing.
///
/// `Buggy = 1` adds one dead mutant per broken real call:
/// `PromptOpensBeside` (`AtMostOneOpenTurn`), `LateStepOrphan` (a
/// `ToolUse` for a closed turn opening its step) and `RestLeavesSteps` (a
/// settle without the cascade: `StepsUnderOpenTurn`), `ScreenConfirms`
/// (`ConfirmedOnlyFromTranscript`), `ReplayAfterLapse` (a lapsed watch
/// re-emitting: `NoReplay`), `ReplayFromProcess` (a floor of the process
/// start alone: `NoTurnBeforeRunOpen`), `AttachEarly`
/// (`AttachOnlyWithinWindow`), `OldPromptAttaches` (an old prompt taken by
/// the turn opened after its window: `AttachOnlyWithinWindow`),
/// `LateNamesOpen` (a prompt dated near the closed turn's open taken by the
/// open turn: `PromptNamesNearestTurn`), `MergeClosesBox`
/// (`TxNeverClosesScreenStep`), `TxCloseTakesWall`
/// (`WallNeedOutlivesTxClose`), `RefuteLeavesSettle`
/// (`RefutedWallStaysShut`) and `PartialCloses` (`PartialNeverOverrules`).
/// Tier-1: aterm-slog's `transcript_join_conforms_to_derived_model`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn transcript_join_model() -> Model {
    aterm_spec::ty_model! {
        TranscriptJoin {
            const Buggy = 0;
            var wm = 0;
            var pt = 0;
            var lt = 0;
            var old = 0;
            var lc = 0;
            var lv = 0;
            var vm = 0;
            var ve = 0;
            var vo = 0;
            var ss = 0;
            var wr = 0;
            var pend = 0;
            var wd = 0;
            var rf = 0;
            var ts = 0;
            var g = 0;

            action ScreenBusy {
                lt = if lt == 0 { if vm == 3 && ve == 0 && vo == 0 { 2 } else { 1 } } else { lt };
                old = if lt == 0 { 0 } else { old };
                vm = if lt == 0 && ve == 0 && (vm == 2 || (vm == 3 && vo == 0)) { 1 } else { vm };
                ss = 0;
                wr = 0;
                pend = 0;
                wd = 0;
                rf = 0;
            }
            action ScreenBox when (lt > 0 && ss == 0 && wr == 0) {
                ss = 1;
                wd = 0;
                rf = 0;
            }
            action ScreenWall when (ss == 0 && wr == 0) {
                ss = if lt == 0 { 0 } else { 2 };
                wr = if lt == 0 { 1 } else { 0 };
                pend = if lt == 0 { 0 } else { 1 };
                wd = 0;
                rf = 0;
            }
            action ScreenRest when (lt > 0 && ss == 0 && pend == 0) {
                lt = 0;
                lc = if lt == 1 && old == 0 { 1 } else { 0 };
                lv = if lt == 1 { 1 } else { 0 };
                old = 0;
                ts = 0;
                vm = if vm == 1 { 2 } else { vm };
                wr = 0;
                wd = 1;
                rf = 0;
            }
            action WallRest when (lt > 0 && ss == 2 && pend == 1) {
                lt = 0;
                lc = if lt == 1 && old == 0 { 1 } else { 0 };
                lv = if lt == 1 { 1 } else { 0 };
                old = 0;
                ts = 0;
                vm = if vm == 1 { 2 } else { vm };
                ss = 0;
                wr = 1;
                pend = 0;
            }
            action Age when (pend == 0 && (wd == 0 || lt == 0)) {
                old = if lt == 0 { 0 } else { 1 };
                lc = 0;
                vo = if vm == 3 { 1 } else { 0 };
            }

            action Bind {
                wm = 1;
                vm = 0;
                ve = 0;
                vo = 0;
            }
            action Lapse when (wm == 1) {
                wm = 1;
            }
            action AdoptWithoutWatermark {
                wm = 0;
                vm = 0;
                ve = 0;
                vo = 0;
                rf = 0;
            }
            action Carry {
                wm = wm;
            }
            action SetPartial when (pt == 0) {
                pt = 1;
            }
            action SetOn when (pt == 1) {
                pt = 0;
            }

            action PromptAttach when (wm == 1 && lt == 1 && old == 0) {
                lt = 2;
                vm = 1;
                ve = 0;
                vo = 0;
            }
            action PromptShare when (wm == 1 && (lt == 2 || (lt == 1 && old == 1)) && (ss > 0 || pt == 1)) {
                vm = 1;
                ve = 0;
                vo = 0;
            }
            action PromptSupersede when (wm == 1 && (lt == 2 || (lt == 1 && old == 1)) && ss == 0 && pt == 0) {
                lt = 2;
                old = 0;
                lc = 0;
                lv = if lt == 1 { 1 } else { 0 };
                ts = 0;
                vm = 1;
                ve = 0;
                vo = 0;
            }
            action PromptLate when (wm == 1 && lt == 0 && lc == 1) {
                lc = 0;
                lv = 0;
                vm = 2;
                ve = 0;
                vo = 0;
            }
            action PromptOld when (wm == 1 && lt > 0) {
                vm = 3;
                ve = 0;
                vo = 1;
            }
            action PromptLateNear when (wm == 1 && lt > 0 && lv == 1) {
                lc = 0;
                lv = 0;
                vm = 2;
                ve = 0;
                vo = 0;
            }
            action PromptUnmapped when (wm == 1 && lt == 0 && lc == 0 && pt == 1) {
                vm = 3;
                ve = 0;
                vo = 0;
            }
            action PromptOpens when (wm == 1 && lt == 0 && lc == 0 && pt == 0) {
                lt = 2;
                old = 0;
                vm = 1;
                ve = 0;
                vo = 0;
            }
            action ToolUse when (wm == 1 && ts <= 1) {
                ts = if vm == 1 { ts + 1 } else { ts };
            }
            action ToolResult when (wm == 1 && ts > 0) {
                ts = ts - 1;
            }
            action TxEndCloses when (wm == 1 && vm == 1 && ve == 0 && ss == 0 && pt == 0) {
                lt = 0;
                lc = if lt == 1 && old == 0 { 1 } else { 0 };
                lv = if lt == 1 { 1 } else { 0 };
                old = 0;
                ts = 0;
                vm = 2;
                ve = 1;
                vo = 0;
            }
            action TxEndHeld when (wm == 1 && vm > 0 && ve == 0 && (vm > 1 || ss > 0 || pt == 1)) {
                ve = 1;
            }
            action Interrupt when (wm == 1 && vm > 0 && ve == 0) {
                ve = 1;
            }
            action Refute when (wm == 1 && pt == 0 && (ss == 2 || wr == 1)) {
                ss = 0;
                wr = 0;
                pend = 0;
                rf = 1;
            }
            action Confirm when (wm == 1 && pt == 0 && (ss == 2 || wr == 1)) {
                wm = 1;
            }
            action Replay when (wm == 1) {
                wm = 1;
            }
            action OldRow when (wm == 1) {
                wm = 1;
            }
            action Compact when (wm == 1) {
                wm = 1;
            }

            action PromptOpensBeside when (Buggy == 1 && g == 0 && wm == 1 && lt > 0 && ss == 0 && pt == 0) {
                lt = 2;
                vm = 1;
                ve = 0;
                vo = 0;
                g = 1;
            }
            action LateStepOrphan when (Buggy == 1 && g == 0 && wm == 1 && lt == 0 && vm == 2 && ts == 0) {
                ts = 1;
            }
            action RestLeavesSteps when (Buggy == 1 && g == 0 && lt > 0 && ts > 0 && ss == 0 && pend == 0) {
                lt = 0;
                lc = 0;
                lv = if lt == 1 { 1 } else { 0 };
                old = 0;
                vm = if vm == 1 { 2 } else { vm };
                wr = 0;
                wd = 1;
                rf = 0;
            }
            action ScreenConfirms when (Buggy == 1 && g == 0 && (ss == 2 || wr == 1)) {
                g = 2;
            }
            action ReplayAfterLapse when (Buggy == 1 && g == 0 && wm == 1) {
                g = 3;
            }
            action ReplayFromProcess when (Buggy == 1 && g == 0 && wm == 1 && lt == 0) {
                lt = 2;
                old = 0;
                vm = 1;
                ve = 0;
                vo = 0;
                g = 4;
            }
            action AttachEarly when (Buggy == 1 && g == 0 && wm == 1 && lt == 1 && old == 1) {
                lt = 2;
                vm = 1;
                ve = 0;
                vo = 0;
                g = 5;
            }
            action OldPromptAttaches when (Buggy == 1 && g == 0 && wm == 1 && lt == 1 && old == 0) {
                lt = 2;
                vm = 1;
                ve = 0;
                vo = 0;
                g = 5;
            }
            action LateNamesOpen when (Buggy == 1 && g == 0 && wm == 1 && lt == 1 && lv == 1) {
                lt = 2;
                vm = 1;
                ve = 0;
                vo = 0;
                g = 9;
            }
            action MergeClosesBox when (Buggy == 1 && g == 0 && wm == 1 && pt == 0 && ss == 1 && (lt == 2 || old == 1)) {
                lt = 2;
                old = 0;
                lc = 0;
                lv = if lt == 1 { 1 } else { 0 };
                ss = 0;
                ts = 0;
                vm = 1;
                ve = 0;
                vo = 0;
                g = 6;
            }
            action TxCloseTakesWall when (Buggy == 1 && g == 0 && wm == 1 && pt == 0 && vm == 1 && ve == 0 && ss == 2) {
                lt = 0;
                lc = 0;
                lv = if lt == 1 { 1 } else { 0 };
                old = 0;
                ss = 0;
                pend = 0;
                ts = 0;
                vm = 2;
                ve = 1;
                vo = 0;
                g = 7;
            }
            action RefuteLeavesSettle when (Buggy == 1 && g == 0 && wm == 1 && pt == 0 && ss == 2 && pend == 1) {
                ss = 0;
                rf = 1;
            }
            action PartialCloses when (Buggy == 1 && g == 0 && wm == 1 && pt == 1 && vm == 1 && ve == 0 && ss == 0) {
                lt = 0;
                lc = 0;
                lv = if lt == 1 { 1 } else { 0 };
                old = 0;
                ts = 0;
                vm = 2;
                ve = 1;
                vo = 0;
                g = 8;
            }

            invariant AtMostOneOpenTurn: 1 > g || g > 1;
            invariant StepsUnderOpenTurn: ts == 0 || lt > 0;
            invariant ConfirmedOnlyFromTranscript: 2 > g || g > 2;
            invariant NoReplay: 3 > g || g > 3;
            invariant NoTurnBeforeRunOpen: 4 > g || g > 4;
            invariant AttachOnlyWithinWindow: 5 > g || g > 5;
            invariant TxNeverClosesScreenStep: 6 > g || g > 6;
            invariant WallNeedOutlivesTxClose: 7 > g || g > 7;
            invariant RefutedWallStaysShut: rf == 0 || (ss <= 1 && pend == 0 && wr == 0);
            invariant PartialNeverOverrules: 8 > g || g > 8;
            invariant PromptNamesNearestTurn: 9 > g || g > 9;
            bound StateBounded: lt <= 2 && lv <= 1 && vm <= 3 && vo <= 1 && ss <= 2 && ts <= 2 && g <= 9;
        }
    }
}

/// SESSION LOG MAIL LINK (phase 5, plan 5D-5; rulings T10, T11, T36, T39;
/// design §2.6, DESIGN:203-207): the heuristic mail → turn link
/// `aterm_slog::Fold::apply` makes as a session's mail and turns arrive and
/// time passes (`aterm_slog::mail_link_of`, `fold_mail.rs`).
///
/// Variables: `cands` the delivered `kind=task` mails waiting for a turn
/// inside the window (the fold's waiting list, 0..2); `old` one waiting past
/// the window (still listed until a turn or a person drops it); `t` the
/// newest turn (0 none, 1 open, 2 closed); `lk` it holds a link and `lc` the
/// link's confidence (0 none, 1 heuristic, 2 any other); `ex` it names its
/// cause; `th` it is a person's; `amb` some turn found mail ambiguous (the
/// totals' `mail_amb=`, T10c). GHOSTS, what the walk knows of the mails
/// themselves: `hm` a task mail inside the window that a person's act took
/// off the list; `notes` a note (any kind but `task`) inside the window
/// since the last person's act; `lm` a mail inside the window already linked
/// (heuristically or by an explicit `cause=`) since then; and the breaks:
/// `ga` a link to a mail a person's act took, `gw` to one past the window,
/// `g2` to one already linked, `gn` to a note, `gm` a link made among two or
/// more.
///
/// Actions, each a real record or time: `TaskMail`, `NoteMail` (`mail
/// dir=in`); `Human` (a `human` mark: the person's input, since 5D-4 also
/// the first after mail that reached the program, T39); `HumanNoReach` (a
/// `human reached=0` mark: a burst's first gesture reached no program, a
/// view scroll or a selection; it answers nothing, so every mail still
/// waits); `Age` (time past the window of every mail so
/// far); `OpenTurn` (a turn of `ctl`, the harness or the screen, no cause:
/// links the one waiting mail, counts two as ambiguous); `OpenCausedTurn`
/// (`cause=`: no heuristic link; the mail it names, the newest waiting one,
/// is taken); `OpenPersonTurn` (`by=human`: no link, every waiting mail
/// taken); `CloseTurn`.
///
/// Laws: `HeuristicStaysHeuristic` (a link is always `~`, never the turn's
/// own confidence); `ExplicitCauseWins`; `NoLinkAcrossHuman` (no link to a
/// mail a person's act stands after, and a person's turn links none);
/// `LinkWithinWindow`; `OneMailOneTurn`; `OnlyTaskLinks`; and
/// `AmbiguityLinksNone` (T10c: two waiting, none linked).
///
/// `Buggy = 1` adds one dead mutant per law, each the broken rule it names:
/// `LinkAtTurnConf`, `LinkOverExplicit`, `LinkAcrossHuman`,
/// `LinkPastWindow`, `MailLinksTwice`, `LinkNote` and `LinkAmongMany`.
/// Tier-1: aterm-slog's `mail_link_conforms_to_derived_model`, its negative
/// controls hand-built successors over the real fold (T11).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn session_log_mail_link_model() -> Model {
    aterm_spec::ty_model! {
        SessionLogMailLink {
            const Buggy = 0;
            var cands = 0;
            var old = 0;
            var t = 0;
            var lk = 0;
            var lc = 0;
            var ex = 0;
            var th = 0;
            var amb = 0;
            var hm = 0;
            var notes = 0;
            var lm = 0;
            var ga = 0;
            var gw = 0;
            var g2 = 0;
            var gn = 0;
            var gm = 0;

            action TaskMail when (2 > cands) {
                cands = cands + 1;
            }
            action NoteMail when (notes == 0) {
                notes = 1;
            }
            action Human {
                hm = if hm + cands > 0 { 1 } else { 0 };
                cands = 0;
                old = 0;
                notes = 0;
                lm = 0;
            }
            action HumanNoReach {
                cands = cands;
            }
            action Age when (cands + hm + notes + lm > 0) {
                old = if old + cands > 0 { 1 } else { 0 };
                cands = 0;
                hm = 0;
                notes = 0;
                lm = 0;
            }
            action OpenTurn when (t == 0 || t == 2) {
                lk = if cands == 1 { 1 } else { 0 };
                lc = if cands == 1 { 1 } else { 0 };
                lm = if cands == 1 { 1 } else { lm };
                amb = if cands == 2 { 1 } else { amb };
                cands = 0;
                old = 0;
                t = 1;
                ex = 0;
                th = 0;
            }
            action OpenCausedTurn when (t == 0 || t == 2) {
                lm = if cands > 0 { 1 } else { lm };
                cands = if cands > 0 { cands - 1 } else { 0 };
                old = 0;
                lk = 0;
                lc = 0;
                t = 1;
                ex = 1;
                th = 0;
            }
            action OpenPersonTurn when (t == 0 || t == 2) {
                hm = if hm + cands > 0 { 1 } else { 0 };
                cands = 0;
                old = 0;
                notes = 0;
                lm = 0;
                lk = 0;
                lc = 0;
                t = 1;
                ex = 0;
                th = 1;
            }
            action CloseTurn when (t == 1) {
                t = 2;
            }
            action LinkAtTurnConf when (Buggy == 1 && (t == 0 || t == 2) && cands == 1) {
                lk = 1;
                lc = 2;
                lm = 1;
                cands = 0;
                old = 0;
                t = 1;
                ex = 0;
                th = 0;
            }
            action LinkOverExplicit when (Buggy == 1 && (t == 0 || t == 2) && cands > 0) {
                lk = 1;
                lc = 1;
                lm = 1;
                cands = cands - 1;
                old = 0;
                t = 1;
                ex = 1;
                th = 0;
            }
            action LinkAcrossHuman when (Buggy == 1 && (t == 0 || t == 2) && cands == 0 && hm == 1) {
                lk = 1;
                lc = 1;
                ga = 1;
                hm = 0;
                old = 0;
                t = 1;
                ex = 0;
                th = 0;
            }
            action LinkPastWindow when (Buggy == 1 && (t == 0 || t == 2) && cands == 0 && old == 1) {
                lk = 1;
                lc = 1;
                gw = 1;
                old = 0;
                t = 1;
                ex = 0;
                th = 0;
            }
            action MailLinksTwice when (Buggy == 1 && (t == 0 || t == 2) && cands == 0 && lm == 1) {
                lk = 1;
                lc = 1;
                g2 = 1;
                old = 0;
                t = 1;
                ex = 0;
                th = 0;
            }
            action LinkNote when (Buggy == 1 && (t == 0 || t == 2) && cands == 0 && notes == 1) {
                lk = 1;
                lc = 1;
                gn = 1;
                notes = 0;
                old = 0;
                t = 1;
                ex = 0;
                th = 0;
            }
            action LinkAmongMany when (Buggy == 1 && (t == 0 || t == 2) && cands == 2) {
                lk = 1;
                lc = 1;
                gm = 1;
                lm = 1;
                cands = 0;
                old = 0;
                t = 1;
                ex = 0;
                th = 0;
            }

            invariant HeuristicStaysHeuristic: lk == 0 || lc == 1;
            invariant ExplicitCauseWins: ex == 0 || lk == 0;
            invariant NoLinkAcrossHuman: ga == 0 && (th == 0 || lk == 0);
            invariant LinkWithinWindow: gw == 0;
            invariant OneMailOneTurn: g2 == 0;
            invariant OnlyTaskLinks: gn == 0;
            invariant AmbiguityLinksNone: gm == 0;
            bound StateBounded: cands <= 2 && old <= 1 && t <= 2 && lk <= 1 && lc <= 2 && ex <= 1 && th <= 1 && amb <= 1 && hm <= 1 && notes <= 1 && lm <= 1 && ga <= 1 && gw <= 1 && g2 <= 1 && gn <= 1 && gm <= 1;
        }
    }
}
