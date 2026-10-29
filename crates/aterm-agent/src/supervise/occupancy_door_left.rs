// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A LINE LEFT IN THE COMPOSER, SETTLED AT THE NEXT LOOK (the slice-2 plan's 2.9b), the same way
//! at every door: the hold door's, the stuck line's, the wake's at a break, and the facts riding
//! a turn end's or a point's act. A write that finds its line written and not submitted
//! (`Written::Left`) books it left ([`Session::left_write`]: a `fact-<kind>@v1` row
//! `waiting … left`, the line in the facts' account, a wake's claim kept the door's), and the
//! session's next look settles it through that one account ([`Session::left_look`],
//! `FactsState::look`): told, its `typed … settled=<how>` row booked and its wake told; released,
//! its `skipped … why=<why>` row booked and its wake's claim given back, so `await wake` answers.
//! Until then no door tells its episode and no door claims its wake. A look is a read the loop
//! made anyway: nothing is read for it, under any mode, and with no line left it reads not even
//! the clock. Where the owner's `prompt` is no longer in force, the policy's draft step sends
//! nothing `prompt` left ([`Session::left_composer`]). A replay of a recording without slice 2's
//! doors (`Fx::slice2_recorded`) keeps nothing left: its doors book the line skipped, as the build
//! that recorded it did.

use crate::supervise::facts::{EpisodeKey, Fact, Left, LeftLook, Settled};
use crate::supervise::occupancy::door::{FactDoor, LeftRow, LeftSaid, left_reason};
use crate::supervise::occupancy::fault::{Fault, armed};
use crate::supervise::policy::turn_end::{Composer, TurnEndReading};

use super::*;

/// Where a look at the lines left was made, for what its settle does beside its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::supervise::run) enum LookAt {
    /// A read of a live turn (`Session::took`).
    Live,
    /// A read of a break of the agent's own background work.
    Break,
    /// The read a decided point was judged on, before its act.
    Point,
}

