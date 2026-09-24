// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The approval policy in the loop: every box a look finds is decided by
//! [`decide`] — the ONE decider (owner decision 1) — and an approval is
//! pressed under the guard it returns ([`Session::press_one_guarded`], or,
//! for an unnumbered dialog, [`Session::press_focused`]).
//!
//! What the loop adds around the pure decision is only what the pure
//! decision cannot know, read FRESH for every box it decides: the session's
//! foreground PROGRAM (`status program=`, lane C: the box is read by that
//! program's reader, so a shell showing a captured box is never judged by
//! Claude Code's grammar), its working directory (`meta cwd=`, for a box
//! that needs it — the rm circuit breaker and the folder-trust dialog; the
//! launch directory, which a trust dialog asks about), the permission mode
//! its footer showed on an earlier read (a 2.1.280 box replaces the footer),
//! the owner's home, uid and `$TMPDIR`, and which rules and roots
//! [`SuperviseOpts::policy`] turns on (behind `--auto-reads`). Nothing is
//! read when the policy is off: every box escalates. Every decision is a row
//! of the approval ledger ([`approvals`]).

use super::super::policy::approval::{
    ApprovalCtx, Choice, Decision, RM_BREAKER_NOTE, RULE_READ_ONLY, decide,
};
use super::*;

/// The box needs the session's working directory: the vendor's rm circuit
/// breaker, the folder-trust dialog.
fn needs_cwd(p: &aterm_phase::PromptV2) -> bool {
    p.kind == PromptKind::Trust
        || p.notes
            .first()
            .is_some_and(|n| n.starts_with(RM_BREAKER_NOTE))
}

/// How long a box on a screen whose named program is no agent waits for
/// the server's own verdict (`await agent prompt`) before it is judged as
/// that program's.
const PROGRAM_SETTLE: Duration = Duration::from_millis(2000);

