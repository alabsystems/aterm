// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// What a Mac's peers say of its clock (docs/DESIGN-fluid-swarm-2026-10-28.md F22): Mac A, whose
/// clock is right or a day or more ahead or behind, hears honest B and C, whose readings of A
/// follow A's clock, and a liar L, which reports whatever it likes. A clock step voids every
/// reading (a step forward makes them stale, a step back dates them in the future). A changed
/// reading is judged before anything reads the standing, as every claims read and write judges
/// the store on its own clock; A writes no claim row while Off.
///
/// `skew`: within T (0), ahead (1), behind (2). `rb`, `rc`, `rl`: B's, C's and L's last
/// fresh reading of A, none (0), Agree (1), Ahead (2), Behind (3); an honest one is `skew + 1`.
/// `lie`: what L reports next, which `Lie` moves through each class in turn. `st`: Unknown (0), Agrees (1), Split (2), Off (3). `dirty`: a
/// reading changed since `st` was judged. `woff`: A wrote while Off. `lastoff`: the last judgement
/// that counted a reading said Off (1) or Agrees or Split (0); silence (no reading) keeps the
/// standing Off where it was Off, so a sleep or a clock set since clears nothing.
///
/// Mutants: 1, Off on any one dissent; 2, a tie is Agrees; 3, A votes for itself; 4, no reading
/// is Split; 5, `Write` ungated; 6, no reading is Unknown even after Off.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn claim_clock_model() -> Model {
    aterm_spec::ty_model! {
        DbClaimClock {
            const Buggy = 0;
            var skew = 0; var rb = 0; var rc = 0; var rl = 0; var lie = 1;
            var st = 0; var dirty = 0; var woff = 0; var lastoff = 0;
            action Ahead when (skew == 0 || skew == 2) { skew = 1; rb = 0; rc = 0; rl = 0; dirty = 1; }
            action Behind when (2 > skew) { skew = 2; rb = 0; rc = 0; rl = 0; dirty = 1; }
            action Fix when (skew > 0) { skew = 0; rb = 0; rc = 0; rl = 0; dirty = 1; }
            action Lie { lie = if lie == 3 {1} else {lie + 1}; }
            action HearB { rb = skew + 1; dirty = 1; }
            action HearC { rc = skew + 1; dirty = 1; }
            action HearL { rl = lie; dirty = 1; }
            action ForgetB when (rb > 0) { rb = 0; dirty = 1; }
            action ForgetC when (rc > 0) { rc = 0; dirty = 1; }
            action ForgetL when (rl > 0) { rl = 0; dirty = 1; }
            action Judge when (dirty == 1) {
                dirty = 0;
                lastoff = if ((if rb > 0 {1} else {0}) + (if rc > 0 {1} else {0}) + (if rl > 0 {1} else {0}) + (if Buggy == 3 {1} else {0})) == 0 {
                    lastoff
                } else {
                    if ((if rb == 2 {1} else {0}) + (if rc == 2 {1} else {0}) + (if rl == 2 {1} else {0})) + ((if rb == 2 {1} else {0}) + (if rc == 2 {1} else {0}) + (if rl == 2 {1} else {0})) > ((if rb > 0 {1} else {0}) + (if rc > 0 {1} else {0}) + (if rl > 0 {1} else {0}) + (if Buggy == 3 {1} else {0}))
                        || ((if rb == 3 {1} else {0}) + (if rc == 3 {1} else {0}) + (if rl == 3 {1} else {0})) + ((if rb == 3 {1} else {0}) + (if rc == 3 {1} else {0}) + (if rl == 3 {1} else {0})) > ((if rb > 0 {1} else {0}) + (if rc > 0 {1} else {0}) + (if rl > 0 {1} else {0}) + (if Buggy == 3 {1} else {0}))
                        || (Buggy == 1 && ((if rb == 2 {1} else {0}) + (if rc == 2 {1} else {0}) + (if rl == 2 {1} else {0})) + ((if rb == 3 {1} else {0}) + (if rc == 3 {1} else {0}) + (if rl == 3 {1} else {0})) > 0) {1} else {0}
                };
                st = if ((if rb > 0 {1} else {0}) + (if rc > 0 {1} else {0}) + (if rl > 0 {1} else {0}) + (if Buggy == 3 {1} else {0})) == 0 {
                    if Buggy == 4 {2} else {if st == 3 {if Buggy == 6 {0} else {3}} else {0}}
                } else {
                    if ((if rb == 2 {1} else {0}) + (if rc == 2 {1} else {0}) + (if rl == 2 {1} else {0})) + ((if rb == 2 {1} else {0}) + (if rc == 2 {1} else {0}) + (if rl == 2 {1} else {0})) > ((if rb > 0 {1} else {0}) + (if rc > 0 {1} else {0}) + (if rl > 0 {1} else {0}) + (if Buggy == 3 {1} else {0}))
                        || ((if rb == 3 {1} else {0}) + (if rc == 3 {1} else {0}) + (if rl == 3 {1} else {0})) + ((if rb == 3 {1} else {0}) + (if rc == 3 {1} else {0}) + (if rl == 3 {1} else {0})) > ((if rb > 0 {1} else {0}) + (if rc > 0 {1} else {0}) + (if rl > 0 {1} else {0}) + (if Buggy == 3 {1} else {0}))
                        || (Buggy == 1 && ((if rb == 2 {1} else {0}) + (if rc == 2 {1} else {0}) + (if rl == 2 {1} else {0})) + ((if rb == 3 {1} else {0}) + (if rc == 3 {1} else {0}) + (if rl == 3 {1} else {0})) > 0) {
                        3
                    } else {
                        if ((if rb == 1 {1} else {0}) + (if rc == 1 {1} else {0}) + (if rl == 1 {1} else {0}) + (if Buggy == 3 {1} else {0})) + ((if rb == 1 {1} else {0}) + (if rc == 1 {1} else {0}) + (if rl == 1 {1} else {0}) + (if Buggy == 3 {1} else {0})) > ((if rb > 0 {1} else {0}) + (if rc > 0 {1} else {0}) + (if rl > 0 {1} else {0}) + (if Buggy == 3 {1} else {0}))
                            || (Buggy == 2 && ((if rb == 1 {1} else {0}) + (if rc == 1 {1} else {0}) + (if rl == 1 {1} else {0}) + (if Buggy == 3 {1} else {0})) + ((if rb == 1 {1} else {0}) + (if rc == 1 {1} else {0}) + (if rl == 1 {1} else {0}) + (if Buggy == 3 {1} else {0})) == ((if rb > 0 {1} else {0}) + (if rc > 0 {1} else {0}) + (if rl > 0 {1} else {0}) + (if Buggy == 3 {1} else {0}))) {
                            1
                        } else {
                            2
                        }
                    }
                };
            }
            action Write when (dirty == 0 && (3 > st || Buggy == 5)) {
                woff = if st == 3 {1} else {woff};
            }
            invariant LiarCannotPush: dirty == 1 || 1 > rb || rb > 1 || 1 > rc || rc > 1 || st == 1;
            invariant HonestMajorityStops: dirty == 1 || skew == 0 || rb == 0 || rc == 0 || st == 3;
            invariant NoBlindAgree: dirty == 1 || skew == 0 || (rb == 0 && rc == 0) || 1 > st || st > 1;
            invariant LoneKeepsWorking: dirty == 1 || rb > 0 || rc > 0 || rl > 0 || st == 0 || (st == 3 && lastoff == 1);
            invariant SilenceKeepsOff: dirty == 1 || lastoff == 0 || st == 3;
            invariant OffWritesNothing: woff == 0;
        }
    }
}
