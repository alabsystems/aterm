// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WINDOW'S CHROME REGROW FLOOR (design ruling 414): after any
//! rise of a window's chrome rows, neither the band's quiet fall nor the tab
//! strip's fall re-grids the window back inside the settle — whichever source
//! rose.

use aterm_spec::derive::*;

/// Two chrome sources of one window's grid — the message band and the tab
/// strip — and the floor across them: `aterm_messages::chrome`
/// (`RegrowFloor`, `fall_allowed`, `at_floor`), read by the band's quiet
/// shrink (`MessageCenter::commit_rows` through `drive::Commit::floor`) and
/// by the strip's fall (`App::set_tab_strip_rows`, `App::settle_strip_fall`).
/// One tick is `CHROME_REGROW_SETTLE` (`Settle = 1`, 400 ms).
///
/// THE BUG (measured 2026-09-29 on main `8b24e99a4`, real windows, a
/// stand-in that reads its size 20 ms after the `SIGWINCH` and ticks a
/// spinner every 5 ms). The band spaced its own fall from its own rise (D1,
/// `SHRINK_QUIET`, 1.5 s), but nothing spaced it from another source's
/// rise: a chrome row rising milliseconds before the band's quiet shrink made
/// a net-zero shrink-then-grow pair; on the alternate screen it shifts every
/// row up one, and the grid's resize undo repairs it only while nothing is
/// written between the halves. `BandFallOwnQuiet` is that law as it shipped.
///
/// THE SOURCES. The presence row was the other measured source until the
/// owner surfaces (2026-10-16) took it out — presence owns no chrome row now
/// — so the second source here is the TAB STRIP, the one other chrome row a
/// window's grid gives way to: its rise (`App::set_tab_strip_rows` growing
/// `tab_strip_rows`) is noted on every window's floor, and its fall — a
/// person's Settings edit — waits the floor too (`StripFall`). Until
/// 2026-10-19 that fall was immediate and exempt by name; the windowed lane
/// (`aterm-lab presence-flap --window --phases X`) then measured a band row
/// and a Settings edit's strip fall re-gridding a window 38 ms apart.
/// `StripFallAtOnce` is that law as it shipped.
///
/// Variables: `bw`/`b` — the band wants a row / the grid reserves it (its
/// committed count); `sw`/`s` — the Settings ask for the strip's rows / the
/// grid reserves them (`App::strip_fall` holds the fall between). A Settings
/// edit down is `StripUnwant`; it re-grids at once when the floor has run (the
/// host takes `StripUnwant` and `StripFall` in one call), else at the floor.
/// An edit back while the fall waits (`StripBack`) re-grids nothing. A band RISE is taken on
/// the step that wants it (`BandRise`: the want and its re-grid in one
/// step), and a want back while a fall waits (`BandBack`) re-grids nothing.
/// `bq`: the band's OWN quiet has run for its pending fall (`BandQuiet`, the
/// band's `SHRINK_QUIET` past its want's drop). `age` — the ground truth:
/// ticks since the last rise of the window's total, saturating at `Settle`;
/// `since` — the same span as the ENGINE counts it
/// (`RegrowFloor::last_rise` against the clock), which its falls decide by.
/// `cramped`: the window's own frame has no room for a reserved row (a drag,
/// a zoom). `quick`: an unforced fall was applied inside the settle of the
/// last rise. `flips` bounds the environment.
///
/// `bq` reads `1` whenever no fall of the band is pending, so the state is
/// what a projection of the real host reads.
///
/// `BandFall` is the fixed law: a fall once its own quiet has run AND the
/// floor has (`Settle <= since`). `BandTaken` is the fall exempt by name, as
/// it is from the band's own quiet: the window's own shrink takes a waiting
/// fall at once (D1's exception). A rise is never delayed: the
/// grow-then-shrink order is benign (the shrink trims the blank row the grow
/// appended).
///
/// ONE WINDOW, AND ONLY A WAITING FALL TAKEN. The machine is one window's
/// grid. The band re-grids every window, so the host hands the band the
/// LATEST floor of every window with glass (`App::band_regrow_floor`,
/// `chrome::latest`). The take is modelled for a fall already waiting
/// (`bw == 0`); the real law also takes a row still WANTED when the window
/// cannot afford it — forced like this one, no fall the floor holds.
///
/// `NoQuickRegrow` — no unforced fall inside the settle of the last rise,
/// whichever source rose and whichever falls. `TheAnchorIsTheLastRise` — the
/// engine's count is the rise's real age: the floor is anchored on the last
/// rise and nothing else. Three `Buggy = 1` mutants, each an action of its own
/// and dead at the committed config: `BandFallOwnQuiet` (the band's law before
/// the floor: `StripRise` just before it is the rise-then-bandshrink trace),
/// `StripFallAtOnce` (the strip's law before 2026-10-19: `BandRise` just
/// before a Settings edit is the band-rise-then-stripfall trace) and
/// `Restart` (the floor re-anchored on a step that is no rise — it breaks
/// the anchor, and since the host re-steps a waiting fall at every wake, it
/// starves the fall: [`chrome_regrow_floor_liveness`]).
///
/// Tier-1: `aterm-messages`' `chrome` and `drive_tests` drive the engine
/// law; `aterm-gui`'s `chrome_regrow_conformance` drives the REAL App path on
/// a headless window standing in for glass, on a clock the rig steps — the
/// band through `MessageCenter::post`/`dismiss` and `App::settle_messages`,
/// the strip through `App::set_tab_strip_rows` and `App::settle_strip_fall`,
/// the window's own shrink as the stand-in's terminal rows taken to none —
/// over the measured schedules and the window's take inside the floor: every
/// host decision, a fall taken or declined, must be the model's
/// (`interp::admits`, `Model::action_enabled`), and the take must be
/// `BandTaken` by name. NEGATIVE CONTROLS: each measured schedule on a window
/// with no floor is admitted only at `Buggy = 1`, as `BandFallOwnQuiet` and
/// `StripFallAtOnce`. `#[refines]` anchors, each driven by a schedule:
/// `BandRise`/`BandFall`/`BandTaken` on `App::regrid_message_band`,
/// `StripRise`/`StripUnwant`/`StripBack` on `App::set_tab_strip_rows`,
/// `StripFall` on `App::settle_strip_fall`; the rest waived.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn chrome_regrow_floor_model() -> Model {
    aterm_spec::ty_model! {
        ChromeRegrowFloor {
            const Buggy = 0;
            const Settle = 1;
            const FlipCap = 4;
            var bw = 0;
            var b = 0;
            var sw = 0;
            var s = 0;
            // No fall pending: the band's own quiet long run.
            var bq = 1;
            // The window starts long settled.
            var age = 1;
            var since = 1;
            var cramped = 0;
            var quick = 0;
            var flips = 0;

            // A band row reaches the glass: the grid shrinks at once, and the
            // floor anchors on the rise.
            action BandRise when (b == 0 && flips <= FlipCap - 1) {
                bw = 1;
                b = 1;
                age = 0;
                since = 0;
                flips = flips + 1;
            }
            // The tab strip grows: every window's grid shrinks, and the floor
            // anchors on the rise.
            action StripRise when (s == 0 && flips <= FlipCap - 1) {
                sw = 1;
                s = 1;
                age = 0;
                since = 0;
                flips = flips + 1;
            }
            // A person's Settings edit takes the strip down: its fall waits.
            action StripUnwant when (sw == 1 && flips <= FlipCap - 1) {
                sw = 0;
                flips = flips + 1;
            }
            // An edit back while the fall waits: no re-grid, no rise.
            action StripBack when (sw == 0 && s == 1 && flips <= FlipCap - 1) {
                sw = 1;
                flips = flips + 1;
            }
            // THE FIXED LAW for the strip: its fall once the floor has run.
            action StripFall when (sw == 0 && s == 1 && Settle <= since) {
                s = 0;
                quick = if (age <= Settle - 1) { 1 } else { quick };
            }
            // A want back while its fall waits: no re-grid, no rise, and no
            // fall pending (its own quiet reads as run, as with none).
            action BandBack when (bw == 0 && b == 1 && flips <= FlipCap - 1) {
                bw = 1;
                bq = 1;
                flips = flips + 1;
            }
            // The band gives its row back: its own quiet starts.
            action BandUnwant when (bw == 1 && flips <= FlipCap - 1) {
                bw = 0;
                bq = 0;
                flips = flips + 1;
            }
            // The band's own quiet runs out for its pending fall.
            action BandQuiet when (bw == 0 && b == 1 && bq == 0) {
                bq = 1;
            }
            // Time: one tick of the floor's clock.
            action Tick when (age <= Settle - 1 || since <= Settle - 1) {
                age = if (age <= Settle - 1) { age + 1 } else { age };
                since = if (since <= Settle - 1) { since + 1 } else { since };
            }
            // THE FIXED LAW: a quiet fall once its own quiet AND the floor
            // have run.
            action BandFall when (bw == 0 && b == 1 && bq == 1 && Settle <= since) {
                b = 0;
                quick = if (age <= Settle - 1) { 1 } else { quick };
            }
            // The window's own frame loses its room, and gets it back.
            action Cramp when (cramped == 0 && flips <= FlipCap - 1) {
                cramped = 1;
                flips = flips + 1;
            }
            action Uncramp when (cramped == 1) {
                cramped = 0;
            }
            // THE EXEMPT FALL: the window's own shrink takes a waiting fall
            // at once — neither quiet nor the floor applies.
            action BandTaken when (cramped == 1 && bw == 0 && b == 1) {
                b = 0;
                bq = 1;
            }
            // MUTANT (the band's law before the floor): the quiet shrink once
            // its own quiet has run, however recently another source rose.
            action BandFallOwnQuiet when (Buggy == 1 && bw == 0 && b == 1 && bq == 1
                && since <= Settle - 1)
            {
                b = 0;
                quick = if (age <= Settle - 1) { 1 } else { quick };
            }
            // MUTANT (the strip's law before 2026-10-19): a Settings edit's
            // fall at once, however recently another source rose.
            action StripFallAtOnce when (Buggy == 1 && sw == 0 && s == 1
                && since <= Settle - 1)
            {
                s = 0;
                quick = if (age <= Settle - 1) { 1 } else { quick };
            }
            // MUTANT: a step that is no rise re-anchors the floor while a fall
            // waits.
            action Restart when (Buggy == 1 && Settle <= since && (bw <= b - 1 || sw <= s - 1)) {
                since = 0;
            }

            invariant NoQuickRegrow: quick == 0;
            invariant TheAnchorIsTheLastRise: since == age;
        }
    }
}