impl<C: Ctl> Session<'_, C> {
    /// The session's foreground program as `status program=` names it —
    /// `None` from a host that does not publish it, whose screen then names
    /// the reader ([`aterm_phase::identify`]) — and `None` too when the
    /// server's own verdict (`agent=`, lane C) already identified an agent
    /// on this screen while `program=` still names something else: the name
    /// is re-read off the server's event loop, and an agent `exec`ed in
    /// place keeps its launcher's name (`bash`) until the screen moves
    /// (measured on this lane's live check), while the verdict sticks to
    /// the process group it identified. A shell showing a captured box gets
    /// no verdict (`agent=-`), so it keeps its name and is never judged as
    /// an agent.
    pub(super) fn foreground_program(&mut self) -> Result<Option<String>, Fail> {
        let r = self.call(&["status"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("status failed: {}", r.stderr.trim())));
        }
        if !r.ok() {
            return Ok(None);
        }
        let field = |f: &str| super::escalate::status_field(&r.stdout, f).map(str::to_string);
        let program = field("program");
        let named_agent = program
            .as_deref()
            .is_some_and(|p| matches!(p, "claude" | "codex"));
        Ok(if field("agent").is_some() && !named_agent {
            None
        } else {
            program
        })
    }

    /// The approval policy's context: the session's cwd (`meta` read fresh
    /// when `fresh_cwd`), the footer's mode, the process's home/uid/
    /// `$TMPDIR`, the rules and trust roots `opts` turns on.
    fn approval_ctx(
        &mut self,
        fresh_cwd: bool,
        opts: &SuperviseOpts,
        allow: &[String],
    ) -> Result<ApprovalCtx, Fail> {
        if fresh_cwd {
            let r = self.call(&["meta"])?;
            if self.unserved(&r) {
                return Err(Fail::Lost(format!("meta failed: {}", r.stderr.trim())));
            }
            if r.ok() {
                self.note_cwd(&r.stdout);
            } else {
                self.cwd = None;
            }
        }
        let env = self.approval_env.clone();
        let cwd = self.cwd.clone();
        let mut ctx = ApprovalCtx::new(
            cwd.clone().unwrap_or_else(|| PathBuf::from("/")),
            env.home,
            env.uid,
            env.tmpdir,
        );
        ctx.cwd_known = cwd.is_some();
        ctx.set_trust_roots(&opts.policy.trust_roots, env.uid);
        ctx.bypass_mode = self.footer == Some(FooterMode::Bypass);
        ctx.toggles = opts.approval_toggles();
        ctx.python_allow = allow.to_vec();
        Ok(ctx)
    }

    /// The decision on the box on `screen`: read by the reader for the
    /// session's foreground program, then [`decide`]d. With every rule off
    /// nothing is read and the box escalates.
    pub(super) fn decide_box(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
        allow: &[String],
    ) -> Result<Decision, Fail> {
        if opts.approval_toggles() == ApprovalToggles::off() {
            return Ok(Decision::Escalate {
                reason: "the approval policy is off".to_string(),
            });
        }
        if let Some(holder) = self.claim.watching_behind() {
            return Ok(Decision::Escalate {
                reason: format!("another supervisor ({holder}) answers this session"),
            });
        }
        self.program = self.foreground_program()?;
        let mut reading = aterm_phase::read(
            self.program.as_deref(),
            &screen.rows,
            Some(screen.cursor_col),
        );
        if reading.prompt.is_none() && aterm_phase::read(None, &screen.rows, None).prompt.is_some()
        {
            // The screen shows an agent's box and the named program is none:
            // a shell showing a capture — or an agent just started, whose
            // name the server resolves off its event loop (lane C). The
            // server's own verdict waits for the name: `await agent prompt`,
            // bounded, then the name is read once more.
            let settle = PROGRAM_SETTLE.as_millis().to_string();
            let r = self.call(&["await", "agent", "prompt", "timeout", &settle])?;
            if self.unserved(&r) {
                return Err(Fail::Lost(format!(
                    "await agent failed: {}",
                    r.stderr.trim()
                )));
            }
            if r.ok() {
                self.program = self.foreground_program()?;
                reading = aterm_phase::read(
                    self.program.as_deref(),
                    &screen.rows,
                    Some(screen.cursor_col),
                );
            }
        }
        let fresh_cwd = reading.prompt.as_ref().is_some_and(needs_cwd);
        let ctx = self.approval_ctx(fresh_cwd, opts, allow)?;
        Ok(decide(&reading, &screen.rows, &ctx))
    }

    /// One row of the approval ledger.
    pub(super) fn ledger_row(
        &mut self,
        rule_id: &str,
        outcome: approvals::Outcome,
        command: &str,
        reason: &str,
        box_seq: u64,
    ) {
        self.ledger.write(
            &approvals::Row {
                rule_id,
                outcome,
                command,
                reason,
                box_seq,
            },
            &mut std::io::stderr(),
        );
    }

    /// One turn under the approval policy. A box [`decide`] approves is
    /// pressed under its guard (noted, ledgered, and reported to `review`)
    /// and the loop looks again once the box has LEFT; a box it escalates —
    /// with the reason kept for the escalation's text ([`Session::box_reason`])
    /// — is a review point, and so is anything that is not a box, the same
    /// approval back after two presses, a guard that matched no row of this
    /// very box, a box that did not move after the press, and a fallback
    /// press withheld because the session survey is open
    /// ([`Pressing::Withheld`]). A press the server skipped because the box
    /// left or the fenced screen moved, parked on a hold, or backed off
    /// after `ERR busy …`/`ERR rate`, is followed by a fresh read and a fresh
    /// decision — never the same press again. When the connection is lost
    /// before the press is seen through, what the loop knows it did is
    /// noted — a `1` the server confirmed is an approval, a press whose
    /// answer never came is noted as such and is not — and the loss is
    /// ridden out.
    pub(super) fn auto_read(
        &mut self,
        turn: Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        approved: &mut Vec<String>,
        deadline: Instant,
        review: &mut dyn Review,
    ) -> Result<Step, Fail> {
        let box_on = (turn.phase == Phase::Prompt)
            .then(|| parse_prompt(&turn.screen.rows))
            .flatten();
        let Some(p) = box_on else {
            return Ok(Step::Review(turn));
        };
        let seq = turn.screen.seq;
        let notes = opts.notes.as_deref();
        let decision = self.decide_box(&turn.screen, opts, allow)?;
        let (rule_id, guard, choice) = match decision {
            Decision::Escalate { reason } => {
                if opts.auto_reads {
                    append_note(
                        notes,
                        &format!("handed to the manager ({reason}): {}", p.command),
                    )?;
                    self.ledger_row("-", approvals::Outcome::Escalated, &p.command, &reason, seq);
                }
                self.box_reason = Some((review_key(&turn, allow), reason));
                return Ok(Step::Review(turn));
            }
            Decision::Approve {
                rule_id,
                guard,
                choice,
                ..
            } => (rule_id, guard, choice),
        };
        let repeats = approved.iter().filter(|c| **c == p.command).count();
        if repeats >= MAX_APPROVALS_OF_ONE_COMMAND {
            let why = "the same prompt came back after two approvals";
            append_note(
                notes,
                &format!("handed to the manager ({why}): {}", p.command),
            )?;
            self.ledger_row(rule_id, approvals::Outcome::Escalated, &p.command, why, seq);
            self.box_reason = Some((review_key(&turn, allow), why.to_string()));
            return Ok(Step::Review(turn));
        }
        let approved_note = if rule_id == RULE_READ_ONLY {
            format!("approved read-only: {}", p.command)
        } else {
            format!("approved ({rule_id}): {}", p.command)
        };
        let pressing = match &choice {
            Choice::Digit(n) => self.press_one_guarded(&p, &guard, &n.to_string(), &turn.screen)?,
            Choice::Focus { steps, label } => {
                self.press_focused(&p, *steps, label, &guard, &turn.screen)?
            }
        };
        let press = match pressing {
            Pressing::Done(press) => press,
            Pressing::Unconfirmed { why } => {
                append_note(
                    notes,
                    &format!("handed to the manager ({why}): {}", p.command),
                )?;
                self.ledger_row(
                    rule_id,
                    approvals::Outcome::Escalated,
                    &p.command,
                    &why,
                    seq,
                );
                self.box_reason = Some((review_key(&turn, allow), why));
                return Ok(Step::Review(turn));
            }
            Pressing::Withheld => {
                append_note(
                    notes,
                    &format!(
                        "handed to the manager (the session survey is open and this host has \
                         no guarded press: an unguarded 1 could rate the session): {}",
                        p.command
                    ),
                )?;
                return Ok(Step::Review(turn));
            }
            Pressing::Halted { why } => {
                self.ledger_row(rule_id, approvals::Outcome::Refused, &p.command, &why, seq);
                self.park_on_hold(&why, seq, review)?;
                return Ok(Step::Again { settle: false });
            }
            Pressing::Refused { why } => {
                self.ledger_row(rule_id, approvals::Outcome::Refused, &p.command, &why, seq);
                self.back_off(&why, seq, Some(&guard), review)?;
                return Ok(Step::Again { settle: false });
            }
            Pressing::Lost { known, why } => {
                match known {
                    Some(Press::Pressed { seq: at }) => {
                        approved.push(p.command.clone());
                        append_note(notes, &approved_note)?;
                        self.ledger_row(
                            rule_id,
                            approvals::Outcome::Approved,
                            &p.command,
                            "pressed; the connection was lost after",
                            seq,
                        );
                        review.approved(at, &p.command)?;
                    }
                    Some(Press::Skipped { .. } | Press::Changed { .. }) => append_note(
                        notes,
                        &format!(
                            "nothing pressed, the box had left the screen: {}",
                            p.command
                        ),
                    )?,
                    None => append_note(
                        notes,
                        &format!(
                            "pressed, no answer came; the box is read again after the \
                             reconnect: {}",
                            p.command
                        ),
                    )?,
                }
                return Err(Fail::Lost(why));
            }
        };
        self.refusal_pause = REFUSAL_PAUSE;
        match press {
            Press::Pressed { seq: at } => {
                self.changed_streak = 0;
                approved.push(p.command.clone());
                append_note(notes, &approved_note)?;
                self.ledger_row(rule_id, approvals::Outcome::Approved, &p.command, "", seq);
                review.approved(at, &p.command)?;
                // The box must LEAVE before the next look: `await gone` is
                // level-triggered and a box shows no busy footer, so an
                // immediate re-read of an unchanged screen would match the same
                // box and press it again — the stray digit the guard exists to
                // prevent.
                let screen = match self.moved_past(at, deadline)? {
                    Past::Moved => return Ok(Step::Again { settle: true }),
                    Past::Still(Some(screen)) => screen,
                    // The budget ran out waiting: nothing is read or handed
                    // over after it — the box as last read is the TIMEOUT's
                    // ([`Self::look`]).
                    Past::Still(None) => return Ok(Step::Review(turn)),
                };
                append_note(
                    notes,
                    &format!(
                        "handed to the manager (the box did not change after the press): {}",
                        p.command
                    ),
                )?;
                Ok(Step::Review(turn_of(screen)))
            }
            Press::Changed { seq: at } => {
                // The screen moved between the read and the press: read and
                // decide again — and after a streak of these, hand the box
                // over rather than press it unfenced ([`MAX_CHANGED_PRESSES`]).
                self.changed_streak += 1;
                self.ledger_row(
                    rule_id,
                    approvals::Outcome::Skipped,
                    &p.command,
                    "the screen moved past the judged read",
                    at,
                );
                if self.changed_streak >= MAX_CHANGED_PRESSES {
                    self.changed_streak = 0;
                    let why = format!(
                        "{MAX_CHANGED_PRESSES} fenced presses in a row did not land (the screen \
                         moved, or the key was not taken)"
                    );
                    append_note(
                        notes,
                        &format!("handed to the manager ({why}): {}", p.command),
                    )?;
                    self.box_reason = Some((review_key(&turn, allow), why));
                    return Ok(Step::Review(turn));
                }
                Ok(Step::Again { settle: false })
            }
            Press::Skipped { seq: at } => {
                self.changed_streak = 0;
                self.ledger_row(
                    rule_id,
                    approvals::Outcome::Skipped,
                    &p.command,
                    "the guarded row was not on the screen",
                    at,
                );
                if at == seq {
                    // The very screen we parsed, and the guard found no row:
                    // this box is not one the supervisor answers.
                    let why = "the guarded press matched no row";
                    append_note(
                        notes,
                        &format!("handed to the manager ({why}): {}", p.command),
                    )?;
                    self.box_reason = Some((review_key(&turn, allow), why.to_string()));
                    return Ok(Step::Review(turn));
                }
                append_note(
                    notes,
                    &format!(
                        "nothing pressed, the box had left the screen: {}",
                        p.command
                    ),
                )?;
                Ok(Step::Again { settle: false })
            }
        }
    }
}
