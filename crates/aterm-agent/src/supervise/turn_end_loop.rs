// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The turn-end policy in the loop: every point where a worker's turn ended
//! is decided by [`decide_turn_end`] — the ONE turn-end decider — and the
//! loop only carries out what it says.
//!
//! What the loop adds around the pure decision is what the decision cannot
//! know: the reading of the screen by the reader for the session's program
//! (`aterm-phase`), the work the turn did (the first busy read since the last
//! point, [`Session::busy_since`]), a wall's reset on the loop's clock (read
//! once per notice text, so a span counts from its print), and the standing
//! rules (the `rules_file`, or `--resume`'s, cut at [`RULES_CAP`]). Nothing
//! it types goes without:
//!
//! * the session's foreground program read FRESH (`status program=`) and its
//!   reader Claude Code's — the only grammar this policy speaks;
//! * THE FENCED WRITE ([`Session::type_fenced`]), where the host fences
//!   `send` on the screen generation: `send if-gen=<the judged read> if=<its
//!   composer row> -- <text>` writes the text only while the screen is still
//!   the one judged — a person's first keystroke since, a box, anything that
//!   moved it, and nothing is written (`OK skipped reason=changed`; the
//!   point is decided again shortly; a guard that matches no row of the
//!   UNCHANGED screen, a bare `OK skipped`, is escalated with what was not
//!   typed, since a retry would miss the same way) — then a read must show
//!   the composer
//!   holding exactly that text (however it wrapped) before Enter goes,
//!   fenced on THAT read (`key if-gen=… if=<the caret row> enter`). Text we
//!   wrote and did not submit is escalated, never left silently;
//! * elsewhere, the guarded submit (`turn submit=guarded:<re> yield=0.2`,
//!   lane C): parked while a person types (`yield=`), then pasted, and Enter
//!   pressed only while the row holding the CURSOR still ends as the text
//!   does ([`composer_guard`]) — the PASTE is not fenced there, only checked
//!   against the read that judged the point, so a person who starts typing
//!   in between can get it spliced into a draft; a miss (`skipped`) leaves
//!   the text typed, and is escalated. A host without either gets nothing
//!   typed;
//! * for the worker's own suggestion, the vendor's accept key — `right` on
//!   an empty composer with a suggestion shown fills it (Claude Code 2.1.280,
//!   read from its binary: `right` and `tab` both `markAccepted` and fill the
//!   text; `right` is the one with no other use on an empty composer) —
//!   fenced on the judged read's generation and guarded on the suggestion's
//!   row, then Enter fenced on the read that shows it filled. A host without
//!   the generation fence gets the suggestion's words typed instead (the
//!   vendor counts a submitted text equal to its suggestion as accepted).
//!
//! A `WaitUntil` is the deadline of the loop's wait for the screen to move
//! ([`Session::wait_for_next`]): a change first decides again at the next
//! look, the deadline decides again on the point still showing
//! ([`Session::turn_end_now`]). Every act is a ledger row under its rule id
//! and a printed line — `CONTINUED seq=<n> rule=<id> <text>` for a
//! continuation or an accepted suggestion, `TYPED seq=<n> rule=<id>
//! <command>` for a slash command; a wait is journaled `WAITING seq=<n>
//! until=<UTC> <why>` once per deadline, and an act not taken `SKIPPED
//! seq=<n> rule=<id> <why>`.

use super::super::phase::{composer_draft, composer_index};
use super::super::policy::turn_end::{RULE_MODEL_FALLBACK, Then, decide_turn_end};
use super::*;

/// The longest text the cursor-row guard names whole, as `^❯ <text>`: it
/// sits on the caret row of any composer 43 columns wide or more.
const GUARD_TAIL_CHARS: usize = 40;

/// The most of the text's last word a longer text's guard names: the tail
/// of a word too long for the composer's width is what its last row holds.
const GUARD_WORD_CHARS: usize = 16;

/// `yield=<floor>` on every `turn` this policy types: the verb parks BEFORE
/// typing until the target's typing momentum has exhaled to the floor, so a
/// person typing is not pasted over (the floor the harness's own typed
/// turns use). A yield that outlives the turn's timeout types nothing
/// (`ERR yield timeout`).
const TURN_YIELD: &str = "yield=0.2";

/// The standing rules ride in the continuation cut to this many characters
/// (at a word, `…` after): a continuation stays a line a composer holds as
/// typed text, not a pasted-text placeholder the fence cannot read back.
pub(super) const RULES_CAP: usize = 400;

