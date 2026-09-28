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
//! rules (the `rules_file`, cut at [`RULES_CAP`]). Nothing
//! it types goes without:
//!
//! * the session's foreground program read FRESH (`status program=`), its
//!   reader a supervised agent's — Claude Code's or Codex's — and every read
//!   of the composer that reader's ([`Session::reader`]);
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
//!   fenced on THAT read (`key if-gen=… if=<the caret row> enter`) — for a
//!   program whose paste guard takes an Enter right behind typed text as a
//!   newline (Codex, measured: [`aterm_phase::ScreenReader::paste_guard`])
//!   once the echo has held still [`PASTE_GUARD_SETTLE_MS`]. Text we wrote
//!   and did not submit is escalated, never left silently;
//! * elsewhere, the guarded submit (`turn submit=guarded:<re> yield=0.2`,
//!   lane C): parked while a person types (`yield=`), then pasted, and Enter
//!   pressed only while the row holding the CURSOR — a row the composer
//!   draws, its caret row or a continuation indented two, never a shell's —
//!   still ends as the text does ([`composer_guard`]) — the PASTE is not
//!   fenced there, only checked against the read that judged the point, so a
//!   person who starts typing in between can get it spliced into a draft; a
//!   miss (`skipped`) leaves the text typed, and is escalated. A host without
//!   either gets nothing typed;
//! * for the worker's own suggestion, the vendor's accept key — `right` on
//!   an empty composer with a suggestion shown fills it (Claude Code 2.1.280,
//!   read from its binary: `right` and `tab` both `markAccepted` and fill the
//!   text; `right` is the one with no other use on an empty composer) —
//!   fenced on the judged read's generation and guarded on the suggestion's
//!   row, then Enter fenced on the read that shows it filled. A host without
//!   the generation fence gets the suggestion's words typed instead (the
//!   vendor counts a submitted text equal to its suggestion as accepted);
//! * for a draft left standing ([`TurnEndAction::Submit`]), Enter alone,
//!   fenced on the judged read's generation and guarded on the caret row as
//!   judged — a keystroke since and nothing is pressed. A host without the
//!   fence gets nothing pressed: the draft is escalated instead.
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

use super::super::policy::approval::squashed;
use super::super::policy::turn_end::{Restart, Then, decide_turn_end};
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

/// The longest the loop waits for the worker to REACT to its own write —
/// the continuation's echo ([`Session::type_fenced`]), the accept key's fill
/// ([`Session::accept_suggestion`]) — before it reads the composer to judge
/// the write. The wait ends the moment the screen moves, so a worker that
/// reacts pays nothing for it; the bound only says when one that has not
/// reacted AT ALL is judged anyway. Judged earlier, the read sees the
/// composer as it was before the write: a continuation merely not drawn yet
/// was escalated as "typed, not submitted", and an accept key merely not
/// read yet was taken for one that fills nothing, its words typed after it —
/// the worker then read both, and the Enter fenced on the read that showed
/// the fill submitted them doubled (`keep goingkeep going`). Both measured
/// with the live headless tests' worker on a loaded machine (load 35-40),
/// where it went more than 2 s without reacting; this was the approval
/// press's 2 s settle, whose worker takes a single digit. What it does NOT
/// cover: a worker starved past this bound is judged on the old composer
/// as before, and one that starts to react and then stalls mid-draw is
/// read once the screen is still for [`SETTLE_MS`] or [`SETTLE_CAP`] has
/// passed — a composer read half drawn is escalated, as it should be.
pub(super) const REACTION_WAIT: Duration = Duration::from_secs(10);

/// How long the echo of a fenced write must hold still before its Enter, for
/// a program with a paste guard ([`ScreenReader::paste_guard`]: Codex takes
/// an Enter within a burst of typed text as a newline): the guarded `turn`'s
/// own settle (`idle=600`), under which Codex submitted every time
/// (measured on 0.156.1).
const PASTE_GUARD_SETTLE_MS: &str = "600";

