// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Raising and clearing worker `attention` for the menu bar.

use super::super::journal::{
    AGAIN_ADOPTED, AGAIN_RETRY, ATTENTION_KEPT, ATTENTION_SKIPPED, CLEARED_BOX, CLEARED_NEEDS,
    ESCALATED_BEHIND, NEEDS,
};
use super::*;
use crate::harness::hold_said::NeedTail;

use crate::supervise::policy::turn_end::REASON_CELLS;

const SUBJECT_CELLS: usize = APPROVAL_LINE_CELLS;

/// `s` on one line and within `cells` terminal cells ([`cut_cells`]).
fn clip_to(s: &str, cells: usize) -> String {
    let (whole, cut) = cut_cells(s, cells);
    if !cut {
        return whole;
    }
    let (mut out, _) = cut_cells(s, cells.saturating_sub(1));
    out.push('…');
    out
}

fn trust_path(rows: &[String]) -> Option<&str> {
    let title = aterm_phase::anchor!("trust.title");
    let at = rows.iter().position(|r| r.trim() == title)?;
    rows[at + 1..]
        .iter()
        .map(|r| r.trim())
        .find(|r| !r.is_empty())
}

/// The question above the box rows, where [`aterm_phase::QuestionDialog::question`] read none.
fn question_above(rows: &[String], first: usize) -> Option<&str> {
    for r in rows[first.saturating_sub(30)..first].iter().rev() {
        let t = r.trim();
        if r.starts_with('⏺') {
            return None;
        }
        if let Some(after) = r.strip_prefix('❯') {
            let after = after.trim_start();
            let digits = after.chars().take_while(char::is_ascii_digit).count();
            if digits == 0 || !after[digits..].starts_with('.') {
                return None;
            }
        }
        if t.ends_with('?') {
            return Some(t);
        }
    }
    None
}

fn kind_and_subject(reader: &dyn ScreenReader, point: &Turn) -> (String, String) {
    match reader.program() {
        aterm_phase::Program::Claude => {}
        aterm_phase::Program::Codex
        | aterm_phase::Program::Grok
        | aterm_phase::Program::Kimi
        | aterm_phase::Program::Agy
        | aterm_phase::Program::Muse
        | aterm_phase::Program::Generic => {
            return kind_and_subject_read(reader, point);
        }
    }
    let rows = &point.screen.rows;
    if point.phase == Phase::Idle {
        if let Some(w) = aterm_phase::wall(rows) {
            return ("wall".to_string(), one_line(&w.message));
        }
        let said = aterm_phase::said_tail(rows).unwrap_or_default();
        let last = said.lines().last().unwrap_or("").trim().to_string();
        return ("idle".to_string(), last);
    }
    if point.phase == Phase::Question {
        let asked = last_said_row(rows).unwrap_or("").trim();
        let asked = asked.strip_prefix('⏺').unwrap_or(asked).trim();
        return ("question".to_string(), asked.to_string());
    }
    if let Some(path) = trust_path(rows)
        && rows
            .iter()
            .any(|r| r.contains(aterm_phase::anchor!("trust.yes")))
    {
        return ("trust".to_string(), path.to_string());
    }
    let Some(p) = parse_prompt(rows) else {
        return ("other".to_string(), String::new());
    };
    let kind = if aterm_phase::parse_prompt_v2(rows).is_some_and(|v| v.rm_breaker().is_some()) {
        "rm-breaker".to_string()
    } else {
        p.kind.name().to_string()
    };
    let asked = (p.kind == PromptKind::Question)
        .then(|| aterm_phase::parse_prompt_v2(rows))
        .flatten()
        .and_then(|v| v.question_dialog)
        .and_then(|d| d.question().map(|q| q.text.clone()));
    let subject = if let Some(asked) = asked {
        asked
    } else if !p.command.is_empty() {
        p.command.clone()
    } else if p.kind == PromptKind::Workflow && !p.description.is_empty() {
        p.description.clone()
    } else {
        let first = prompt_box_first_row(rows);
        let footer = prompt_box_span(rows).map(|(_, b)| b);
        match (first, footer) {
            (Some(a), Some(b)) if a <= b => rows[a..=b]
                .iter()
                .rev()
                .map(|r| r.trim())
                .find(|r| r.ends_with('?'))
                .or_else(|| question_above(rows, a))
                .map_or_else(|| approval_line(rows), str::to_string),
            _ => approval_line(rows),
        }
    };
    (kind, subject)
}

/// [`kind_and_subject`] from a program's [`aterm_phase::Reading`] alone.
fn kind_and_subject_read(reader: &dyn ScreenReader, point: &Turn) -> (String, String) {
    let rows = &point.screen.rows;
    let last_said = || {
        reader
            .said_tail(rows)
            .and_then(|t| t.lines().last().map(|l| l.trim().to_string()))
            .unwrap_or_default()
    };
    match point.phase {
        Phase::Idle => match reader.wall(rows) {
            Some(w) => ("wall".to_string(), one_line(&w.message)),
            None => ("idle".to_string(), last_said()),
        },
        Phase::Question => ("question".to_string(), last_said()),
        _ => match reader.prompt(rows) {
            Some(p) => {
                let subject = [&p.command, &p.description, &p.title]
                    .into_iter()
                    .find(|t| !t.is_empty())
                    .cloned()
                    .unwrap_or_default();
                let kind = match p.kind {
                    PromptKind::Trust => "trust",
                    PromptKind::Question => "question",
                    k => k.name(),
                };
                (kind.to_string(), subject)
            }
            None => ("other".to_string(), String::new()),
        },
    }
}

/// `<program> <kind>: <subject>`, the subject cut at 64 cells.
pub(crate) fn point_label(reader: &dyn ScreenReader, point: &Turn) -> String {
    let (kind, subject) = kind_and_subject(reader, point);
    format!(
        "{} {kind}: {}",
        reader.program().name(),
        or_dash(&clip_to(&subject, SUBJECT_CELLS))
    )
}

/// `<program> <kind>: <subject> (<reason>)` ([`point_label`]), within [`ATTENTION_BYTES`].
pub(crate) fn attention_text(reader: &dyn ScreenReader, point: &Turn, reason: &str) -> String {
    let label = point_label(reader, point);
    crate::harness::resume::never_cut(reason, |reason| fit_reason(&label, reason))
}

