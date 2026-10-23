// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Holder fence model: which main-screen rows a card drawer wrote while holding the terminal.

use aterm_spec::derive::Model;

/// The fence's protocol over one subject drawer `A`.
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
                ma = if Buggy == 10 { 0 } else { ma };
                ta = if Buggy == 10 { 0 } else { ta };
                fa = if Buggy == 10 { 0 } else { fa };
                ga = if Buggy == 10 { 0 } else { ga };
                ka = if Buggy == 10 { 0 } else { ka };
                spans = if Buggy == 10 { 0 } else { spans };
                marks = if Buggy == 10 { 1 } else { ma + 1 };
            }

            action StopA when (fg == 1) {
                fg = 0;
                aa = if Buggy == 11 { aa } else { 0 };
                spans = if idle == 1 { spans - 1 } else { spans };
                idle = 0;
            }

            action StopB when (fg == 2) {
                fg = 0;
            }

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

            action Rewind when (fg == 0 && ink == 0 && alt == 0) {
                ink = 1;
            }

            action Climb when (fg == 0 && ink == 0 && alt == 0) {
                ink = 2;
            }

            action Settle when (fg == 0 && ink > 0 && alt == 0) {
                ink = 0;
            }

            action Scribble when (fg == 0 && ma == 1 && spans > 0 && ta == 0 && fa == 0 && alt == 0) {
                fa = 1;
                ka = 0;
                hurt = 1;
            }

            action Draw when (fg == 1 && alt == 0) {
                idle = 0;
            }

            action BandMove when (fg == 1 && alt == 0) {
                fa = if Buggy == 1 && ta == 0 { 1 } else { fa };
            }

            action Splice when (fg == 0 && ma == 1 && spans > 0 && alt == 0) {
                ga = if Buggy == 2 { 1 } else { ga };
            }

            action Reflow when (ink == 0 && alt == 0 && (fg == 1 || ra == 0)) {
                fa = if Buggy == 3 && ma == 1 && ta == 0 { 1 } else { fa };
            }

            action Renumber when (alt == 0 && (fg == 1 || ra == 0)) {
                ta = if Buggy == 4 { ta } else { if ma == 1 && spans > 0 { 1 } else { ta } };
                fa = if Buggy == 4 { if ma == 1 && ta == 0 { 1 } else { fa } } else { 0 };
                ka = if Buggy == 4 { ka } else { 0 };
                hurt = if Buggy == 4 { hurt } else { ra };
                tb = if Buggy == 4 { tb } else { mb };
                idle = 0;
            }

            action Ed3 when (alt == 0) {
                ink = ink;
            }

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

            action Admit when (fg == 1) {
                ink = ink;
            }

            bound Bounds:
                fg <= 2 && marks <= MaxMarks && spans <= MaxSpans && ink <= 2;
            invariant AdmittedIsOwn: if fg == 1 && ta == 0 { fa == 0 } else { 1 == 1 };
            invariant AdmittedAltIsOwn:
                if fg == 1 && alt == 1 && aa == 1 { aw == 0 } else { 1 == 1 };
            invariant SuspendedSpansStaySealed: ga == 0;
            invariant StoppedKeepsMark: if ra == 1 { ma == 1 } else { 1 == 1 };
            invariant CardSurvives: if ra == 1 && hurt == 0 { ka == 1 } else { 1 == 1 };
        }
    }
}