/// THE GRID REGROWS: `[]<>(b <= bw && s <= sw)` — again and again, the
/// band's and the strip's pending falls have been applied. The floor only
/// ever DELAYS a fall (by at most `CHROME_REGROW_SETTLE` past another
/// source's rise), never loses one: a row given up is never reserved blank
/// for good.
///
/// THE ENVIRONMENT IS NOT FAIR: the band's want, the strip, the frame's room
/// — any of them may happen or not. What the verdict assumes, and nothing
/// more: `Tick` (time passes), `BandQuiet` (the band's own quiet runs out),
/// `BandFall` (the host takes a fall that is due — the band's state
/// deadline names the later of the fall's own due and the floor,
/// `chrome::at_floor`, and the loop wakes there) and `StripFall` (the same
/// wake takes the strip's, `App::strip_fall_due`), each weakly fair and each
/// load-bearing (`verify::audit_liveness`). The mutant that breaks it alone:
/// `Restart` — a floor re-anchored on every step never runs out while the
/// host keeps stepping.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(chrome_regrow_floor_model)]
pub fn chrome_regrow_floor_liveness() -> Liveness {
    Liveness {
        name: "TheGridRegrows",
        goal: and_(le(var("b"), var("bw")), le(var("s"), var("sw"))),
        weak: vec!["Tick", "BandQuiet", "BandFall", "StripFall"],
        strong: vec![],
        mutants: vec!["Restart"],
    }
}
