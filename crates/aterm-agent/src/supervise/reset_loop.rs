// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A LIMIT RESET IN THE LOOP
//! (`docs/SPEC-claude-accounts-resets-2026-10-02.md` §3; the model is
//! aterm-spec's `supervisor_limit_reset_model`): the usage-limit dialog's
//! GRANT ROW taken where the host gives the go, and what the row opens seen
//! through to its end — a step of the loop's own, shaped as the accounts
//! probe is (`turn_end_loop.rs`'s `usage_probe`): fenced keys, an owed part
//! at the top of every look, and no act of the turn-end policy's.
//!
//! NOTHING OF IT HAS BEEN SEEN. Every text the readers this stands on read
//! (`aterm_phase::reset`) and every transition below is READ in the 2.1.287
//! bundle, never on a screen; `[harness] limit_reset` ships `false`, and with
//! it off no row is taken — [`Session::reset_take`] is reached only through a
//! decision the approval policy makes with the key on — and the only step
//! that runs is one an earlier loop left open in the session ledger:
//! Escape-only, ended untold ([`Session::reset_end`]). [`ResetStep`] is
//! `Idle` for the life of every loop whose ledger holds no open row of the
//! rule and that takes none, and of every attended loop.
//!
//! A RESET IS THE OWNER'S SCARCE PURCHASE, so every part of the step refuses
//! by default:
//!
//! * **Keys only, and never a command.** Nothing here calls `send` or `turn`:
//!   the row is reached by the approval loop's own confirmed focus move
//!   (`press_focused`, every key of it fenced on the judged read's person
//!   count too), the confirmation's Enter is one fenced key, and everything
//!   else is an Escape.
//! * **A. The row** ([`Session::reset_take`]): the loop's gates, then the
//!   host's word ([`IdleHost::reset_take`]) LAST, immediately before the
//!   row's first key. Anything but its go passes the reset over for the
//!   dialog's mount ([`Session::reset_passed`]), and the dialog is answered
//!   with the decision made without the reset's rule on the same read — the
//!   wait row, by the same keys a build without resets writes.
//! * **B. What follows** ([`Session::reset_run`]): one ordinary read at a
//!   time, every bound counted from the first read that showed what it
//!   bounds. A pending line is waited on and never keyed — but `Checking…`
//!   past its bound, which gets one guarded Escape (the vendor reads it as a
//!   cancel that prints nothing). A box the row opened is the step's however
//!   it is reworded, met on any read of the step but one after a box the
//!   approval policy already judged: never another rule's to answer. A box
//!   with nothing of the confirmation and an answer kept for good is never
//!   the step's, and one with its head off the screen and no Esc read is a
//!   frame half drawn: its Esc may be kept too (Claude Code's MCP server
//!   dialogs).
//! * **C. The confirmation** ([`Session::reset_confirmation`]): keyed only
//!   once it has stood the box settle in this loop's sight (the vendor
//!   refuses a key sooner and re-arms its refusal), with nobody's hand on
//!   the session by a fresh `status`, the claim still this session's by the
//!   host's word asked again. ENTER goes only where
//!   [`confirm_verdict`] says so — the confirmation read exactly and agreeing
//!   with the row taken — as `key if-human=<n> if-gen=<the read> if=<the Yes
//!   row as drawn> enter`. EVERYTHING ELSE IS ESCAPE, `key if-human=<n>
//!   if=<the title row> esc`: never an arrow, never the No row by keys. Once
//!   an Escape was written no Enter follows it.
//! * **D. The end** ([`Session::reset_end`]): the host reads the vendor's one
//!   answer line from the transcript ([`IdleHost::reset_told`]) — the screen
//!   says only that the dialog is gone — and the turn-end policy is told
//!   what it owes ([`TurnEndState::reset_told`]): one continuation, typed by
//!   its limit-resume rule.
//! * **A successor never enters** (§3.1). A step the session ledger says an
//!   earlier loop opened ([`approvals::reset_owed`]), and one whose own press
//!   got no answer, carry no go: their confirmation is Escaped once it has
//!   stood one person's grace in this loop's sight. A confirmation on a
//!   session with no such step is a PERSON's, and is not touched here.
//!   An open row older than a step can be ([`RESET_STALE`]) is a step all
//!   the same, Escape-only, whose end is told to nobody.
//! * **Nothing raises attention.** Ledger rows, journal lines
//!   (`RESET seq=<n> rule=limit-reset@v1 …`), and one record through
//!   [`IdleHost::inform`] for a dialog whose Escape does not take, or that
//!   a step whose end is told leaves on a screen it cannot read.

use super::super::policy::approval::{RULE_LIMIT_RESET, kept_for_good};
use super::super::policy::reset::{Verdict as ResetVerdict, confirm_verdict};
use super::super::policy::row_guard;
use super::*;
use crate::harness::accounts::RESET_CLAIM_S;
use aterm_phase::{ConfirmRead, ResetPending};

/// How many times in a row the host may answer that it cannot say yet
/// ([`ResetWord::Again`]: the wall's own row is not written, its lock is
/// held) before the reset is passed over for the mount: each is waited
/// [`ResetWaits::again`], and the next one is a pass.
pub(super) const RESET_AGAIN_MAX: u8 = 5;

/// How many Enters the confirmation gets: one the vendor dropped is judged
/// and written again a second later ([`ResetWaits::enter`]); after this many
/// the confirmation is Escaped ([`KeepWhy::EnterNotTaken`]).
pub(super) const RESET_ENTERS: u32 = 3;

/// How many Enters in a row the server may skip for a screen that moved
/// under the read before the confirmation is Escaped
/// ([`KeepWhy::Inexact`], `moving`): a screen that repaints for ever is not
/// read and keyed for the claim's whole life.
pub(super) const RESET_SKIPS: u32 = 5;

/// How old the session ledger's open row may be for a later loop to tell
/// the end of its step ([`Session::reset_take_up`]): the claim's life and
/// five minutes more — every bound of the step, twice over. Past it the
/// host's claim has lapsed and a line in the transcript may be a later
/// dialog's: the step is ended untold.
pub(super) const RESET_STALE: Duration = Duration::from_secs(RESET_CLAIM_S.unsigned_abs() + 300);

/// From which failed Escape on the host is told the dialog is stuck
/// ([`IdleHost::inform`], once) while the tries go on: as the probe's.
pub(super) const RESET_STUCK_AT: u32 = 2;

/// The one record a dialog that will not close leaves with the host: a title
/// and what a person does about it. A record, never a badge.
const RESET_STUCK: &str = "The limit-reset confirmation did not close\nPress Esc in this tab.";

/// The one record a step leaves that ended on a screen its readers could
/// not read: whatever stands there is a person's to look at.
const RESET_UNREAD: &str = "A limit-reset dialog could not be read\nLook at this tab.";

/// How long a screen that moved must hold still before the step's next read
/// of it, in the server's `await idle` milliseconds: as the probe's.
const RESET_STILL_MS: &str = "50";

/// What a `HELD` line names as the act a person's hand holds.
const RESET_KEY_ACT: &str = "the reset confirmation's key";

/// What the step's `waiting` row names: the step, not a text of the vendor's.
const RESET_SUBJECT: &str = "limit reset";

/// What the `waiting` row says of a step ended under `[harness] limit_reset
/// = false` ([`Session::reset_end`]): nobody was told how it ended.
const RESET_UNTOLD: &str = "ended: untold (key off)";

/// What the `waiting` row says of a step whose open row was older than a
/// step can be ([`RESET_STALE`]): ended with nobody told, at either key.
const RESET_STALE_END: &str = "ended: stale";

