// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Limit-reset state machine for the supervisor loop ([`Session::reset_take`],
//! [`IdleHost::reset_take`], [`Session::reset_run`], [`Session::reset_confirmation`],
//! [`confirm_verdict`], [`Session::reset_end`], [`IdleHost::reset_told`],
//! [`TurnEndState::reset_told`]).

use super::super::policy::approval::{RULE_LIMIT_RESET, kept_for_good};
use super::super::policy::reset::{Verdict as ResetVerdict, confirm_verdict};
use super::super::policy::row_guard;
use super::*;
use crate::harness::accounts::RESET_CLAIM_S;
use aterm_phase::{ConfirmRead, ResetPending};

/// How many times in a row the host may answer [`ResetWord::Again`].
pub(super) const RESET_AGAIN_MAX: u8 = 5;

/// How many Enters the confirmation gets.
pub(super) const RESET_ENTERS: u32 = 3;

/// How many Enters in a row the server may skip for a screen that moved under the read.
pub(super) const RESET_SKIPS: u32 = 5;

/// How old the session ledger's open row may be for a later loop to tell the end of its step.
pub(super) const RESET_STALE: Duration = Duration::from_secs(RESET_CLAIM_S.unsigned_abs() + 300);

/// From which failed Escape on the host is told the dialog is stuck ([`IdleHost::inform`], once).
pub(super) const RESET_STUCK_AT: u32 = 2;

const RESET_UNTITLED: &str = "the reset confirmation";

const RESET_STUCK: &str = "The limit-reset confirmation did not close\nPress Esc in this tab.";

const RESET_UNREAD: &str = "A limit-reset dialog could not be read\nLook at this tab.";

const RESET_STILL_MS: &str = "50";

const RESET_KEY_ACT: &str = "the reset confirmation's key";

const RESET_SUBJECT: &str = "limit reset";

const RESET_UNTOLD: &str = "ended: untold (key off)";

const RESET_STALE_END: &str = "ended: stale";

/// The bounds of one reset step ([`RESET_WAITS`]; a test shortens them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResetWaits {
    /// From the row's Enter to a pending line, a confirmation or the dialog's end.
    pub(super) open: Duration,
    /// How long `Checking for a reset…` may stand (the vendor makes one usage read under it).
    pub(super) check: Duration,
    /// How long `Resetting your limits…` is waited for.
    pub(super) claim: Duration,
    /// How long the host is asked again for the answer line.
    pub(super) line: Duration,
    /// The pause before the host is asked again where it could not say.
    pub(super) again: Duration,
    /// How long a written Enter is given before a confirmation still up is judged again.
    pub(super) enter: Duration,
    /// The latest an Enter may be written, from when the claim was asked.
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
    /// The reset is passed over for this mount.
    Pass,
}

enum Shows {
    /// A pending line under its dialog's title, and its row.
    Pending(ResetPending, usize),
    /// The usage-limit dialog.
    Wall,
    /// The confirmation, as it read, and its box.
    Confirm(ConfirmRead, Option<Box<aterm_phase::PromptV2>>),
    /// Another dialog box.
    Other,
    /// A composer frame, or the worker at work.
    Gone,
    /// None of them, or a box whose head and Esc are both unread.
    Unread,
}

/// What a part of the step asks of the reads that drive it.
#[allow(
    clippy::large_enum_variant,
    reason = "one Flow lives on the stack per read of the step and is matched at once; \
              boxing the screen would put a heap allocation on every read to save nothing"
)]
enum Flow {
    /// Read again, after the screen's next move or this long at most.
    Again(Duration),
    /// Go on from this read, already made.
    Read(Screen),
    /// Nothing more at this look.
    Leave,
}

enum Judged {
    /// Enter, on the yes row as read.
    Enter,
    /// Press Escape and end the step.
    Keep(KeepWhy),
    /// The host cannot say yet whether the claim still stands.
    Again,
}

fn named_year(when: &str) -> Option<i64> {
    when.split(|c: char| c.is_whitespace() || c == ',')
        .filter(|word| !word.is_empty())
        .nth(2)
        .filter(|word| word.len() == 4 && word.bytes().all(|b| b.is_ascii_digit()))?
        .parse()
        .ok()
}

