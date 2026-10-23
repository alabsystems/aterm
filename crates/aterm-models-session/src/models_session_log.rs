// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The session log's formal models (`docs/DESIGN-session-log-2026-10-15.md` §9): the per-session ring with its open/close discipline and level-aware eviction, a reader's cursor over it, the turn tracker over screen verdict words, the disk journal's reader view, the mark quota, the program share, the timeline rows, the fold and its claims, the transcript join and the mail link.

use aterm_spec::derive::*;

/// SESSION LOG RING: one session's bounded ring of records, evicting level-aware (§5.1), over the open path of one run, its turn and the turn's step.
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

/// SESSION LOG CURSOR: what a reader pulling through `aterm_slog::Log::read` is told, over four seqs, where level-aware eviction takes records from the middle of the window and a carried update leaves seqs the log never held (`Log::skip_to`).
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

/// TURN TRACKER: at most one open turn per agent run, driven by the screen verdict (design §2.4).
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

/// SESSION LOG JOURNAL VIEW: what a `log @<sid>` answered from the disk journal shows, over three seqs of one session's space: the session's open (seq 1), a run's under it (2) and a turn's under the run (3).
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

/// SESSION LOG MARK QUOTA (design §7.4): the claims a consumer writes back (`mark`, `src=mark`) hold at most a quarter of a ring's capacity, and the quota keeps that by taking only marks, the oldest first, never the record just appended and never one the readers' fence holds during an update's overlap.
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

/// SESSION LOG PROGRAM SHARE: the program marks (OSC 7501's reports) hanging from one node hold at most its share of the ring, and the share keeps that by taking only that node's program marks, the oldest first, never the record just appended and never one the readers' fence holds, each counted lost: a flapping program spends its own share, never another node's nor the facts beside it.
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

/// SESSION LOG ROWS: the records a host's timeline prints as rows (`aterm_slog::Log::keep_rows`), numbered densely from a counter the host carries across an update, and the row floor: the newest rows, up to a quarter of the ring, which an eviction batch takes only once every other record it may take is taken.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn session_log_rows_model() -> Model {
    aterm_spec::ty_model! {
        SessionLogRows {
            const Buggy = 0;
            const MaxSeq = 9;
            const Floor = 2;
            const Reserve = 1;
            const MaxEp = 1;
            const MaxId = 24;
            var seq = 0;
            var lost = 0;
            var u = 0;
            var ro = 0;
            var rf = 0;
            var nw = 0;
            var rid = 0;
            var rfirst = 1;
            var rapp = 0;
            var rprev = 0;
            var ups = 0;
            var under = 0;

            action AppendOther when (MaxSeq > seq) {
                u = u + 1;
                seq = seq + 1;
                nw = 1;
            }
            action AppendRow when (MaxSeq > seq) {
                ro = if Floor > rf { ro } else { ro + 1 };
                rf = if Floor > rf { rf + 1 } else { rf };
                rid = rid + 1;
                rapp = rapp + 1;
                seq = seq + 1;
                nw = 2;
            }
            action EvictOther when (u > (if nw == 1 { 1 } else { 0 })) {
                u = u - 1;
                lost = lost + 1;
            }
            action EvictOutside when (ro > 0) {
                ro = ro - 1;
                lost = lost + 1;
            }
            action EvictFloor when (rf > (if nw == 2 { 1 } else { 0 }) && ro == 0 && u <= (if nw == 1 { 1 } else { 0 })) {
                rf = rf - 1;
                lost = lost + 1;
            }
            action HandoffCarried when (MaxEp > ups && MaxSeq > seq) {
                u = u + ro + rf;
                ro = 0;
                rf = 1;
                rprev = rid + Reserve;
                rfirst = rid + Reserve + 1;
                rid = rid + Reserve + 1;
                rapp = 1;
                seq = seq + 1;
                nw = 2;
                ups = ups + 1;
            }
            action HandoffUncarried when (MaxEp > ups) {
                seq = 1;
                lost = 0;
                u = 0;
                ro = 0;
                rf = 1;
                rprev = rid + Reserve;
                rfirst = rid + Reserve + 1;
                rid = rid + Reserve + 1;
                rapp = 1;
                nw = 2;
                ups = ups + 1;
            }
            action Crash when (MaxEp > ups) {
                seq = 1;
                lost = 0;
                u = 0;
                ro = 0;
                rf = 1;
                rprev = 0;
                rfirst = 1;
                rid = 1;
                rapp = 1;
                nw = 2;
                ups = ups + 1;
            }
            action EvictRowUnderFloor when (Buggy == 1 && under == 0 && rf > (if nw == 2 { 1 } else { 0 }) && u > (if nw == 1 { 1 } else { 0 })) {
                rf = rf - 1;
                lost = lost + 1;
                under = 1;
            }
            action HandoffRowsRestart when (Buggy == 1 && MaxEp > ups && MaxSeq > seq) {
                u = u + ro + rf;
                ro = 0;
                rf = 1;
                rprev = rid + Reserve;
                rfirst = 1;
                rid = 1;
                rapp = 1;
                seq = seq + 1;
                nw = 2;
                ups = ups + 1;
            }
            action CrashKeepsEpoch when (Buggy == 1 && MaxEp > ups) {
                seq = 1;
                lost = 0;
                u = 0;
                ro = 0;
                rf = 1;
                rprev = rid;
                rfirst = 1;
                rid = 1;
                rapp = 1;
                nw = 2;
                ups = ups + 1;
            }
            action CarryShedUncounted when (Buggy == 1 && MaxEp > ups && MaxSeq > seq && u + ro + rf > 0) {
                u = u + ro + rf - 1;
                ro = 0;
                rf = 1;
                rprev = rid + Reserve;
                rfirst = rid + Reserve + 1;
                rid = rid + Reserve + 1;
                rapp = 1;
                seq = seq + 1;
                nw = 2;
                ups = ups + 1;
            }

            invariant RowFloor: under == 0;
            invariant DenseRowIds: rid + 1 == rfirst + rapp && rfirst > rprev;
            invariant NoSilentLoss: lost + u + ro + rf == seq;
            bound StateBounded: seq <= MaxSeq && lost <= MaxSeq && u <= MaxSeq && ro <= MaxSeq && rf <= Floor && nw <= 2 && rid <= MaxId && rfirst <= MaxId && rapp <= MaxId && rprev <= MaxId && ups <= MaxEp && under <= 1;
        }
    }
}

/// SESSION LOG CLAIMS (design §3.7 and §7.3): the claim half of the fold.
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

/// SESSION LOG FOLD (design §7.1): what `aterm_slog::Fold::apply` and the clause make of one agent run, its one turn, one need-bearing step of that turn (a wall the screen saw), one claimed need and one label, as records arrive and time passes.
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

/// TRANSCRIPT JOIN (design §2.4): what `aterm_slog::TurnTracker::transcript` makes of one agent run's vendor turns while the screen drives the same tracker.
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

/// SESSION LOG MAIL LINK (design §2.6): the heuristic mail → turn link `aterm_slog::Fold::apply` makes as a session's mail and turns arrive and time passes (`aterm_slog::mail_link_of`, `fold_mail.rs`).
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