/// The bounds of one reset step ([`RESET_WAITS`]; a test shortens them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResetWaits {
    /// From the row's Enter to a pending line, a confirmation or the
    /// dialog's end: past it the row opened nothing
    /// ([`KeepWhy::NotOpened`]).
    pub(super) open: Duration,
    /// How long `Checking for a reset…` may stand (the vendor makes one
    /// usage read under it) before its one Escape
    /// ([`KeepWhy::CheckTimeout`]).
    pub(super) check: Duration,
    /// How long `Resetting your limits…` is waited on: the vendor bounds its
    /// claim at 35 s (READ). Past it the host is asked one last time, and no
    /// key is written.
    pub(super) claim: Duration,
    /// How long the host is asked again for the answer line: the
    /// transcript's row lags the screen.
    pub(super) line: Duration,
    /// The pause before the host is asked again where it could not say yet.
    pub(super) again: Duration,
    /// How long a written Enter is given before a confirmation still up is
    /// judged again: far past the vendor's 250 ms guard.
    pub(super) enter: Duration,
    /// The latest an Enter may be written, counted from when the claim was
    /// asked for: the claim's ten minutes
    /// ([`RESET_CLAIM_S`]) less the time the vendor's claim and its line may
    /// take, so the host's claim never lapses — and frees the account's
    /// other sessions to move off it — over a reset still in the making.
    pub(super) enter_by: Duration,
    /// The longest pause between two reads of the step.
    pub(super) step: Duration,
    /// How long the dialog is given to leave after an Escape.
    pub(super) close: Duration,
}

pub(super) const RESET_WAITS: ResetWaits = ResetWaits {
    open: Duration::from_secs(10),
    check: Duration::from_secs(45),
    claim: Duration::from_secs(60),
    line: Duration::from_secs(10),
    again: Duration::from_secs(2),
    enter: Duration::from_secs(1),
    enter_by: Duration::from_secs(RESET_CLAIM_S.unsigned_abs() - 60 - 10),
    step: Duration::from_secs(1),
    close: Duration::from_secs(2),
};

/// What came of asking for the grant row ([`Session::reset_take`]).
pub(super) enum ResetAsked {
    /// The host's go: the row is pressed as decided.
    Take,
    /// The host cannot say yet: the dialog is looked at again in a moment.
    Again,
    /// The reset is passed over for this mount: the dialog is answered as it
    /// is with resets off.
    Pass,
}

/// What a read shows of a step that is owed ([`Session::reset_run`]).
enum Shows {
    /// A pending line under its dialog's title, and its row.
    Pending(ResetPending, usize),
    /// The usage-limit dialog.
    Wall,
    /// The confirmation, as it read, and its box — `None` where its title
    /// stands on the screen and no box reads under it (the vendor's other
    /// layout of the buttons): never entered.
    Confirm(ConfirmRead, Option<Box<aterm_phase::PromptV2>>),
    /// A box that is none of the reset's: the approval policy's to judge.
    Other,
    /// A composer frame, or the worker at work: the dialog is gone.
    Gone,
    /// None of them, or a box whose head and Esc are both unread: a frame
    /// half drawn.
    Unread,
}

/// What a part of the step asks of the reads that drive it.
enum Flow {
    /// Read again, after the screen's next move or this long at most.
    Again(Duration),
    /// Go on from this read, already made.
    Read(Screen),
    /// Nothing more at this look: the step stays owed.
    Leave,
}

/// What is done at a confirmation ([`Session::reset_verdict`]).
enum Judged {
    /// Enter, on the yes row as read.
    Enter,
    /// Escape, and why.
    Keep(KeepWhy),
    /// The host cannot say yet whether the claim still stands: asked again
    /// in a moment, nothing keyed.
    Again,
}

/// The year a day of the vendor's names (`Jan 2, 2027 at 9am`: the formatter
/// adds it whenever it is not this year's): its third word, four digits.
fn named_year(when: &str) -> Option<i64> {
    when.split(|c: char| c.is_whitespace() || c == ',')
        .filter(|word| !word.is_empty())
        .nth(2)
        .filter(|word| word.len() == 4 && word.bytes().all(|b| b.is_ascii_digit()))?
        .parse()
        .ok()
}

/// The outcome as the journal's `RESET` line words it.
fn outcome_words(outcome: &ResetOutcome, kept: Option<KeepWhy>) -> String {
    match outcome {
        ResetOutcome::Spent { left: Some(n), .. } => format!("used ({n} left)"),
        ResetOutcome::Kept => format!("kept ({})", kept.unwrap_or(KeepWhy::Closed)),
        ResetOutcome::Failed { login } => {
            format!("failed ({})", if *login { "login" } else { "answer" })
        }
        other => other.word().to_string(),
    }
}