/// A fenced write the server skipped (the screen moved between the judged
/// read and the write — a person's keystroke, a box): the point is decided
/// again this far off, on the deadline of the loop's own wait.
const REDECIDE_AFTER_SKIP: Duration = Duration::from_secs(2);

/// The cursor-row guard for `text` typed into Claude Code's composer: `^❯
/// <text>` to the row's end when the text is short enough to sit on the
/// caret row ([`GUARD_TAIL_CHARS`]); else the end of its last WORD (at most
/// [`GUARD_WORD_CHARS`]) to the row's end. Claude Code wraps its composer
/// between words, and where the wrap falls depends on the width, so the row
/// the cursor ends on holds the last word — never a fixed count of
/// characters (lane B2's review: a 40-character tail missed whenever the
/// wrap put fewer on the last row). Whitespace-free
/// ([`super::super::policy::row_guard`]'s escaping).
pub(super) fn composer_guard(text: &str) -> String {
    if text.chars().count() <= GUARD_TAIL_CHARS {
        // The caret's space is whatever Claude Code draws there — a
        // NO-BREAK SPACE on 2.1.280 and 2.1.281 (`❯\u{a0}keep going`) —
        // so it is `\s`, never the `\x20` a typed space is.
        let guard = super::super::policy::row_guard(&format!("❯ {text}"));
        return guard.replacen("^❯\\x20", "^❯\\s", 1);
    }
    let word = text.split_whitespace().last().unwrap_or(text);
    let n = word.chars().count();
    let tail: String = word
        .chars()
        .skip(n.saturating_sub(GUARD_WORD_CHARS))
        .collect();
    let anchored = super::super::policy::row_guard(&tail);
    // `row_guard` anchors at the row's start: the tail ends the row.
    anchored.trim_start_matches('^').to_string()
}

/// `rules` as one line of at most [`RULES_CAP`] characters: cut at the last
/// word that fits, `…` after.
pub(super) fn capped_rules(line: &str) -> String {
    if line.chars().count() <= RULES_CAP {
        return line.to_string();
    }
    let head: String = line.chars().take(RULES_CAP).collect();
    let cut = head.rfind(' ').filter(|&i| i > 0).unwrap_or(head.len());
    format!("{}…", head[..cut].trim_end())
}

/// A model switch that will not be switched back, said to a person: the
/// owner's default model for NEW sessions is now the fallback, and stays so
/// until someone runs `/model` again (`None`: the switch back is known —
/// the bucket's model and its reset — and restores the default with it).
pub(super) fn default_moved(m: &ModelSwitch) -> Option<String> {
    match (&m.from, m.back_at) {
        (Some(_), Some(_)) => None,
        (from, _) => Some(format!(
            "/model {} is now the default for new sessions: not switched back ({})",
            m.to,
            if from.is_some() {
                "no reset named"
            } else {
                "no model /model knows named"
            }
        )),
    }
}