fn outcome_words(outcome: &ResetOutcome, kept: Option<KeepWhy>) -> String {
    match outcome {
        ResetOutcome::Spent {
            kind,
            left: Some(n),
            ..
        } => format!("used ({} left)", kind.resets(*n)),
        ResetOutcome::Spent { kind, .. } => format!("used ({})", kind.name()),
        ResetOutcome::Walled { kind, left } => {
            format!("used ({} left), the wall still stands", kind.resets(*left))
        }
        ResetOutcome::Kept => format!("kept ({})", kept.unwrap_or(KeepWhy::Closed)),
        ResetOutcome::Failed { login } => {
            format!("failed ({})", if *login { "login" } else { "answer" })
        }
        other => other.word().to_string(),
    }
}

impl<C: Ctl> Session<'_, C> {
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

    /// The row's Enter was written.
    pub(super) fn reset_row_taken(&mut self) {
        let (go, since) = match self.reset_taken.take() {
            Some((go, since)) => (Some(go), since),
            None => (None, self.fx.mono()),
        };
        self.reset_step_owed(go, since, false);
    }

    /// The row's Enter got no answer (`subject`, read at `seq`).
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

    /// A step an earlier loop opened ([`approvals::reset_owed`]: its ledger row, and whether the
    /// confirmation's Enter was written since) is this loop's to end: Escape-only, as every step
    /// with no go.
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

    /// Whether the reset was passed over for the dialog mount now up ([`Session::reset_passed`]).
    pub(super) fn reset_passed_here(&self) -> bool {
        self.reset_passed.is_some()
            && self.reset_passed.as_deref() == self.box_first_seen.as_ref().map(|(k, _)| k.as_str())
    }

    /// Mark the reset passed over for the current dialog mount.
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

    /// A claim the host gave with no row taken on it.
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

    /// A day and hour of the vendor's (`Oct 5 at 3pm`, `Jan 2, 2027 at 9:30am`) as unix seconds.
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
        let about = named_year(when).map_or(now, |year| {
            limit::days_from_civil(year, *month, *day) * 86_400 + 43_200
        });
        let at = limit::reset_at_with(&spec, about, local, &self.zone_lookup());
        Some(match (zone, self.local_offset_then(at)) {
            (None, Some(there)) => at + local - there,
            _ => at,
        })
    }

    fn reset_day_start(&mut self, day: &str) -> Option<i64> {
        self.reset_place(&format!("{day} at 12am"))
    }

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
        if !opts.policy.resume_limits {
            return Ok(Some("resume-limits"));
        }
        let screen = &turn.screen;
        if screen.generation.is_none() || screen.human_seq.is_none() || !self.gen_fence_known()? {
            return Ok(Some("no-fence"));
        }
        Ok(None)
    }

    /// Check whether the limit-reset grant row on `turn` may be taken.
    pub(super) fn reset_take(
        &mut self,
        turn: &Turn,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<ResetAsked, Fail> {
        let seq = turn.screen.seq;
        let shut = self.reset_gate(turn, opts, review)?;
        self.reset_kinds.clone_from(&opts.policy.limit_reset_kinds);
        let row = aterm_phase::parse_prompt_v2(&turn.screen.rows)
            .and_then(|p| aterm_phase::reset_row(&p, &self.reset_kinds));
        let (None, Some(row), Some(host)) = (shut, row, opts.idle_host.clone()) else {
            let gate = shut.unwrap_or("row");
            self.reset_pass(seq, &format!("gate={gate}"), opts, review);
            return Ok(ResetAsked::Pass);
        };
        let ask = ResetAsk {
            kind: row.kind,
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
            None if self.reset_owed().is_some_and(|owed| !owed.entered)
                && rows
                    .iter()
                    .any(|row| aterm_phase::reset_title(row).is_some()) =>
            {
                Shows::Confirm(ConfirmRead::Inexact("options"), None)
            }
            None => Shows::Unread,
        }
    }

    fn reset_wait(&self, until: Option<Instant>, deadline: Instant) -> Duration {
        let now = self.fx.mono();
        until
            .map_or(WAIT_STEP, |t| t.saturating_duration_since(now))
            .min(WAIT_STEP)
            .min(deadline.saturating_duration_since(now))
    }

    /// Step an owed limit reset at the top of [`Session::look`].
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
                    .and_then(|p| aterm_phase::reset_row(&p, &opts.policy.limit_reset_kinds))
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

    /// Drive post-row reset reads until the dialog completes.
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
            let since = match (before, shown) {
                (Some((was, at)), Some(is)) if was == is => at.max(keyed),
                _ => self.fx.mono(),
            };
            if let Some(owed) = self.reset_owed_mut() {
                owed.shown = shown.map(|is| match before {
                    Some((was, at)) if was == is => (was, at),
                    _ => (is, since),
                });
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
                        // No screen of the step's that this build reads: said once, never
                        // keyed, and not said of a step that ends untold.
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
                    let reader = self.reader(&screen.rows);
                    let live = reader.phase(&screen.rows) == Phase::Busy
                        && reader.background_wait(&screen.rows).is_none();
                    self.reset_end(seq, false, opts, review)?;
                    if live && self.reset == ResetStep::Idle {
                        self.took(seq, Some(&opts.policy), review);
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
                    if matches!(self.wait(&["seq", &seq.to_string()], step)?, Wait::Latched) {
                        self.wait(&["idle", RESET_STILL_MS], waits.step)?;
                    }
                    self.screen()?
                }
            };
        }
    }

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
        let titled = rows
            .iter()
            .position(|row| aterm_phase::reset_title(row).is_some());
        let title: String = titled
            .map(|row| rows[row].trim().to_string())
            .or_else(|| {
                let owed = self.reset_owed()?;
                owed.go.as_ref().map(|go| go.kind.title())
            })
            .unwrap_or_else(|| RESET_UNTITLED.to_string());
        let title = title.as_str();
        let bound_to = titled.or_else(|| {
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
            owed.offer = Some(ResetOffer {
                kind: confirm.kind,
                left: confirm.left,
                use_by: placed,
            });
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
                    let why = KeepWhy::Inexact("fence");
                    return self.reset_escape(screen, &guard, why, title, opts, review);
                };
                let person = format!("if-human={human}");
                let (r, args) =
                    self.fenced_key(&row_guard(yes), "enter", generation, Some(&person))?;
                return match self.refused_press(&r, &args)? {
                    None if !r.skipped() => {
                        self.reset_yes_written(confirm.kind, &confirm.head, seq, review);
                        Ok(Flow::Again(waits.enter))
                    }
                    None if r.skipped_person() => Ok(Flow::Leave),
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
                            self.reset_yes_written(confirm.kind, &confirm.head, seq, review);
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
                    Some(_) => {
                        let why = KeepWhy::Inexact("fence");
                        self.reset_escape(screen, &guard, why, title, opts, review)
                    }
                };
            }
            (Judged::Enter, _) => KeepWhy::Closed,
        };
        self.reset_escape(screen, &guard, why, title, opts, review)
    }

    fn reset_yes_written(
        &mut self,
        kind: aterm_phase::ResetKind,
        head: &str,
        seq: u64,
        review: &mut dyn Review,
    ) {
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
        let yes = kind.yes();
        let approved = approvals::Outcome::Approved;
        self.ledger_row(RULE_LIMIT_RESET, approved, &yes, head, seq);
        review.note(&format!(
            "RESET seq={seq} rule={RULE_LIMIT_RESET} confirmed ({})",
            clip(head)
        ));
    }

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
            kind: go.kind,
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

    fn reset_stands(&self, screen: &Screen) -> bool {
        let before = self.reset_owed().and_then(|owed| owed.shown);
        matches!(
            self.reset_shows(screen, before.map(|(shown, _)| shown)),
            Shows::Confirm(..) | Shows::Pending(ResetPending::Checking, _)
        )
    }

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

    /// Complete the reset step once the dialog closes or times out.
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
        if matches!(
            outcome,
            ResetOutcome::Spent { .. } | ResetOutcome::Walled { .. }
        ) {
            review.say(&line)?;
        } else {
            review.note(&line);
        }
        Ok(())
    }

    /// End an owed reset step when the worker is observed busy during `await_turn_from`.
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
            self.took(screen.seq, Some(&opts.policy), review);
        }
        Ok(())
    }
}
