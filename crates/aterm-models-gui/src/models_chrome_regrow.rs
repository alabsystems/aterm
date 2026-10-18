// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WINDOW'S CHROME REGROW FLOOR (design ruling 414): after any
//! rise of a window's chrome rows, no chrome source's quiet fall re-grids the
//! window back inside the settle — whichever source rose.

use aterm_spec::derive::*;

/// Two chrome sources of one window's grid — the message band and the
/// presence row — each spacing its own fall by its own quiet, and the floor
/// across them: `aterm_messages::chrome` (`RegrowFloor`, `fall_allowed`,
/// `at_floor`), read by the band's quiet shrink (`MessageCenter::commit_rows`
/// through `drive::Commit::floor`) and the presence row's quiet fold
/// (`presence::drive::commit_rows` through `Desk::chrome_floor`). One tick is
/// `CHROME_REGROW_SETTLE` (`Settle = 1`, 400 ms).
///
/// THE BUG (measured 2026-09-29 on main `8b24e99a4`, real windows, a
/// stand-in that reads its size 20 ms after the `SIGWINCH` and ticks a
/// spinner every 5 ms). Each source already spaced its own fall from its own
/// rise — the band's D1 (`SHRINK_QUIET`, 1.5 s) and the presence row's fold
/// quiet (`FOLD_QUIET`, 1.5 s, ruling 394) — so every single-source pair was
/// 1.5 s apart. Across sources nothing was: the presence row BORN 4–6 ms
/// before the band's own quiet shrink (3 of 3 runs desynced), a band row
/// RISING 11–13 ms before the presence row's timed fold (3 of 3), and a band
/// row rising 3–6 ms before a person's key folded a presence row that had
/// stood its quiet (2 of 3). Each is a net-zero shrink-then-grow pair; on the
/// alternate screen it shifts every row up one, and the grid's resize undo
/// repairs it only while nothing is written between the halves — the
/// spinner's tick between them dropped it. `BandFallOwnQuiet` and
/// `PresFallOwnQuiet` are those two laws as they shipped: a fall once its own
/// quiet has run, whatever the other source did just before.
///
/// Variables: `bw`/`b` — the band wants a row / the grid reserves it (its
/// committed count); `pw`/`p` the same for the presence row. A RISE is taken
/// on the step that wants it (`BandRise`, `PresRise`: the want and its
/// re-grid in one step — the presence row's holds, a driver's hand and a
/// handoff's freeze, are `DriverGeometry`'s), and a want back while a fall
/// waits (`BandBack`, `PresBack`) re-grids nothing. `bq`/`pq`: the source's
/// OWN quiet has run for its pending fall (`BandQuiet`, `PresQuiet`: the
/// band's `SHRINK_QUIET` past its want's drop, the presence row's `FOLD_QUIET`
/// past its want's drop or a driver's look; `PresRead` is a person's read of
/// a row that has stood its quiet since birth, whose own due has already
/// passed). `age` — the ground truth: ticks since the last rise of the
/// window's total, saturating at `Settle`; `since` — the same span as the
/// ENGINE counts it (`RegrowFloor::last_rise` against the clock), which its
/// falls decide by. `cramped`: the window's own frame has no room for a
/// reserved row (a drag, a zoom). `quick`: an unforced fall was applied
/// inside the settle of the last rise. `flips` bounds the environment.
///
/// The TAB STRIP is a third source, outside this machine: its rise is noted
/// on every window as the band's is (so both falls here wait it), and its
/// fall — a person's edit of Settings — stays immediate, unfloored.
///
/// `bq`/`pq` read `1` whenever no fall of that source is pending, so the
/// state is what a projection of the real host reads.
///
/// `BandFall` and `PresFall` are the fixed laws: a fall once its own quiet has
/// run AND the floor has (`Settle <= since`). `BandTaken` and `PresTaken` are
/// the falls exempt by name, as they are from the sources' own quiet: the
/// window's own shrink takes a waiting fall at once (D1's exception), and
/// `PresTaken` also stands for the person's View-menu hide of the presence
/// band. A rise is never delayed: the grow-then-shrink order is benign (the
/// shrink trims the blank row the grow appended).
///
/// ONE WINDOW, AND ONLY A WAITING FALL TAKEN. The machine is one window's
/// grid. The band re-grids every window, so the host hands the band the
/// LATEST floor of every window with glass (`App::band_regrow_floor`,
/// `chrome::latest`); a rise in another window is a rise here for the
/// band's fall, which the Tier-1 rig's two-window schedule drives. The takes
/// are modelled for a fall already waiting (`bw == 0`, `pw == 0`). The real
/// laws also take a row that is still WANTED when the window cannot afford
/// it (`afford < committed`, whatever the want). That take is forced like
/// these, it is no fall the floor holds, and the row's return is an
/// ordinary rise, so `NoQuickRegrow` has nothing to see in it. Admitting it
/// here would also reach states no projection of the host reads (a want
/// dropped with no row, its quiet pending).
///
/// `NoQuickRegrow` — no unforced fall inside the settle of the last rise,
/// whichever source rose. `TheAnchorIsTheLastRise` — the engine's count is
/// the rise's real age: the floor is anchored on the last rise and nothing
/// else. Three `Buggy = 1` mutants, each an action of its own and dead at the
/// committed config: `BandFallOwnQuiet` (the band's law before the floor:
/// `PresRise` just before it is the raise-then-bandshrink trace),
/// `PresFallOwnQuiet` (the fold law before it: `BandRise` just before it is
/// the band-then-fold trace, timed or read), and `Restart` (the floor
/// re-anchored on a step that is no rise — it breaks the anchor, and since the
/// host re-steps a waiting fall at every wake, it starves the fall:
/// [`chrome_regrow_floor_liveness`]).
///
/// Tier-1: `aterm-messages`' `chrome`, `drive_tests` and
/// `presence_drive_tests` drive the engine law; `aterm-gui`'s
/// `chrome_regrow_conformance` drives the REAL App path on a headless window
/// standing in for glass, on a clock the rig steps — the band through
/// `MessageCenter::post`/`dismiss` and `App::settle_messages`, the presence
/// row through a turn's lease and `App::on_presence_wake`, a told story and
/// `App::note_human_acted`, the wake through `App::settle_messages` and
/// `App::presence_tick`, the window's own shrink as the stand-in's terminal
/// rows taken to none — over the three measured schedules and the window's
/// two takes inside the floor: every host decision, a fall taken or
/// declined, must be the model's (`interp::admits`, `Model::action_enabled`),
/// and each take must be `BandTaken` or `PresTaken` by name. NEGATIVE
/// CONTROL: the measured schedules on a window with no floor are admitted
/// only at `Buggy = 1`, as the two own-quiet mutants. A two-window schedule
/// (outside this one-window machine) pins that the band waits another
/// window's floor. `#[refines]` anchors, each driven by a schedule:
/// `BandRise`/`BandFall`/`BandTaken` on `App::regrid_message_band`,
/// `PresRise`/`PresFall`/`PresTaken` on `App::regrid_window_for_chrome_rows`;
/// the rest waived. The View-menu hide half of `PresTaken` is pinned by the
/// engine's `presence_drive_tests` only.
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
            var pw = 0;
            var p = 0;
            // No fall pending: each source's own quiet long run.
            var bq = 1;
            var pq = 1;
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
            // The presence row is born: the same.
            action PresRise when (p == 0 && flips <= FlipCap - 1) {
                pw = 1;
                p = 1;
                age = 0;
                since = 0;
                flips = flips + 1;
            }
            // A want back while its fall waits: no re-grid, no rise, and no
            // fall pending (its own quiet reads as run, as with none).
            action BandBack when (bw == 0 && b == 1 && flips <= FlipCap - 1) {
                bw = 1;
                bq = 1;
                flips = flips + 1;
            }
            action PresBack when (pw == 0 && p == 1 && flips <= FlipCap - 1) {
                pw = 1;
                pq = 1;
                flips = flips + 1;
            }
            // A source gives its row back: its own quiet starts.
            action BandUnwant when (bw == 1 && flips <= FlipCap - 1) {
                bw = 0;
                bq = 0;
                flips = flips + 1;
            }
            action PresUnwant when (pw == 1 && flips <= FlipCap - 1) {
                pw = 0;
                pq = 0;
                flips = flips + 1;
            }
            // A person reads a row that has stood its quiet since birth: its
            // own due has already passed.
            action PresRead when (pw == 1 && p == 1 && flips <= FlipCap - 1) {
                pw = 0;
                pq = 1;
                flips = flips + 1;
            }
            // A source's own quiet runs out for its pending fall.
            action BandQuiet when (bw == 0 && b == 1 && bq == 0) {
                bq = 1;
            }
            action PresQuiet when (pw == 0 && p == 1 && pq == 0) {
                pq = 1;
            }
            // Time: one tick of the floor's clock.
            action Tick when (age <= Settle - 1 || since <= Settle - 1) {
                age = if (age <= Settle - 1) { age + 1 } else { age };
                since = if (since <= Settle - 1) { since + 1 } else { since };
            }
            // THE FIXED LAWS: a quiet fall once its own quiet AND the floor
            // have run.
            action BandFall when (bw == 0 && b == 1 && bq == 1 && Settle <= since) {
                b = 0;
                quick = if (age <= Settle - 1) { 1 } else { quick };
            }
            action PresFall when (pw == 0 && p == 1 && pq == 1 && Settle <= since) {
                p = 0;
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
            // THE EXEMPT FALLS: the window's own shrink (and, for the
            // presence row, the person's View-menu hide) takes a waiting fall
            // at once — neither quiet nor the floor applies.
            action BandTaken when (cramped == 1 && bw == 0 && b == 1) {
                b = 0;
                bq = 1;
            }
            action PresTaken when (cramped == 1 && pw == 0 && p == 1) {
                p = 0;
                pq = 1;
            }
            // MUTANT (the band's law before the floor): the quiet shrink once
            // its own quiet has run, however recently another source rose.
            action BandFallOwnQuiet when (Buggy == 1 && bw == 0 && b == 1 && bq == 1
                && since <= Settle - 1)
            {
                b = 0;
                quick = if (age <= Settle - 1) { 1 } else { quick };
            }
            // MUTANT (the presence fold law before the floor).
            action PresFallOwnQuiet when (Buggy == 1 && pw == 0 && p == 1 && pq == 1
                && since <= Settle - 1)
            {
                p = 0;
                quick = if (age <= Settle - 1) { 1 } else { quick };
            }
            // MUTANT: a step that is no rise re-anchors the floor while a fall
            // waits.
            action Restart when (Buggy == 1 && Settle <= since
                && (bw <= b - 1 || pw <= p - 1))
            {
                since = 0;
            }

            invariant NoQuickRegrow: quick == 0;
            invariant TheAnchorIsTheLastRise: since == age;
        }
    }
}

