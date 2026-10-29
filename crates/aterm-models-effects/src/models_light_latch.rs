// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The light latch (`docs/design/PRISM-GLASS.md` §2.1, §15.2 1.2): a window's frames draw by the
//! light rules only while the front session's configured scheme is light and the live page is a
//! light ground, entered at once on a fresh key, on a page back within the hold of last being lit,
//! or after the hold, and left on the frame the page or the scheme goes dark.

use aterm_spec::derive::Model;

/// One window's latch over ticks of 100 ms (`Hold` is the 300 ms entry hold), against ghosts that
/// keep the rule's own account of each frame.
///
/// The environment moves the page (`Page`: DECSCNM, OSC 10/11), the key within one side
/// (`Rescheme`: another front session, or another configured pair on the same side) and the
/// scheme across sides (`Flip`: an OS appearance flip, a Settings change, a tab to a session in
/// the other side's theme); `Tick` lets time pass with no frame. A key is `key + 2 * scheme`, so
/// the four are distinct. `Frame` is one `LightLatch::admits` call: `seen`, `pend`, `since`,
/// `last` and the outputs `on`, `fell` and `changed` are the latch's; `gkey`, `gpend`, `run`,
/// `glast`, `lic` and `gchg` are the ghosts, which no control touches: the key of the last frame,
/// whether a key change met no light frame yet, the run of light frames the frame closes, the
/// last frame the rule itself lit (written only on a frame whose licence the ghosts grant, so a
/// latch that corrupts its own `last` cannot widen the licence it is judged by), whether the
/// frame holds a licence to enter, and whether its key moved. `since == Late` is no light run,
/// and `last` and `glast` are 0 until a frame is lit, more than the hold before the first tick
/// (4). Every update reads the state before the frame (the actions' assignments are
/// simultaneous), so `lic` reads the `glast` of the frames before it.
///
/// `Buggy = 1` is v5's short-circuit (`ok && admits(..)`): a dark frame never reaches the latch,
/// so its run and its record of the key go stale. `Buggy = 2` consumes a key change on a dark
/// frame (§2.1 as written, `scheme_bg.replace`). `Buggy = 3` drops the scheme gate (v4's raw page
/// alone), `Buggy = 4` latches the exit too (v3's 300 ms both ways, the third check's dark 0),
/// and `Buggy = 5` writes `last` on every light frame, lit or not, so a page back after the hold
/// enters on its second frame.
/// Tier-1 (`aterm-effects/tests/light_latch_conformance.rs`) walks the real latch over every
/// transition this model reaches and holds every control to a frame where the latch leaves it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn light_latch_model() -> Model {
    aterm_spec::ty_model! {
        LightLatch {
            const Buggy = 0;
            const Hold = 3;
            const Horizon = 8;
            const Late = 99;
            var now = 4;
            var raw = 0;
            var scheme = 0;
            var key = 1;
            var seen = 0;
            var pend = 0;
            var since = 99;
            var last = 0;
            var on = 0;
            var fell = 0;
            var changed = 0;
            var fraw = 0;
            var fscheme = 0;
            var gkey = 0;
            var gpend = 0;
            var run = 99;
            var glast = 0;
            var lic = 0;
            var gchg = 0;

            action Page { raw = 1 - raw; }
            action Rescheme { key = 3 - key; }
            action Flip { scheme = 1 - scheme; }
            action Tick when (now <= Horizon - 1) { now = now + 1; }
            action Frame {
                since = if raw == 1 && (scheme == 1 || Buggy == 3) {
                    if since == Late { now } else { since }
                } else if Buggy == 1 { since } else { Late };
                last = if raw == 1 && (scheme == 1 || Buggy == 3) {
                    if Buggy == 5
                        || (if seen == key + scheme + scheme { pend } else { 1 }) == 1
                        || (if since == Late { now } else { since }) <= now - Hold
                        || (if since == Late { now } else { since }) <= last + Hold
                    { now } else { last }
                } else { last };
                on = if raw == 1 && (scheme == 1 || Buggy == 3) {
                    if (if seen == key + scheme + scheme { pend } else { 1 }) == 1
                        || (if since == Late { now } else { since }) <= now - Hold
                        || (if since == Late { now } else { since }) <= last + Hold
                    { 1 } else { 0 }
                } else if Buggy == 4 && now <= last + Hold { 1 } else { 0 };
                fell = if on == 1 {
                    if raw == 1 && (scheme == 1 || Buggy == 3) {
                        if (if seen == key + scheme + scheme { pend } else { 1 }) == 1
                            || (if since == Late { now } else { since }) <= now - Hold
                            || (if since == Late { now } else { since }) <= last + Hold
                        { 0 } else { 1 }
                    } else if Buggy == 4 && now <= last + Hold { 0 } else { 1 }
                } else { 0 };
                pend = if raw == 1 && (scheme == 1 || Buggy == 3) { 0 }
                    else if Buggy == 1 { pend }
                    else if Buggy == 2 { 0 }
                    else if seen == key + scheme + scheme { pend } else { 1 };
                seen = if raw == 1 && (scheme == 1 || Buggy == 3) { key + scheme + scheme }
                    else if Buggy == 1 { seen } else { key + scheme + scheme };
                changed = if Buggy == 1 && (raw == 0 || scheme == 0) { 0 }
                    else if seen == key + scheme + scheme { 0 } else { 1 };
                fraw = raw;
                fscheme = scheme;
                run = if raw == 1 && scheme == 1 {
                    if run == Late { now } else { run }
                } else { Late };
                glast = if raw == 1 && scheme == 1 {
                    if (if gkey == key + scheme + scheme { gpend } else { 1 }) == 1
                        || (if run == Late { now } else { run }) <= now - Hold
                        || (if run == Late { now } else { run }) <= glast + Hold
                    { now } else { glast }
                } else { glast };
                lic = if (if gkey == key + scheme + scheme { gpend } else { 1 }) == 1
                    || (if raw == 1 && scheme == 1 {
                        if run == Late { now } else { run }
                    } else { Late }) <= now - Hold
                    || (if raw == 1 && scheme == 1 {
                        if run == Late { now } else { run }
                    } else { Late }) <= glast + Hold
                { 1 } else { 0 };
                gpend = if raw == 1 && scheme == 1 { 0 }
                    else if gkey == key + scheme + scheme { gpend } else { 1 };
                gkey = key + scheme + scheme;
                gchg = if gkey == key + scheme + scheme { 0 } else { 1 };
            }

            invariant NoDarkSchemeEnters: on <= fscheme;
            invariant ExitIsImmediate: on <= fraw;
            invariant EntryIsLicensed: on <= lic;
            invariant ALicensedLightFrameEnters:
                if fraw == 1 && fscheme == 1 { lic <= on } else { on <= 1 };
            invariant EveryFrameSeesTheKey: changed == gchg;
            bound NowBounded: now <= Horizon;
            bound ClocksBounded: since <= Late && run <= Late && last <= Horizon
                && glast <= Horizon;
            bound KeysBounded: key <= 2 && seen <= 4 && gkey <= 4;
            bound FlagsBounded: raw <= 1 && scheme <= 1 && pend <= 1 && gpend <= 1 && on <= 1
                && fell <= 1 && changed <= 1 && gchg <= 1 && lic <= 1 && fraw <= 1
                && fscheme <= 1;
        }
    }
}
