// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The steps of `LineCustody` (`aterm_models_harness::line_custody_model`) that no function here
//! performs: the worker's program, the person, the session's state, and the model's mutants. The
//! custody's own steps, the host's deferral of the restart among them (`Restarts::deferred`), are
//! anchored on the pure core (`supervise::custody`), each projected by the Tier-1 bind
//! (`tests/conformance_custody.rs`). Proof and test builds only.

/// The program's, the person's and the session's steps, and the mutants of `LineCustody`.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "ProgramReads",
        reason = "The worker's program reading the queued line from its input before any turn since the write: the program's. The Tier-1 bind renders the next status read clear exactly there, unless input queued behind the line is unread (Queues)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "ReadsAndDraws",
        reason = "The program reading the queued line after a turn since the write and drawing it: the program's. The Tier-1 bind renders the composer holding it exactly there. Under a box over the composer (DialogShows) it renders every read as one that cannot say instead."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "ReadsAndDrops",
        reason = "The program reading the queued line after a turn since the write and dropping it: the program's. The Tier-1 bind renders the input clear, unless input queued behind the line is unread (Queues), and the composer empty exactly there. Under a box over the composer (DialogShows) it renders every read as one that cannot say instead."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "ProgramDraws",
        reason = "The program drawing a line it read: the program's. The Tier-1 bind renders the composer holding it (whole, arriving, or joined) exactly there. Under a box over the composer (DialogShows) it renders every read as one that cannot say instead."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "ProgramDrops",
        reason = "The program dropping a line it read, drawn never: the program's, which no read shows. The Tier-1 bind changes nothing the loop reads there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "Minute",
        reason = "ECHO_HANG passing since the first clear read: the loop's clock. The Tier-1 bind advances the clock it hands the core by ECHO_HANG exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "PersonDraws",
        reason = "A person's keys drawn after the line: the person's. The Tier-1 bind raises the read's key count and renders their text in the composer exactly there. Under a box over the composer (DialogShows) it renders every read as one that cannot say instead."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "Queues",
        reason = "Input queued unread behind a line held (a person's typeahead, another client's keys): the person's or the client's. The Tier-1 bind renders every status read unread exactly there, and nothing on the screen."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "ReadsBehind",
        reason = "The program reading the input queued behind the line, after the line: the program's. The Tier-1 bind renders the status read clear again exactly there, and nothing on the screen."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "PersonClears",
        reason = "Someone clearing the drawn line from the composer: the person's. The Tier-1 bind renders the composer empty exactly there. Under a box over the composer (DialogShows) it renders every read as one that cannot say instead."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "ProgramEnds",
        reason = "The agent's process ending of itself (a crash, an exit), at any state: the program's. The host's upgrade, which LineHeld::Stands holds, is HostUpgrades, anchored on line_held. The Tier-1 bind has the host name another agent process exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "Stirs",
        reason = "The session leaving rest (a turn running, a frame torn mid-redraw, the agent's own background work running under its turn's end, a hold, the word Off, a person's key within the grace, a background call making no progress): the session's. A box over the composer is DialogShows. The Tier-1 bind renders one of those into the read's Rest exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "Rests",
        reason = "The session at rest again: the session's. The Tier-1 bind renders the read's Rest at rest exactly there. Under a box over the composer (DialogShows) it renders the read's Rest as the shell judges that read: a dialog drawn or no composer drawn as no turn's end, the composer's glyph cell unread as the turn's end the rest says with its composer never clear, as a read that cannot say what the composer holds is no rest."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "DialogShows",
        reason = "A box drawn over the composer (a permission box, a question), no composer drawn (a full-screen view), or the composer's glyph cell unread, while a line is held: the program's or the window's, which the loop or a person answers or closes, never the custody. The Tier-1 bind renders every read from there, the gate's included, as one that cannot say what the composer holds (Shows::stands None: no composer text, the composer's text with its cell unread, or no key count), the line drawn under it or not, and the read's Rest as the shell judges it: no turn's end under a dialog or with no composer drawn, and under the glyph cell unread the turn's end the rest says with its composer never clear (Rest::clear false), so the gate's hold there is the clear read's alone."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "DialogGoes",
        reason = "The box over the composer going (answered, closed, the view left, the cell read): the program's, the person's, or the approval loop's answer to a permission box, which asks no custody (the turn-end loop's answer through the custody's gate is AnswersBox). The Tier-1 bind renders the composer as the model holds it again exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "NextAct",
        reason = "The turn an act submitted ran, and the next point is the policy's to decide: the policy's. The Tier-1 bind changes nothing the custody reads there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "SettleOnMove",
        reason = "Buggy=1 negative control only; a look settles the line on a read that shows none of it, and the line the worker still holds is written again (the Tier-1 bind plants CustodySettlesUnshown in Line::look)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "SettleWithoutTurn",
        reason = "Buggy=2 negative control only; a clear read settles the line with no turn read running since the write (a busy spell of break reads alone), and the line read and held is written again (the Tier-1 bind probes Line::turned at every state with no turn run and requires it settle nothing)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "TurnSettlesUnread",
        reason = "Buggy=3 negative control only; a turn settles the line on an input read unread or unknown, and the line still queued is written again (the Tier-1 bind plants CustodyTurnAnyWord in Line::turned)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "RestartUnread",
        reason = "Buggy=4 negative control only; a restart in place for a line no read found clear, so the queued line goes to the shell (the Tier-1 bind plants CustodyLostUnread in Line::gate_read)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "LostWithinMinute",
        reason = "Buggy=5 negative control only; a line read within its minute is judged lost and the host told so, so it types behind a line that may still come (the Tier-1 bind plants CustodyLostWithinMinute in Line::gate_read)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "RestartAgain",
        reason = "Buggy=6 negative control only; a second loss short of an act submitted is restarted again (the Tier-1 bind plants CustodyRestartAgain in Line::lose)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "HostOverStands",
        reason = "Buggy=7 negative control only; the host types behind a line that stands (the Tier-1 bind plants CustodyHostUntold in line_held)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "WriteWhileHeld",
        reason = "Buggy=8 negative control only; a writer writes while the line stands held, the gate the session's custody checks keep (Session::custody_gate, the occupancy guards' draft, the accounts probe's PendingInput)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "RestartBehind",
        reason = "Buggy=9 negative control only; a restart in place for a line read while input queued behind it is unread, so that input goes to the shell (the Tier-1 bind plants CustodyLostBehind in Line::gate_read)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "UpgradeOverStands",
        reason = "Buggy=10 negative control only; the host's upgrade moves behind a line that stands, so a line still queued goes to the shell (the Tier-1 bind plants CustodyHostUntold in line_held)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "TurnSettlesUnderDialog",
        reason = "Buggy=11 negative control only; a turn settles the line on a read that cannot say what the composer holds (a box over it), so a line drawn under the box leaves the custody unhanded and is taken for a person's draft (the Tier-1 bind plants CustodyTurnUnreadable in Line::turned)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "AnswersComposer",
        reason = "Buggy=12 negative control only; a key that is no line's goes past the line held where no box is over the composer, so it reaches the composer and enters or clears a line drawn there, which no writer gets back (the Tier-1 bind plants CustodyAnswersComposer in answer_goes)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "AnswersWithoutRead",
        reason = "Buggy=13 negative control only; a dialog's key goes past a line no clear input read proved the worker read, so the still-queued line may reach the dialog before its answer (the Tier-1 bind plants CustodyAnswersWithoutRead in answer_goes)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "RestartTakenOnWord",
        reason = "Buggy=14 negative control only; the host's word that it made the restart settles the line and books the restart made though the host names the process the line was written to, so the line that process may still draw is written again (the Tier-1 bind plants CustodyRestartOnWord in Line::restarted)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LineCustody",
        action = "SettleKeepsAsked",
        reason = "Buggy=15 negative control only; the line held settles with its program while the restart asked for it stands deferred, and the restart is left unspent, so one the host made and answered as a deferral is asked again for the next loss of the line written to the process it started (the Tier-1 bind plants CustodySettleKeepsAsked in Restarts::settled)"
    )
)]
#[doc(hidden)]
pub fn line_custody_environment() {}