/// THE GRID REGROWS: `[]<>(b <= bw /\ p <= pw)` — again and again, every
/// source's pending fall has been applied. The floor only ever DELAYS a fall
/// (by at most `CHROME_REGROW_SETTLE` past another source's rise), never
/// loses one: a row given up is never reserved blank for good.
///
/// THE ENVIRONMENT IS NOT FAIR: a source's want, a person's read, the frame's
/// room — any of them may happen or not. What the verdict assumes, and
/// nothing more: `Tick` (time passes), `BandQuiet` and `PresQuiet` (a source's
/// own quiet runs out), and `BandFall` and `PresFall` (the host takes a fall
/// that is due — the band's state deadline and the presence row's
/// `rows_deadline` both name the later of the fall's own due and the floor,
/// `chrome::at_floor`, and the loop wakes there), each weakly fair and each
/// load-bearing (`verify::audit_liveness`). The mutant that breaks it alone:
/// `Restart` — a floor re-anchored on every step never runs out while the
/// host keeps stepping.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(chrome_regrow_floor_model)]
pub fn chrome_regrow_floor_liveness() -> Liveness {
    Liveness {
        name: "TheGridRegrows",
        goal: and_(le(var("b"), var("bw")), le(var("p"), var("pw"))),
        weak: vec!["Tick", "BandQuiet", "PresQuiet", "BandFall", "PresFall"],
        strong: vec![],
        mutants: vec!["Restart"],
    }
}