/// `<label> (<reason>)`, the reason cut to fit ([`attention_text`]).
fn fit_reason(label: &str, reason: &str) -> String {
    let room = ATTENTION_BYTES.saturating_sub(label.len() + " ()".len());
    let reason = clip_bytes(clip_to(reason, REASON_CELLS), room);
    fit_bytes(&format!("{label} ({reason})"), ATTENTION_BYTES)
}

/// `s` within `room` bytes ([`ATTENTION_BYTES`]).
fn clip_bytes(s: String, room: usize) -> String {
    if s.len() <= room {
        return s;
    }
    let mut cut = fit_bytes(&s, room.saturating_sub('…'.len_utf8()));
    cut.truncate(cut.trim_end_matches('…').len());
    cut.push('…');
    cut
}

fn refused_cap(r: &CtlReply, sent: &str) -> Option<usize> {
    let err = r.err_text();
    let rest = &err[err.find("too long (max ")? + "too long (max ".len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits
        .parse()
        .ok()
        .filter(|&n: &usize| n > 0 && n < sent.len())
}

pub(crate) use super::super::screen::status_field;

/// What one escalation hands the person ([`Session::escalate`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Escalation {
    /// A box, a question, a point the turn-end policy hands over: the person is asked.
    Ask,
    /// A limit (`limited: <message>`).
    Limit { waits: bool },
}

impl Escalation {
    fn kind_token(self) -> Option<String> {
        match self {
            Self::Ask => None,
            Self::Limit { waits } => {
                let kind = if waits {
                    aterm_types::needs::AttentionKind::Wait
                } else {
                    aterm_types::needs::AttentionKind::Ask
                };
                Some(format!("kind={}", kind.word()))
            }
        }
    }
}

/// The supervisor's current intent, independent of its last acknowledged write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AttentionTarget {
    Set { text: String, kind: Escalation },
    Clear,
}

/// An ask badge withheld for [`Session::stall_suspect`], raised if the next probe finds no stall.
#[derive(Clone, Debug)]
pub(super) struct WithheldAsk {
    pub(super) text: String,
    pub(super) duty: Option<Scope>,
    pub(super) again: String,
    pub(super) turn_end_badge: bool,
}