impl<C: Ctl> Session<'_, C> {
    /// A LINE LEFT at a door: `key`'s line `text` (its rows' `command`) written into the composer
    /// from `row`'s door and not submitted. Its `waiting … left` row is booked at the session's
    /// submit count as the loop's last read saw it, and the line kept in the facts' account
    /// (`FactsState::leave`); a wake's claim stays the door's. False where the account keeps
    /// nothing: on a replay of a recording without slice 2's doors (`Fx::slice2_recorded`, whose
    /// build kept no line left), and under the planted `ReleaseOnLeft` alone; the door books it
    /// skipped, as every door did before.
    pub(in crate::supervise::run) fn left_write(
        &mut self,
        key: EpisodeKey,
        row: LeftRow,
        text: &str,
        seq: u64,
        review: &mut dyn Review,
    ) -> bool {
        let submits = self.last.as_ref().and_then(|s| s.submits);
        let left = Left {
            row,
            text: text.to_string(),
            at_ms: self.now_unix_ms(),
            submits,
            ran: false,
            seeded: false,
            counted: true,
        };
        if !self.fx.slice2_recorded() || !self.facts.state.leave(key, left) {
            return false;
        }
        self.ledger_row(
            key.kind.rule(),
            Outcome::Waiting,
            text,
            &left_reason(row, key, LeftSaid::Left { submits }),
            seq,
        );
        Self::left_note(key, row, "left", seq, Some(review));
        true
    }

    /// THE LOOK AT `screen`, a read the loop made `at` ([`LookAt`]): its composer and submit count
    /// as the read drew them ([`Self::left_seen`]), each line it settles booked
    /// ([`Self::left_look`]). Nothing where no line stands left.
    pub(in crate::supervise::run) fn left_look_at(
        &mut self,
        screen: &Screen,
        at: LookAt,
        cfg: Option<&SupervisorConfig>,
        review: Option<&mut dyn Review>,
    ) {
        if self.facts.state.left_lines().next().is_none() {
            return;
        }
        let look = self.left_seen(screen, at == LookAt::Live);
        self.left_look(look, at, screen.seq, cfg, review);
    }

    /// The look a read of `screen` is (`live`: a turn runs on it): whether its composer holds a
    /// draft (unread under a prompt, or where none is drawn) and its submit count.
    pub(in crate::supervise::run) fn left_seen(&self, screen: &Screen, live: bool) -> LeftLook {
        let reader = self.reader(&screen.rows);
        let draft = if reader.prompt(&screen.rows).is_some() && !armed(Fault::BoxedComposerRead) {
            None
        } else {
            reader
                .composer(&screen.rows)
                .map(|c| typed_draft_in(Some(&c), reader.placeholder(), screen))
        };
        LeftLook::Read {
            draft,
            submits: screen.submits,
            live,
        }
    }

    /// THE ONE SETTLE OF EVERY DOOR'S LEFT LINES: `look` folded into the facts' account
    /// (`FactsState::look`), and for each line it settles, made `at` (`None`: the policy's draft
    /// step's Enter, or the session's end), what its door's own write would have done:
    ///
    /// * told: its `typed … settled=<how>` row, the host told the line (`IdleHost::inform`) where
    ///   the grant that typed it is in force under `cfg` (nothing `prompt` typed is raised under
    ///   `observe` or `off`) and this loop typed it (a line read back from a loop's row before
    ///   this one, `Left::seeded`, is the ledger's text, raised to no one), and a wake told
    ///   (`IdleHost::wake_told` through its claim's door); the line booked in the door's books
    ///   and the session's pace, as its write would have booked it, a rider cut from
    ///   the policy's kept text; and settled at a break, a break's line kept as the harness's
    ///   turn (`TurnEndState::fact_typed`, a rule's and the stuck line's answering the hold,
    ///   `HoldEvent::HarnessLine`), as its write would have kept it, since the agent has not
    ///   taken it yet. The host's break bookkeeping (`IdleHost::break_taken`) stays the write's:
    ///   a later break did not type it;
    /// * released: its `skipped … why=<why>` row, and a wake's claim given back
    ///   (`IdleHost::wake_release`), so the book reads it ready and `await wake` answers it. The
    ///   door tries it again past the pace its write set.
    ///
    /// Nothing at all where no line stands left: not even the clock is read, so a run with none
    /// records what a build before 2.9b recorded.
    pub(in crate::supervise::run) fn left_look(
        &mut self,
        look: LeftLook,
        at: impl Into<Option<LookAt>>,
        seq: u64,
        cfg: Option<&SupervisorConfig>,
        mut review: Option<&mut dyn Review>,
    ) {
        if self.facts.state.left_lines().next().is_none() {
            return;
        }
        let at = at.into();
        let now_ms = self.now_unix_ms();
        let granted = cfg.map(|c| {
            (
                c.occupancy.grant().is_some(),
                c.stuck_break_grant().is_some(),
            )
        });
        for (key, left, settled) in self.facts.state.look(look, now_ms) {
            let told = matches!(settled, Settled::Told(_));
            if told {
                self.left_booked(key, &left, at);
            }
            let outcome = if told {
                Outcome::Typed
            } else {
                Outcome::Skipped
            };
            self.ledger_row(
                key.kind.rule(),
                outcome,
                &left.text,
                &left_reason(left.row, key, LeftSaid::Settled(settled)),
                seq,
            );
            if let Some(host) = self.stall_host.clone() {
                if told
                    && !left.seeded
                    && (armed(Fault::InformUngranted)
                        || granted.is_some_and(|(prompt, stuck)| left.row.granted(prompt, stuck)))
                {
                    host.inform(&if left.row.rides() {
                        templates::stuck_line(&left.text)
                    } else {
                        left.text.clone()
                    });
                }
                if let LeftRow::Wake { ord, door, .. } = left.row {
                    if told {
                        host.wake_told(ord, key.id, door);
                    } else {
                        host.wake_release(ord, key.id, door);
                    }
                }
            }
            let words = match settled {
                Settled::Told(how) => format!("told settled={how}"),
                Settled::Released(why) => format!("released why={why}"),
            };
            Self::left_note(key, left.row, &words, seq, review.as_deref_mut());
            if told
                && at == Some(LookAt::Break)
                && matches!(
                    left.row,
                    LeftRow::Hold { .. }
                        | LeftRow::Fact {
                            door: FactDoor::Break
                        }
                )
                && !armed(Fault::ToldLineUnanswered)
                && let Some(review) = review.as_deref_mut()
            {
                self.hold_line(seq, cfg, review);
            }
        }
    }

    /// A left line told: booked as its door's write books a line typed, at the clock's now; a
    /// fact that rode an act is cut from the act's text the policy keeps for a `/compact`'s
    /// continuation (`TurnEndState::fact_rode`), so the act typed again carries it no more. No
    /// act left in the composer is recorded by `TurnEndState::acted` today (`turn_end_execute`
    /// returns before it), so that kept text never carries a left fact and the cut finds nothing
    /// to cut: it stands for the act that would be recorded.
    fn left_booked(&mut self, key: EpisodeKey, left: &Left, at: Option<LookAt>) {
        let clock_ms = self.hold_ms(self.fx.mono());
        if left.row.rides() {
            self.turn_end.fact_rode(&Fact {
                key,
                text: left.text.clone(),
                as_of_ms: left.at_ms,
            });
        }
        let breaks = match left.row {
            LeftRow::Hold { rule, since_ms, .. } => {
                let this_hold =
                    since_ms.is_some() && since_ms == self.hold.door.books.hold_since_ms();
                self.hold.door.books.fired(rule, clock_ms, this_hold);
                self.break_lined(clock_ms);
                true
            }
            LeftRow::Wake { door, .. } => {
                if !armed(Fault::ToldWakeUnpaced) {
                    self.hold.door.books.woke(key.id, clock_ms);
                }
                let breaks = door == WakeDoor::Break;
                if breaks {
                    self.break_lined(clock_ms);
                }
                breaks
            }
            LeftRow::Fact {
                door: FactDoor::Break,
            } => {
                self.door_lined(clock_ms);
                self.break_lined(clock_ms);
                true
            }
            LeftRow::Fact {
                door: FactDoor::TurnEnd,
            } => false,
        };
        if breaks && at == Some(LookAt::Break) && !armed(Fault::ToldLineUntaken) {
            self.turn_end.fact_typed();
        }
    }

    /// A left line's `FACT` journal line: its kind, episode and door, then `words`.
    fn left_note(
        key: EpisodeKey,
        row: LeftRow,
        words: &str,
        seq: u64,
        review: Option<&mut (dyn Review + '_)>,
    ) {
        let Some(review) = review else {
            return;
        };
        let door = match row {
            LeftRow::Hold { .. } => WakeDoor::Break.word(),
            LeftRow::Wake { door, .. } => door.word(),
            LeftRow::Fact { door } => door.word(),
        };
        review.note(&format!(
            "FACT seq={seq} kind={} episode={:016x} door={door} {words}",
            key.kind.word(),
            key.id,
        ));
    }

    /// THE COMPOSER A POINT DECIDES ON (`r`'s), under `cfg`: a draft standing while a line left
    /// stands whose grant is no longer in force (the owner's `prompt`, or the stuck line's
    /// `stuck_break_note`) reads as aterm's own words (`Composer::Own`), which the policy asks a
    /// person about and never sends (`FactsState::draft_may_go`): nothing `prompt` typed goes
    /// under `observe` or `off`. The person sends or clears it, and the next look settles it.
    pub(in crate::supervise::run) fn left_composer(
        &self,
        r: &mut TurnEndReading,
        cfg: &SupervisorConfig,
    ) {
        let prompt = cfg.occupancy.grant().is_some();
        let stuck = cfg.stuck_break_grant().is_some();
        if r.composer == Composer::Typed && !self.facts.state.draft_may_go(prompt, stuck) {
            r.composer = Composer::Own;
        }
    }
}
