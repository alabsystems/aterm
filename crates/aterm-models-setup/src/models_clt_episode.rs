// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the account's record of setup's administrator-password attempt
//! (`aterm_types::clt_mark`): the claim, the hand to the Mac Setup tab's program, the end, the
//! cut-off, a person's own retries and their lift of a decline, and the window's and a terminal
//! `aterm init`'s decisions of how their attempts ended (`aterm_types::clt_mark::tab_end`,
//! `aterm_types::clt_mark::term_end`).

use aterm_spec::derive::*;

/// CltEpisode: the one record every asker of setup's Mac password shares (the window, whose
/// automatic ask or Install… it hands to its Mac Setup tab's program, a terminal `aterm init`'s
/// automatic ask, and a person's typed `aterm init`), the processes that ask ending at any
/// point, aterm's quit, and time passing.
///
/// The record keeps every attempt that may still ask over how the last attempt before them
/// ended (none, `unfinished`, `done`, `declined`), so all three askers can ask at once: a
/// terminal's automatic ask, the window's Install… over it and a typed `aterm init` over both.
/// An automatic claim goes only where no attempt the record keeps asks and the end under them
/// lets an automatic ask go now (a decline's passed hold reads as the finish before it), or,
/// where every attempt it keeps was cut off, once their pause is; a person's own retry claims
/// whatever the record says. Each attempt writes its own end in its own place: it goes from the
/// record and its end becomes the end the others ask over, superseding the attempts that were
/// cut off; an attempt that asked nothing leaves the end under it exactly as it was, a decline's
/// count included. The window hands its claim to its tab's program, so the window's restart (an
/// update's handoff, which the program outlives) cuts nothing off and the attempt stays the
/// window's to end. Attempts whose askers are gone with no end written read as an unfinished
/// try, due again by itself from the latest claim and adding none, never as a decline: only a
/// person's no is a decline, which holds automatic asks for its long pause, counts on while
/// declines come in a row, and is lifted by their yes. aterm's quit, noted by the close of its
/// last window while the program asks, writes no end for what its hangup ends the program with,
/// and the program's own status (a finish) is still its end. Every pause is read at the
/// reader's clock; `Tick` is enough time passing for every pause.
///
/// Not walked: a second attempt of one asker at once (each asker has one attempt at a time; the
/// record's own test holds two of one asker); a record past `clt_mark::ATTEMPTS` attempts, whose
/// oldest goes; a third decline in a row, past the two this walk counts; of the exits the
/// window's decision reads, only one for each of its ends (a finish, a failure, a stop, a tab
/// closed with no status seen, a program that never ran) and, at a quit, the hangup's three (a
/// signal, no status, the stop's code), the rest bound by `tab_end`'s own test; and an update's
/// carry of an earlier build's live tab (`clt_mark::carry`): until the carry names it, that
/// tab's program asks under a historical `opened` line, a format with no state here, and the
/// carry's own test holds it (written over such a line or none only, the same claim given back
/// after, also from under a person's retry, and every other line left as found).
///
/// Variables: `rec` is the end the attempts ask over (0 none, 2 unfinished, 3 done, 4
/// declined), `tries` its unfinished tries in a row, `dec` its declines in a row, `fin` whether
/// it says the elevation ever finished, `due` and `fdue` whether its pause and the last
/// finish's are over. `we` is the window's attempt the record keeps while its asker runs (0
/// none, 1 naming the window, 2 naming its tab's program), `te` the terminal's and `oe` the
/// person's, each with whether the pause from its claim is over (`wd`, `td`, `od`); `cut`
/// whether it keeps an attempt whose asker is gone, and `cd` whether those attempts' pause is
/// over. `win` is the window's attempt (0 none, 1 the window asks, 2 its tab's program asks, 3
/// its tab's program asks under no claim the window holds), `term` and `own` the other two (0
/// none, 1 asking), `quitting` whether aterm's quit is noted. The rest are what the conformance
/// test reads off the real record's behaviour: `said` (the last reading said declined), `ends`
/// (unfinished ends written since the last finish or decline), `decls` (declines written since
/// the last finish, unfinished end or lift), `ever` (an attempt finished), `early` (an automatic
/// claim through a held decline), `beside` (an automatic claim while another asker asked),
/// `cutend` (an end written for what aterm's quit ended its tab's program with), and `cutno` (a
/// decline written for a terminal `aterm init` its hangup or a `SIGTERM` stopped); `bug` whether
/// a mutant fired, so each behaviour of the `Buggy=1` walk plays one defect.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn clt_episode_model() -> Model {
    aterm_spec::ty_model! {
        CltEpisode {
            const Buggy = 0;

            var rec = 0;
            var tries = 0;
            var dec = 0;
            var fin = 0;
            var due = 1;
            var fdue = 1;
            var we = 0;
            var wd = 1;
            var te = 0;
            var td = 1;
            var oe = 0;
            var od = 1;
            var cut = 0;
            var cd = 1;
            var win = 0;
            var term = 0;
            var own = 0;
            var quitting = 0;
            var said = 0;
            var ends = 0;
            var decls = 0;
            var ever = 0;
            var early = 0;
            var beside = 0;
            var cutend = 0;
            var cutno = 0;
            var bug = 0;

            // An automatic claim, the window's or a terminal `aterm init`'s: only where no
            // attempt the record keeps asks, and the end under them lets an automatic ask go
            // now (a decline's passed hold reads as the finish before it), or, where every
            // attempt it keeps was cut off, once their pause is.
            action ClaimW when (win == 0 && quitting == 0 && we == 0 && te == 0 && oe == 0 &&
                ((cut == 1 && (tries == 0 || cd == 1)) || (cut == 0 && (rec == 0 || (due == 1
                && (rec <= 3 || fin == 0 || fdue == 1)))))) {
                we = 1;
                wd = if (tries > 0) { 0 } else { 1 };
                win = 1;
                beside = if (term == 1 || own == 1) { 1 } else { beside };
                said = 0;
            }
            action ClaimT when (term == 0 && we == 0 && te == 0 && oe == 0 &&
                ((cut == 1 && (tries == 0 || cd == 1)) || (cut == 0 && (rec == 0 || (due == 1
                && (rec <= 3 || fin == 0 || fdue == 1)))))) {
                te = 1;
                td = if (tries > 0) { 0 } else { 1 };
                term = 1;
                beside = if (win > 0 || own == 1) { 1 } else { beside };
                said = 0;
            }
            // A person's own retry, their yes whatever the record says, other attempts asking
            // included: the window's Install… (which it hands to its tab's program as it hands
            // an automatic claim) or a typed `aterm init`.
            action ExplicitW when (win == 0 && quitting == 0) {
                we = 1;
                wd = if (tries > 0) { 0 } else { 1 };
                win = 1;
                said = 0;
            }
            action ExplicitO when (own == 0) {
                oe = 1;
                od = if (tries > 0) { 0 } else { 1 };
                own = 1;
                said = 0;
            }
            // The window hands its claim to its tab's program, which asks: the record names the
            // program from now, wherever it keeps the attempt.
            action Hand when (win == 1 && we == 1) {
                we = 2;
                win = 2;
                said = 0;
            }

            // The window's attempt ends, as the window writes it for its tab's program (its
            // decision, `tab_end`): the program's own status, at a quit too, but a stop of its own
            // (a Ctrl-C, System State's Stop) or its tab closed only while aterm runs on, since
            // a quit's hangup ends the program as such a stop does. Its end becomes the end the
            // others ask over, and supersedes the attempts that were cut off.
            action DoneW when (win > 0 && win <= 2 && we > 0) {
                rec = 3;
                tries = 0;
                dec = 0;
                fin = 1;
                due = 0;
                fdue = 0;
                we = 0;
                wd = 1;
                td = 1;
                od = 1;
                cut = 0;
                cd = 1;
                ends = 0;
                decls = 0;
                ever = 1;
                said = 0;
                win = 0;
            }
            action UnfinishedW when (win > 0 && win <= 2 && we > 0 && tries == 0) {
                rec = 2;
                tries = 1;
                dec = 0;
                due = 0;
                we = 0;
                wd = 1;
                td = if (te == 1) { 0 } else { 1 };
                od = if (oe == 1) { 0 } else { 1 };
                cut = 0;
                cd = 1;
                ends = ends + 1;
                decls = 0;
                said = 0;
                win = 0;
            }
            action DeclinedW when (win > 0 && win <= 2 && we > 0 && quitting == 0 &&
                decls <= 1) {
                rec = 4;
                tries = 0;
                dec = if (rec == 4) { dec + 1 } else { 1 };
                due = 0;
                we = 0;
                wd = 1;
                td = 1;
                od = 1;
                cut = 0;
                cd = 1;
                ends = 0;
                decls = decls + 1;
                said = 0;
                win = 0;
            }
            action ClosedW when (win > 0 && win <= 2 && we > 0 && quitting == 0 &&
                decls <= 1) {
                rec = 4;
                tries = 0;
                dec = if (rec == 4) { dec + 1 } else { 1 };
                due = 0;
                we = 0;
                wd = 1;
                td = 1;
                od = 1;
                cut = 0;
                cd = 1;
                ends = 0;
                decls = decls + 1;
                said = 0;
                win = 0;
            }
            action ReleasedW when (win > 0 && win <= 2 && we > 0) {
                we = 0;
                wd = 1;
                said = 0;
                win = 0;
            }

            // A terminal `aterm init`'s automatic attempt ends, and its process with it, as its
            // run reads its own end (its decision, `term_end`): a finish read back, an unfinished
            // try, a person's key at its terminal (Ctrl-C, Ctrl-\), which is their decline, or a
            // `sudo` that never started.
            action DoneT when (term == 1 && te == 1) {
                rec = 3;
                tries = 0;
                dec = 0;
                fin = 1;
                due = 0;
                fdue = 0;
                te = 0;
                wd = 1;
                td = 1;
                od = 1;
                cut = 0;
                cd = 1;
                ends = 0;
                decls = 0;
                ever = 1;
                said = 0;
                term = 0;
            }
            action UnfinishedT when (term == 1 && te == 1 && tries == 0) {
                rec = 2;
                tries = 1;
                dec = 0;
                due = 0;
                te = 0;
                wd = if (we > 0) { 0 } else { 1 };
                td = 1;
                od = if (oe == 1) { 0 } else { 1 };
                cut = 0;
                cd = 1;
                ends = ends + 1;
                decls = 0;
                said = 0;
                term = 0;
            }
            // The terminal's hangup (it closed, the aterm it ran in quit, a logout ended its
            // login) or a `SIGTERM` (a logout, launchd, an update's handoff) stops it while it
            // asks: nobody at that terminal said no, so the run writes the unfinished try it
            // can, asked again by itself, never a decline.
            action InterruptedT when (term == 1 && te == 1 && tries == 0) {
                rec = 2;
                tries = 1;
                dec = 0;
                due = 0;
                te = 0;
                wd = if (we > 0) { 0 } else { 1 };
                td = 1;
                od = if (oe == 1) { 0 } else { 1 };
                cut = 0;
                cd = 1;
                ends = ends + 1;
                decls = 0;
                said = 0;
                term = 0;
            }
            action DeclinedT when (term == 1 && te == 1 && decls <= 1) {
                rec = 4;
                tries = 0;
                dec = if (rec == 4) { dec + 1 } else { 1 };
                due = 0;
                te = 0;
                wd = 1;
                td = 1;
                od = 1;
                cut = 0;
                cd = 1;
                ends = 0;
                decls = decls + 1;
                said = 0;
                term = 0;
            }
            action ReleasedT when (term == 1 && te == 1) {
                te = 0;
                td = 1;
                said = 0;
                term = 0;
            }

            // A person's typed `aterm init` ends, and its process with it.
            action DoneO when (own == 1 && oe == 1) {
                rec = 3;
                tries = 0;
                dec = 0;
                fin = 1;
                due = 0;
                fdue = 0;
                oe = 0;
                wd = 1;
                td = 1;
                od = 1;
                cut = 0;
                cd = 1;
                ends = 0;
                decls = 0;
                ever = 1;
                said = 0;
                own = 0;
            }
            action UnfinishedO when (own == 1 && oe == 1 && tries == 0) {
                rec = 2;
                tries = 1;
                dec = 0;
                due = 0;
                oe = 0;
                wd = if (we > 0) { 0 } else { 1 };
                td = if (te == 1) { 0 } else { 1 };
                od = 1;
                cut = 0;
                cd = 1;
                ends = ends + 1;
                decls = 0;
                said = 0;
                own = 0;
            }
            action DeclinedO when (own == 1 && oe == 1 && decls <= 1) {
                rec = 4;
                tries = 0;
                dec = if (rec == 4) { dec + 1 } else { 1 };
                due = 0;
                oe = 0;
                wd = 1;
                td = 1;
                od = 1;
                cut = 0;
                cd = 1;
                ends = 0;
                decls = decls + 1;
                said = 0;
                own = 0;
            }
            action ReleasedO when (own == 1 && oe == 1) {
                oe = 0;
                od = 1;
                said = 0;
                own = 0;
            }

            // The environment. An update's handoff replaces the window's process, and its tab's
            // program, which the next window adopts, asks on; a claim naming the window is cut
            // off.
            action WindowGoes when (quitting == 0) {
                cut = if (we == 1) { 1 } else { cut };
                cd = if (we == 1) { if (cut == 1 && cd == 0) { 0 } else { wd } } else { cd };
                wd = if (we == 1) { 1 } else { wd };
                we = if (we == 1) { 0 } else { we };
                said = 0;
                win = if (win == 1) { 0 } else { win };
            }
            // The close of aterm's last window notes its quit while the tab's program asks: the
            // end the window reads from now is decided as a quit's.
            action QuitNoted when (win == 2 && quitting == 0) {
                quitting = 1;
                said = 0;
            }
            // aterm ends (a quit, a crash, a logout or a restart of the Mac): the window and its
            // tab's program end with it, with no end written.
            action AtermQuits {
                cut = if (we > 0) { 1 } else { cut };
                cd = if (we > 0) { if (cut == 1 && cd == 0) { 0 } else { wd } } else { cd };
                wd = 1;
                we = 0;
                said = 0;
                win = 0;
                quitting = 0;
            }
            // The tab's program ends with no end written (killed).
            action ProgramGoes when (win == 2) {
                cut = if (we == 2) { 1 } else { cut };
                cd = if (we == 2) { if (cut == 1 && cd == 0) { 0 } else { wd } } else { cd };
                wd = if (we == 2) { 1 } else { wd };
                we = if (we == 2) { 0 } else { we };
                said = 0;
                win = 0;
            }
            action TermGoes when (term == 1) {
                cut = if (te == 1) { 1 } else { cut };
                cd = if (te == 1) { if (cut == 1 && cd == 0) { 0 } else { td } } else { cd };
                td = 1;
                te = 0;
                said = 0;
                term = 0;
            }
            action OwnGoes when (own == 1) {
                cut = if (oe == 1) { 1 } else { cut };
                cd = if (oe == 1) { if (cut == 1 && cd == 0) { 0 } else { od } } else { cd };
                od = 1;
                oe = 0;
                said = 0;
                own = 0;
            }
            // Time passes past every pause: the longest, a decline's, included.
            action Tick when (due + fdue + wd + td + od + cd <= 5) {
                due = 1;
                fdue = 1;
                wd = 1;
                td = 1;
                od = 1;
                cd = 1;
                said = 0;
            }

            // A reading of the record: declined only of a person's decline whose hold is ahead,
            // with no attempt over it.
            action Read {
                said = if (rec == 4 && due == 0 && we == 0 && te == 0 && oe == 0 && cut == 0) {
                    1
                } else {
                    0
                };
            }
            // A person's yes to setup (Set up now) lifts their decline: back to the last finish,
            // or to no record.
            action Lift when (rec == 4 && we == 0 && te == 0 && oe == 0 && cut == 0) {
                rec = if (fin == 1) { 3 } else { 0 };
                due = if (fin == 1) { fdue } else { 1 };
                dec = 0;
                decls = 0;
                said = 0;
            }

            // Each mutant fires where one defect would, and no further: cut-off attempts read as
            // a decline; an automatic claim beside a running asker, or through a held decline; a
            // hand that leaves the window named; a hand of a claim a person's retry went over
            // refused (the window then holds none, so nobody ends what the record names); an
            // end of an older attempt that drops the newer ones; the newest attempt's end that
            // drops an older one still asking (the record no longer names it, so an automatic
            // claim goes beside it); a release that counts a try; an end that forgets the
            // elevation finished; a third attempt that drops the oldest (the record kept two, as
            // the code had it); a release that keeps only the tries and finish of the end under
            // it, so a decline and its count are lost (as the code had it); the window's
            // decision reading aterm's quit as a decline, or dropping the program's own finish
            // at a quit; and a terminal's decision reading its hangup or a `SIGTERM` as a
            // decline (any stop it caught, as `init_clt::episode_end` once had it).
            action CutOffReadsDeclined when (Buggy == 1 && bug == 0 && cut == 1 && we == 0 && te == 0 &&
                oe == 0) {
                bug = 1;
                said = 1;
            }
            action ClaimBesideLive when (Buggy == 1 && bug == 0 && term == 0 && we > 0) {
                bug = 1;
                te = 1;
                td = if (tries > 0) { 0 } else { 1 };
                term = 1;
                beside = 1;
                said = 0;
            }
            action ClaimThroughDecline when (Buggy == 1 && bug == 0 && term == 0 && rec == 4 && due == 0 &&
                we == 0 && te == 0 && oe == 0 && cut == 0) {
                bug = 1;
                te = 1;
                td = 1;
                term = 1;
                beside = if (win > 0 || own == 1) { 1 } else { beside };
                said = 0;
                early = 1;
            }
            action HandLeavesWindow when (Buggy == 1 && bug == 0 && win == 1 && we == 1) {
                bug = 1;
                win = 2;
                said = 0;
            }
            action HandDropsClaim when (Buggy == 1 && bug == 0 && win == 1 && we == 1 && oe == 1) {
                bug = 1;
                win = 3;
                said = 0;
            }
            action StaleEndWrites when (Buggy == 1 && bug == 0 && win > 0 && win <= 2 && we > 0 &&
                oe == 1 && tries == 0) {
                bug = 1;
                rec = 2;
                tries = 1;
                dec = 0;
                due = 0;
                we = 0;
                wd = 1;
                te = 0;
                td = 1;
                oe = 0;
                od = 1;
                cut = 0;
                cd = 1;
                ends = ends + 1;
                decls = 0;
                said = 0;
                win = 0;
            }
            action RetryEndDropsAsk when (Buggy == 1 && bug == 0 && own == 1 && oe == 1 && we > 0) {
                bug = 1;
                rec = 3;
                tries = 0;
                dec = 0;
                fin = 1;
                due = 0;
                fdue = 0;
                we = 0;
                wd = 1;
                te = 0;
                td = 1;
                oe = 0;
                od = 1;
                cut = 0;
                cd = 1;
                ends = 0;
                decls = 0;
                ever = 1;
                said = 0;
                own = 0;
            }
            action ReleaseCountsATry when (Buggy == 1 && bug == 0 && win > 0 && win <= 2 && we > 0 &&
                te == 0 && oe == 0 && cut == 0 && tries == 0) {
                bug = 1;
                rec = 2;
                tries = 1;
                dec = 0;
                due = 0;
                we = 0;
                wd = 1;
                said = 0;
                win = 0;
            }
            action EndForgetsFinish when (Buggy == 1 && bug == 0 && win > 0 && win <= 2 && we > 0 &&
                fin == 1 && tries == 0 && te == 0 && oe == 0) {
                bug = 1;
                rec = 2;
                tries = 1;
                dec = 0;
                fin = 0;
                due = 0;
                fdue = 1;
                we = 0;
                wd = 1;
                cut = 0;
                cd = 1;
                ends = ends + 1;
                decls = 0;
                said = 0;
                win = 0;
            }
            action ThirdDropsOldest when (Buggy == 1 && bug == 0 && own == 0 && te == 1 && we > 0) {
                bug = 1;
                te = 0;
                td = 1;
                oe = 1;
                od = if (tries > 0) { 0 } else { 1 };
                own = 1;
                said = 0;
            }
            action ReleaseForgetsDecline when (Buggy == 1 && bug == 0 && own == 1 && oe == 1 && rec == 4) {
                bug = 1;
                rec = if (fin == 1) { 3 } else { 0 };
                due = if (fin == 1) { fdue } else { 1 };
                dec = 0;
                oe = 0;
                od = 1;
                said = 0;
                own = 0;
            }
            action InterruptionDeclines when (Buggy == 1 && bug == 0 && win == 2 && quitting == 1 && we > 0
                && decls <= 1) {
                bug = 1;
                rec = 4;
                tries = 0;
                dec = if (rec == 4) { dec + 1 } else { 1 };
                due = 0;
                we = 0;
                wd = 1;
                td = 1;
                od = 1;
                cut = 0;
                cd = 1;
                ends = 0;
                decls = decls + 1;
                said = 0;
                win = 0;
                quitting = 0;
                cutend = 1;
            }
            action HangupDeclinesT when (Buggy == 1 && bug == 0 && term == 1 && te == 1 && decls <= 1) {
                bug = 1;
                rec = 4;
                tries = 0;
                dec = if (rec == 4) { dec + 1 } else { 1 };
                due = 0;
                te = 0;
                wd = 1;
                td = 1;
                od = 1;
                cut = 0;
                cd = 1;
                ends = 0;
                decls = decls + 1;
                said = 0;
                term = 0;
                cutno = 1;
            }
            action QuitDropsFinish when (Buggy == 1 && bug == 0 && win == 2 && quitting == 1 && we == 2 &&
                fin == 0) {
                bug = 1;
                cut = 1;
                cd = if (cut == 1 && cd == 0) { 0 } else { wd };
                wd = 1;
                we = 0;
                said = 0;
                ever = 1;
                win = 0;
            }

            // A reading says declined only of a person's decline: attempts whose askers are
            // gone with no end written (aterm quit, crashed or was killed, the Mac restarted)
            // read as an unfinished try, asked again by itself.
            invariant OnlyADeclineReadsDeclined: said == 0 || rec == 4;
            // No automatic asker asks beside another asker: a claim goes only where no attempt
            // the record keeps still asks.
            invariant NoAutomaticAskBeside: beside == 0;
            // A person's decline holds automatic asks for its long pause.
            invariant ADeclineHolds: early == 0;
            // Every attempt that asks is one the record keeps, so its end is the record's and an
            // automatic claim sees it, however many ask at once.
            invariant EveryAskIsNamed: (win == 0 || we > 0) && (term == 0 || te == 1) &&
                (own == 0 || oe == 1);
            // Every attempt the record keeps whose asker runs is one its asker's process will
            // end: nothing reads asking under a line nobody ends.
            invariant EveryNamedAskIsHeld: (we == 0 || (win > 0 && win <= 2)) && (te == 0 ||
                term == 1) && (oe == 0 || own == 1);
            // The tries in a row are the unfinished ends: an attempt that asked nothing, or one
            // cut off, adds none, and one that asked nothing loses none.
            invariant TriesCountUnfinishedEnds: tries == ends;
            // The declines in a row are the declines written: an attempt that asked nothing,
            // over however many others that did too, loses none.
            invariant DeclinesCountDeclines: dec == decls;
            // Once the elevation finished, the record says so whatever comes after it: the
            // program's own finish at a quit included.
            invariant FinishOutlivesLaterTries: ever <= fin;
            // What aterm's quit ends its tab's program with writes no end: the attempt reads cut
            // off, never declined.
            invariant AnInterruptionWritesNoEnd: cutend == 0;
            // What stops a terminal `aterm init` from outside its terminal (its hangup, a
            // `SIGTERM`) is never a person's decline: the attempt is asked again by itself.
            invariant AHangupNeverDeclines: cutno == 0;
            bound EpisodeBox: tries <= 1 && ends <= 1 && dec <= 2 && decls <= 2;
        }
    }
}

/// The window's ask the record names ends by itself: `[]<>` the record keeps no attempt of the
/// window's whose asker runs, with the window's finish weakly fair (once its attempt asks, it
/// ends; the walk's other ends are ways it may end too). The window's attempt is always one the
/// record keeps and one the window ends, so the record never names it asking for good. The
/// other askers' attempts end with their own processes; the record as a whole may read asking
/// for as long as people keep retrying over each other, which is theirs to do. A hand of a claim
/// a person's retry went over that the record refuses (`HandDropsClaim`, as the code once had
/// it) leaves the window holding none: the record names the window, which nobody ends, and
/// reads asking for the window's whole life, holding every automatic ask.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(clt_episode_model)]
pub fn clt_episode_liveness() -> Liveness {
    Liveness {
        name: "TheWindowsAskEnds",
        goal: eq(var("we"), int(0)),
        weak: vec!["DoneW"],
        strong: vec![],
        mutants: vec!["HandDropsClaim"],
    }
}