impl<C: Ctl> Session<'_, C> {
    /// Current desired text, or an adopted entry before this loop changed it.
    pub(super) fn attention_text(&self) -> Option<&str> {
        match self.attention_desired.as_ref() {
            Some(AttentionTarget::Set { text, .. }) => Some(text),
            Some(AttentionTarget::Clear) => None,
            None => self.attention_ours.as_deref(),
        }
    }

    fn forget_attention(&mut self) {
        self.withheld_ask = None;
        self.attention_desired = None;
        self.attention_duty = None;
        self.attention_ours = None;
        self.attention_pending = false;
        self.attention_retry_at = None;
    }

    fn arm_attention_retry(&mut self) {
        self.attention_pending = true;
        self.attention_retry_at = Some(
            self.fx.mono()
                + self
                    .setup
                    .redecide_after_skip
                    .max(Duration::from_millis(100)),
        );
    }

    /// THE DUTY AN ATTENTION WRITE GOES UNDER: the one that raised the entry, else the one in hand.
    /// An entry this loop did not raise (a loop before it left it, or it came with an adopted box)
    /// has neither, and comes down as the turn end's: needs-you is the loop's own to take down, and
    /// the host's door refuses a write that names no duty, every retry, for the session's life.
    fn attention_scope(&self) -> Scope {
        self.attention_duty
            .or(self.duty.get())
            .unwrap_or(Scope::TurnEnd)
    }

    pub(super) fn write_attention_target(&mut self) -> Result<String, Fail> {
        let _duty = self.metadata_duty_guard(self.attention_scope());
        if self.handing_over() {
            self.arm_attention_retry();
            return Ok("skipped: the watcher is handing its badges over".to_string());
        }
        if self.metadata_context == claim::MetadataContext::Normal && self.metadata_requires_proof()
        {
            self.arm_attention_retry();
            match self.metadata_authority(true) {
                Ok(claim::MetadataAuthority::Ours) => {
                    return self.metadata_only(true, |s| s.write_attention_target());
                }
                Ok(claim::MetadataAuthority::Gone) => {
                    self.forget_attention();
                    return Ok(ATTENTION_KEPT.to_string());
                }
                Ok(claim::MetadataAuthority::Unknown) => {
                    self.arm_attention_retry();
                    return Ok("unconfirmed ownership".to_string());
                }
                Err(why) => {
                    self.arm_attention_retry();
                    return Err(why);
                }
            }
        }
        if self.metadata_context != claim::MetadataContext::Normal
            && self.metadata_mode() == Some(Autopilot::Off)
            && matches!(self.attention_desired, Some(AttentionTarget::Set { .. }))
        {
            self.attention_desired = Some(AttentionTarget::Clear);
            let said = self.write_attention_target()?;
            return Ok(format!(
                "skipped: the session's autopilot is off; cleanup {said}"
            ));
        }
        if self.stopped() && matches!(self.attention_desired, Some(AttentionTarget::Set { .. })) {
            self.arm_attention_retry();
            return Ok("the watcher stopped before publication".to_string());
        }
        let Some(target) = self.attention_desired.clone() else {
            return Ok("none".to_string());
        };
        self.arm_attention_retry();
        let owner = format!("owner={ATTENTION_OWNER}");
        let request = |text: &str, kind: Escalation| {
            ["meta", "set", "attention", owner.as_str()]
                .into_iter()
                .map(str::to_string)
                .chain(kind.kind_token())
                .chain(std::iter::once(text.to_string()))
                .collect::<Vec<_>>()
        };
        let result = (|| match target {
            AttentionTarget::Set { mut text, kind } => {
                let args = request(&text, kind);
                let mut r = self.call(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
                if let Some(cap) = refused_cap(&r, &text) {
                    text = fit_bytes(&text, cap);
                    self.attention_desired = Some(AttentionTarget::Set {
                        text: text.clone(),
                        kind,
                    });
                    let args = request(&text, kind);
                    r = self.call(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
                }
                if r.ok() {
                    self.attention_ours = Some(text);
                }
                Ok(r)
            }
            AttentionTarget::Clear => {
                let r = self.call(&["meta", "unset", "attention", &owner])?;
                if r.ok() {
                    self.attention_ours = None;
                }
                Ok(r)
            }
        })();
        match result {
            Ok(r) => {
                if r.ok() {
                    self.attention_pending = false;
                    self.attention_retry_at = None;
                } else if self.claim.watching_behind().is_some() {
                    self.forget_attention();
                } else {
                    self.arm_attention_retry();
                }
                Ok(reply_word(&r))
            }
            Err(why) => {
                self.arm_attention_retry();
                Err(why)
            }
        }
    }

    /// Request cleanup even where the original SET was never acknowledged.
    pub(super) fn clear_attention_entry(&mut self) -> Result<String, Fail> {
        self.withheld_ask = None;
        self.attention_desired = Some(AttentionTarget::Clear);
        self.write_attention_target()
    }

    /// Final cleanup remains owed even after the box, limit or turn-end flag
    /// was consumed by an earlier unsuccessful close.
    pub(super) fn finish_supervisor_attention(
        &mut self,
        seq: u64,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        if !self.attention_pending
            && !matches!(self.attention_desired, Some(AttentionTarget::Set { .. }))
        {
            return Ok(());
        }
        let said = self.clear_attention_entry()?;
        self.note_attention_clear(seq, "", &said, review);
        Ok(())
    }

    /// Journal cleanup only as acknowledged once its native entry is down.
    pub(super) fn note_attention_clear(
        &self,
        seq: u64,
        what: &str,
        said: &str,
        review: &mut dyn Review,
    ) {
        if self.attention_pending && matches!(self.attention_desired, Some(AttentionTarget::Clear))
        {
            review.note(&format!(
                "SKIPPED seq={seq} {what} attention={said}; take-down still owed"
            ));
        } else {
            review.note(&format!("CLEARED seq={seq} {what}attention={said}"));
        }
    }

    /// Retry metadata at the point still showing, under a freshly verified
    /// existing lease. It acquires no claim and asks no program question again.
    pub(super) fn retry_supervisor_attention(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let _duty = self.metadata_duty_guard(self.attention_scope());
        self.metadata_global = opts.policy.autopilot;
        self.arm_attention_retry();
        match self.metadata_authority(false) {
            Ok(claim::MetadataAuthority::Ours) => {}
            Ok(claim::MetadataAuthority::Gone) => {
                self.forget_attention();
                return Ok(());
            }
            Ok(claim::MetadataAuthority::Unknown) => {
                self.arm_attention_retry();
                return Ok(());
            }
            Err(why) => {
                self.arm_attention_retry();
                return Err(why);
            }
        }
        self.metadata_only(false, |s| {
            if matches!(s.attention_desired, Some(AttentionTarget::Set { .. }))
                && s.metadata_mode().is_none()
            {
                s.arm_attention_retry();
                return Ok(());
            }
            if s.metadata_mode() == Some(Autopilot::Off) {
                s.attention_desired = Some(AttentionTarget::Clear);
            }
            if let Some(AttentionTarget::Set { kind, .. }) = s.attention_desired.as_ref()
                && s.withhold_badge(screen.seq, *kind, review)?
            {
                s.arm_attention_retry();
                return Ok(());
            }
            let clearing = matches!(s.attention_desired, Some(AttentionTarget::Clear));
            let said = s.write_attention_target()?;
            if clearing {
                s.note_attention_clear(screen.seq, "", &said, review);
            } else {
                review.note(&format!(
                    "ESCALATED seq={} attention={said} again={AGAIN_RETRY}, the pending metadata",
                    screen.seq
                ));
            }
            Ok(())
        })
    }

    /// Pace needs metadata recovery separately from the policy's own deadline.
    fn arm_needs_retry(&mut self) {
        self.needs_pending = true;
        self.needs_retry_at = Some(
            self.fx.mono()
                + self
                    .setup
                    .redecide_after_skip
                    .max(Duration::from_millis(100)),
        );
    }

    /// A needs write retains cleanup before transport and publishes its host
    /// projection only after authority and mode checks, before the native write.
    /// A refusal or lost request waits from completion before another attempt.
    fn request_needs(
        &mut self,
        args: &[&str],
        key: u64,
        needs: &[String],
        entry: &str,
    ) -> Result<CtlReply, Fail> {
        self.arm_needs_retry();
        let refused = |why: &str| CtlReply {
            code: 1,
            stdout: String::new(),
            stderr: format!("ERR {why}"),
        };
        if self.handing_over() {
            return Ok(refused("the watcher is handing its needs entry over"));
        }
        if self.metadata_context == claim::MetadataContext::Normal && self.metadata_requires_proof()
        {
            match self.metadata_authority(true) {
                Ok(claim::MetadataAuthority::Ours) => {
                    return self.metadata_only(true, |s| s.request_needs(args, key, needs, entry));
                }
                Ok(claim::MetadataAuthority::Gone) => {
                    self.forget_needs();
                    return Ok(CtlReply {
                        code: 1,
                        stdout: String::new(),
                        stderr: "ERR metadata ownership changed".to_string(),
                    });
                }
                Ok(claim::MetadataAuthority::Unknown) => {
                    self.arm_needs_retry();
                    return Ok(CtlReply {
                        code: 1,
                        stdout: String::new(),
                        stderr: "ERR unconfirmed metadata ownership".to_string(),
                    });
                }
                Err(why) => {
                    self.arm_needs_retry();
                    return Err(why);
                }
            }
        }
        let inactive = |s: &Self| {
            args.get(1) == Some(&"set")
                && (s.stopped()
                    || s.autopilot.word() == Some(Autopilot::Off)
                    || s.metadata_mode() == Some(Autopilot::Off))
        };
        if inactive(self) {
            self.arm_needs_retry();
            return Ok(refused("the needs publication is no longer active"));
        }
        self.tell_needs(key, needs, entry);
        if self.handing_over() {
            self.arm_needs_retry();
            return Ok(refused("the watcher is handing its needs entry over"));
        }
        if inactive(self) {
            self.arm_needs_retry();
            return Ok(refused("the needs publication is no longer active"));
        }
        let reply = self.call(args);
        if !matches!(&reply, Ok(r) if r.ok()) {
            self.arm_needs_retry();
        }
        reply
    }

    fn forget_needs(&mut self) {
        self.needs_told = None;
        self.needs_told_text.clear();
        self.needs_pending = false;
        self.needs_retry_at = None;
        self.tell_needs(0, &[], "");
    }

    /// Retry only needs metadata on an unchanged point. The session's fresh
    /// Autopilot word may require cleanup instead of restoring an entry.
    pub(super) fn retry_needs(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let _duty = self.metadata_duty_guard(Scope::TurnEnd);
        self.metadata_global = opts.policy.autopilot;
        // A refused or lost policy read also waits before another attempt.
        self.arm_needs_retry();
        match self.metadata_authority(false) {
            Ok(claim::MetadataAuthority::Ours) => {}
            Ok(claim::MetadataAuthority::Gone) => {
                self.forget_needs();
                return Ok(());
            }
            Ok(claim::MetadataAuthority::Unknown) => {
                self.arm_needs_retry();
                return Ok(());
            }
            Err(why) => {
                self.arm_needs_retry();
                return Err(why);
            }
        }
        self.metadata_only(false, |s| {
            if s.turn_end.shown_key() != 0 && s.metadata_mode().is_none() {
                s.arm_needs_retry();
                return Ok(());
            }
            if s.metadata_mode() == Some(Autopilot::Off) {
                s.needs_down(screen.seq, "the session's autopilot is off", review)
            } else {
                s.sync_needs(screen, opts, review)
            }
        })
    }

    /// Escalate via `meta set attention <text>` on the worker (bounded to the server cap) and
    /// journal `ESCALATED seq=<n> attention=<reply>`.
    pub(super) fn escalate(
        &mut self,
        seq: u64,
        text: &str,
        again: Option<u64>,
        kind: Escalation,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        if let Some(holder) = self.claim.watching_behind() {
            review.note(&format!(
                "ESCALATED seq={seq} {ATTENTION_SKIPPED}{ESCALATED_BEHIND} ({holder}) answers \
                 this session"
            ));
            return Ok(());
        }
        let duty = self.duty.get().or(match kind {
            Escalation::Limit { .. } => Some(Scope::Walls),
            Escalation::Ask => None,
        });
        let adopted =
            matches!(kind, Escalation::Limit { .. }) && std::mem::take(&mut self.adopted_limit);
        let again = match again {
            Some(of) => format!(" again={AGAIN_RETRY}, the episode of seq={of}"),
            None if adopted => format!(" again={AGAIN_ADOPTED}"),
            None => String::new(),
        };
        let text = fit_bytes(text, ATTENTION_BYTES);
        if self.withhold_badge(seq, kind, review)? {
            self.withheld_ask = Some(WithheldAsk {
                text,
                duty,
                again,
                turn_end_badge: false,
            });
            return Ok(());
        }
        self.withheld_ask = None;
        self.attention_duty = duty;
        self.attention_desired = Some(AttentionTarget::Set { text, kind });
        let said = self.write_attention_target()?;
        review.note(&format!("ESCALATED seq={seq} attention={}{again}", said));
        Ok(())
    }

    /// Escalate a box the loop did not answer, or a point the turn-end policy hands over, once
    /// per review point (`reason`: why, such as a question the owner's policy may not answer).
    pub(super) fn escalate_point(
        &mut self,
        point: &Turn,
        reason: Option<&str>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let reader = self.reader(&point.screen.rows);
        // A dialog of no kind aterm reads has no duty to answer it ([`duty::prompt_scope`]), and
        // telling a person of it needs one: it is the approvals' to tell, whose words it carries
        // ("no approval rule answers this box"), and only the badge goes under that duty.
        let scope = if let Some(prompt) = reader.prompt(&point.screen.rows) {
            duty::prompt_scope(prompt.kind).unwrap_or(Scope::Approvals)
        } else if reader.wall(&point.screen.rows).is_some() {
            Scope::Walls
        } else if point.phase == Phase::Question {
            Scope::Questions
        } else {
            Scope::TurnEnd
        };
        let _duty = self.metadata_duty_guard(scope);
        let key = self.review_key(point);
        if std::mem::take(&mut self.adopted_box)
            && self.box_ask.as_ref().is_some_and(|b| b.key == key)
        {
            return Ok(());
        }
        if point.phase == Phase::Question
            && last_said_row(&point.screen.rows).is_some_and(|r| r.trim_start().starts_with('❯'))
        {
            return Ok(());
        }
        let reason = match (reason, &point.phase, &self.box_reason) {
            (Some(why), _, _) => why.to_string(),
            (None, _, Some((k, why))) if *k == key => why.clone(),
            _ => "no approval rule answers this box".to_string(),
        };
        if let Some(notice) = self.exception_now.as_ref().and_then(|e| e.notice.as_ref()) {
            review.note(&format!(
                "NOT ESCALATED seq={} the notice {} stands: {}",
                point.screen.seq,
                notice.id,
                clip(&reason)
            ));
            return Ok(());
        }
        let text = attention_text(self.reader(&point.screen.rows), point, &reason);
        self.escalate(point.screen.seq, &text, None, Escalation::Ask, review)?;
        self.box_ask = Some(BoxAsk { key, memory: false });
        Ok(())
    }

    /// Unset this loop's own attention entry (`meta unset attention owner=supervisor`) when
    /// `ours` says its text is the kind being closed.
    pub(super) fn unset_if_ours(&mut self, ours: fn(&str) -> bool) -> Result<String, Fail> {
        if self.withheld_ask.as_ref().is_some_and(|w| ours(&w.text)) {
            self.withheld_ask = None;
        }
        Ok(match self.attention_text() {
            Some(text) if ours(text) => return self.clear_attention_entry(),
            Some(_) => ATTENTION_KEPT.to_string(),
            None if self.attention_pending
                && matches!(self.attention_desired, Some(AttentionTarget::Clear)) =>
            {
                return self.write_attention_target();
            }
            None => "none".to_string(),
        })
    }

    /// The escalated point has left the screen.
    pub(super) fn close_box(&mut self, seq: u64, review: &mut dyn Review) -> Result<(), Fail> {
        self.adopted_box = false;
        if self.box_ask.take().is_none() {
            return Ok(());
        }
        let said = self.unset_if_ours(|t| !t.starts_with(ATTENTION_PREFIX))?;
        self.note_attention_clear(seq, &format!("{CLEARED_BOX} "), &said, review);
        Ok(())
    }

    /// The episode is over: clear the `limited:` attention if still ours (another writer's badge
    /// stays) and journal `CLEARED seq=<n> attention=<said> <why>`.
    pub(super) fn close_episode(
        &mut self,
        seq: u64,
        why: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        if self.limit.take().is_none() {
            return Ok(());
        }
        if let Some(host) = &self.stall_host {
            host.limited(false);
        }
        let said = self.unset_if_ours(|t| t.starts_with(ATTENTION_PREFIX))?;
        if self.attention_pending && matches!(self.attention_desired, Some(AttentionTarget::Clear))
        {
            review.note(&format!(
                "SKIPPED seq={seq} attention={said} {why}; take-down still owed"
            ));
        } else {
            review.note(&format!("CLEARED seq={seq} attention={said} {why}"));
        }
        Ok(())
    }

    /// Take down a limit's badge adopted from a previous loop ([`Session::adopted_limit`]).
    pub(super) fn lower_adopted_limit(
        &mut self,
        seq: u64,
        why: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        if !self
            .attention_text()
            .is_some_and(|t| t.starts_with(ATTENTION_PREFIX))
        {
            return Ok(());
        }
        let said = self.unset_if_ours(|t| t.starts_with(ATTENTION_PREFIX))?;
        if self.attention_pending {
            review.note(&format!(
                "SKIPPED seq={seq} attention={said} {why}; take-down still owed"
            ));
        } else {
            review.note(&format!("CLEARED seq={seq} attention={said} {why}"));
        }
        Ok(())
    }

    /// The worker answered on a point that is not the limit's notice (a look's point, or a read
    /// of an Off park), with no episode open.
    pub(super) fn adopted_limit_over(
        &mut self,
        seq: u64,
        why: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        if self.limit.is_some() || !std::mem::take(&mut self.adopted_limit) {
            return Ok(());
        }
        self.lower_adopted_limit(seq, why, review)
    }
}

/// The key [`Session::needs_told`] holds for a needs entry a loop before this one left.
pub(super) const NEEDS_LEFT: u64 = 0;

/// Why the entry a loop before this one left ([`NEEDS_LEFT`]) is taken down.
pub(super) const NEEDS_LEFT_DOWN: &str = "no need stands at the loop's start";

/// Why the entry is taken down where every need standing is a line the owner's standing go-ahead
/// answers (DESIGN-harness-autopilot §15.6).
pub(super) const NEEDS_ANSWERED_DOWN: &str = "the go-ahead answers what stood";

/// Whether a `meta` reply's `attention_kinds=` (`<ask|wait|->:<owner>` per entry, comma-joined,
/// oldest first; an owner may hold a `:`) names an entry of `owner`.
pub(super) fn kinds_name_owner(kinds: &str, owner: &str) -> bool {
    kinds
        .split(',')
        .any(|entry| entry.split_once(':').is_some_and(|(_, o)| o == owner))
}

/// The needs entry's text: the first need and a count of the rest, then what the harness does
/// beside them (`tail`), within [`ATTENTION_BYTES`].
pub(super) fn needs_attention_text(program: &str, needs: &[String], tail: NeedTail) -> String {
    let first = needs.first().map_or("", String::as_str);
    let more = match needs.len() {
        0 | 1 => String::new(),
        n => format!(" (+{} more)", n - 1),
    };
    let head = format!("{program} needs you: ");
    let tail = format!("{more} \u{2014} {}", tail.words());
    let room = ATTENTION_BYTES.saturating_sub(head.len() + tail.len());
    let need = clip_bytes(clip_to(first, SUBJECT_CELLS), room);
    fit_bytes(&format!("{head}{need}{tail}"), ATTENTION_BYTES)
}

/// The key the needs entry is told under ([`Session::needs_told`]): the set's own
/// (`TurnEndState::shown_key`, `0` for none) where the harness carries on beside it, another
/// where the work waits only on the person, so the entry's words follow a rest or a landing that
/// leaves the set as it was.
fn entry_key(shown: u64, tail: NeedTail) -> u64 {
    match (shown, tail) {
        (0, _) | (_, NeedTail::CarryingOn) => shown,
        (key, NeedTail::WaitsOnYou) => (key ^ 0x5741_4954_535f_4f4e).max(1),
    }
}

impl<C: Ctl> Session<'_, C> {
    /// WHAT THE NEEDS ENTRY SAYS THE HARNESS DOES BESIDE THE NEEDS under `mode` ([`NeedTail`]):
    /// under Land, where it rests on them or the work landed beside them, the work waits only on
    /// the person; it carries on otherwise.
    fn need_tail(&self, mode: Autopilot) -> NeedTail {
        if mode == Autopilot::Land && (self.turn_end.land_resting() || self.turn_end.landed()) {
            NeedTail::WaitsOnYou
        } else {
            NeedTail::CarryingOn
        }
    }

    /// Bring the needs entry in line with the worker's standing needs less the lines the owner's
    /// standing go-ahead answers (`TurnEndState::shown_needs`, DESIGN-harness-autopilot §15.6):
    /// raise it (loud, never blocking; not under Off), or take it down when none of them stands or
    /// another supervisor holds it.
    pub(super) fn sync_needs(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        self.sync_needs_under(screen, &opts.policy, review)
    }

    /// [`Self::sync_needs`] under the policy `cfg`, which a break's read of the needs its report
    /// names (`Session::break_needs`) holds where no options do.
    pub(super) fn sync_needs_under(
        &mut self,
        screen: &Screen,
        cfg: &super::super::config::SupervisorConfig,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let _duty = self.metadata_duty_guard(Scope::TurnEnd);
        self.metadata_global = cfg.autopilot;
        if self.needs_not_ours() {
            return self.needs_down(
                screen.seq,
                "another supervisor holds the needs entry",
                review,
            );
        }
        let seq = screen.seq;
        let tail = self.need_tail(cfg.autopilot_for(self.autopilot.word()));
        let key = entry_key(self.turn_end.shown_key(), tail);
        if key == 0 {
            let why = if self.needs_told == Some(NEEDS_LEFT) {
                NEEDS_LEFT_DOWN
            } else if self.turn_end.needs_answered() > 0 {
                NEEDS_ANSWERED_DOWN
            } else {
                "none stands now"
            };
            return self.needs_down(seq, why, review);
        }
        if self.needs_told == Some(key) && !self.needs_pending {
            self.retell_standing(key);
            return Ok(());
        }
        if cfg.autopilot_for(self.autopilot.word()) == Autopilot::Off {
            return Ok(());
        }
        let needs = self.turn_end.shown_needs().to_vec();
        let program = self.reader(&screen.rows).program().name();
        let mut text = needs_attention_text(program, &needs, tail);
        let owner = format!("owner={NEEDS_ATTENTION_OWNER}");
        let kind = format!("kind={}", aterm_types::needs::AttentionKind::Ask.word());
        self.arm_needs_retry();
        let mut r = self.request_needs(
            &["meta", "set", "attention", &owner, &kind, &text],
            key,
            &needs,
            &text,
        )?;
        if let Some(cap) = refused_cap(&r, &text) {
            text = fit_bytes(&text, cap);
            r = self.request_needs(
                &["meta", "set", "attention", &owner, &kind, &text],
                key,
                &needs,
                &text,
            )?;
        }
        if r.lost() {
            return Ok(());
        }
        if r.ok() {
            self.needs_told = Some(key);
            self.needs_told_text = text;
            self.needs_pending = false;
            self.needs_retry_at = None;
        }
        let first = needs.first().map_or("", String::as_str);
        let answered = match self.turn_end.needs_answered() {
            0 => String::new(),
            n => format!(" answered={n}"),
        };
        review.note(&format!(
            "{NEEDS} seq={seq} count={} attention={}{answered} {}",
            needs.len(),
            reply_word(&r),
            clip_to(first, SUBJECT_CELLS)
        ));
        Ok(())
    }

    /// Whether the needs entry is owed a write, its retry due ([`Self::arm_needs_retry`]): a write
    /// of it is pending, or the set it was told is not the one the menu bar shows
    /// (`TurnEndState::shown_key`). An entry a loop before this one left ([`NEEDS_LEFT`]) where
    /// none is shown is owed nothing here: a point takes it down ([`NEEDS_LEFT_DOWN`]).
    pub(super) fn needs_owed(&self) -> bool {
        let told = self.needs_told.unwrap_or(NEEDS_LEFT);
        let tail = self.need_tail(self.autopilot.word().unwrap_or(self.metadata_global));
        (self.needs_pending || told != entry_key(self.turn_end.shown_key(), tail))
            && self.needs_retry_at.is_none_or(|at| self.fx.mono() >= at)
    }

    /// Take the needs entry down for `why` (none stands now, an Off park, the loop's end).
    pub(super) fn needs_down(
        &mut self,
        seq: u64,
        why: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let _duty = self.metadata_duty_guard(Scope::TurnEnd);
        if self.handing_over() {
            return Ok(());
        }
        if self.needs_told.is_none() && !self.needs_pending {
            self.needs_retry_at = None;
            if self.needs_heard.is_some() {
                self.tell_needs(0, &[], "");
            }
            return Ok(());
        }
        if self.needs_not_ours() {
            self.needs_told = None;
            self.needs_told_text.clear();
            self.needs_pending = false;
            self.needs_retry_at = None;
            if self.needs_heard.is_some() {
                self.tell_needs(0, &[], "");
            }
            return Ok(());
        }
        let owner = format!("owner={NEEDS_ATTENTION_OWNER}");
        let r = self.request_needs(&["meta", "unset", "attention", &owner], 0, &[], "")?;
        if !r.ok() {
            if !r.lost() && self.needs_pending {
                review.note(&format!(
                    "SKIPPED seq={seq} needs attention={} {why}; take-down still owed",
                    reply_word(&r)
                ));
            }
            return Ok(());
        }
        self.needs_told = None;
        self.needs_told_text.clear();
        self.needs_pending = false;
        self.needs_retry_at = None;
        review.note(&format!(
            "CLEARED seq={seq} {CLEARED_NEEDS} attention={} {why}",
            reply_word(&r)
        ));
        Ok(())
    }

    /// Tell the window's host `key`'s needs set with `entry` ([`IdleHost::needs_standing`]),
    /// unless it heard that last.
    fn tell_needs(&mut self, key: u64, needs: &[String], entry: &str) {
        if self.handing_over() {
            return;
        }
        let Some(host) = &self.stall_host else {
            return;
        };
        if self.needs_heard_as(key, entry) {
            return;
        }
        host.needs_standing(needs, entry);
        self.needs_heard = Some((key, entry.to_string()));
    }

    /// Whether the window's host was last told `key`'s set with `entry` ([`Session::needs_heard`]).
    fn needs_heard_as(&self, key: u64, entry: &str) -> bool {
        self.needs_heard
            .as_ref()
            .is_some_and(|(k, told)| *k == key && told == entry)
    }

    /// Tell the window's host again what stands, unless it heard that last.
    fn retell_standing(&mut self, key: u64) {
        if self.stall_host.is_none() || self.needs_heard_as(key, &self.needs_told_text) {
            return;
        }
        let needs = self.turn_end.shown_needs().to_vec();
        let entry = self.needs_told_text.clone();
        self.tell_needs(key, &needs, &entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE: &dyn ScreenReader = &aterm_phase::ClaudeReader;

    fn turn(phase: Phase, rows: Vec<String>) -> Turn {
        Turn {
            phase,
            screen: Screen {
                rows,
                ..Screen::default()
            },
            timed_out: false,
        }
    }

    #[test]
    fn the_attention_names_the_kind_the_subject_and_the_reason() {
        let rows: Vec<String> = include_str!("policy/fixtures/cap-rm.txt")
            .lines()
            .map(str::to_string)
            .collect();
        let t = attention_text(
            CLAUDE,
            &turn(Phase::Prompt, rows),
            "rm circuit breaker (newline reading): unresolved $1",
        );
        assert!(t.starts_with("claude rm-breaker: "), "{t}");
        assert!(
            t.contains("rm -rf $S/$1") && t.contains("unresolved $1"),
            "{t}"
        );
        let trust: Vec<String> = include_str!("policy/fixtures/cap-trust.txt")
            .lines()
            .map(str::to_string)
            .collect();
        let t = attention_text(CLAUDE, &turn(Phase::Prompt, trust), "x");
        assert!(t.starts_with("claude trust: "), "{t}");
        assert!(t.contains("/private/tmp/claude-502/scratch/work1"), "{t}");
    }

    #[test]
    fn an_rm_breaker_boxs_attention_fits_the_keyed_cap_and_stays_closed() {
        let rows: Vec<String> = include_str!("policy/fixtures/cap-rm.txt")
            .lines()
            .map(str::to_string)
            .collect();
        let why = "the box carries a vendor note: Dangerous rm operation on possibly-empty \
                   variable path: $S/$1 in `rm -rf $S/$1` (bind $1 and rewrite its $S as \
                   \"${S:?}\" or use a literal path) ⚠ Claude Code will automatically deny \
                   this request in 1:59, to avoid blocking progress on an unattended session";
        let t = attention_text(CLAUDE, &turn(Phase::Prompt, rows), why);
        assert!(t.len() <= 200, "{} bytes: {t}", t.len());
        assert!(t.ends_with("…)"), "{t}");
        assert!(t.starts_with("claude rm-breaker: "), "{t}");
    }

    #[test]
    fn a_question_dialogs_badge_names_its_question() {
        let rule = "─".repeat(120);
        let mut rows: Vec<String> = [
            "⏺ chunk 3 done",
            "",
            "❯ Stop now and ask me to choose between option A (delete a.txt) and option B.",
            &rule,
            " ☐ a.txt",
            "",
            "What should I do with a.txt?",
            "",
            "❯ 1. Delete a.txt",
            "     Remove the a.txt file",
            "  2. Keep it",
            "     Leave a.txt in place",
            "  3. Type something.",
            &rule,
            "  4. Chat about this",
            "",
            "Enter to select · ↑/↓ to navigate · Esc to cancel",
        ]
        .map(str::to_string)
        .to_vec();
        let t = attention_text(CLAUDE, &turn(Phase::Prompt, rows.clone()), "no rule");
        assert!(t.contains("What should I do with a.txt?"), "{t}");
        assert!(!t.contains("Chat about this"), "{t}");
        rows[6] = "Pick one.".to_string();
        rows[0] = "⏺ Which one should I do?".to_string();
        let t = attention_text(CLAUDE, &turn(Phase::Prompt, rows), "no rule");
        assert!(!t.contains("Which one should I do?"), "{t}");
    }

    #[test]
    fn a_question_read_whole_is_badged_by_its_question() {
        use aterm_phase::prompt::fixtures as f;
        let rows = f::screen(f::QUESTION_TABS_INCIDENT);
        let t = attention_text(
            CLAUDE,
            &turn(Phase::Prompt, rows),
            "a person has begun answering",
        );
        assert!(
            t.starts_with("claude question: On a row with a progress bar, how should its"),
            "{t}"
        );
        assert!(!t.contains('│') && !t.contains("Chat about this"), "{t}");
    }

    #[test]
    fn a_long_subject_and_reason_are_clipped_and_the_whole_fits_256_bytes() {
        let mut rows = aterm_phase::prompt::fixtures::bash_one_row();
        let at = rows
            .iter()
            .position(|r| r == "   git log --oneline -5")
            .expect("row");
        rows[at] = format!("   echo {}", "é".repeat(300));
        let t = attention_text(CLAUDE, &turn(Phase::Prompt, rows), &"why ".repeat(100));
        assert!(t.len() <= ATTENTION_BYTES, "{}", t.len());
        assert!(t.starts_with("claude bash: echo é"), "{t}");
        assert!(t.contains('…'));
    }

    #[test]
    fn the_needs_entry_keeps_its_frame_for_a_long_need_in_any_script() {
        for letter in ["\u{915}", "\u{e01}", "\u{9a8}", "\u{25b8}"] {
            let needs = vec![letter.repeat(80), "sign 0.48.0".to_string()];
            let t = needs_attention_text("Muse Code", &needs, NeedTail::CarryingOn);
            assert!(t.len() <= ATTENTION_BYTES, "{letter}: {}", t.len());
            assert!(
                t.starts_with(&format!("Muse Code needs you: {letter}"))
                    && t.ends_with("\u{2026} (+1 more) \u{2014} carrying on"),
                "{letter}: {t}"
            );
        }
        let need = "grant Screen Recording to com.aterm.aterm".to_string();
        assert_eq!(
            needs_attention_text("claude", std::slice::from_ref(&need), NeedTail::CarryingOn),
            "claude needs you: grant Screen Recording to com.aterm.aterm \u{2014} carrying on"
        );
        assert_eq!(
            needs_attention_text("agy", &[need], NeedTail::WaitsOnYou),
            "agy needs you: grant Screen Recording to com.aterm.aterm \u{2014} waits on you"
        );
    }

    #[test]
    fn the_memory_escalation_names_its_conversation_whole_or_not_at_all() {
        const ID: &str = "5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f";
        let mut rows = aterm_phase::prompt::fixtures::bash_one_row();
        let at = rows
            .iter()
            .position(|r| r == "   git log --oneline -5")
            .expect("row");
        rows[at] = format!("   echo {}", "x".repeat(200));
        let point = turn(Phase::Prompt, rows);
        let banner = format!("{} (140.4GB)", aterm_phase::anchor!("wall.memory"));
        let flagged =
            format!("claude --dangerously-skip-permissions --model claude-opus-4-1 --resume {ID}");
        let reason = |line: &str| {
            format!(
                "memory critical: restart it{}{line}: {banner}",
                crate::harness::resume::THEN_RESUME_WITH
            )
        };
        let whole_or_absent = |t: &str| {
            t.match_indices("resume with claude")
                .all(|(i, _)| t[i..].contains(&format!("--resume {ID}")))
                && t.match_indices("--resum")
                    .all(|(i, _)| t[i..].starts_with(&format!("--resume {ID}")))
        };
        for (name, reason) in [
            ("flagged", reason(&flagged)),
            (
                "flagged, (relaunch is off)",
                format!("{} (relaunch is off)", reason(&flagged)),
            ),
            (
                "behind a failed restart",
                format!(
                    "the restart could not be made (failed:signal-refused): {}",
                    reason(&flagged)
                ),
            ),
            ("bare", reason(&format!("claude --resume {ID}"))),
        ] {
            let t = attention_text(CLAUDE, &point, &reason);
            assert!(t.len() <= ATTENTION_BYTES, "{name}: {}", t.len());
            assert!(whole_or_absent(&t), "{name}: a cut id: {t}");
            assert!(t.starts_with("claude bash: echo x"), "{name}: {t}");
        }
        let t = attention_text(CLAUDE, &point, &reason(&flagged));
        assert!(
            t.contains(&format!("then resume with claude --resume {ID}")),
            "{t}"
        );
        let label = point_label(CLAUDE, &point);
        let old = fit_reason(&label, &reason(&flagged));
        assert!(
            old.contains("then resume with claude --") && !old.contains(ID),
            "{old}"
        );
    }

    #[test]
    fn a_status_field_is_read_off_the_ok_line() {
        let st = "OK schema=1 sid=- hold=0 fabric=absent\n";
        assert_eq!(status_field(st, "fabric"), Some("absent"));
        assert_eq!(status_field(st, "hold"), Some("0"));
        assert_eq!(status_field(st, "identity"), None);
        assert_eq!(status_field(st, "nope"), None);
    }

    #[test]
    fn every_rm_breaker_kind_is_badged_rm_breaker() {
        use aterm_phase::prompt::fixtures as f;
        let incident = f::screen(f::BOX_RM_UNRESOLVABLE_WORKFLOW);
        let (kind, _) =
            kind_and_subject(&aterm_phase::ClaudeReader, &turn(Phase::Prompt, incident));
        assert_eq!(kind, "rm-breaker");
        let (kind, _) = kind_and_subject(
            &aterm_phase::ClaudeReader,
            &turn(Phase::Prompt, f::screen(f::BOX_RM)),
        );
        assert_eq!(kind, "rm-breaker");
        let (kind, _) = kind_and_subject(
            &aterm_phase::ClaudeReader,
            &turn(Phase::Prompt, f::bash_one_row()),
        );
        assert_eq!(kind, "bash");
        for (text, want) in [
            (f::BOX_RM_TOO_MANY_2_1_307, "rm-breaker"),
            (f::BOX_RM_TOO_MANY_NO_RM_2_1_307, "rm-breaker"),
            (f::BOX_RM_TOO_MANY_NO_RM_BYPASS_2_1_307, "rm-breaker"),
            (f::BOX_RM_NESTED_2_1_307, "rm-breaker"),
            (f::BOX_RM_MOUNTED_DRIVE_2_1_307, "rm-breaker"),
            (f::BOX_BASH_TOUCH_2_1_307, "bash"),
            (f::BOX_STATEMENT_FOLLOWS_16_2_1_307, "bash"),
            (f::BOX_RM_TOO_MANY_2_1_308, "rm-breaker"),
            (f::BOX_RM_TOO_MANY_NO_RM_2_1_308, "rm-breaker"),
            (f::BOX_RM_MOUNTED_DRIVE_2_1_308, "rm-breaker"),
            (f::BOX_RM_TOO_MANY_NO_RM_BYPASS_2_1_308, "rm-breaker"),
            (f::BOX_RM_NESTED_2_1_308, "rm-breaker"),
            (f::BOX_BASH_TOUCH_2_1_308, "bash"),
            (f::BOX_STATEMENT_FOLLOWS_16_2_1_308, "bash"),
            (f::BOX_RM_PARSE_DIVERGED_2_1_309, "rm-breaker"),
            (f::BOX_BASH_TOUCH_2_1_309, "bash"),
        ] {
            let (kind, _) = kind_and_subject(
                &aterm_phase::ClaudeReader,
                &turn(Phase::Prompt, f::screen(text)),
            );
            assert_eq!(kind, want, "{}", text.lines().next().unwrap_or(""));
        }
    }

    /// THE SIGN-IN STEP IS NEVER CUT OFF THE BADGE: a lost login's escalation through the badge's
    /// own cut ([`attention_text`]: its reason within [`REASON_CELLS`] and within the bytes the
    /// wall's label leaves), on agy's 401 and 400, Muse Code's rejected key, and Claude Code's
    /// lost login at every length of its wall's words from a short one to past the badge, with
    /// `retry_api_errors` off and over a draft (`turn_end::login_gone`).
    #[test]
    fn a_lost_logins_sign_in_step_survives_the_badges_own_cut() {
        use crate::supervise::SupervisorConfig;
        use crate::supervise::policy::turn_end::{
            self, Composer, TurnEndAction, TurnEndReading, TurnEndState, decide_turn_end,
        };
        let t = std::time::Instant::now() + std::time::Duration::from_secs(10 * 3600);
        let off = SupervisorConfig {
            retry_api_errors: false,
            ..SupervisorConfig::default()
        };
        // Read as `turn_end`'s own tests read each program's screen.
        let badge =
            |program: &str, rows: &[String], composer: Option<Composer>, c: &SupervisorConfig| {
                let claude = program == "claude";
                let reading = aterm_phase::read(Some(program), rows, claude.then_some(2));
                let mut r = TurnEndReading::of(
                    &reading,
                    rows,
                    false,
                    Some(std::time::Duration::from_secs(180)),
                    None,
                    None,
                );
                r.restartable = claude;
                if let Some(composer) = composer {
                    r.composer = composer;
                }
                let how = turn_end::sign_in_how(r.program, &r.wall_message);
                let mut st = TurnEndState::default();
                st.observe(&r, t);
                let a = decide_turn_end(&st, &r, c, t);
                let TurnEndAction::Escalate { reason } = &a else {
                    panic!("{program}: not an escalation: {a:?}");
                };
                let text = attention_text(
                    r.program.reader(),
                    &turn(Phase::Idle, rows.to_vec()),
                    reason,
                );
                (text, how)
            };
        let screen = aterm_phase::prompt::fixtures::screen;
        for (program, text) in [
            ("agy", aterm_phase::agy::fixtures::lab::ERROR_401),
            ("agy", aterm_phase::agy::fixtures::lab::ERROR_400_KEY),
            ("muse", aterm_phase::muse::fixtures::KEY_REJECTED),
        ] {
            for c in [SupervisorConfig::default(), off.clone()] {
                let (text, how) = badge(program, &screen(text), None, &c);
                assert!(
                    text.contains(&how),
                    "{program}: the badge {text:?} lost {how:?}"
                );
            }
        }
        let wall_rows = |words: &str| {
            let mut rows = screen(aterm_phase::prompt::fixtures::LOGIN_EXPIRED);
            let at = rows
                .iter()
                .position(|row| row.starts_with("⏺ Login expired"))
                .expect("the wall row");
            rows[at] = format!("⏺ {words}");
            rows
        };
        // From a wall that fits both cuts to one past the cells, in plain and in multibyte words
        // (`—` is three bytes in one cell): between them the bytes the label leaves run out first.
        for (n, filler) in (0..=100).flat_map(|n| [(n, "x"), (n, "—")]) {
            let words = format!("Please run /login · API Error: 401 {}", filler.repeat(n));
            let rows = wall_rows(&words);
            for (composer, c) in [
                (None, &off),
                (Some(Composer::Typed), &SupervisorConfig::default()),
            ] {
                let (text, how) = badge("claude", &rows, composer.clone(), c);
                assert!(
                    text.contains(&how) && text.contains("the login is gone"),
                    "{n} ({composer:?}): the badge {text:?} lost {how:?}"
                );
            }
        }
    }

    #[test]
    fn the_kinds_field_names_an_owner_by_its_whole_name() {
        for yes in [
            "ask:needs",
            "-:supervisor,ask:needs",
            "wait:-,-:needs,ask:x",
        ] {
            assert!(kinds_name_owner(yes, "needs"), "{yes}");
        }
        for no in ["ask:supervisor,-:-", "ask:needs-2", "ask:a:needs", "-", ""] {
            assert!(!kinds_name_owner(no, "needs"), "{no}");
        }
    }
}