/// The cursor-row guard for `text` typed into the composer whose caret row
/// starts with `caret` (the program's, [`ScreenReader::caret`]: `❯` Claude
/// Code, `›` Codex): `^<caret> <text>` to the row's end when the text is
/// short enough to sit on the caret row ([`GUARD_TAIL_CHARS`]); else the end
/// of its last WORD (at most [`GUARD_WORD_CHARS`]) to the row's end, on a
/// row the COMPOSER draws — its caret row, or a continuation row indented
/// two. Both wrap their composer between words, and where the wrap falls
/// depends on the width, so the row the cursor ends on holds the last word —
/// never a fixed count of characters (lane B2's review: a 40-character tail
/// missed whenever the wrap put fewer on the last row). Unanchored, that
/// tail matched a SHELL's cursor row too: the hazards review of 2026-09-25
/// fed `answer_text` to a zsh row and the guard passed, so an agent that
/// exited mid-write would have had its answer run as a command. A shell's
/// row opens with its prompt, and its soft-wrapped continuation with the
/// text itself — neither with a caret or two spaces. Whitespace-free
/// ([`super::super::policy::row_guard`]'s escaping).
pub(in crate::supervise) fn composer_guard(caret: char, text: &str) -> String {
    if text.chars().count() <= GUARD_TAIL_CHARS {
        // The caret's space is whatever the program draws there — a
        // NO-BREAK SPACE on Claude Code 2.1.280 and 2.1.281 (`❯\u{a0}keep
        // going`) — so it is `\s`, never the `\x20` a typed space is.
        let guard = super::super::policy::row_guard(&format!("{caret} {text}"));
        return guard.replacen(&format!("^{caret}\\x20"), &format!("^{caret}\\s"), 1);
    }
    let word = text.split_whitespace().last().unwrap_or(text);
    let n = word.chars().count();
    let tail: String = word
        .chars()
        .skip(n.saturating_sub(GUARD_WORD_CHARS))
        .collect();
    let anchored = super::super::policy::row_guard(&tail);
    // `row_guard` anchors at the row's start: the tail ends the row, on a
    // row that opens with the caret or the composer's two-space indent.
    format!("^[{caret}\\s]\\s.*{}", anchored.trim_start_matches('^'))
}

/// The composer's caret row as `reader` reads it (`None`: no composer).
fn caret_row(reader: &dyn ScreenReader, rows: &[String]) -> Option<usize> {
    reader.composer(rows).map(|(caret, _)| caret)
}

/// The text on the composer's caret row as `reader` reads it, the caret
/// stripped — typed, or the placeholder ([`typed_draft`] tells which).
fn caret_text(reader: &dyn ScreenReader, rows: &[String]) -> Option<String> {
    reader
        .composer(rows)
        .and_then(|(_, lines)| lines.into_iter().next())
}

