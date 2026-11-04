// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A LINE HELD IN CUSTODY (ATERM-KI-1101-3): a line the loop wrote and gave up on undrawn, on a
//! read that shows none of it, which the screen cannot tell dropped from held.
//!
//! The model (`LineCustody`): one session, one line at a time. `q` is where the loop's last
//! line is: 0 nowhere (never drawn, dropped, gone with its program, or entered), 1 drawn in the
//! composer, 2 read by the program and drawn none of past its minute, 3 read and drawn none of
//! within its minute, 4 queued in the input unread. `bq` is input queued unread behind a line
//! held (`Queues`: a person's typeahead, another client's keys), which the program reads after
//! the line (`ReadsBehind`), and which keeps `status input=` unread meanwhile, so a line read stays
//! held at the gate however long ago its first clear read. A write the loop gives up on undrawn
//! (`WritesQueued`, `WritesRead`) forms the custody (`cust`) with what the first `status` read
//! said (`held`: 0 unread, 1 read, 2 no read can say, on a host that serves no input word,
//! `Served = 0`); a write submitted is `Types`. The custody settles only by evidence: its draw
//! (`LookDrawn`, handed back to its writer, `st = 1`, for its Enter, `Enters`), a person's keys
//! drawn after it (`LookPerson`), its program's end (`SeesEnded`), or a turn read running after
//! it (`TurnRuns`, `ran`) once `status` reads the input clear (`TurnSettles`; a line unread, one
//! with input unread behind it, or one no read can say of, outlasts the turn, `TurnOutlasted`).
//! A break keeps a draw it saw as evidence (`BreakKeepsDraw`, `drawn`) for the point's look. A
//! write a point decides is held (`GateHolds`) until the line was read (`held = 1`), its minute
//! passed since that first clear read (`Minute`, `ripe`), `status` reads the input clear (no
//! `bq`) and the session is at rest (`rest`): then, where no restart asked was answered since the
//! last act submitted (`restart`: 0 none, 1 made, 2 unmade), the loop asks the host for a restart
//! in place (`AsksRestart`, `asked`), which the host makes (`RestartMade`, the program ended
//! with the line), refuses (`RestartUnmade`) or defers (`RestartDeferred`, asked again); with a
//! restart already answered, or with no restart lane (`Lane = 0`: relaunch off, no host, a host
//! that makes none for the program), it waits, lost for good to the loop (`WaitsLost`, `lost`),
//! and the host is told so. The host types (`HostTypes`) and makes its upgrade's move, which ends
//! the agent's process (`HostUpgrades`), only where no line is held or the line held is lost.
//!
//! `dlg` is a box over the composer (`DialogShows`, `DialogGoes`: a permission box, a question),
//! or anything else under which no read can say what the composer holds (no composer drawn, as
//! under a full-screen view, or its glyph's cell unread): the line may stand drawn under it
//! (`q = 1`) or not. Under it the look settles only a draw a break kept, a person's keys and the
//! line's draw are unseen, a turn after the line neither settles nor outlasts it (its evidence,
//! `ran`, is kept for the next read that can say), the gate holds every line's write, as a box is
//! no rest, and no restart is asked; the custody lets the loop's own answer to the box, a key
//! fenced on its row (a dialog's Enter or Esc), pass only once an earlier or this gate's `status`
//! read found the input clear (`AnswersBox`, which leaves `held = 1`), proving the worker read the
//! line. `answered` records that normal custody permission as well as its mutant. It does not mean
//! the key reached or closed the box: input queued behind a line already proved read is held by the
//! session's next gate, and `DialogGoes` is the environment's later observation that the box went.
//! An unread line, or one no earlier input word proved read and no current word can say of, holds
//! the key at custody. The line stays held for the read after a key that goes. The loop's writes
//! and a draw's Enter are fenced on the
//! composer's rows, so none is made under it; the program's end takes its box with it.
//!
//! Its laws:
//!
//! - `NoTwoCopies`: nothing is written (given up on or submitted) while a copy of the last line
//!   may still reach the program (`q > 0`): every settle needs its evidence, and the restart is
//!   the only way past a read line short of it.
//! - `ARestartHandsTheShellNothing`: no restart in place, the loop's or the host's upgrade's, is
//!   made while the line may still be queued in the input, nor the loop's while input queued
//!   behind the line is unread: the shell would read either once the agent exits.
//! - `OneRestartPerLine`: the host answers at most one restart in place asked for a lost line
//!   (made or refused, `asks`) short of an act submitted; a deferral is no answer, and is asked
//!   again.
//! - `TheHostWaitsOnALineThatMayCome`: the host never types behind a line that stands (one
//!   queued, or read within its minute).
//! - `ADrawnLineGoesBackToItsWriter`: a line drawn in the composer while it is held goes back to
//!   its writer (`LookDrawn`), so its Enter is the act that wrote it, never a person's draft's, nor
//!   a key no writer gave (`stray`): the custody never ends on it otherwise (a turn settles only a
//!   line no read shows), and the loop's key that is no line's goes only to a box over the
//!   composer (`AnswersBox`), never to the composer the line is drawn in.
//! - `ABoxAnswerFollowsARead`: custody never permits a box's answer while the held line may still
//!   be queued; an earlier or fresh clear input read must first prove the worker read it. Both the
//!   normal and mutant transitions write `answered`; the law relates that observation to the real
//!   custody state rather than a mutant-only error flag.
//!
//! What the model takes as given, the code's premises, which no bind checks: the session's input
//! is one queue, read in order, so a clear read proves the line read, and input queued behind it
//! is read after it; the program draws in order, so a person's keys drawn came after a line read
//! and drawn none of only if that line was dropped (`PersonDraws` from `q < 2`); input queued
//! behind the line, read, changes nothing the custody reads (a person's keys drawn are
//! `PersonDraws`); a turn runs only once the lines read are drawn or dropped (`TurnRuns`); after a
//! turn since the write the program draws or drops what it reads at once (`ProgramReads` only
//! before a turn, `ReadsAndDraws`, `ReadsAndDrops` after one): the turn's settle rests on this, so
//! a line read and kept unseen across a turn (a program that queues typed input it shows
//! nowhere) is a copy the model does not have; a line read and drawn none of past its minute at
//! rest is lost, the code's judgment (`q = 2` counts as no copy that may come); the host names the
//! agent's process, so another one is its program ended. `ProgramEnds` is the program's own end
//! (a crash, an exit), which may come at any state; the host's upgrade is `HostUpgrades`.
//! That no writer writes while the custody stands (`cust == 0` on the writes) is the session's
//! own checks (`Session::custody_gate`, the occupancy guards' draft, the accounts probe's
//! `PendingInput`, the survey's `0`), held by the loop's tests, not by the bind; that none is
//! made under a box (`dlg == 0` on the writes and on `Enters`) is the writes' fences on the
//! composer's rows, that a box goes with its program (`dlg = 0` at its end) is the program's, and
//! that a key custody permits reaches the box and closes it (`DialogGoes`) is the answer's fence on
//! the box's own row and the session's later gates. Input queued behind a line already proved read
//! is held by the ordinary unread-input gate after this one; with no proof the custody itself holds
//! the key. The dues, the doubling wait, the records and their thinning are the loop's tests' too.
//!
//! The mutants: a look settles the line on a read that shows none of it (`SettleOnMove`); a
//! clear read settles it with no turn read running (`SettleWithoutTurn`); a turn settles it on
//! an input read unread (`TurnSettlesUnread`); the restart is made for a line no read found
//! clear (`RestartUnread`); a line read within its minute is judged lost (`LostWithinMinute`); a
//! second loss is restarted again (`RestartAgain`); the host types behind a line that stands
//! (`HostOverStands`); a writer writes while the line stands (`WriteWhileHeld`); the restart is
//! made for a line read while input behind it is unread (`RestartBehind`); the host's upgrade
//! moves behind a line that stands (`UpgradeOverStands`); a turn settles the line on a read that
//! cannot say what the composer holds, under a box (`TurnSettlesUnderDialog`); a key that is no
//! line's goes where no box is over the composer, and enters or clears a line drawn there
//! (`AnswersComposer`); a box's key goes without an earlier or fresh clear input read
//! (`AnswersWithoutRead`). The Tier-1 bind is
//! `aterm-agent/tests/conformance_custody.rs`, over the pure core `supervise::custody`.