/// `text` with every whitespace character dropped: a composer's rows,
/// however the text wrapped (between words, or inside one too long for the
/// width), read back as the text that was written.
fn squashed(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

impl<C: Ctl> Session<'_, C> {
    /// `t` on the loop's clock as unix seconds.
    fn unix_of(&self, t: Instant) -> i64 {
        let now = Instant::now();
        let secs = |d: Duration| i64::try_from((d.as_millis() + 500) / 1000).unwrap_or(0);
        if t >= now {
            self.now_unix() + secs(t - now)
        } else {
            self.now_unix() - secs(now - t)
        }
    }

    /// `t` on the loop's clock as a UTC stamp, for the journal.
    fn stamp_in(&self, t: Instant) -> String {
        let ahead = (t.saturating_duration_since(Instant::now()).as_millis() + 500) / 1000;
        let secs = self.now_unix() + i64::try_from(ahead).unwrap_or(0);
        utc_stamp(u64::try_from(secs).unwrap_or(0))
    }

    /// The policy the decider runs under: the options' `policy`, a usage
    /// limit resumed exactly when this loop resumes — `--resume`, or the
    /// hosted loop's `resume`, which [`SuperviseOpts::hosted_with`] sets from
    /// `policy.resume_limits` — so the CLI's `watch` without the flag
    /// escalates it.
    fn turn_end_cfg(&self, opts: &SuperviseOpts) -> SupervisorConfig {
        SupervisorConfig {
            resume_limits: self.resume.is_some(),
            ..opts.policy.clone()
        }
    }

    /// The standing rules, read now (an edit since the launch counts), as
    /// one line: `policy.rules_file`, else `--resume`'s file. `None` when
    /// neither is set, or the file cannot be read or is empty (journaled).
    fn rules_text(&self, opts: &SuperviseOpts) -> Option<String> {
        let path = opts.policy.rules_file.clone().or_else(|| self.rules())?;
        let text = std::fs::read_to_string(&path).ok()?;
        let line = capped_rules(&limit::one_line(&text));
        (!line.is_empty()).then_some(line)
    }

    /// A wall's `reset=` text as the loop's clock, read once per text: the
    /// same text is the same reset ([`Self::refresh_reset`]'s rule). A reset
    /// already past is that long ago.
    fn wall_reset_at(&mut self, text: Option<&str>) -> Option<Instant> {
        let key = text.unwrap_or("-").to_string();
        if let Some((k, at)) = &self.wall_reset
            && *k == key
        {
            return *at;
        }
        let at = self.reset_unix(text).map(|unix| {
            let now = Instant::now();
            let delta = unix - self.now_unix();
            let d = Duration::from_secs(delta.unsigned_abs());
            if delta >= 0 {
                now + d
            } else {
                now.checked_sub(d).unwrap_or(now)
            }
        });
        self.wall_reset = Some((key, at));
        at
    }

    /// The turn-end reading of `screen`: the reader for the session's
    /// program (as last read), the work since the last point, the wall's
    /// reset, the rules.
    fn turn_end_reading(
        &mut self,
        screen: &Screen,
        worked: Option<Duration>,
        opts: &SuperviseOpts,
    ) -> TurnEndReading {
        let reading = aterm_phase::read(
            self.program.as_deref(),
            &screen.rows,
            Some(screen.cursor_col),
        );
        let reset_at = match &reading.wall {
            Some(w) => {
                let text = w.reset.clone();
                self.wall_reset_at(text.as_deref())
            }
            None => None,
        };
        TurnEndReading::of(
            &reading,
            &screen.rows,
            typed_draft(screen),
            worked,
            reset_at,
            self.rules_text(opts),
        )
    }

    /// The decision at a NEW point: the point folded into the policy's
    /// memory ([`TurnEndState::observe`]) — once, here — then
    /// [`decide_turn_end`]. A loop watching behind another supervisor's claim
    /// decides nothing: that one answers the session.
    pub(super) fn turn_end_at_point(
        &mut self,
        point: &Turn,
        worked: Option<Duration>,
        opts: &SuperviseOpts,
    ) -> TurnEndAction {
        self.turn_end_due = None;
        if self.claim.watching_behind().is_some() {
            return TurnEndAction::Nothing;
        }
        let r = self.turn_end_reading(&point.screen, worked, opts);
        let now = Instant::now();
        self.turn_end.observe(&r, now);
        decide_turn_end(&self.turn_end, &r, &self.turn_end_cfg(opts), now)
    }

    /// The policy's `WaitUntil` ran out on the point still showing: read the
    /// screen again, and when it still shows the SAME point (its review key)
    /// fold it in again with no work ([`TurnEndState::observe`]: the same
    /// point read again changes nothing, except an act whose point has
    /// shown past [`TurnEndTiming::take_within`] with no busy read, which is
    /// judged now) and decide again, and carry it out; an escalation is the
    /// point's ([`Self::escalate_point`]). A screen that shows another point
    /// is left to the next look.
    pub(super) fn turn_end_now(
        &mut self,
        seen: &Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        self.turn_end_due = None;
        if self.claim.watching_behind().is_some() {
            return Ok(());
        }
        let screen = self.screen()?;
        let turn = turn_of(screen);
        if review_key(&turn, allow) != review_key(seen, allow) {
            return Ok(());
        }
        let r = self.turn_end_reading(&turn.screen, None, opts);
        let now = Instant::now();
        self.turn_end.observe(&r, now);
        let action = decide_turn_end(&self.turn_end, &r, &self.turn_end_cfg(opts), now);
        if let TurnEndAction::Escalate { reason } = &action
            && matches!(turn.phase, Phase::Idle | Phase::Question)
        {
            self.escalate_point(&turn, allow, Some(reason), review)?;
        }
        self.turn_end_execute(&turn, action, opts, review)
    }

    /// Carry out what the policy decided at `point` (module header). An act
    /// the server took is recorded in the policy's memory
    /// ([`TurnEndState::acted`]); one it did not take is not.
    pub(super) fn turn_end_execute(
        &mut self,
        point: &Turn,
        action: TurnEndAction,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = point.screen.seq;
        let (text, rule) = match &action {
            TurnEndAction::Nothing | TurnEndAction::Escalate { .. } => return Ok(()),
            TurnEndAction::WaitUntil { until, why } => {
                let said = self.turn_end_due.as_ref().is_some_and(|(t, _)| t == until);
                if !said {
                    review.note(&format!(
                        "WAITING seq={seq} until={} {why}",
                        self.stamp_in(*until)
                    ));
                }
                self.turn_end_due = Some((*until, why.clone()));
                return Ok(());
            }
            TurnEndAction::Type { text, rule_id } | TurnEndAction::Accept { text, rule_id } => {
                (text.clone(), *rule_id)
            }
            TurnEndAction::TypeCommand {
                command, rule_id, ..
            } => (command.clone(), *rule_id),
        };
        if !self.speaks_claude(seq, rule, review)? {
            return Ok(());
        }
        let typed = match &action {
            TurnEndAction::Accept { .. } => self.accept_suggestion(point, &text, rule, review)?,
            _ => self.type_text(point, &text, rule, review)?,
        };
        if !typed {
            return Ok(());
        }
        let r = self.turn_end_reading(&point.screen, None, opts);
        self.turn_end.acted(&action, &r, Instant::now());
        self.mail_turn_boundary();
        let word = if matches!(action, TurnEndAction::TypeCommand { .. }) {
            "TYPED"
        } else {
            "CONTINUED"
        };
        // A model switch says whether, and when, it will be switched back —
        // and that `/model` also SAVED it as the default for new sessions
        // (Claude Code 2.1.280: `Set model to … and saved as your default for
        // new sessions`). A switch never switched back leaves that default
        // moved: escalated once, below.
        let switch = self
            .turn_end
            .model_switch()
            .filter(|_| word == "TYPED" && rule == RULE_MODEL_FALLBACK)
            .cloned();
        let back = match &switch {
            Some(m) => match (&m.from, m.back_at) {
                (Some(from), Some(at)) => format!(
                    " (from {from}, back at {}; the default for new sessions until then)",
                    self.stamp_in(at)
                ),
                (Some(from), None) => format!(
                    " (from {from}; the notice names no reset: not switched back; now the \
                     default for new sessions)"
                ),
                (None, _) => " (the notice names no model /model knows: not switched back; \
                               now the default for new sessions)"
                    .to_string(),
            },
            None => String::new(),
        };
        review.say(&format!(
            "{word} seq={seq} rule={rule} {}{back}",
            clip(&text)
        ))?;
        // A switch's row carries how to undo it, for a loop that starts
        // before its reset ([`approvals::open_model_switch`]).
        let reason = match &switch {
            Some(m) => approvals::model_switch_reason(
                m.from.as_deref(),
                m.back_at.map(|at| self.unix_of(at)),
            ),
            None => "the turn-end policy".to_string(),
        };
        self.ledger_row(rule, approvals::Outcome::Typed, &text, &reason, seq);
        if let Some(reason) = switch.as_ref().and_then(default_moved) {
            let text = escalate::attention_text(point, &reason);
            self.escalate(seq, &text, None, "ask", review)?;
            self.turn_end_badge = true;
        }
        if let TurnEndAction::TypeCommand {
            then: Then::Escalate(reason),
            ..
        } = &action
        {
            let text = escalate::attention_text(point, reason);
            self.escalate(seq, &text, None, "ask", review)?;
            self.turn_end_badge = true;
        }
        Ok(())
    }

    /// Whether the session's foreground program, read fresh, is read by
    /// Claude Code's reader — the grammar this policy types into. Not:
    /// journaled `SKIPPED`, nothing typed.
    fn speaks_claude(
        &mut self,
        seq: u64,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        self.program = self.foreground_program()?;
        let rows = self
            .last
            .as_ref()
            .map(|s| s.rows.clone())
            .unwrap_or_default();
        let reader = aterm_phase::identify(self.program.as_deref(), &rows);
        if reader.program() == aterm_phase::Program::Claude {
            return Ok(true);
        }
        review.note(&format!(
            "SKIPPED seq={seq} rule={rule} the session runs {}, not Claude Code",
            self.program.as_deref().unwrap_or("-")
        ));
        Ok(false)
    }

    /// `text` typed and submitted under the guarded submit (module header):
    /// `true` when the server says it submitted it (`submitted=1`), or wrote
    /// the Enter unverified (`pressed=1`, journaled so). A guard that missed
    /// (`skipped`), a host without the guarded submit, a hold and a refusal
    /// are `false`, journaled; the hold is waited out and the refusal backed
    /// off, and the next look decides again.
    fn type_guarded(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let seq = point.screen.seq;
        if self.caps.guarded_submit == Some(false) {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} this host has no guarded submit (turn \
                 submit=guarded:)"
            ));
            return Ok(false);
        }
        let submit = format!("submit=guarded:{}", composer_guard(text));
        let r = self.call(&["turn", &submit, TURN_IDLE, TURN_TIMEOUT, TURN_YIELD, text])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!(
                "turn (the {rule} text) failed: {}",
                r.stderr.trim()
            )));
        }
        let why = one_line(r.err_text());
        if r.is_err("halted") {
            self.park_on_hold(&why, seq, review)?;
            return Ok(false);
        }
        if r.is_err("busy") || r.is_err("rate") {
            self.back_off(&why, seq, None, review)?;
            return Ok(false);
        }
        // A person typing outlived the yield: nothing was typed. Decided
        // again shortly, where the draft (if one is left) types nothing.
        if r.is_err("yield") {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} a person is typing: {why}"
            ));
            self.redecide_soon("a person was typing");
            return Ok(false);
        }
        if r.unknown_form() {
            self.caps.guarded_submit = Some(false);
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} this host has no guarded submit: {why}"
            ));
            return Ok(false);
        }
        self.caps.guarded_submit = Some(true);
        if r.submitted() {
            return Ok(true);
        }
        let verdict = r.turn_verdict().unwrap_or("").to_string();
        if verdict.split_whitespace().any(|w| w == "pressed=1") {
            review.note(&format!(
                "UNVERIFIED seq={seq} rule={rule} Enter written, the submit not seen: {verdict}"
            ));
            return Ok(true);
        }
        let verdict = if verdict.is_empty() { why } else { verdict };
        review.note(&format!(
            "SKIPPED seq={seq} rule={rule} not submitted: {verdict}"
        ));
        // `turn` pastes before its guard is checked: a miss leaves the text
        // typed (`reason=guard`), in the composer or wherever the cursor
        // was. Said, never left silently for the next look to read as a
        // person's draft.
        if r.skipped() || verdict.contains("reason=guard") {
            self.left_typed(
                point,
                text,
                &format!("the guard missed ({verdict})"),
                review,
            )?;
        }
        Ok(false)
    }

    /// A fenced write the server skipped on the UNCHANGED screen (a bare
    /// `OK skipped`: the generation held and the guard matched no row) —
    /// nothing written, and a retry would miss the same way: journaled
    /// `SKIPPED … the guard matched no row …` and escalated, so the worker
    /// is not left silently waiting on an act that cannot land.
    fn not_written(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        guard: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = point.screen.seq;
        review.note(&format!(
            "SKIPPED seq={seq} rule={rule} the guard matched no row of the judged screen \
             ({guard}); nothing written"
        ));
        let reason = format!(
            "not typed: the composer guard matched no row: `{}`",
            clip(text)
        );
        let attention = escalate::attention_text(point, &reason);
        self.escalate(seq, &attention, None, "ask", review)?;
        self.turn_end_badge = true;
        Ok(())
    }

    /// The point decided again [`REDECIDE_AFTER_SKIP`] from now
    /// ([`Self::turn_end_now`], on the deadline of the loop's own wait).
    fn redecide_soon(&mut self, why: &str) {
        self.turn_end_due = Some((Instant::now() + REDECIDE_AFTER_SKIP, why.to_string()));
    }

    /// Text this policy wrote and did not submit: escalated (the worker's
    /// `attention`, one ask), the badge cleared when the worker works.
    fn left_typed(
        &mut self,
        point: &Turn,
        text: &str,
        why: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        // What happened first: the badge is cut to fit.
        let reason = format!("typed, not submitted ({why}): `{}`", clip(text));
        let attention = escalate::attention_text(point, &reason);
        self.escalate(point.screen.seq, &attention, None, "ask", review)?;
        self.turn_end_badge = true;
        Ok(())
    }

    /// `text` typed and submitted: [`Self::type_fenced`] where the host
    /// fences `send`, else [`Self::type_guarded`] (module header).
    fn type_text(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        match self.type_fenced(point, text, rule, review)? {
            Some(typed) => Ok(typed),
            None => self.type_guarded(point, text, rule, review),
        }
    }

    /// Whether the host's `send` takes the `if-gen=` fence (`help send`
    /// names it; an older `send` types a leading `if-gen=` as TEXT, so it is
    /// never sent unprobed).
    fn send_fence_known(&mut self) -> Result<bool, Fail> {
        if let Some(known) = self.caps.send_gen {
            return Ok(known);
        }
        let r = self.call(&["help", "send"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("help send failed: {}", r.stderr.trim())));
        }
        let known = r.ok() && super::super::policy::server_fences_send(&r.stdout);
        self.caps.send_gen = Some(known);
        Ok(known)
    }

    /// THE FENCED WRITE (module header): `None` when the host or the read
    /// cannot fence it (no generation on the read, no composer, `key`/`send`
    /// without `if-gen=`) — the caller falls back to [`Self::type_guarded`];
    /// else whether the text was submitted.
    fn type_fenced(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<Option<bool>, Fail> {
        let seq = point.screen.seq;
        let rows = &point.screen.rows;
        let (Some(generation), Some(caret)) =
            (point.screen.generation.clone(), composer_index(rows))
        else {
            return Ok(None);
        };
        if self.caps.key_if == Some(false)
            || !self.gen_fence_known()?
            || !self.send_fence_known()?
        {
            return Ok(None);
        }
        let fence = format!("if-gen={generation}");
        let guard = format!("if={}", super::super::policy::row_guard(&rows[caret]));
        let r = self.call(&["send", &fence, &guard, "--", text])?;
        match self.refused_press(&r, &format!("{fence} {guard} (send)"))? {
            None => {}
            Some(Pressing::Lost { why, .. }) => return Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => {
                self.park_on_hold(&why, seq, review)?;
                return Ok(Some(false));
            }
            Some(Pressing::Refused { why }) => {
                self.back_off(&why, seq, None, review)?;
                return Ok(Some(false));
            }
            Some(Pressing::Unconfirmed { why }) => {
                // Refused outright (a usage line): nothing written; never
                // retried unfenced.
                self.caps.send_gen = Some(false);
                review.note(&format!("SKIPPED seq={seq} rule={rule} {why}"));
                return Ok(Some(false));
            }
            Some(_) => return Ok(Some(false)),
        }
        if r.skipped_changed() {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the screen moved before the write; nothing \
                 written"
            ));
            self.redecide_soon("the screen moved before the write");
            return Ok(Some(false));
        }
        if r.skipped() {
            // The screen was the one judged and the composer's guard matched
            // no row of it: a retry misses the same way. Nothing was written;
            // the point is the owner's, said — never a silent stall.
            self.not_written(point, text, rule, &format!("{fence} {guard}"), review)?;
            return Ok(Some(false));
        }
        let at = r.seq().unwrap_or(seq);
        self.wait(&["seq", &at.to_string()], STRAY_SETTLE)?;
        self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
        let now = self.screen()?;
        let held = composer_draft(&now.rows)
            .is_some_and(|(_, lines)| squashed(&lines.concat()) == squashed(text))
            && !is_placeholder(&now.rows, now.cursor_col)
            && parse_prompt(&now.rows).is_none();
        let (true, Some(generation), Some(caret)) =
            (held, now.generation.clone(), composer_index(&now.rows))
        else {
            let turn = turn_of(now);
            self.left_typed(
                &turn,
                text,
                "the composer does not show it as written",
                review,
            )?;
            return Ok(Some(false));
        };
        let guard = super::super::policy::row_guard(&now.rows[caret]);
        let args = super::super::policy::key_args(&guard, "enter", Some(&generation));
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        let r = self.call(&words)?;
        let turn = turn_of(now);
        match self.refused_press(&r, &args)? {
            None if !r.skipped() => Ok(Some(true)),
            None => {
                self.left_typed(&turn, text, "the screen moved before Enter", review)?;
                Ok(Some(false))
            }
            Some(Pressing::Lost { why, .. }) => Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => {
                self.park_on_hold(&why, seq, review)?;
                self.left_typed(&turn, text, "the session is halted", review)?;
                Ok(Some(false))
            }
            Some(Pressing::Refused { why }) | Some(Pressing::Unconfirmed { why }) => {
                self.left_typed(&turn, text, &why, review)?;
                Ok(Some(false))
            }
            Some(_) => Ok(Some(false)),
        }
    }

    /// The worker's own suggestion accepted (module header): `right`, fenced
    /// on the judged read's generation and guarded on the suggestion's row;
    /// the screen read once it has settled; then — when it shows the
    /// composer holding exactly the suggestion, typed now (the cursor off
    /// the placeholder's column) — Enter, fenced on that read and guarded on
    /// that row. A host or a read without the generation fence types the
    /// suggestion's words instead ([`Self::type_guarded`]); so does a
    /// suggestion the accept key did not fill.
    fn accept_suggestion(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let seq = point.screen.seq;
        let rows = &point.screen.rows;
        let (Some(generation), Some(caret)) =
            (point.screen.generation.clone(), composer_index(rows))
        else {
            return self.type_text(point, text, rule, review);
        };
        if self.caps.key_if == Some(false) || !self.gen_fence_known()? {
            return self.type_text(point, text, rule, review);
        }
        let guard = super::super::policy::row_guard(&rows[caret]);
        let args = super::super::policy::key_args(&guard, "right", Some(&generation));
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        let r = self.call(&words)?;
        match self.refused_press(&r, &args)? {
            None => {}
            Some(Pressing::Lost { why, .. }) => return Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => {
                self.park_on_hold(&why, seq, review)?;
                return Ok(false);
            }
            Some(Pressing::Refused { why }) => {
                self.back_off(&why, seq, None, review)?;
                return Ok(false);
            }
            Some(Pressing::Unconfirmed { why }) => {
                review.note(&format!("SKIPPED seq={seq} rule={rule} {why}"));
                return Ok(false);
            }
            Some(_) => return Ok(false),
        }
        if r.skipped_changed() {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the screen moved before the accept key"
            ));
            self.redecide_soon("the screen moved before the accept key");
            return Ok(false);
        }
        if r.skipped() {
            // The judged screen, and its suggestion's row guard matched no
            // row of it ([`Self::type_fenced`]'s bare skip).
            self.not_written(point, text, rule, &args, review)?;
            return Ok(false);
        }
        let at = r.seq().unwrap_or(seq);
        self.wait(&["seq", &at.to_string()], STRAY_SETTLE)?;
        self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
        let now = self.screen()?;
        let filled = composer_text(&now.rows).as_deref() == Some(text)
            && !is_placeholder(&now.rows, now.cursor_col)
            && parse_prompt(&now.rows).is_none();
        let (Some(generation), Some(caret)) = (now.generation.clone(), composer_index(&now.rows))
        else {
            return Ok(false);
        };
        if !filled {
            // The key did not fill it (a build without the accept key, the
            // suggestion gone): its words, typed — once, guarded.
            let still = composer_text(&now.rows).as_deref() == Some(text)
                && is_placeholder(&now.rows, now.cursor_col);
            if !still {
                review.note(&format!(
                    "SKIPPED seq={seq} rule={rule} the composer no longer shows the suggestion"
                ));
                return Ok(false);
            }
            let turn = turn_of(now);
            return self.type_text(&turn, text, rule, review);
        }
        let guard = super::super::policy::row_guard(&now.rows[caret]);
        let args = super::super::policy::key_args(&guard, "enter", Some(&generation));
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        let r = self.call(&words)?;
        match self.refused_press(&r, &args)? {
            None if !r.skipped() => Ok(true),
            None => {
                review.note(&format!(
                    "SKIPPED seq={seq} rule={rule} the composer moved before Enter; the \
                     suggestion stays filled"
                ));
                Ok(false)
            }
            Some(Pressing::Lost { why, .. }) => Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => {
                self.park_on_hold(&why, seq, review)?;
                Ok(false)
            }
            Some(Pressing::Refused { why }) => {
                self.back_off(&why, seq, None, review)?;
                Ok(false)
            }
            Some(Pressing::Unconfirmed { why }) => {
                review.note(&format!("SKIPPED seq={seq} rule={rule} {why}"));
                Ok(false)
            }
            Some(_) => Ok(false),
        }
    }
}