/// A wall the host's measure of the API's reach answers: an API error
/// that never reached the API, a reply the connection cut off, or a
/// certificate or proxy refused ([`aterm_phase::ApiCause`]) — the arms of
/// the turn-end policy that read [`TurnEndReading::reach`]. The server's
/// own failure (a status, an overload) is not: a handshake says nothing
/// of the API's health, and it keeps its ladder whatever the measure.
fn answered_by_the_network(kind: aterm_phase::WallKind) -> bool {
    matches!(
        kind,
        aterm_phase::WallKind::ApiError {
            cause: aterm_phase::ApiCause::Unreachable
                | aterm_phase::ApiCause::CutOff
                | aterm_phase::ApiCause::Config,
            ..
        }
    )
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

    /// The standing rules, read now (an edit since the launch counts), as
    /// one line: `policy.rules_file`. `None` when it is not set, or the file
    /// cannot be read or is empty.
    fn rules_text(&self, opts: &SuperviseOpts) -> Option<String> {
        let path = opts.policy.rules_file.clone()?;
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

    /// How long ago a person last typed into the session, as the policy
    /// reads it ([`TurnEndReading::person`]): the later of the server's last
    /// status (`human_ms=`) and the draft in the composer last changing.
    fn person_ago(&self, now: Instant) -> Option<Duration> {
        let draft = self.draft_seen.as_ref().map(|(_, at)| *at);
        self.person
            .max(draft)
            .map(|typed| now.saturating_duration_since(typed))
    }

    /// The turn-end reading of `screen`: the reader for the session's
    /// program (as last read), the work since the last point, the wall's
    /// reset, the rules, and how long ago a person typed — a draft that
    /// changed on any read included ([`Self::person_ago`]). At a wall the
    /// network answers ([`answered_by_the_network`]) — and there alone — the
    /// host's measure of the API's reach ([`IdleHost::reach`]), remembered
    /// as [`Self::reach_seen`]; everywhere else nothing is asked, and it is
    /// forgotten.
    fn turn_end_reading(
        &mut self,
        screen: &Screen,
        worked: Option<Duration>,
        opts: &SuperviseOpts,
    ) -> TurnEndReading {
        let reader = self.reader(&screen.rows);
        let reading = reader.read(&screen.rows, Some(screen.cursor_col));
        let reset_at = match &reading.wall {
            Some(w) => {
                let text = w.reset.clone();
                self.wall_reset_at(text.as_deref())
            }
            None => None,
        };
        self.reach_seen = reading
            .wall
            .as_ref()
            .filter(|w| answered_by_the_network(w.kind))
            .map(|_| {
                opts.idle_host
                    .as_ref()
                    .map_or(Reach::Unknown, |h| h.reach())
            });
        let r = TurnEndReading::of(
            &reading,
            &screen.rows,
            typed_draft(reader, screen) || self.homed_draft(reader, screen),
            worked,
            reset_at,
            self.rules_text(opts),
        );
        let host = opts.idle_host.as_ref();
        TurnEndReading {
            reach: self.reach_seen.unwrap_or_default(),
            person: self.person_ago(Instant::now()),
            // Never while a limit episode stands: the upgrade types nothing
            // at a limit, and the wall is the loop's to wait out
            // ([`crate::supervise::IdleHost::limited`]).
            upgrading: self.limit.is_none() && host.is_some_and(|h| h.owns_turn_end()),
            restartable: host.is_some_and(|h| h.can_restart()),
            // The screen's launch card, or the host's record of whose turns
            // the conversation holds (the harness's own are no task).
            taskless: r.taskless || host.is_some_and(|h| h.taskless()),
            ..r
        }
    }

    /// A draft the rows cannot tell from the placeholder: one row of text on
    /// the composer's caret row with the cursor at its column 2 — where both
    /// programs park it on an empty composer, and where a person's draft sits
    /// after its caret was moved home (←, Home, ctrl-a; measured on Claude
    /// Code 2.1.280 and 2.1.281). The `cell` there says which: the
    /// placeholder is drawn DIM ([`crate::supervise::screen::cell_is_dim`]), a typed
    /// draft is not. Only a reply that names no `dim` reads as a draft; an
    /// unreadable cell reads as the placeholder, as before.
    fn homed_draft(&mut self, reader: &dyn ScreenReader, screen: &Screen) -> bool {
        let Some((caret, lines)) = reader.composer(&screen.rows) else {
            return false;
        };
        let one_row = lines.first().is_some_and(|l| !l.is_empty())
            && lines.iter().skip(1).all(String::is_empty);
        if !one_row || screen.cursor_col != 2 || screen.cursor_index() != Some(caret) {
            return false;
        }
        let row = (screen.first + caret).to_string();
        match self.call(&["cell", &row, "2"]) {
            Ok(r) if r.ok() => !crate::supervise::screen::cell_is_dim(&r.stdout),
            _ => false,
        }
    }

    /// The decision at a NEW point: the point folded into the policy's
    /// memory ([`TurnEndState::observe`]) — once, here — then
    /// [`decide_turn_end`]; and when that is an act, decided again on a
    /// fresh `status` ([`Self::decide_to_act`]). A loop watching behind
    /// another supervisor's claim decides nothing: that one answers the
    /// session.
    pub(super) fn turn_end_at_point(
        &mut self,
        point: &Turn,
        worked: Option<Duration>,
        opts: &SuperviseOpts,
    ) -> Result<TurnEndAction, Fail> {
        if !self.held {
            self.turn_end_due = None;
        }
        if self.claim.watching_behind().is_some() {
            return Ok(TurnEndAction::Nothing);
        }
        let r = self.turn_end_reading(&point.screen, worked, opts);
        // Decided while the host owns the turn ends: decided again once it
        // owns nothing ([`Self::host_held`]).
        self.host_held = r.upgrading;
        let now = Instant::now();
        self.turn_end.observe(&r, now);
        self.decide_to_act(r, opts, now)
    }

    /// [`decide_turn_end`] on `r`; when it says to type, the session's
    /// program and a person's last keystroke are read fresh (`status`,
    /// [`Self::foreground_program`]) and the point decided again on them —
    /// so a status read is paid only where there is something to type, and
    /// no act goes on an older word of a person than its own read.
    fn decide_to_act(
        &mut self,
        mut r: TurnEndReading,
        opts: &SuperviseOpts,
        now: Instant,
    ) -> Result<TurnEndAction, Fail> {
        let action = decide_turn_end(&self.turn_end, &r, &opts.policy, now);
        if action.rule_id().is_none() {
            return Ok(action);
        }
        self.program = self.foreground_program()?;
        r.person = self.person_ago(now);
        Ok(decide_turn_end(&self.turn_end, &r, &opts.policy, now))
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
        // An act held for a person: the next look judges the point again.
        if std::mem::take(&mut self.held) {
            self.rejudge = true;
            return Ok(());
        }
        let screen = self.screen()?;
        let turn = self.turn_of(screen);
        if review_key(&turn, allow) != review_key(seen, allow) {
            return Ok(());
        }
        let r = self.turn_end_reading(&turn.screen, None, opts);
        self.host_held = r.upgrading;
        let now = Instant::now();
        self.turn_end.observe(&r, now);
        let action = self.decide_to_act(r, opts, now)?;
        if let TurnEndAction::Escalate { reason } = &action
            && matches!(turn.phase, Phase::Idle | Phase::Question)
        {
            self.escalate_point(&turn, allow, Some(reason), review)?;
        }
        self.turn_end_execute(&turn, action, opts, allow, review)
    }

    /// Carry out what the policy decided at `point` (module header). An act
    /// the server took is recorded in the policy's memory
    /// ([`TurnEndState::acted`]); one it did not take is not.
    pub(super) fn turn_end_execute(
        &mut self,
        point: &Turn,
        action: TurnEndAction,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = point.screen.seq;
        let (text, rule) = match &action {
            TurnEndAction::Nothing | TurnEndAction::Escalate { .. } => return Ok(()),
            TurnEndAction::Restart {
                why,
                rule_id,
                otherwise,
            } => {
                return self.restart_agent(
                    point,
                    why,
                    rule_id,
                    otherwise.as_ref().clone(),
                    opts,
                    allow,
                    review,
                );
            }
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
            TurnEndAction::Submit { rule_id } => (
                self.reader(&point.screen.rows)
                    .composer(&point.screen.rows)
                    .map(|(_, lines)| lines.join(" "))
                    .unwrap_or_default(),
                *rule_id,
            ),
        };
        if !self.speaks_to_its_agent(seq, rule, review) {
            return Ok(());
        }
        // The `status` that named the program says the worker is not reading
        // its input: nothing is typed; the wait holds first (`stall.rs`).
        if self.stall_in_hand(review) {
            return Ok(());
        }
        let typed = match &action {
            TurnEndAction::Accept { .. } => self.accept_suggestion(point, &text, rule, review)?,
            TurnEndAction::Submit { .. } => self.submit_draft(point, &text, rule, review)?,
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
        review.say(&format!("{word} seq={seq} rule={rule} {}", clip(&text)))?;
        self.ledger_row(
            rule,
            approvals::Outcome::Typed,
            &text,
            "the turn-end policy",
            seq,
        );
        if let TurnEndAction::TypeCommand {
            then: Then::Escalate(reason),
            ..
        } = &action
        {
            let text = escalate::attention_text(self.reader(&point.screen.rows), point, reason);
            self.escalate(seq, &text, None, "ask", review)?;
            self.turn_end_badge = true;
        }
        Ok(())
    }

    /// THE RESTART the policy asked for ([`TurnEndAction::Restart`]: the
    /// memory banner, D3; a model bucket's fallback and its reset, D7): the
    /// session's host's ([`IdleHost::restart`]), journaled `HOST seq=<n>
    /// restart:<why> step=<word>`, and what the loop does with its word. The
    /// agent relaunched (`adopted`) is the loop's again at once — the host
    /// asks for the next idle point to carry it on — said `RESTARTED seq=<n>
    /// rule=<id> <what>`, remembered by the policy
    /// ([`TurnEndState::restarted`]: a bucket's switch, undone at its reset)
    /// and, for a model, ledgered as `relaunch --model <m>`, the row a later
    /// loop reads the open switch from ([`approvals::open_model_switch`]).
    /// One that must wait (`wait:…`: not idle yet, a person's hand, a job the
    /// relaunch cannot see yet; `busy:another-sweep`) is decided again after
    /// a pause that grows ([`TurnEndTiming::restart_backoff`]), never
    /// escalated for the wait. One that is never made — no host restarts
    /// here, a launch that cannot be carried, a signal refused — gets the
    /// point's `otherwise`: the memory banner's escalation (naming why), a
    /// bucket's reset waited out, the continuation a reset owed. Nothing
    /// while the worker's stall is in hand (`stall.rs`): the hold is first.
    #[allow(clippy::too_many_arguments)]
    fn restart_agent(
        &mut self,
        point: &Turn,
        why: &Restart,
        rule: &'static str,
        otherwise: TurnEndAction,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = point.screen.seq;
        if self.stall_in_hand(review) {
            return Ok(());
        }
        let step = opts.idle_host.as_ref().and_then(|h| h.restart(why));
        if let Some(step) = &step {
            review.note(&format!(
                "HOST seq={seq} restart:{} step={step}",
                why.word()
            ));
        }
        let word = step
            .as_deref()
            .map(|s| s.split_once(':').map_or(s, |(head, _)| head));
        match (word, step.as_deref()) {
            (Some("adopted" | "relaunched" | "continued" | "done"), _) => {
                self.restart_waits = 0;
                let r = self.turn_end_reading(&point.screen, None, opts);
                let now = Instant::now();
                self.turn_end.restarted(why, true, &r, now);
                // A host that typed the carry-on inside its restart typed a
                // turn of its own: its answer is awaited as the harness's,
                // never a short turn of the worker's.
                if word == Some("continued") {
                    self.turn_end.host_typed(now);
                }
                self.mail_turn_boundary();
                let what = match why {
                    Restart::Memory => "the agent, for its critical-memory banner".to_string(),
                    Restart::Model { to } => format!("relaunch --model {to}"),
                    Restart::ModelBack { to } => {
                        format!(
                            "relaunch --model {}",
                            to.as_deref().unwrap_or("(its launch's)")
                        )
                    }
                };
                let switch = self
                    .turn_end
                    .model_switch()
                    .filter(|_| matches!(why, Restart::Model { .. }))
                    .cloned();
                let back = match &switch {
                    Some(m) => format!(
                        " (from {}, back at {}; session-only)",
                        m.from.as_deref().unwrap_or("the launch's model"),
                        m.back_at
                            .map_or_else(|| "its reset".to_string(), |at| self.stamp_in(at))
                    ),
                    None => String::new(),
                };
                review.say(&format!("RESTARTED seq={seq} rule={rule} {what}{back}"))?;
                if !matches!(why, Restart::Memory) {
                    // A switch's row carries how to undo it, for a loop that
                    // starts before its reset.
                    let reason = match &switch {
                        Some(m) => approvals::model_switch_reason(
                            m.from.as_deref(),
                            m.back_at.map(|at| self.unix_of(at)),
                        ),
                        None => "the turn-end policy".to_string(),
                    };
                    self.ledger_row(rule, approvals::Outcome::Typed, &what, &reason, seq);
                }
                Ok(())
            }
            (Some("wait"), Some(step)) => self.restart_later(seq, why, step, review),
            (Some("busy"), Some(step)) if step == "busy:another-sweep" => {
                self.restart_later(seq, why, step, review)
            }
            _ => {
                self.restart_waits = 0;
                let r = self.turn_end_reading(&point.screen, None, opts);
                self.turn_end.restarted(why, false, &r, Instant::now());
                match otherwise {
                    TurnEndAction::Escalate { reason } => {
                        let reason = match &step {
                            Some(step) => {
                                format!("the restart could not be made ({step}): {reason}")
                            }
                            None => reason,
                        };
                        self.escalate_point(point, allow, Some(&reason), review)
                    }
                    other => self.turn_end_execute(point, other, opts, allow, review),
                }
            }
        }
    }

    /// A restart that must wait: decided again after the next pause of
    /// [`TurnEndTiming::restart_backoff`], `WAITING … until=<t>
    /// restart:<why> waits: <step>`.
    fn restart_later(
        &mut self,
        seq: u64,
        why: &Restart,
        step: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let pause = crate::supervise::ladder::Ladder(&self.turn_end.timing.restart_backoff)
            .step(usize::try_from(self.restart_waits).unwrap_or(usize::MAX));
        self.restart_waits = self.restart_waits.saturating_add(1);
        let until = Instant::now() + pause;
        let why = format!("restart:{} waits: {step}", why.word());
        review.note(&format!(
            "WAITING seq={seq} until={} {why}",
            self.stamp_in(until)
        ));
        self.turn_end_due = Some((until, why));
        Ok(())
    }

    /// Whether the session's foreground program — read fresh for this
    /// decision ([`Self::turn_end_at_point`], [`Self::turn_end_now`]) — is an
    /// agent this policy types into: one whose reader is measured
    /// ([`aterm_phase::Program::supervisable`] — Claude Code, Codex), never a
    /// shell or a program nothing reads. Not: journaled `SKIPPED`, nothing
    /// typed.
    fn speaks_to_its_agent(&mut self, seq: u64, rule: &str, review: &mut dyn Review) -> bool {
        let rows = self
            .last
            .as_ref()
            .map(|s| s.rows.clone())
            .unwrap_or_default();
        if self.reader(&rows).program().supervisable() {
            return true;
        }
        review.note(&format!(
            "SKIPPED seq={seq} rule={rule} the session runs {}, no agent this policy types into",
            self.program.as_deref().unwrap_or("-")
        ));
        false
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
                "SKIPPED seq={seq} rule={rule} this aterm is too old for a guarded submit"
            ));
            return Ok(false);
        }
        let Some(caret) = self.reader(&point.screen.rows).caret() else {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} no composer this policy types into"
            ));
            return Ok(false);
        };
        let submit = format!("submit=guarded:{}", composer_guard(caret, text));
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
                "SKIPPED seq={seq} rule={rule} this aterm is too old for a guarded submit: {why}"
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
            "not typed: aterm could not find the prompt line: `{}`",
            clip(text)
        );
        let attention = escalate::attention_text(self.reader(&point.screen.rows), point, &reason);
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
        let attention = escalate::attention_text(self.reader(&point.screen.rows), point, &reason);
        self.escalate(point.screen.seq, &attention, None, "ask", review)?;
        self.turn_end_badge = true;
        Ok(())
    }

    /// `text` typed and submitted: [`Self::type_fenced`] where the host
    /// fences `send`, else [`Self::type_guarded`] (module header). A program
    /// that takes an Enter right behind typed text as a newline (Codex's
    /// paste guard, [`ScreenReader::paste_guard`]) is written FENCED too —
    /// its Enter held until the echo has been still [`PASTE_GUARD_SETTLE_MS`],
    /// as the guarded `turn` holds it (the hazards review of 2026-09-25: the
    /// unfenced `turn` pasted with no check of the program, so an agent that
    /// exited under it had the text land in the shell).
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
    pub(super) fn send_fence_known(&mut self) -> Result<bool, Fail> {
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
        let (Some(generation), Some(caret)) = (
            point.screen.generation.clone(),
            caret_row(self.reader(rows), rows),
        ) else {
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
            Some(Pressing::Unconfirmed { why, .. }) => {
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
        self.wait(&["seq", &at.to_string()], REACTION_WAIT)?;
        let settle = if self.reader(rows).paste_guard() {
            PASTE_GUARD_SETTLE_MS
        } else {
            SETTLE_MS
        };
        self.wait(&["idle", settle], SETTLE_CAP)?;
        let now = self.screen()?;
        let reader = self.reader(&now.rows);
        let held = reader
            .composer(&now.rows)
            .is_some_and(|(_, lines)| squashed(&lines.concat()) == squashed(text))
            && typed_draft(reader, &now)
            && reader.prompt(&now.rows).is_none();
        let (true, Some(generation), Some(caret)) =
            (held, now.generation.clone(), caret_row(reader, &now.rows))
        else {
            let turn = self.turn_of(now);
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
        let turn = self.turn_of(now);
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
            Some(Pressing::Refused { why }) | Some(Pressing::Unconfirmed { why, .. }) => {
                self.left_typed(&turn, text, &why, review)?;
                Ok(Some(false))
            }
            Some(_) => Ok(Some(false)),
        }
    }

    /// The draft standing in the composer submitted as it is (module header):
    /// Enter, fenced on the judged read's generation and guarded on its caret
    /// row. `text` is the draft as read, for what is said. A host or a read
    /// without the fence gets nothing pressed — the draft is escalated,
    /// never submitted blind.
    fn submit_draft(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let seq = point.screen.seq;
        let rows = &point.screen.rows;
        let fenced = match (
            point.screen.generation.clone(),
            caret_row(self.reader(rows), rows),
        ) {
            (Some(generation), Some(caret))
                if self.caps.key_if != Some(false) && self.gen_fence_known()? =>
            {
                Some((generation, caret))
            }
            _ => None,
        };
        let Some((generation, caret)) = fenced else {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} a draft stands in the composer and this host \
                 has no fenced Enter"
            ));
            let reason = format!(
                "a draft stands in the composer, not submitted: `{}`",
                clip(text)
            );
            let attention = escalate::attention_text(self.reader(rows), point, &reason);
            self.escalate(seq, &attention, None, "ask", review)?;
            self.turn_end_badge = true;
            return Ok(false);
        };
        let guard = super::super::policy::row_guard(&rows[caret]);
        let args = super::super::policy::key_args(&guard, "enter", Some(&generation));
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        let r = self.call(&words)?;
        match self.refused_press(&r, &args)? {
            None if r.skipped_changed() => {
                // A keystroke since the read: decided again on the screen as
                // it is now.
                review.note(&format!(
                    "SKIPPED seq={seq} rule={rule} the screen moved before Enter; the draft \
                     stays"
                ));
                self.redecide_soon("the screen moved before the draft's Enter");
                Ok(false)
            }
            None if r.skipped() => {
                // The judged screen, and its caret row matched no row of it
                // ([`Self::type_fenced`]'s bare skip).
                self.not_written(point, text, rule, &args, review)?;
                Ok(false)
            }
            None => Ok(true),
            Some(Pressing::Lost { why, .. }) => Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => {
                self.park_on_hold(&why, seq, review)?;
                Ok(false)
            }
            Some(Pressing::Refused { why }) => {
                self.back_off(&why, seq, None, review)?;
                Ok(false)
            }
            Some(Pressing::Unconfirmed { why, .. }) => {
                review.note(&format!("SKIPPED seq={seq} rule={rule} {why}"));
                Ok(false)
            }
            Some(_) => Ok(false),
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
        let (Some(generation), Some(caret)) = (
            point.screen.generation.clone(),
            caret_row(self.reader(rows), rows),
        ) else {
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
            Some(Pressing::Unconfirmed { why, .. }) => {
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
        self.wait(&["seq", &at.to_string()], REACTION_WAIT)?;
        self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
        let now = self.screen()?;
        let reader = self.reader(&now.rows);
        let shows = caret_text(reader, &now.rows).as_deref() == Some(text);
        let filled = shows && typed_draft(reader, &now) && reader.prompt(&now.rows).is_none();
        let (Some(generation), Some(caret)) =
            (now.generation.clone(), caret_row(reader, &now.rows))
        else {
            return Ok(false);
        };
        if !filled {
            // The key did not fill it (a build without the accept key, the
            // suggestion gone): its words, typed — once, guarded.
            if !(shows && !typed_draft(reader, &now)) {
                review.note(&format!(
                    "SKIPPED seq={seq} rule={rule} the composer no longer shows the suggestion"
                ));
                return Ok(false);
            }
            let turn = self.turn_of(now);
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
            Some(Pressing::Unconfirmed { why, .. }) => {
                review.note(&format!("SKIPPED seq={seq} rule={rule} {why}"));
                Ok(false)
            }
            Some(_) => Ok(false),
        }
    }
}