use aterm_spec::derive::Model;

/// THE LINE HELD.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn line_custody_model() -> Model {
    aterm_spec::ty_model! {
        LineCustody {
            const Buggy = 0;
            const Served = 1;
            const Lane = 1;
            scenario Served = 0;
            scenario Lane = 0;
            var q = 0;
            var tp = 0;
            var pk = 0;
            var moved = 0;
            var rest = 1;
            var st = 0;
            var cust = 0;
            var held = 0;
            var ripe = 0;
            var ran = 0;
            var drawn = 0;
            var lost = 0;
            var restart = 0;
            var asked = 0;
            var asks = 0;
            var twice = 0;
            var shell = 0;
            var hb = 0;
            var bq = 0;
            var dlg = 0;
            var stray = 0;
            var answered = 0;

            action WritesQueued when (cust == 0 && st == 0 && dlg == 0) {
                answered = 0;
                twice = if (q > 0) { 1 } else { twice };
                q = 4;
                tp = 0;
                pk = 0;
                bq = 0;
                moved = 0;
                cust = 1;
                held = if (Served == 1) { 0 } else { 2 };
            }
            action WritesRead when (cust == 0 && st == 0 && dlg == 0) {
                answered = 0;
                twice = if (q > 0) { 1 } else { twice };
                q = 3;
                tp = 0;
                pk = 0;
                bq = 0;
                moved = 0;
                cust = 1;
                held = if (Served == 1) { 1 } else { 2 };
            }
            action Types when (cust == 0 && st == 0 && dlg == 0) {
                twice = if (q > 0) { 1 } else { twice };
                st = 2;
                restart = 0;
                asks = 0;
            }
            action Enters when (st == 1 && q == 1 && dlg == 0) {
                q = 0;
                st = 2;
                restart = 0;
                asks = 0;
            }
            action NextAct when (st == 2) { st = 0; }

            action ProgramReads when (q == 4 && tp == 0) { q = 3; }
            action ReadsAndDraws when (q == 4 && tp == 1) { q = 1; }
            action ReadsAndDrops when (q == 4 && tp == 1) { q = 0; }
            action ProgramDraws when (q == 2 || q == 3) { q = 1; }
            action ProgramDrops when (q == 2 || q == 3) { q = 0; }
            action Minute when (cust == 1 && held == 1 && ripe == 0) {
                ripe = 1;
                q = if (q == 3) { 2 } else { q };
            }
            action PersonDraws when (cust == 1 && pk == 0 && 2 > q) {
                pk = 1;
                q = 0;
            }
            action Queues when (cust == 1 && asked == 0 && bq == 0) { bq = 1; }
            action ReadsBehind when (bq == 1 && 4 > q) { bq = 0; }
            action PersonClears when (q == 1) {
                q = 0;
                st = if (st == 1) { 0 } else { st };
            }
            action TurnRuns when (cust == 1 && asked == 0 && (2 > q || q == 4)) {
                tp = 1;
                ran = 1;
            }
            action ProgramEnds when (asked == 0) {
                q = 0;
                tp = 0;
                pk = 0;
                bq = 0;
                dlg = 0;
                moved = if (cust == 1) { 1 } else { moved };
                st = if (st == 1) { 0 } else { st };
            }
            action Stirs when (rest == 1) { rest = 0; }
            action Rests when (rest == 0) { rest = 1; }
            action DialogShows when (cust == 1 && dlg == 0) {
                dlg = 1;
                answered = 0;
            }
            action DialogGoes when (dlg == 1) {
                dlg = 0;
                answered = 0;
            }
            action AnswersBox when (
                cust == 1 && asked == 0 && dlg == 1
                    && (held == 1 || (Served == 1 && 4 > q && bq == 0))
            ) {
                held = 1;
                answered = 1;
            }

            action LookDrawn when (
                cust == 1 && asked == 0
                    && ((dlg == 0 && pk == 0 && (q == 1 || drawn == 1)) || (dlg == 1 && drawn == 1))
            ) {
                st = if (q == 1) { 1 } else { 0 };
                cust = 0;
                answered = 0;
                held = 0;
                ripe = 0;
                ran = 0;
                drawn = 0;
                lost = 0;
                moved = 0;
            }
            action LookPerson when (cust == 1 && asked == 0 && dlg == 0 && pk == 1) {
                stray = if (q == 1) { 1 } else { stray };
                pk = 0;
                cust = 0;
                answered = 0;
                held = 0;
                ripe = 0;
                ran = 0;
                drawn = 0;
                lost = 0;
                moved = 0;
            }
            action BreakKeepsDraw when (
                cust == 1 && asked == 0 && dlg == 0 && q == 1 && pk == 0 && drawn == 0
            ) {
                drawn = 1;
            }
            action SeesEnded when (cust == 1 && asked == 0 && moved == 1) {
                stray = if (q == 1) { 1 } else { stray };
                cust = 0;
                answered = 0;
                held = 0;
                ripe = 0;
                ran = 0;
                drawn = 0;
                lost = 0;
                moved = 0;
            }
            action TurnSettles when (
                cust == 1 && asked == 0 && ran == 1 && Served == 1 && dlg == 0 && 4 > q
                    && (q == 0 || q > 1) && pk == 0 && bq == 0
            ) {
                stray = if (q == 1) { 1 } else { stray };
                cust = 0;
                answered = 0;
                held = 0;
                ripe = 0;
                ran = 0;
                drawn = 0;
                lost = 0;
                moved = 0;
            }
            action TurnOutlasted when (
                cust == 1 && asked == 0 && ran == 1 && dlg == 0
                    && (Served == 0 || q == 4 || bq == 1) && (q == 0 || q > 1) && pk == 0
            ) {
                ran = 0;
                held = if (Served == 1) { if (held == 1) { 1 } else { 0 } } else { held };
            }
            action GateHolds when (
                cust == 1 && asked == 0 && drawn == 0 && moved == 0
                    && (dlg == 1 || ((q == 0 || q > 1) && pk == 0))
                    && (held == 0 || held == 2 || ripe == 0 || rest == 0 || bq == 1 || dlg == 1)
            ) {
                held = if (Served == 0) {
                    held
                } else {
                    if (q == 4 || bq == 1) { if (held == 1) { 1 } else { 0 } } else { 1 }
                };
            }
            action AsksRestart when (
                cust == 1 && asked == 0 && dlg == 0 && (q == 0 || q > 1) && pk == 0 && drawn == 0
                    && moved == 0 && held == 1 && ripe == 1 && rest == 1 && bq == 0
                    && restart == 0 && Lane == 1
            ) {
                asked = 1;
            }
            action WaitsLost when (
                cust == 1 && asked == 0 && dlg == 0 && (q == 0 || q > 1) && pk == 0 && drawn == 0
                    && moved == 0 && held == 1 && ripe == 1 && rest == 1 && bq == 0
                    && (restart > 0 || Lane == 0)
            ) {
                lost = 1;
            }
            action RestartMade when (asked == 1) {
                shell = if (q == 4 || bq == 1) { 1 } else { shell };
                asks = if (2 > asks) { asks + 1 } else { asks };
                q = 0;
                tp = 0;
                pk = 0;
                bq = 0;
                dlg = 0;
                asked = 0;
                restart = 1;
                cust = 0;
                answered = 0;
                held = 0;
                ripe = 0;
                ran = 0;
                drawn = 0;
                lost = 0;
                moved = 0;
            }
            action RestartUnmade when (asked == 1) {
                asks = if (2 > asks) { asks + 1 } else { asks };
                asked = 0;
                restart = 2;
            }
            action RestartDeferred when (asked == 1) { asked = 0; }
            action HostTypes when (asked == 0 && (cust == 0 || lost == 1)) {
                hb = if (q > 2) { 1 } else { hb };
            }
            action HostUpgrades when (asked == 0 && (cust == 0 || lost == 1)) {
                shell = if (q == 4) { 1 } else { shell };
                q = 0;
                tp = 0;
                pk = 0;
                bq = 0;
                dlg = 0;
                moved = if (cust == 1) { 1 } else { moved };
                st = if (st == 1) { 0 } else { st };
            }

            action SettleOnMove when (
                Buggy == 1 && cust == 1 && asked == 0 && (q == 0 || q > 1) && pk == 0
                    && drawn == 0
            ) {
                cust = 0;
                answered = 0;
                held = 0;
                ripe = 0;
                ran = 0;
                drawn = 0;
                lost = 0;
                moved = 0;
            }
            action SettleWithoutTurn when (
                Buggy == 2 && cust == 1 && asked == 0 && ran == 0 && Served == 1 && 4 > q
                    && (q == 0 || q > 1) && pk == 0 && bq == 0
            ) {
                cust = 0;
                answered = 0;
                held = 0;
                ripe = 0;
                ran = 0;
                drawn = 0;
                lost = 0;
                moved = 0;
            }
            action TurnSettlesUnread when (
                Buggy == 3 && cust == 1 && asked == 0 && ran == 1
                    && (Served == 0 || q == 4 || bq == 1) && pk == 0
            ) {
                cust = 0;
                answered = 0;
                held = 0;
                ripe = 0;
                ran = 0;
                drawn = 0;
                lost = 0;
                moved = 0;
            }
            action RestartUnread when (
                Buggy == 4 && cust == 1 && asked == 0 && dlg == 0 && (q == 0 || q > 1) && pk == 0
                    && drawn == 0 && moved == 0 && held == 0 && rest == 1 && restart == 0
                    && Lane == 1
            ) {
                asked = 1;
            }
            action LostWithinMinute when (
                Buggy == 5 && cust == 1 && asked == 0 && dlg == 0 && (q == 0 || q > 1) && pk == 0
                    && drawn == 0 && moved == 0 && held == 1 && ripe == 0 && rest == 1
                    && bq == 0 && (restart > 0 || Lane == 0)
            ) {
                lost = 1;
            }
            action RestartAgain when (
                Buggy == 6 && cust == 1 && asked == 0 && dlg == 0 && (q == 0 || q > 1) && pk == 0
                    && drawn == 0 && moved == 0 && held == 1 && ripe == 1 && rest == 1
                    && bq == 0 && restart > 0
            ) {
                asked = 1;
            }
            action HostOverStands when (Buggy == 7 && asked == 0 && cust == 1 && lost == 0) {
                hb = if (q > 2) { 1 } else { hb };
            }
            action WriteWhileHeld when (Buggy == 8 && cust == 1 && asked == 0) {
                twice = if (q > 0) { 1 } else { twice };
            }
            action RestartBehind when (
                Buggy == 9 && cust == 1 && asked == 0 && dlg == 0 && (q == 0 || q > 1) && pk == 0
                    && drawn == 0 && moved == 0 && held == 1 && ripe == 1 && rest == 1
                    && bq == 1 && restart == 0 && Lane == 1
            ) {
                asked = 1;
            }
            action UpgradeOverStands when (Buggy == 10 && asked == 0 && cust == 1 && lost == 0) {
                shell = if (q == 4) { 1 } else { shell };
                q = 0;
                tp = 0;
                pk = 0;
                bq = 0;
                dlg = 0;
                moved = 1;
                st = if (st == 1) { 0 } else { st };
            }
            action TurnSettlesUnderDialog when (
                Buggy == 11 && cust == 1 && asked == 0 && ran == 1 && Served == 1 && dlg == 1
                    && 4 > q && bq == 0
            ) {
                stray = if (q == 1) { 1 } else { stray };
                cust = 0;
                answered = 0;
                held = 0;
                ripe = 0;
                ran = 0;
                drawn = 0;
                lost = 0;
                moved = 0;
            }
            action AnswersComposer when (
                Buggy == 12 && cust == 1 && asked == 0 && dlg == 0
                    && (held == 1 || (Served == 1 && 4 > q && bq == 0))
            ) {
                stray = if (q == 1) { 1 } else { stray };
                q = if (q == 1) { 0 } else { q };
                held = 1;
            }
            action AnswersWithoutRead when (
                Buggy == 13 && cust == 1 && asked == 0 && dlg == 1
                    && (Served == 0 || ((held == 0 || held == 2) && (q == 4 || bq == 1)))
            ) {
                answered = 1;
            }

            invariant NoTwoCopies: twice == 0;
            invariant ARestartHandsTheShellNothing: shell == 0;
            invariant OneRestartPerLine: 2 > asks;
            invariant TheHostWaitsOnALineThatMayCome: hb == 0;
            invariant ADrawnLineGoesBackToItsWriter: stray == 0;
            invariant ABoxAnswerFollowsARead: answered == 0 || cust == 0 || held == 1;
        }
    }
}