impl<C: Ctl> Session<'_, C> {
    /// The step owed, if one is.
    fn reset_owed(&self) -> Option<&ResetOwed> {
        match &self.reset {
            ResetStep::Idle => None,
            ResetStep::Owed(owed) => Some(owed),
        }
    }

    fn reset_owed_mut(&mut self) -> Option<&mut ResetOwed> {
        match &mut self.reset {
            ResetStep::Idle => None,
            ResetStep::Owed(owed) => Some(owed),
        }
    }

    /// A step owed from now, with `go` what its confirmation is held to
    /// (`None`: never entered) and the claim asked for at `since`.
    fn reset_step_owed(&mut self, go: Option<ResetGo>, since: Instant, entered: bool) {
        let now = self.fx.mono();
        self.reset = ResetStep::Owed(Box::new(ResetOwed {
            go,
            since,
            keyed: now,
            entered,
            enters: 0,
            kept: None,
            tries: 0,
            at: now,
            told: false,
            seen: None,
            offer: None,
            hold: None,
            shown: None,
            yes_said: false,
            skips: 0,
            agains: 0,
            stale: false,
        }));
    }

    /// THE ROW'S ENTER WAS WRITTEN: the step is owed from here, on the go
    /// the host gave for this row ([`Self::reset_take`]) — this loop's own,
    /// and the only step whose confirmation may be entered. Called before
    /// the row's ledger row is written, so the memory never trails it.
    pub(super) fn reset_row_taken(&mut self) {
        let (go, since) = match self.reset_taken.take() {
            Some((go, since)) => (Some(go), since),
            None => (None, self.fx.mono()),
        };
        self.reset_step_owed(go, since, false);
    }

    /// THE ROW'S ENTER GOT NO ANSWER (`subject`, read at `seq`): it may have
    /// been written, and have taken the row. The step is owed with NO go —
    /// whatever the row opened is closed, never entered — and the ledger says
    /// so for the loop that follows. Not for a press lost at an arrow
    /// ([`Session::focus_key_tried`]): an arrow takes no row, and the dialog
    /// is decided again after the reconnect, the claim still in hand.
    pub(super) fn reset_row_lost(&mut self, subject: &str, seq: u64) {
        self.reset_taken = None;
        self.reset_step_owed(None, self.fx.mono(), false);
        self.ledger_row(
            RULE_LIMIT_RESET,
            approvals::Outcome::Approved,
            subject,
            "taken: the press's answer never came",
            seq,
        );
    }

    /// A STEP AN EARLIER LOOP OPENED ([`approvals::reset_owed`]: its ledger
    /// row, and whether the confirmation's Enter was written since) is this
    /// loop's to end, from now: Escape-only, as every step with no go.
    ///
    /// A row OLDER THAN A STEP CAN BE ([`RESET_STALE`]) is owed all the
    /// same, and ends UNTOLD ([`Self::reset_untold`]): the host's claim for
    /// it has lapsed and a line in the transcript may be a later dialog's,
    /// so the host is neither asked nor told and no continuation is owed —
    /// but a box that still stands under it is Escaped, never left to a
    /// rule of the approval policy that could say yes to it.
    pub(super) fn reset_take_up(&mut self, open: &approvals::ResetOpen, review: &mut dyn Review) {
        let age_ms = self.fx.stamp_ms().saturating_sub(open.taken_ms);
        self.reset_step_owed(None, self.fx.mono(), open.entered);
        if u128::try_from(age_ms).unwrap_or(u128::MAX) <= RESET_STALE.as_millis() {
            return;
        }
        if let Some(owed) = self.reset_owed_mut() {
            owed.stale = true;
        }
        review.note(&format!(
            "RESET seq=0 rule={RULE_LIMIT_RESET} stale (taken {} s ago): its end is told to nobody",
            age_ms / 1000
        ));
    }

    /// WHETHER THE STEP OWED ENDS UNTOLD, and the word its `waiting` row then
    /// carries: its open row was stale ([`Self::reset_take_up`]), or
    /// `[harness] limit_reset` is off. Such a step asks and tells the host
    /// nothing, leaves it no record of a screen it could not read, and owes
    /// the turn-end policy nothing ([`Self::reset_end`]).
    fn reset_untold(&self, opts: &SuperviseOpts) -> Option<(&'static str, &'static str)> {
        let owed = self.reset_owed()?;
        if owed.stale {
            Some((RESET_STALE_END, "stale"))
        } else if !opts.policy.limit_reset {
            Some((RESET_UNTOLD, "limit_reset off"))
        } else {
            None
        }
    }

    /// Whether the reset was passed over for the dialog mount now up
    /// ([`Session::reset_passed`]): the rest of it is decided without the
    /// reset's rule.
    pub(super) fn reset_passed_here(&self) -> bool {
        self.reset_passed.is_some()
            && self.reset_passed.as_deref() == self.box_first_seen.as_ref().map(|(k, _)| k.as_str())
    }

    /// The reset is passed over for the dialog mount now up, and why:
    /// `SKIPPED seq=<n> rule=limit-reset@v1 <why>`, once a mount — the host
    /// is not asked again at it. A claim the host had given for the row is
    /// told kept ([`Self::reset_untaken`]).
    pub(super) fn reset_pass(
        &mut self,
        seq: u64,
        why: &str,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) {
        self.reset_passed = self.box_first_seen.as_ref().map(|(key, _)| key.clone());
        self.reset_again = 0;
        review.note(&format!("SKIPPED seq={seq} rule={RULE_LIMIT_RESET} {why}"));
        self.reset_untaken(seq, opts, review);
    }

    /// A CLAIM THE HOST GAVE AND NO ROW WAS TAKEN ON — the press never
    /// landed, or the dialog left first: the host is told it was kept
    /// ([`KeepWhy::NotOpened`]), so the wall has had its one attempt and its
    /// claim holds nobody. Nothing where no claim is in hand. No row of the
    /// session ledger and nothing owed to the turn-end policy: no step was
    /// opened, and the dialog is the wait row's. A host that answers that
    /// the limits were refilled all the same (a person took the row) is
    /// believed: the policy is told that.
    pub(super) fn reset_untaken(
        &mut self,
        seq: u64,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) {
        if self.reset_taken.take().is_none() {
            return;
        }
        let kept = Some(KeepWhy::NotOpened);
        let end = ResetEnd {
            entered: false,
            kept,
            offer: None,
            last: true,
        };
        let outcome = opts
            .idle_host
            .as_ref()
            .map_or(ResetOutcome::Unread, |host| host.reset_told(&end));
        review.note(&format!(
            "RESET seq={seq} rule={RULE_LIMIT_RESET} {}",
            outcome_words(&outcome, kept)
        ));
        if matches!(
            outcome,
            ResetOutcome::Spent { .. } | ResetOutcome::Lifted { .. }
        ) {
            self.turn_end.reset_told(&outcome, self.fx.mono());
        }
    }

    /// The zone the wall's notice named, where the loop holds one: the open
    /// limit episode's reset text, else the wall it remembers waiting on.
    /// Held only where an idle point showed the wall BEFORE its dialog
    /// mounted: the look at a dialog closes the episode, and the dialog as
    /// composed draws no notice. Mounted straight after the rejection, as
    /// the vendor does it, there is none, and the local zone places the day.
    fn reset_zone(&self) -> Option<String> {
        let named = |text: &str| match limit::parse_reset(text) {
            Some(ResetSpec::At { zone, .. }) => zone,
            _ => None,
        };
        self.limit
            .as_ref()
            .and_then(|episode| named(&episode.reset_text))
            .or_else(|| {
                let wall = &self.wall_memory.as_ref()?.wall;
                named(wall.reset.as_deref()?)
            })
    }

    /// A DAY AND HOUR OF THE VENDOR'S (`Oct 5 at 3pm`, `Jan 2, 2027 at
    /// 9:30am`) AS UNIX SECONDS: placed in the wall notice's zone where the
    /// loop holds one ([`Self::reset_zone`]), else the local one — the vendor
    /// draws these in the process's zone and names none — at the offset that
    /// zone has ON THAT DAY where the machine can say
    /// ([`Session::local_offset_at`]), so a day across a daylight-saving
    /// change lands where its clock shows it. A day drawn WITH ITS YEAR is
    /// placed in that year; one drawn without, in the year nearest now
    /// ([`limit::reset_at_with`]). `None` for anything that is not a dated
    /// clock time: a bare clock and a span place no deadline.
    pub(super) fn reset_place(&mut self, when: &str) -> Option<i64> {
        let zone = self.reset_zone();
        let text = match &zone {
            Some(zone) => format!("{when} ({zone})"),
            None => when.to_string(),
        };
        let spec = limit::parse_reset(&text)?;
        let ResetSpec::At {
            date: Some((month, day)),
            ..
        } = &spec
        else {
            return None;
        };
        let (now, local) = (self.now_unix(), self.local_offset_s());
        // The nearest of three years is taken about `now` — or about the
        // day itself in the year it names, which is then that year's.
        let about = named_year(when).map_or(now, |year| {
            limit::days_from_civil(year, *month, *day) * 86_400 + 43_200
        });
        let at = limit::reset_at_with(&spec, about, local, &self.zone_lookup());
        Some(match (zone, self.local_offset_then(at)) {
            (None, Some(there)) => at + local - there,
            _ => at,
        })
    }

    /// The first second of a grant's last day as the row draws it (`Oct 5`):
    /// a day, not a deadline — what the host's pre-filter reads.
    fn reset_day_start(&mut self, day: &str) -> Option<i64> {
        self.reset_place(&format!("{day} at 12am"))
    }

    /// The loop's own gates on the grant row, in order: `None` with every
    /// one open, else the word of the first that shut. Nothing is written by
    /// any of them, and the one request (`help key`) is made once a host.
    fn reset_gate(
        &mut self,
        turn: &Turn,
        opts: &SuperviseOpts,
        review: &dyn Review,
    ) -> Result<Option<&'static str>, Fail> {
        // A loop that acts on the session by itself, for a host, and keeps
        // a ledger the loop after it can read.
        if !review.unattended() {
            return Ok(Some("attended"));
        }
        if opts.idle_host.is_none() {
            return Ok(Some("no-host"));
        }
        if self.claim.watching_behind().is_some() {
            return Ok(Some("watching"));
        }
        if self.setup.ledger_path.is_none() {
            return Ok(Some("no-ledger"));
        }
        if self.reset != ResetStep::Idle {
            return Ok(Some("step-owed"));
        }
        // A reset is a way of resuming a limit.
        if !opts.policy.resume_limits {
            return Ok(Some("resume-limits"));
        }
        // Every fence the confirmation's keys carry is one this host has.
        let screen = &turn.screen;
        if screen.generation.is_none() || screen.human_seq.is_none() || !self.gen_fence_known()? {
            return Ok(Some("no-fence"));
        }
        Ok(None)
    }

    /// A. MAY THE GRANT ROW ON `turn` BE TAKEN? Asked where the approval
    /// policy decided the row ([`RULE_LIMIT_RESET`]) and the stall check and
    /// the person's hold have run as for any press, BEFORE ANY KEY:
    ///
    /// 1. the loop's gates ([`Self::reset_gate`]), any one shut a pass;
    /// 2. the row read again from the same rows, and its day placed
    ///    ([`Self::reset_day_start`]: `None` where it does not place);
    /// 3. a session `status` says is halted is waited on
    ///    ([`Session::park_on_hold`]) and looked at again: no key can be
    ///    written under a hold, and a claim taken there would run out with
    ///    none;
    /// 4. the host's word, last ([`IdleHost::reset_take`]).
    ///    [`ResetWord::Take`]: the go is kept for the row's press
    ///    ([`Session::reset_taken`]). [`ResetWord::Again`]: one wait of
    ///    [`ResetWaits::again`], and the dialog is looked at again — the
    ///    sixth in a row is a pass. [`ResetWord::No`]: a pass, with its word.
    ///
    /// A pass says why once ([`Self::reset_pass`]) and the caller answers
    /// the dialog with the decision made without the reset's rule.
    pub(super) fn reset_take(
        &mut self,
        turn: &Turn,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<ResetAsked, Fail> {
        let seq = turn.screen.seq;
        let shut = self.reset_gate(turn, opts, review)?;
        let row = aterm_phase::parse_prompt_v2(&turn.screen.rows)
            .as_ref()
            .and_then(aterm_phase::reset_row);
        let (None, Some(row), Some(host)) = (shut, row, opts.idle_host.clone()) else {
            let gate = shut.unwrap_or("row");
            self.reset_pass(seq, &format!("gate={gate}"), opts, review);
            return Ok(ResetAsked::Pass);
        };
        let ask = ResetAsk {
            left: row.left,
            day_start: self.reset_day_start(&row.day),
            day: row.day,
        };
        if self.halted {
            self.park_on_hold("the session is held: no reset is claimed", seq, review)?;
            return Ok(ResetAsked::Again);
        }
        let asked = self.fx.mono();
        match host.reset_take(&ask) {
            ResetWord::Take(go) => {
                self.reset_again = 0;
                // The claim is as old as its first go at this mount.
                let since = self.reset_taken.take().map_or(asked, |(_, since)| since);
                self.reset_taken = Some((go, since));
                Ok(ResetAsked::Take)
            }
            ResetWord::Again if self.reset_again < RESET_AGAIN_MAX => {
                if self.reset_again == 0 {
                    review.note(&format!(
                        "WAITING seq={seq} rule={RULE_LIMIT_RESET} the host cannot say yet: asked \
                         again in {} ms",
                        self.setup.reset_waits.again.as_millis()
                    ));
                }
                self.reset_again += 1;
                self.wait(&["seq", &seq.to_string()], self.setup.reset_waits.again)?;
                Ok(ResetAsked::Again)
            }
            ResetWord::Again => {
                self.reset_pass(seq, "host=again", opts, review);
                Ok(ResetAsked::Pass)
            }
            ResetWord::No(why) => {
                self.reset_pass(seq, &format!("host={}", why.word()), opts, review);
                Ok(ResetAsked::Pass)
            }
        }
    }

    /// What `screen` shows of a step that is owed, read by the reset's own
    /// readers first: a pending line reads as an idle screen with no box to
    /// every other reader. `before` is what the step's last read showed.
    ///
    /// A box that keeps NO part of the confirmation is still the step's
    /// where it is of no kind the reader knows and draws unnumbered options,
    /// on ANY READ OF THE STEP but one whose last read handed a box to the
    /// approval policy (`before` is [`ResetShown::Other`]): the step's first
    /// read (`Checking…` may come and go between two reads, and a step taken
    /// up from the ledger has read nothing before) and a read after a frame
    /// half drawn as much as one after the dialog. The row opened it,
    /// whatever the vendor's server reworded it to, and no rule of the
    /// approval policy is to say yes to it: it reads as a confirmation with
    /// no title, and is Escaped. A box the policy was handed stays the
    /// policy's while the reads go on showing a box, so one box never
    /// changes hands. NEVER a box that offers an answer kept for good, read by
    /// its role or its label as the press gate reads it ([`kept_for_good`]: a
    /// frame whose roles the reader withheld, two cursors mid-move, still
    /// offers one), nor a box whose head is off the screen and that reads no
    /// Esc, a frame still being drawn: it reads as one ([`Shows::Unread`]),
    /// never keyed. Claude Code's MCP server dialogs save their Esc as a
    /// refusal for good (2.1.301). Read as one, its kind is
    /// `McpServer`, never [`PromptKind::Other`]; retitled or cut, the
    /// one-server dialog's three answers and the several-server dialog's
    /// `Enable selected` are kept by their labels, and the several-server
    /// dialog drawn only down to its server rows, its head cut, reads no Esc.
    /// Such a box is the policy's — a person's.
    ///
    /// With no box read at all, a row that is the confirmation's title on a
    /// screen with no composer is the confirmation in a layout the box
    /// reader does not take (the vendor's other drawing of the buttons, READ
    /// in the bundle): a confirmation with no options, Escaped on its title —
    /// while no Enter was written; after one, that title stands over the
    /// vendor's claim.
    fn reset_shows(&self, screen: &Screen, before: Option<ResetShown>) -> Shows {
        let rows = &screen.rows;
        if let Some((line, row)) = aterm_phase::reset_pending(rows) {
            return Shows::Pending(line, row);
        }
        let reader = self.reader(rows);
        match aterm_phase::read(self.program.as_deref(), rows, Some(screen.cursor_col)).prompt {
            Some(prompt) if prompt.kind == PromptKind::UsageLimit => Shows::Wall,
            Some(prompt) => match aterm_phase::reset_confirm(&prompt, rows) {
                ConfirmRead::Not => {
                    let judged = before == Some(ResetShown::Other);
                    let unnumbered =
                        !prompt.options.is_empty() && prompt.options.iter().all(|o| o.n.is_none());
                    // A box with an answer kept for good is no confirmation
                    // the vendor reworded (an MCP server dialog retitled or
                    // cut, whose Esc too is kept); one whose head and Esc are
                    // both unread is a frame half drawn, read again.
                    let kept = prompt.options.iter().any(kept_for_good);
                    let unfinished = prompt.head_off_screen && prompt.cancel.is_none();
                    let reworded =
                        !judged && unnumbered && !kept && prompt.kind == PromptKind::Other;
                    if reworded && unfinished {
                        Shows::Unread
                    } else if reworded {
                        Shows::Confirm(ConfirmRead::Inexact("title"), Some(Box::new(prompt)))
                    } else {
                        Shows::Other
                    }
                }
                read => Shows::Confirm(read, Some(Box::new(prompt))),
            },
            None if reader.composer(rows).is_some() || reader.phase(rows) == Phase::Busy => {
                Shows::Gone
            }
            // Not once the yes was taken: the title then stands over the
            // vendor's claim, whatever its line reads as.
            None if self.reset_owed().is_some_and(|owed| !owed.entered)
                && rows
                    .iter()
                    .any(|row| row.trim() == aterm_phase::anchor("reset.confirm.title")) =>
            {
                Shows::Confirm(ConfirmRead::Inexact("options"), None)
            }
            None => Shows::Unread,
        }
    }

    /// One wait of the step's, to `until` at most: a step of the loop's and
    /// the budget bound it.
    fn reset_wait(&self, until: Option<Instant>, deadline: Instant) -> Duration {
        let now = self.fx.mono();
        until
            .map_or(WAIT_STEP, |t| t.saturating_duration_since(now))
            .min(WAIT_STEP)
            .min(deadline.saturating_duration_since(now))
    }

    /// THE STEP OWED, at the top of every look ([`Session::look`], right
    /// after the usage panel's own; the spec's §3.1). Nothing while none is
    /// owed — but a claim given for a row that was never taken is told kept
    /// once its dialog has left ([`Self::reset_untaken`]). `true`: a pending
    /// line or a confirmation was handled — waited on, or keyed — or the
    /// step's end was told, and the loop looks again; `false`: the screen
    /// shows another box, which the approval policy judges while the step
    /// stays owed.
    ///
    /// A stall of the worker's is held, never keyed into.
    pub(super) fn reset_look(
        &mut self,
        turn: &Turn,
        state: &mut Looking,
        deadline: Instant,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let seq = turn.screen.seq;
        if self.reset == ResetStep::Idle {
            let shows_row = || {
                aterm_phase::parse_prompt_v2(&turn.screen.rows)
                    .as_ref()
                    .and_then(aterm_phase::reset_row)
                    .is_some()
            };
            if self.reset_taken.is_some() && !shows_row() {
                self.reset_untaken(seq, opts, review);
            }
            return Ok(false);
        }
        if self.stall_look(seq, state, review)? {
            let step = self.reset_wait(None, deadline);
            self.wait(&["seq", &seq.to_string()], step)?;
            return Ok(true);
        }
        self.reset_run(Some(turn.screen.clone()), deadline, opts, review)
    }

    /// B. WHAT FOLLOWS THE ROW, one ordinary read at a time, each after the
    /// screen's next move ([`ResetWaits::step`] at most; a screen that moved
    /// is given [`RESET_STILL_MS`] to hold still first), bounded by the
    /// budget and ended by the loop's stop. `first`: a read already made
    /// (the look's); `None` reads. What each read shows, and then:
    ///
    /// * `Checking for a reset…` — waited on; past [`ResetWaits::check`] one
    ///   guarded Escape on its own row ([`KeepWhy::CheckTimeout`]);
    /// * `Resetting your limits…` — waited on and NEVER keyed; past
    ///   [`ResetWaits::claim`] the host is asked one last time
    ///   ([`Self::reset_end`]);
    /// * the usage-limit dialog still — read again until
    ///   [`ResetWaits::open`]; then the row opened nothing
    ///   ([`KeepWhy::NotOpened`]) and the step ends;
    /// * a frame that reads as nothing — read again until
    ///   [`ResetWaits::open`], or [`ResetWaits::claim`] once the
    ///   confirmation's Enter was written (the vendor's claim may be under
    ///   way on a screen this build does not read); then the step ends, and
    ///   the host is left one record that a dialog may stand unread
    ///   ([`RESET_UNREAD`]);
    /// * the confirmation — [`Self::reset_confirmation`];
    /// * another box — `false`: the approval policy judges it, the step
    ///   stays owed;
    /// * a composer frame, or the worker at work — the step ends
    ///   ([`Self::reset_end`]).
    ///
    /// EVERY BOUND IS COUNTED FROM THE FIRST READ THAT SHOWED WHAT IT BOUNDS,
    /// or from the step's last key where that is later
    /// ([`ResetOwed::shown`], [`ResetOwed::keyed`]): a line first seen long
    /// after the key — a person's own Enter on the yes while the loop held
    /// its hand, an outage — has its whole bound from there, so a reset the
    /// vendor is claiming is never told as ended at first sight.
    pub(super) fn reset_run(
        &mut self,
        first: Option<Screen>,
        deadline: Instant,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let waits = self.setup.reset_waits;
        let mut screen = match first {
            Some(screen) => screen,
            None => self.screen()?,
        };
        loop {
            if self.stopped() || self.fx.mono() >= deadline {
                return Ok(true);
            }
            let Some((keyed, before, entered)) = self
                .reset_owed()
                .map(|owed| (owed.keyed, owed.shown, owed.entered))
            else {
                return Ok(true);
            };
            let seq = screen.seq;
            let shows = self.reset_shows(&screen, before.map(|(shown, _)| shown));
            let shown = match &shows {
                Shows::Pending(ResetPending::Checking, _) => Some(ResetShown::Checking),
                Shows::Pending(ResetPending::Resetting, _) => Some(ResetShown::Resetting),
                Shows::Wall => Some(ResetShown::Wall),
                Shows::Confirm(..) => Some(ResetShown::Confirm),
                Shows::Other => Some(ResetShown::Other),
                Shows::Unread => Some(ResetShown::Unread),
                Shows::Gone => None,
            };
            // What this read shows has stood since the first read of an
            // unbroken run that showed it, and no longer than the last key.
            let since = match (before, shown) {
                (Some((was, at)), Some(is)) if was == is => at.max(keyed),
                _ => self.fx.mono(),
            };
            if let Some(owed) = self.reset_owed_mut() {
                owed.shown = shown.map(|is| match before {
                    Some((was, at)) if was == is => (was, at),
                    _ => (is, since),
                });
                // A read with no confirmation on it ends the confirmation's
                // mount: one that comes after settles afresh.
                if shown != Some(ResetShown::Confirm) {
                    owed.seen = None;
                }
            }
            let flow = match shows {
                Shows::Pending(ResetPending::Checking, row) => {
                    if self.fx.since(since) < waits.check {
                        Flow::Again(waits.check.saturating_sub(self.fx.since(since)))
                    } else {
                        let guard = row_guard(&screen.rows[row]);
                        if self.reset_not_ready(seq, &guard, deadline, opts, review)? {
                            Flow::Leave
                        } else {
                            let what = aterm_phase::anchor("reset.checking");
                            let why = KeepWhy::CheckTimeout;
                            self.reset_escape(&screen, &guard, why, what, opts, review)?
                        }
                    }
                }
                Shows::Pending(ResetPending::Resetting, _) => {
                    // The yes was taken, whoever's key it was.
                    if let Some(owed) = self.reset_owed_mut() {
                        owed.entered = true;
                    }
                    if self.fx.since(since) >= waits.claim {
                        self.reset_end(seq, true, opts, review)?;
                        return Ok(true);
                    }
                    Flow::Again(waits.claim.saturating_sub(self.fx.since(since)))
                }
                Shows::Wall => {
                    if self.fx.since(since) >= waits.open {
                        if let Some(owed) = self.reset_owed_mut() {
                            owed.kept.get_or_insert(KeepWhy::NotOpened);
                        }
                        self.reset_end(seq, false, opts, review)?;
                        return Ok(true);
                    }
                    Flow::Again(waits.open.saturating_sub(self.fx.since(since)))
                }
                Shows::Unread => {
                    let bound = if entered { waits.claim } else { waits.open };
                    if self.fx.since(since) >= bound {
                        // Whatever stands there is no screen of the step's
                        // that this build reads: said once, never keyed —
                        // and not said of a step that ends untold.
                        if self.reset_untold(opts).is_none()
                            && let Some(host) = opts.idle_host.as_ref()
                        {
                            host.inform(RESET_UNREAD);
                        }
                        self.reset_end(seq, entered, opts, review)?;
                        return Ok(true);
                    }
                    Flow::Again(bound.saturating_sub(self.fx.since(since)))
                }
                Shows::Confirm(read, prompt) => self.reset_confirmation(
                    &screen,
                    prompt.as_deref(),
                    &read,
                    deadline,
                    opts,
                    review,
                )?,
                Shows::Other => return Ok(false),
                Shows::Gone => {
                    // The worker already at work is the turn the step's end
                    // leaves owed: paid, as at a busy read of the wait
                    // ([`Self::reset_at_work`]).
                    let reader = self.reader(&screen.rows);
                    let live = reader.phase(&screen.rows) == Phase::Busy
                        && reader.background_wait(&screen.rows).is_none();
                    self.reset_end(seq, false, opts, review)?;
                    if live && self.reset == ResetStep::Idle {
                        self.took(seq);
                    }
                    return Ok(true);
                }
            };
            screen = match flow {
                Flow::Leave => return Ok(true),
                Flow::Read(next) => next,
                Flow::Again(pause) => {
                    let left = deadline.saturating_duration_since(self.fx.mono());
                    let step = pause.min(waits.step).min(left);
                    // A redraw arrives in several writes: a screen that
                    // moved is read once it has held still a moment.
                    if matches!(self.wait(&["seq", &seq.to_string()], step)?, Wait::Latched) {
                        self.wait(&["idle", RESET_STILL_MS], waits.step)?;
                    }
                    self.screen()?
                }
            };
        }
    }

    /// Whether the step's next key must wait — `true`, and one wait on the
    /// row `guard` names leaving was made: a failed Escape's pause is not
    /// over; or, by a `status` read NOW, the worker is not reading its input
    /// or a hand is on the session within its grace (`HELD`, said once a
    /// hold). `false`: the key may go.
    fn reset_not_ready(
        &mut self,
        seq: u64,
        guard: &str,
        deadline: Instant,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let Some((tries, at, hold)) = self
            .reset_owed()
            .map(|owed| (owed.tries, owed.at, owed.hold))
        else {
            return Ok(true);
        };
        let now = self.fx.mono();
        let mut until = (tries > 0 && now < at).then_some(at);
        if until.is_none() {
            until = hold.filter(|until| *until > now);
        }
        if until.is_none() {
            self.program = self.foreground_program()?;
            if self.stall_in_hand(review) {
                let step = self.reset_wait(None, deadline);
                self.wait_row_gone(guard, seq, step)?;
                return Ok(true);
            }
            until = self.hands_off_until(opts);
            if let Some(owed) = self.reset_owed_mut() {
                owed.hold = until;
            }
            if let Some(until) = until {
                review.note(&self.held_line(seq, until, RESET_KEY_ACT));
            }
        }
        let Some(until) = until else {
            return Ok(false);
        };
        let step = self.reset_wait(Some(until), deadline);
        self.wait_row_gone(guard, seq, step)?;
        Ok(true)
    }

    /// C. THE CONFIRMATION on `screen` (`prompt`, read as `read`; no box
    /// where only its title reads), of a step that is owed. In order, each a
    /// reason to write nothing yet:
    ///
    /// 1. it has stood the setup's box settle since this loop first read it
    ///    — else the rest is waited and it is read again; a step taken up
    ///    (no go) has stood one person's grace from there;
    /// 2. an Enter already written is given [`ResetWaits::enter`];
    /// 3. a failed Escape's pause is over, the worker reads its input and
    ///    nobody's hand is on the session, by a fresh `status`
    ///    ([`Self::reset_not_ready`]);
    /// 4. the verdict ([`Self::reset_verdict`]); a host that cannot say yet
    ///    whether the claim stands is asked again in a moment,
    ///    [`RESET_AGAIN_MAX`] times, and then the reset is kept.
    ///
    /// ENTER: `key if-human=<n> if-gen=<gen> if=<the Yes row as drawn>
    /// enter` ([`Session::fenced_key`]). Written, or cut off before its
    /// answer came: the ledger's `approved` row with the head as read and
    /// the journal's `confirmed` line, once a step
    /// ([`Self::reset_yes_written`]). Skipped for a person's key: nothing,
    /// the step stays owed. Skipped for a moved screen: settled, read and
    /// judged again — [`RESET_SKIPS`] times in a row, and then the
    /// confirmation is Escaped (`inexact-moving`). Turned away: parked or
    /// backed off.
    ///
    /// ANYTHING ELSE: the guarded Escape on the title row
    /// ([`Self::reset_escape`]) — on the Yes row as drawn where no title row
    /// is on the screen, else on the box's first row.
    fn reset_confirmation(
        &mut self,
        screen: &Screen,
        prompt: Option<&aterm_phase::PromptV2>,
        read: &ConfirmRead,
        deadline: Instant,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<Flow, Fail> {
        let waits = self.setup.reset_waits;
        let (seq, rows) = (screen.seq, &screen.rows);
        let title = aterm_phase::anchor("reset.confirm.title");
        let bound_to = rows.iter().position(|row| row.trim() == title).or_else(|| {
            let prompt = prompt?;
            let yes = prompt
                .options
                .iter()
                .find(|o| aterm_phase::spends_reset(&o.label));
            Some(yes.map_or(prompt.span.0, |yes| yes.row))
        });
        let guard = bound_to
            .and_then(|row| rows.get(row))
            .map_or_else(String::new, |r| row_guard(r));
        let grace = Duration::from_secs(u64::from(opts.policy.human_grace_s));
        // The door's clocks, cloned: the step owed is borrowed from `self`.
        let fx = self.fx.clone();
        let Some(owed) = self.reset_owed_mut() else {
            return Ok(Flow::Leave);
        };
        let seen = *owed.seen.get_or_insert_with(|| fx.mono());
        let (own, entered, kept, keyed) = (owed.go.is_some(), owed.entered, owed.kept, owed.keyed);
        let young = self.setup.box_settle.saturating_sub(self.fx.since(seen));
        if !young.is_zero() {
            return Ok(Flow::Again(young));
        }
        if !own && self.fx.since(seen) < grace {
            let step = self.reset_wait(seen.checked_add(grace), deadline);
            self.wait_row_gone(&guard, seq, step)?;
            return Ok(Flow::Leave);
        }
        if entered && kept.is_none() && self.fx.since(keyed) < waits.enter {
            return Ok(Flow::Again(
                waits.enter.saturating_sub(self.fx.since(keyed)),
            ));
        }
        if self.reset_not_ready(seq, &guard, deadline, opts, review)? {
            return Ok(Flow::Leave);
        }
        let placed = match read {
            ConfirmRead::Exact(confirm) => self.reset_place(&confirm.deadline),
            _ => None,
        };
        if let (ConfirmRead::Exact(confirm), Some(owed)) = (read, self.reset_owed_mut()) {
            owed.offer = Some((confirm.left, placed));
        }
        let why = match (self.reset_verdict(read, placed, opts), read) {
            (Judged::Keep(why), _) => why,
            (Judged::Again, _) => {
                let asked = self.reset_owed_mut().map_or(u8::MAX, |owed| {
                    owed.agains = owed.agains.saturating_add(1);
                    owed.agains
                });
                if asked <= RESET_AGAIN_MAX {
                    if asked == 1 {
                        review.note(&format!(
                            "WAITING seq={seq} rule={RULE_LIMIT_RESET} the host cannot say yet: \
                             asked again in {} ms",
                            waits.again.as_millis()
                        ));
                    }
                    self.wait(&["seq", &seq.to_string()], waits.again)?;
                    return Ok(Flow::Again(Duration::ZERO));
                }
                KeepWhy::Claim
            }
            (Judged::Enter, ConfirmRead::Exact(confirm)) => {
                let (Some(generation), Some(human), Some(yes)) = (
                    screen.generation.as_deref(),
                    screen.human_seq,
                    rows.get(confirm.yes_row),
                ) else {
                    // A read that names no fence is never entered on.
                    let why = KeepWhy::Inexact("fence");
                    return self.reset_escape(screen, &guard, why, title, opts, review);
                };
                let person = format!("if-human={human}");
                let (r, args) =
                    self.fenced_key(&row_guard(yes), "enter", generation, Some(&person))?;
                return match self.refused_press(&r, &args)? {
                    None if !r.skipped() => {
                        self.reset_yes_written(&confirm.head, seq, review);
                        Ok(Flow::Again(waits.enter))
                    }
                    // A person keyed since the read: theirs now, until their
                    // grace has passed.
                    None if r.skipped_person() => Ok(Flow::Leave),
                    // The screen moved under the read: once it holds still,
                    // read and judged again — and a screen that never holds
                    // still is not keyed at for the claim's whole life.
                    None => {
                        let skips = self.reset_owed_mut().map_or(u32::MAX, |owed| {
                            owed.skips = owed.skips.saturating_add(1);
                            owed.skips
                        });
                        if skips > RESET_SKIPS {
                            review.note(&format!(
                                "WAITING seq={seq} rule={RULE_LIMIT_RESET} the confirmation \
                                 moved under {RESET_SKIPS} reads: kept"
                            ));
                            let why = KeepWhy::Inexact("moving");
                            return self.reset_escape(screen, &guard, why, title, opts, review);
                        }
                        self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
                        Ok(Flow::Again(Duration::ZERO))
                    }
                    Some(Pressing::Lost { why, .. }) => {
                        // Cut mid-exchange, the Enter may have been written:
                        // the books say so before the outage is ridden out.
                        if !r.untouched() {
                            self.reset_yes_written(&confirm.head, seq, review);
                        }
                        Err(Fail::Lost(why))
                    }
                    Some(Pressing::Halted { why }) => {
                        self.park_on_hold(&why, seq, review)?;
                        Ok(Flow::Leave)
                    }
                    Some(Pressing::Refused { why }) => {
                        self.back_off(&why, seq, None, review)?;
                        Ok(Flow::Leave)
                    }
                    // Refused outright: this host has no fenced key after
                    // all, and nothing was written.
                    Some(_) => {
                        let why = KeepWhy::Inexact("fence");
                        self.reset_escape(screen, &guard, why, title, opts, review)
                    }
                };
            }
            // The verdict enters only what read exactly.
            (Judged::Enter, _) => KeepWhy::Closed,
        };
        self.reset_escape(screen, &guard, why, title, opts, review)
    }

    /// AN ENTER WAS WRITTEN ON THE CONFIRMATION, or may have been (its
    /// answer never came): counted, the wait for what it opens runs from
    /// now, and — once a step, with the first — the session ledger's
    /// `approved` row on the vendor's yes (`head`: the body and detail as
    /// read) and the journal's `confirmed` line. A loop that follows reads
    /// the row as an Enter written, and the host is told so.
    fn reset_yes_written(&mut self, head: &str, seq: u64, review: &mut dyn Review) {
        let fx = self.fx.clone();
        let Some(owed) = self.reset_owed_mut() else {
            return;
        };
        owed.entered = true;
        owed.enters = owed.enters.saturating_add(1);
        owed.skips = 0;
        owed.keyed = fx.mono();
        if std::mem::replace(&mut owed.yes_said, true) {
            return;
        }
        let yes = aterm_phase::anchor("reset.yes");
        let approved = approvals::Outcome::Approved;
        self.ledger_row(RULE_LIMIT_RESET, approved, yes, head, seq);
        review.note(&format!(
            "RESET seq={seq} rule={RULE_LIMIT_RESET} confirmed ({})",
            clip(head)
        ));
    }

    /// THE VERDICT ON THE CONFIRMATION AS `read`, its deadline placed at
    /// `placed`: Enter, the reason it is kept, or to ask again. In order:
    ///
    /// * an Escape was written for it already — that reason, for good: no
    ///   Enter follows an Escape;
    /// * a step with no go (taken up from the ledger, or opened by a press
    ///   whose answer never came) — [`KeepWhy::Successor`]: a loop that did
    ///   not see the row taken holds nothing to hold the confirmation to;
    /// * [`RESET_ENTERS`] Enters were written and it still stands —
    ///   [`KeepWhy::EnterNotTaken`];
    /// * what [`confirm_verdict`] says of the read against the row taken;
    /// * and, only where that is Enter, THE CLAIM: asked for no longer ago
    ///   than [`ResetWaits::enter_by`], and still this session's by the
    ///   host's word, asked again now for the same wall
    ///   ([`IdleHost::reset_take`]) — else [`KeepWhy::Claim`]; a host that
    ///   cannot say yet ([`ResetWord::Again`]) is [`Judged::Again`];
    /// * and THE WALL AS THE HOST READS IT NOW: the confirmation is judged
    ///   once more against the go just given, so a wall that stood more than
    ///   a day when the row was taken and no longer does is held to the
    ///   deadline's rule like any short one.
    fn reset_verdict(
        &mut self,
        read: &ConfirmRead,
        placed: Option<i64>,
        opts: &SuperviseOpts,
    ) -> Judged {
        let Some(owed) = self.reset_owed() else {
            return Judged::Keep(KeepWhy::Closed);
        };
        if let Some(kept) = owed.kept {
            return Judged::Keep(kept);
        }
        let Some(go) = owed.go.clone() else {
            return Judged::Keep(KeepWhy::Successor);
        };
        if owed.enters >= RESET_ENTERS {
            return Judged::Keep(KeepWhy::EnterNotTaken);
        }
        let in_time = self.fx.since(owed.since) <= self.setup.reset_waits.enter_by;
        if let ResetVerdict::Keep(why) = confirm_verdict(read, &go, placed, self.now_unix()) {
            return Judged::Keep(why);
        }
        let ask = ResetAsk {
            left: go.left,
            day_start: self.reset_day_start(&go.day),
            day: go.day.clone(),
        };
        let word = match opts.idle_host.as_ref() {
            Some(host) if in_time => host.reset_take(&ask),
            _ => return Judged::Keep(KeepWhy::Claim),
        };
        match word {
            ResetWord::Take(fresh) if fresh.until == go.until => {
                match confirm_verdict(read, &fresh, placed, self.now_unix()) {
                    ResetVerdict::Enter => Judged::Enter,
                    ResetVerdict::Keep(why) => Judged::Keep(why),
                }
            }
            ResetWord::Again => Judged::Again,
            ResetWord::Take(_) | ResetWord::No(_) => Judged::Keep(KeepWhy::Claim),
        }
    }

    /// Whether `screen` still shows a dialog an Escape of the step's is
    /// for: the confirmation, or `Checking…`.
    fn reset_stands(&self, screen: &Screen) -> bool {
        let before = self.reset_owed().and_then(|owed| owed.shown);
        matches!(
            self.reset_shows(screen, before.map(|(shown, _)| shown)),
            Shows::Confirm(..) | Shows::Pending(ResetPending::Checking, _)
        )
    }

    /// ONE GUARDED ESCAPE of the step's, and what came of it: `key
    /// if-human=<the read's person count> if=<guard> esc`
    /// ([`Session::guarded_key`]; no `if-gen=`, as the probe's), then the
    /// guarded row's leaving waited for ([`ResetWaits::close`]) and the
    /// screen read again. `why` is the reason the reset is kept, written with
    /// the first Escape as the ledger's `declined` row on `what` — and kept
    /// from then on.
    ///
    /// * The dialog gone: the reads go on from that read.
    /// * Still up — the Escape did not take, no row matched under a dialog
    ///   that stands, or nothing could be pressed (a read with no person
    ///   count: never an unfenced Escape): a failed try. The next is a pause of the survey's ladder away
    ///   ([`SURVEY_RETRY`]: 5 s first, far outside the vendor's 250 ms
    ///   re-arm, the last pause for ever), `WAITING` journaled, and at the
    ///   [`RESET_STUCK_AT`]-th the host is told once ([`IdleHost::inform`])
    ///   while the tries go on. Never an Enter.
    /// * Skipped for a person's key: no try spent. Turned away: parked or
    ///   backed off, no try spent.
    ///
    /// The caller has checked what the server cannot: the settle, the
    /// ladder's pause, the worker's input and a person's grace.
    fn reset_escape(
        &mut self,
        screen: &Screen,
        guard: &str,
        why: KeepWhy,
        what: &str,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<Flow, Fail> {
        let seq = screen.seq;
        if !guard.is_empty() && screen.human_seq.is_some() {
            match self.guarded_key(seq, guard, "esc", screen.human_seq)? {
                Dismiss::Pressed { .. } => {
                    let fx = self.fx.clone();
                    let first = self.reset_owed_mut().and_then(|owed| {
                        owed.keyed = fx.mono();
                        owed.kept.is_none().then(|| *owed.kept.insert(why))
                    });
                    if let Some(kept) = first {
                        let declined = approvals::Outcome::Declined;
                        self.ledger_row(RULE_LIMIT_RESET, declined, what, &kept.to_string(), seq);
                    }
                    self.wait_row_gone(guard, seq, self.setup.reset_waits.close)?;
                }
                Dismiss::Yielded => return Ok(Flow::Leave),
                Dismiss::Refused { halted, why } => {
                    self.refused(halted, &why, seq, review)?;
                    return Ok(Flow::Leave);
                }
                // Judged by the read below: the dialog left first, or it
                // stands and nothing was pressed.
                Dismiss::Skipped => {}
            }
        }
        let after = self.screen()?;
        if !self.reset_stands(&after) {
            return Ok(Flow::Read(after));
        }
        let tries = self
            .reset_owed()
            .map_or(1, |owed| owed.tries.saturating_add(1));
        let pause = crate::supervise::ladder::Ladder(&self.setup.survey_retry)
            .step(usize::try_from(tries - 1).unwrap_or(usize::MAX));
        let mut stuck = false;
        let fx = self.fx.clone();
        if let Some(owed) = self.reset_owed_mut() {
            owed.tries = tries;
            owed.at = fx.mono() + pause;
            stuck = tries >= RESET_STUCK_AT && !owed.told;
            owed.told |= stuck;
        }
        review.note(&format!(
            "WAITING seq={seq} rule={RULE_LIMIT_RESET} the dialog is still up ({tries} tries): \
             Escape again in {} s",
            pause.as_secs()
        ));
        if stuck && let Some(host) = opts.idle_host.as_ref() {
            host.inform(RESET_STUCK);
        }
        Ok(Flow::Leave)
    }

    /// D. THE STEP'S END: the dialog is gone (or `last`: its bound ran out),
    /// and the host reads what the vendor answered
    /// ([`IdleHost::reset_told`]: the transcript's one line, never the
    /// screen). [`ResetOutcome::Pending`] is asked again — a pause of
    /// [`ResetWaits::step`] between, the server's own wait — until
    /// [`ResetWaits::line`] has passed, then once more with `last`. Any
    /// other answer ends the step:
    ///
    /// * the ledger's `waiting` row (`ended: <the outcome's word>`), which
    ///   is what a later loop reads the step closed by;
    /// * the turn-end policy told what it owes ([`TurnEndState::reset_told`]:
    ///   one continuation, at once or after the vendor's own);
    /// * the journal's `RESET seq=<n> rule=limit-reset@v1 used (<n> left)`,
    ///   SAID — and `lifted`, `kept (<why>)`, `failed (<why>)`, `unread`,
    ///   journal only.
    ///
    /// A step that ends UNTOLD ([`Self::reset_untold`]: `[harness]
    /// limit_reset = false`, or a stale open row) tells the host and the
    /// turn-end policy nothing: the `waiting` row (`ended: untold (key off)`,
    /// `ended: stale`) and one journal note.
    ///
    /// No act of the turn-end policy is booked, and nothing is typed here.
    /// A stop while the host is asked leaves the step owed: the loop that
    /// follows reads the ledger.
    pub(super) fn reset_end(
        &mut self,
        seq: u64,
        last: bool,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let Some((entered, kept, offer)) = self
            .reset_owed()
            .map(|owed| (owed.entered, owed.kept, owed.offer))
        else {
            return Ok(());
        };
        // UNTOLD: the host records nothing of a reset with its key off, or
        // of one whose claim lapsed long ago, and an outcome told to the
        // turn-end policy would owe the session a continuation typed for the
        // reset — against the owner's switch, or for a line that may be a
        // later dialog's. The ledger's `waiting` row closes it.
        if let Some((ended, why)) = self.reset_untold(opts) {
            let waiting = approvals::Outcome::Waiting;
            self.ledger_row(RULE_LIMIT_RESET, waiting, RESET_SUBJECT, ended, seq);
            self.reset = ResetStep::Idle;
            self.reset_taken = None;
            review.note(&format!(
                "RESET seq={seq} rule={RULE_LIMIT_RESET} ended untold ({why}{})",
                if entered {
                    ", its Enter was written"
                } else {
                    ""
                }
            ));
            return Ok(());
        }
        let waits = self.setup.reset_waits;
        let line_by = self.fx.mono() + waits.line;
        let mut last = last;
        let outcome = loop {
            let end = ResetEnd {
                entered,
                kept,
                offer,
                last,
            };
            let outcome = opts
                .idle_host
                .as_ref()
                .map_or(ResetOutcome::Unread, |host| host.reset_told(&end));
            match outcome {
                ResetOutcome::Pending if last => break ResetOutcome::Unread,
                ResetOutcome::Pending => {}
                outcome => break outcome,
            }
            if self.stopped() {
                return Ok(());
            }
            let left = line_by.saturating_duration_since(self.fx.mono());
            if left.is_zero() {
                last = true;
                continue;
            }
            let pause = left.min(waits.step);
            let ms = pause.as_millis().to_string();
            self.wait(&["idle", &ms], pause)?;
        };
        let waiting = approvals::Outcome::Waiting;
        let ended = format!("ended: {}", outcome.word());
        self.ledger_row(RULE_LIMIT_RESET, waiting, RESET_SUBJECT, &ended, seq);
        self.turn_end.reset_told(&outcome, self.fx.mono());
        self.reset = ResetStep::Idle;
        self.reset_taken = None;
        let line = format!(
            "RESET seq={seq} rule={RULE_LIMIT_RESET} {}",
            outcome_words(&outcome, kept)
        );
        if matches!(outcome, ResetOutcome::Spent { .. }) {
            review.say(&line)?;
        } else {
            review.note(&line);
        }
        Ok(())
    }

    /// THE WORKER WORKS while a step is owed (a busy read of the wait,
    /// [`Session::await_turn_from`]): its dialog is gone — the vendor's own
    /// continuation after a reset, a person's turn — and the step ends here
    /// ([`Self::reset_end`]), since the turn may run for hours and the look
    /// that would tell the host comes only after it. Nothing while none is
    /// owed, or while a pending line still stands under the busy row.
    ///
    /// `live`: the read shows the agent's LIVE TURN, not a break of its own
    /// background work. That turn is the one the step's end leaves owed —
    /// it is running — so the continuation the policy was just told of is
    /// paid here ([`TurnEndState::took`]): told at this read, it was not
    /// there for the read's own `took`, and a turn that ends before the
    /// next busy read would be followed by a second, typed one.
    pub(super) fn reset_at_work(
        &mut self,
        screen: &Screen,
        live: bool,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        if self.reset == ResetStep::Idle || aterm_phase::reset_pending(&screen.rows).is_some() {
            return Ok(());
        }
        self.reset_end(screen.seq, false, opts, review)?;
        if live && self.reset == ResetStep::Idle {
            self.took(screen.seq);
        }
        Ok(())
    }
}
