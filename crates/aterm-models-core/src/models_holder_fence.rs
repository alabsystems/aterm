// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The holder fence (docs/DESIGN-card-engine-2026-10-19.md §3.7): which
//! main-screen rows a card drawer wrote while it held the terminal.

use aterm_spec::derive::Model;

/// The fence's protocol over one subject drawer `A`, one other drawer `B`
/// and the shell (any holder that is not a drawer), on the geometry a
/// line-oriented session keeps: each holder draws whole lines below what is
/// on screen and leaves the cursor below them (the frontier), and the screen
/// is tall enough that nothing the drawers draw scrolls into history. Rows
/// are not modelled one by one; each variable is a fact the real fence and
/// screen show (Tier-1 projects every one but `hurt`), and the drawers are
/// symmetric in the code (a token is opaque), so the laws stated for `A`
/// hold for every drawer.
///
/// State:
/// * `fg` — who holds the foreground: 0 the shell, 1 `A`, 2 `B`.
/// * `ra`, `rb` — the drawer runs (current or stopped); `ma`, `mb` — the
///   fence holds its mark; `marks` — how many marks it holds.
/// * `ta`, `tb` — the mark admits nothing (its lay found ink, or a lost
///   anchor tainted it).
/// * `fa` — a row in `A`'s spans holds another writer's ink and `A` is not
///   tainted: while `A` is current, the gate admits a row `A` did not write.
/// * `ga` — `A` is stopped and its spans hold a row its seal does not name.
/// * `ka` — `A`'s card (the first row it drew) is in its spans, untainted.
/// * `hurt` — ghost: since `A` drew that card, something took it that the
///   fence must not keep (a rewrite, a lay over ink, an unlent renumber).
/// * `spans` — `A`'s spans; `idle` — `A` is current but its open span holds
///   no row yet (it came back while the alternate screen was up).
/// * `ink` — what the shell left from the main cursor on: 0 nothing, 1 on
///   the cursor's own row, 2 only below it.
/// * `alt` — the alternate screen is up; `aa` — `A`'s `alt_fresh`; `aw` —
///   the alternate buffer holds ink (`A` never draws there, so all of it is
///   another writer's).
///
/// The design's actions: `Cut` is `LaunchA`/`LaunchB`/`StopA`/`StopB`/
/// `FgA`/`FgB`/`ExitA`/`ExitB`/`ExecAB`/`ExecBA` (and `KillA`, a stopped
/// drawer dying); `Write(row, holder)` is `Draw` (the current drawer's own
/// line), `Rewind`/`Climb`/`Settle` (the shell's lines around the cursor),
/// `Scribble` (the shell rewriting a stopped drawer's row) and `AltDraw`;
/// `BandMove` is an insert-line above the current drawer's open span;
/// `Splice` the archival scroll of a region ending where `A`'s newest span
/// ends; `Reflow` a width change lent through the rewrap; `Renumber` an
/// unlent renumbering (a Kitty unscroll); `Ed3`, `FullClear`, `Alt1049`,
/// `Alt1047`, `AltClear` and `AltExit` as named; `Admit` the gate's read.
///
/// Scope: `Reflow` and `Renumber` move rows while `A` is current or gone. A
/// stopped `A` whose rows moved loses them at its reselect, where the seal
/// cuts each changed row (fail closed); which rows that is depends on the
/// geometry, and the writer oracle's fuzz drives it. Two drawers never fill
/// the fence, so `marks <= MaxMarks` bounds it and eviction past it is a
/// unit test's.
///
/// Mutants (`Buggy`), one per `aterm_grid::FenceKnob`, in its order:
/// 1 band moves leave anchors, 2 a splice shifts an End at the insertion
/// point, 3 a reflow lends nothing, 4 an unlent renumber keeps anchors,
/// 5 an alt-screen swap loses them, 6 a reselect skips the seal check,
/// 7 no lay taint, 8 a lay taint that skips the cursor row, 9 an exec
/// inherits the outgoing mark, 10 count eviction (the fence keeps one
/// mark), 11 a stop keeps `alt_fresh`.
///
/// Tier-0: `aterm-models-core/tests/derived_holder_fence.rs`. Tier-1:
/// `aterm-core/src/terminal/holder_fence_conformance.rs` drives the real
/// `Terminal` over this model's whole reachable space and rejects every
/// knob.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn holder_fence_model() -> Model {
    aterm_spec::ty_model! {
        HolderFence {
            const MaxMarks = 8;
            const MaxSpans = 4;
            const Buggy = 0;
            var fg = 0;
            var ra = 0;
            var ma = 0;
            var ta = 0;
            var fa = 0;
            var ga = 0;
            var ka = 0;
            var hurt = 0;
            var spans = 0;
            var idle = 0;
            var rb = 0;
            var mb = 0;
            var tb = 0;
            var marks = 0;
            var ink = 0;
            var alt = 0;
            var aa = 0;
            var aw = 0;

            // The shell launches A, which draws its card and a line at the
            // lay row and settles below them. The lay taints on any ink from
            // the cursor on; what it skips stays under the card.
            action LaunchA when (fg == 0 && ra == 0 && alt == 0) {
                fg = 1;
                ra = 1;
                ma = 1;
                ta = if Buggy == 7 { 0 }
                     else { if Buggy == 8 { if ink == 2 { 1 } else { 0 } }
                     else { if ink > 0 { 1 } else { 0 } } };
                fa = if ink > 0 && (Buggy == 7 || (Buggy == 8 && ink == 1)) { 1 } else { 0 };
                ka = if ink == 0 || Buggy == 7 || (Buggy == 8 && ink == 1) { 1 } else { 0 };
                hurt = if ink > 0 { 1 } else { 0 };
                spans = 1;
                idle = 0;
                ga = 0;
                aa = 0;
                ink = 0;
                mb = if Buggy == 10 { 0 } else { mb };
                tb = if Buggy == 10 { 0 } else { tb };
                marks = if Buggy == 10 { 1 } else { 1 + mb };
            }

            action LaunchB when (fg == 0 && rb == 0 && alt == 0) {
                fg = 2;
                rb = 1;
                mb = 1;
                tb = if Buggy == 7 { 0 }
                     else { if Buggy == 8 { if ink == 2 { 1 } else { 0 } }
                     else { if ink > 0 { 1 } else { 0 } } };
                ink = 0;
                // Count eviction drops the stopped A, live as it is.
                ma = if Buggy == 10 { 0 } else { ma };
                ta = if Buggy == 10 { 0 } else { ta };
                fa = if Buggy == 10 { 0 } else { fa };
                ga = if Buggy == 10 { 0 } else { ga };
                ka = if Buggy == 10 { 0 } else { ka };
                spans = if Buggy == 10 { 0 } else { spans };
                marks = if Buggy == 10 { 1 } else { ma + 1 };
            }

            // Ctrl-Z: A's open span closes at the main cursor (an empty one
            // goes) and the mark is sealed.
            action StopA when (fg == 1) {
                fg = 0;
                aa = if Buggy == 11 { aa } else { 0 };
                spans = if idle == 1 { spans - 1 } else { spans };
                idle = 0;
            }

            action StopB when (fg == 2) {
                fg = 0;
            }

            // `fg`: the seal cuts every row rewritten since the stop (a
            // rewrite of the oldest span's first row empties it), then a new
            // span is laid at the cursor, tainting on ink. A draws a line
            // unless the alternate screen is up. A mark count eviction took
            // starts afresh.
            action FgA when (fg == 0 && ra == 1) {
                fg = 1;
                ma = 1;
                ta = if ma == 1 && ta == 1 { 1 }
                     else { if Buggy == 7 { 0 }
                     else { if Buggy == 8 { if ink == 2 { 1 } else { 0 } }
                     else { if ink > 0 { 1 } else { 0 } } } };
                fa = if ma == 1 && ta == 1 { 0 }
                     else { if ink > 0 && (Buggy == 7 || (Buggy == 8 && ink == 1)) { 1 }
                     else { if ink > 0 { 0 }
                     else { if Buggy == 6 && ma == 1 { fa } else { 0 } } } };
                ka = if ma == 0 { 0 }
                     else { if ink > 0 && (Buggy == 7 || (Buggy == 8 && ink == 1)) { ka }
                     else { if ink > 0 { 0 } else { ka } } };
                hurt = if ink > 0 { 1 } else { hurt };
                spans = if ma == 0 { 1 }
                        else { if fa == 1 && spans > 0 && Buggy == 6 { if spans > MaxSpans - 1 { MaxSpans } else { spans + 1 } }
                        else { if fa == 1 && spans > 0 { spans }
                        else { if spans > MaxSpans - 1 { MaxSpans } else { spans + 1 } } } };
                idle = if alt == 1 { 1 } else { 0 };
                ga = 0;
                ink = 0;
                mb = if Buggy == 10 && ma == 0 { 0 } else { mb };
                tb = if Buggy == 10 && ma == 0 { 0 } else { tb };
                marks = if Buggy == 10 && ma == 0 { 1 } else { if ma == 0 { marks + 1 } else { marks } };
            }

            action FgB when (fg == 0 && rb == 1 && alt == 0) {
                fg = 2;
                mb = 1;
                tb = if mb == 1 && tb == 1 { 1 }
                     else { if Buggy == 7 { 0 }
                     else { if Buggy == 8 { if ink == 2 { 1 } else { 0 } }
                     else { if ink > 0 { 1 } else { 0 } } } };
                ink = 0;
                ma = if Buggy == 10 && mb == 0 { 0 } else { ma };
                ta = if Buggy == 10 && mb == 0 { 0 } else { ta };
                fa = if Buggy == 10 && mb == 0 { 0 } else { fa };
                ga = if Buggy == 10 && mb == 0 { 0 } else { ga };
                ka = if Buggy == 10 && mb == 0 { 0 } else { ka };
                spans = if Buggy == 10 && mb == 0 { 0 } else { spans };
                marks = if Buggy == 10 && mb == 0 { 1 } else { if mb == 0 { marks + 1 } else { marks } };
            }

            action ExitA when (fg == 1) {
                fg = 0;
                ra = 0;
                ma = 0;
                ta = 0;
                fa = 0;
                ga = 0;
                ka = 0;
                hurt = 0;
                spans = 0;
                idle = 0;
                aa = 0;
                marks = mb;
            }

            action ExitB when (fg == 2) {
                fg = 0;
                rb = 0;
                mb = 0;
                tb = 0;
                marks = ma;
            }

            // A stopped A dies: the host retires its mark.
            action KillA when (ra == 1 && (fg == 0 || fg == 2)) {
                ra = 0;
                ma = 0;
                ta = 0;
                fa = 0;
                ga = 0;
                ka = 0;
                hurt = 0;
                spans = 0;
                aa = 0;
                marks = mb;
            }

            // B's image execs into a new A: B's mark goes, A lays its own.
            action ExecBA when (fg == 2 && ra == 0 && alt == 0) {
                fg = 1;
                rb = 0;
                mb = 0;
                tb = 0;
                ra = 1;
                ma = 1;
                ta = if Buggy == 9 { tb } else { 0 };
                fa = if Buggy == 9 && tb == 0 { 1 } else { 0 };
                ka = if Buggy == 9 && tb == 1 { 0 } else { 1 };
                hurt = 0;
                spans = 1;
                idle = 0;
                ga = 0;
                aa = 0;
                marks = 1;
            }

            action ExecAB when (fg == 1 && rb == 0 && alt == 0) {
                fg = 2;
                ra = 0;
                ma = 0;
                ta = 0;
                fa = 0;
                ga = 0;
                ka = 0;
                hurt = 0;
                spans = 0;
                idle = 0;
                aa = 0;
                rb = 1;
                mb = 1;
                tb = if Buggy == 9 { ta } else { 0 };
                marks = 1;
            }

            // The shell's lines: one left under the cursor at column 0, one
            // drawn below a blank cursor row, and the newline past them.
            action Rewind when (fg == 0 && ink == 0 && alt == 0) {
                ink = 1;
            }

            action Climb when (fg == 0 && ink == 0 && alt == 0) {
                ink = 2;
            }

            action Settle when (fg == 0 && ink > 0 && alt == 0) {
                ink = 0;
            }

            // The shell rewrites the first row of a stopped A's oldest span
            // (`clear; cat card.txt` writes over a card).
            action Scribble when (fg == 0 && ma == 1 && spans > 0 && ta == 0 && fa == 0 && alt == 0) {
                fa = 1;
                ka = 0;
                hurt = 1;
            }

            // The current drawer draws a line on the main screen.
            action Draw when (fg == 1 && alt == 0) {
                idle = 0;
            }

            // A inserts a line above its open span: the row above moves down
            // into the span's old start, which the anchor leaves.
            action BandMove when (fg == 1 && alt == 0) {
                fa = if Buggy == 1 && ta == 0 { 1 } else { fa };
            }

            // The shell scrolls a top-anchored region ending where A's
            // newest span ends: the blank row inserted at that End is not A's.
            action Splice when (fg == 0 && ma == 1 && spans > 0 && alt == 0) {
                ga = if Buggy == 2 { 1 } else { ga };
            }

            // A width change at the frontier rewraps the lines above the
            // current A's span.
            action Reflow when (ink == 0 && alt == 0 && (fg == 1 || ra == 0)) {
                fa = if Buggy == 3 && ma == 1 && ta == 0 { 1 } else { fa };
            }

            // An unlent renumbering loses the anchors: every mark taints.
            // (A stopped A's seal then also cuts whichever rows moved, which
            // the geometry decides; the writer oracle's fuzz drives that.)
            action Renumber when (alt == 0 && (fg == 1 || ra == 0)) {
                ta = if Buggy == 4 { ta } else { if ma == 1 && spans > 0 { 1 } else { ta } };
                fa = if Buggy == 4 { if ma == 1 && ta == 0 { 1 } else { fa } } else { 0 };
                ka = if Buggy == 4 { ka } else { 0 };
                hurt = if Buggy == 4 { hurt } else { ra };
                tb = if Buggy == 4 { tb } else { mb };
                // The cursor stays on its content, one row on.
                idle = 0;
            }

            // History is erased; nothing a drawer drew is in it.
            action Ed3 when (alt == 0) {
                ink = ink;
            }

            // The current A clears the screen and draws its card again: one
            // span from the screen's top, untainted.
            action FullClear when (fg == 1 && alt == 0) {
                ta = 0;
                fa = 0;
                ka = 1;
                hurt = 0;
                spans = 1;
                idle = 0;
            }

            action Alt1049 when (alt == 0 && ink == 0) {
                alt = 1;
                aw = 0;
                aa = if fg == 1 { 1 } else { 0 };
                ta = if Buggy == 5 && ma == 1 && spans > 0 { 1 } else { ta };
                fa = if Buggy == 5 { 0 } else { fa };
                ka = if Buggy == 5 { 0 } else { ka };
                tb = if Buggy == 5 { mb } else { tb };
            }

            action Alt1047 when (alt == 0 && ink == 0) {
                alt = 1;
                ta = if Buggy == 5 && ma == 1 && spans > 0 { 1 } else { ta };
                fa = if Buggy == 5 { 0 } else { fa };
                ka = if Buggy == 5 { 0 } else { ka };
                tb = if Buggy == 5 { mb } else { tb };
            }

            action AltDraw when (alt == 1 && (fg == 0 || fg == 2)) {
                aw = 1;
            }

            action AltClear when (alt == 1) {
                aw = 0;
                aa = if fg == 1 { 1 } else { aa };
            }

            action AltExit when (alt == 1) {
                alt = 0;
                aa = 0;
                ta = if Buggy == 5 && ma == 1 && spans > 0 { 1 } else { ta };
                fa = if Buggy == 5 { 0 } else { fa };
                ka = if Buggy == 5 { 0 } else { ka };
                tb = if Buggy == 5 { mb } else { tb };
            }

            // The gate reads the fence for a frame; it changes nothing.
            action Admit when (fg == 1) {
                ink = ink;
            }

            bound Bounds:
                fg <= 2 && marks <= MaxMarks && spans <= MaxSpans && ink <= 2;
            // I2: every row the gate admits is the current drawer's or blank.
            invariant AdmittedIsOwn: if fg == 1 && ta == 0 { fa == 0 } else { 1 == 1 };
            invariant AdmittedAltIsOwn:
                if fg == 1 && alt == 1 && aa == 1 { aw == 0 } else { 1 == 1 };
            // A stopped drawer's spans grow by no row its seal did not see.
            invariant SuspendedSpansStaySealed: ga == 0;
            // Marks are retired by liveness, never by count.
            invariant StoppedKeepsMark: if ra == 1 { ma == 1 } else { 1 == 1 };
            // What nobody took, the fence keeps: a stop, a full-screen tool
            // and an `fg` keep the card.
            invariant CardSurvives: if ra == 1 && hurt == 0 { ka == 1 } else { 1 == 1 };
        }
    }
}
