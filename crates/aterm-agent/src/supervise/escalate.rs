// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the loop hands a person, and taking it back: the worker's `attention`
//! (the menu bar badges it). Nothing is mailed: a manager waits on the
//! worker's task record, whose `attention=` this entry raises (`await task`).
//!
//! The attention text says what the person is asked: `<program> <kind>:
//! <command, path or question, cut at 64 cells> (<why no rule answered it>)`,
//! within the server's 200 bytes for a keyed entry, the reason cut with its
//! closing `)` kept. A box with no command, path or question is named by its
//! own first row ([`prompt_box_first_row`]). Every part is on one line and cut
//! by terminal cells ([`cut_cells`]). A limit keeps `limited: <message>`.
//!
//! The attention is keyed: this loop writes and clears only its own entry
//! (`meta set|unset attention owner=supervisor`), never the bare form, which
//! is the human's. The key proves ownership; the text
//! ([`Session::attention_ours`]) only tells a box's badge from a limit's. A
//! loop watching behind another supervisor's claim writes none.

use super::super::journal::{
    AGAIN_ADOPTED, AGAIN_RETRY, ATTENTION_KEPT, ATTENTION_SKIPPED, CLEARED_BOX, CLEARED_NEEDS,
    ESCALATED_BEHIND, NEEDS,
};
use super::*;

/// How much of the subject (command, path, question) the attention carries,
/// in terminal cells.
const SUBJECT_CELLS: usize = APPROVAL_LINE_CELLS;
/// How much of the reason the attention carries, in terminal cells.
const REASON_CELLS: usize = 120;

/// `s` on one line and within `cells` terminal cells ([`cut_cells`]), `…`
/// (one cell) where it was cut.
fn clip_to(s: &str, cells: usize) -> String {
    let (whole, cut) = cut_cells(s, cells);
    if !cut {
        return whole;
    }
    let (mut out, _) = cut_cells(s, cells.saturating_sub(1));
    out.push('…');
    out
}

/// The path a folder-trust dialog names (the row under `Accessing
/// workspace:`).
fn trust_path(rows: &[String]) -> Option<&str> {
    let title = aterm_phase::anchor("trust.title");
    let at = rows.iter().position(|r| r.trim() == title)?;
    rows[at + 1..]
        .iter()
        .map(|r| r.trim())
        .find(|r| !r.is_empty())
}

/// The question a dialog asks above the rows the box parser reads as the box
/// (the parser starts at the rule over `4. Chat about this`). A fallback for a
/// dialog aterm-phase did not read whole (form `Unknown`;
/// [`aterm_phase::QuestionDialog::question`] otherwise): the nearest row ending
/// in `?` above `first`, over rules and option rows, stopping at the
/// transcript (a `⏺` row or a user's `❯` row that is not an option's cursor).
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

/// The kind and subject a point's attention names, read by the session's
/// reader: a question's last row; at an idle point the turn-end policy
/// escalated, the wall it ended on (`wall`) or the worker's last words
/// (`idle`); a box's command, path or question. Another program's point
/// (Codex's, agy's, Muse Code's, Gemini CLI's) is named from its reading
/// alone ([`kind_and_subject_read`]).
fn kind_and_subject(reader: &dyn ScreenReader, point: &Turn) -> (String, String) {
    match reader.program() {
        aterm_phase::Program::Claude => {}
        aterm_phase::Program::Codex
        | aterm_phase::Program::Agy
        | aterm_phase::Program::Muse
        | aterm_phase::Program::Gemini
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
            .any(|r| r.contains(aterm_phase::anchor("trust.yes")))
    {
        return ("trust".to_string(), path.to_string());
    }
    let Some(p) = parse_prompt(rows) else {
        return ("other".to_string(), String::new());
    };
    // The rm/rmdir breaker family, whatever its kind (aterm-phase's
    // `PromptV2::rm_breaker`, the one reading the decider uses).
    let kind = if aterm_phase::parse_prompt_v2(rows).is_some_and(|v| v.rm_breaker().is_some()) {
        "rm-breaker".to_string()
    } else {
        p.kind.name().to_string()
    };
    // A question dialog read whole names its question, the `│` gutter
    // stripped and its rows joined.
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
        // An unparsed box: its question row, the nearest row above the
        // options that asks one, else the box's own first row — never a
        // transcript row above a headerless box ([`prompt_box_first_row`]).
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

/// [`kind_and_subject`] from a program's [`aterm_phase::Reading`] alone —
/// its wall, its last words, its box's kind and command, path or title.
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

/// `<program> <kind>: <subject>`, the subject cut at 64 cells: what names a
/// point in its attention, whatever the reason — the program its reader's
/// (`claude`, `codex`).
pub(crate) fn point_label(reader: &dyn ScreenReader, point: &Turn) -> String {
    let (kind, subject) = kind_and_subject(reader, point);
    format!(
        "{} {kind}: {}",
        reader.program().name(),
        or_dash(&clip_to(&subject, SUBJECT_CELLS))
    )
}

/// `<program> <kind>: <subject> (<reason>)` ([`point_label`]), the reason
/// cut at 120 cells and then to what the label leaves of the server's
/// [`ATTENTION_BYTES`], `…` where it was cut and its `)` kept, so the whole
/// is always one entry the server takes.
///
/// A resume line the reason names (`…, then resume with claude … --resume
/// <id>`) is never cut: kept whole, else in its bare form, else dropped with
/// its clause ([`crate::harness::resume::never_cut`]); a cut id names no
/// conversation.
pub(crate) fn attention_text(reader: &dyn ScreenReader, point: &Turn, reason: &str) -> String {
    let label = point_label(reader, point);
    crate::harness::resume::never_cut(reason, |reason| fit_reason(&label, reason))
}

/// `<label> (<reason>)`, the reason cut to fit ([`attention_text`]).
fn fit_reason(label: &str, reason: &str) -> String {
    let reason = clip_to(reason, REASON_CELLS);
    let room = ATTENTION_BYTES.saturating_sub(label.len() + " ()".len());
    let reason = if reason.len() <= room {
        reason
    } else {
        let mut cut = fit_bytes(&reason, room.saturating_sub('…'.len_utf8()));
        cut.truncate(cut.trim_end_matches('…').len());
        cut.push('…');
        cut
    };
    fit_bytes(&format!("{label} ({reason})"), ATTENTION_BYTES)
}

/// The cap a refusal names (`ERR attention too long (max <n> bytes)`), when
/// it is tighter than the text that was sent.
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
    /// A box, a question, a point the turn-end policy hands over: the person
    /// is asked. Its entry names no `kind=`: an entry with none whose text
    /// does not open with [`ATTENTION_PREFIX`] is an ask to every reader
    /// (`docs/DESIGN-owner-surfaces-2026-10-16.md` §2.4).
    Ask,
    /// A limit (`limited: <message>`). `waits`: this loop waits it out by
    /// itself ([`Session::waits_out`]), so the entry is `kind=wait`, a
    /// record; otherwise `kind=ask`, as a limit nothing here ends.
    Limit { waits: bool },
}

impl Escalation {
    /// The `kind=` token the entry is written with, if any.
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

impl<C: Ctl> Session<'_, C> {
    /// The escalation: `meta set attention <text>` on the worker (cut to the
    /// server's keyed cap, and once more to the cap a `too long (max <n>
    /// bytes)` refusal names), and one journal line, `ESCALATED seq=<n>
    /// attention=<reply>`. A refusal is recorded, never the loop's end. `kind`
    /// is what is escalated ([`Escalation`]): an ask (a box, a question, a
    /// point the turn-end policy hands over) or a limit, written `kind=wait`
    /// where this loop waits it out and `kind=ask` where it does not.
    ///
    /// The same episode opened again (`again`, [`Retry`]) or a limit's badge
    /// adopted from a previous watcher is the same ask set again: its line
    /// ends `again=<why>` ([`AGAIN_RETRY`], [`AGAIN_ADOPTED`]), which the
    /// harness's stats count as no second escalation. While a stall is in
    /// hand an ask raises no badge ([`Self::withhold_badge`],
    /// [`stall::WITHHELD`]).
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
        if self.withhold_badge(seq, kind, review)? {
            return Ok(());
        }
        let mut attention = fit_bytes(text, ATTENTION_BYTES);
        let owner = format!("owner={ATTENTION_OWNER}");
        let kind_token = kind.kind_token();
        let set = |attention: &str| -> Vec<String> {
            ["meta", "set", "attention", owner.as_str()]
                .into_iter()
                .map(str::to_string)
                .chain(kind_token.clone())
                .chain(std::iter::once(attention.to_string()))
                .collect()
        };
        let args = set(&attention);
        let mut r = self.call(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
        if let Some(cap) = refused_cap(&r, &attention) {
            attention = fit_bytes(&attention, cap);
            let args = set(&attention);
            r = self.call(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
        }
        if r.ok() {
            self.attention_ours = Some(attention.clone());
        }
        let adopted =
            matches!(kind, Escalation::Limit { .. }) && std::mem::take(&mut self.adopted_limit);
        let again = match again {
            Some(of) => format!(" again={AGAIN_RETRY}, the episode of seq={of}"),
            None if adopted => format!(" again={AGAIN_ADOPTED}"),
            None => String::new(),
        };
        review.note(&format!(
            "ESCALATED seq={seq} attention={}{again}",
            reply_word(&r)
        ));
        Ok(())
    }

    /// A box the loop did not answer, or a point the turn-end policy
    /// escalates (`reason`: why — a question the owner's policy may not
    /// answer among them), escalated ONCE
    /// PER REVIEW POINT: the worker's `attention` set to [`attention_text`] —
    /// `reason`, else the reason the approval policy gave for a box
    /// ([`Session::box_reason`]); see [`Self::escalate`]. It is called only
    /// where the loop hands a NEW point over — a later look at the same point
    /// raises nothing again: the box redrawn, re-wrapped, its focus moved, its
    /// countdown ticking, the transcript reflowed above it
    /// ([`Session::review_key`]) — so the same box back after the worker worked
    /// (or after an outage) is a new point, a new `EVENT prompt` AND a new
    /// badge (deduping on the box's key alone leaves a cleared badge gone),
    /// and so is another box drawn over it with no read between (another
    /// command, another diff on the same path).
    /// The one exception is the point a previous watcher's badge was
    /// ADOPTED for at the start ([`Self::reconcile`]): its first review
    /// raises nothing, the badge standing already. The manager's own
    /// `key`/`turn` that answers it is the screen change the loop already
    /// waits on ([`Self::wait_for_next`]), and the badge goes when the point
    /// has ([`Self::close_box`]).
    pub(super) fn escalate_point(
        &mut self,
        point: &Turn,
        reason: Option<&str>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let key = self.review_key(point);
        if std::mem::take(&mut self.adopted_box)
            && self.box_ask.as_ref().is_some_and(|b| b.key == key)
        {
            return Ok(());
        }
        // A "question" whose last row is a `❯` user row is the watcher's
        // own probe, typed and unanswered: nothing the worker asked.
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
        // THE OWNER'S NOTICE STANDS FOR THIS POINT ([`super::policy::notice`],
        // Claude Code's lost login: `/login` held back, its method picker
        // left to them): it is the one thing they see, with its step — a
        // badge beside it would tell them twice, in other words. Journaled.
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

    /// This loop's own attention entry unset (`meta unset attention
    /// owner=supervisor`) when `ours` says its text is the kind being
    /// closed: the unset's reply word, `kept` when the entry holds another
    /// kind (a box badge over a limit's), `none` when the loop holds no entry.
    /// Another writer's entry is never touched. (`meta set attention ''` is a
    /// usage error, so the clear is `meta unset`.)
    pub(super) fn unset_if_ours(&mut self, ours: fn(&str) -> bool) -> Result<String, Fail> {
        Ok(match self.attention_ours.as_deref() {
            Some(text) if ours(text) => {
                let owner = format!("owner={ATTENTION_OWNER}");
                let r = self.call(&["meta", "unset", "attention", &owner])?;
                if r.ok() || !self.unserved(&r) {
                    self.attention_ours = None;
                }
                reply_word(&r)
            }
            Some(_) => ATTENTION_KEPT.to_string(),
            None => "none".to_string(),
        })
    }

    /// The escalated point has left the screen: its badge cleared if it is
    /// still ours ([`Self::unset_if_ours`]) and journaled `CLEARED seq=<n>
    /// box attention=<said>`.
    pub(super) fn close_box(&mut self, seq: u64, review: &mut dyn Review) -> Result<(), Fail> {
        self.adopted_box = false;
        if self.box_ask.take().is_none() {
            return Ok(());
        }
        let said = self.unset_if_ours(|t| !t.starts_with(ATTENTION_PREFIX))?;
        review.note(&format!("CLEARED seq={seq} {CLEARED_BOX} attention={said}"));
        Ok(())
    }

    /// The episode is over: the `limited:` attention cleared if it is still
    /// ours (another writer's badge stays) and journaled as `CLEARED seq=<n>
    /// attention=<said> <why>`, `seq` the read it closed on.
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
        review.note(&format!("CLEARED seq={seq} attention={said} {why}"));
        Ok(())
    }

    /// A limit's badge adopted from a previous loop ([`Session::adopted_limit`])
    /// is taken down while it is still this loop's entry, its marker
    /// untouched: journaled as an episode's close is, `CLEARED seq=<n>
    /// attention=<said> <why>`. Nothing while the entry is no `limited:` badge
    /// of the loop's (an episode's close took it down already).
    pub(super) fn lower_adopted_limit(
        &mut self,
        seq: u64,
        why: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        if !self
            .attention_ours
            .as_deref()
            .is_some_and(|t| t.starts_with(ATTENTION_PREFIX))
        {
            return Ok(());
        }
        let said = self.unset_if_ours(|t| t.starts_with(ATTENTION_PREFIX))?;
        review.note(&format!("CLEARED seq={seq} attention={said} {why}"));
        Ok(())
    }

    /// The worker answered on a point that is not the limit's notice (a look's
    /// point, or a read of an Off park), with no episode open: the limit a
    /// previous loop's badge was adopted for ([`Session::adopted_limit`]) is
    /// over. Its marker goes, so a later limit is a new episode, a new ask —
    /// never `again=adopted …` for a wall nobody was told of — and its badge,
    /// if still up, goes with it ([`Self::lower_adopted_limit`]). Nothing
    /// while an episode is open: its close is the episode's.
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

/// THE KEY [`Session::needs_told`] HOLDS FOR A NEEDS ENTRY A LOOP BEFORE THIS
/// ONE LEFT (`lifecycle.rs`'s `reconcile`: a crash, a kill, an update's old
/// loop that ended before its last act): no set of needs keys to `0`
/// ([`super::policy::needs::needs_key`]), so the first set this loop puts up
/// replaces that entry, and the first point with none standing takes it down
/// ([`NEEDS_LEFT_DOWN`]), as an Off park and the loop's end take down one of
/// this loop's own.
pub(super) const NEEDS_LEFT: u64 = 0;

/// Why the entry a loop before this one left ([`NEEDS_LEFT`]) is taken
/// down at this loop's first point: `CLEARED seq=<n> needs attention=<reply>
/// no need stands at the loop's start`.
pub(super) const NEEDS_LEFT_DOWN: &str = "no need stands at the loop's start";

/// Whether a `meta` reply's `attention_kinds=` (`<ask|wait|->:<owner>` per
/// entry, comma-joined, oldest first; an owner may hold a `:`) names an
/// entry of `owner`.
pub(super) fn kinds_name_owner(kinds: &str, owner: &str) -> bool {
    kinds
        .split(',')
        .any(|entry| entry.split_once(':').is_some_and(|(_, o)| o == owner))
}

/// THE NEEDS ENTRY'S TEXT ([`Session::sync_needs`]): `<program> needs you:
/// <first need, cut at 64 cells>[ (+N more)] — carrying on`, on one line and
/// within the server's keyed cap ([`ATTENTION_BYTES`]). aterm's own words
/// around the worker's; the need itself is the worker's line, sanitized and
/// cut by the policy (`policy::needs::needs_you`) and here by cells.
pub(super) fn needs_attention_text(program: &str, needs: &[String]) -> String {
    let first = needs.first().map_or("", String::as_str);
    let more = match needs.len() {
        0 | 1 => String::new(),
        n => format!(" (+{} more)", n - 1),
    };
    fit_bytes(
        &format!(
            "{program} needs you: {}{more} — carrying on",
            clip_to(first, SUBJECT_CELLS)
        ),
        ATTENTION_BYTES,
    )
}

impl<C: Ctl> Session<'_, C> {
    /// THE WORKER'S NEEDS YOU, ON THE PERSON'S SCREEN (FOREVER CRUISE /
    /// FOREVER LAND: loud, never blocking): where the needs the policy holds
    /// standing ([`TurnEndState::standing_needs`], its set's
    /// [`TurnEndState::needs_key`]) differ from the set this loop last put
    /// up ([`Session::needs_told`]), the `needs` owner's entry is set —
    /// `meta set attention owner=needs kind=ask <text>`
    /// ([`needs_attention_text`]), journaled `NEEDS seq=<n> count=<k>
    /// attention=<reply> <first need>`, never `ESCALATED`: nothing is handed
    /// over and the worker carries on — or, none standing, taken down
    /// ([`Self::needs_down`]; an entry a loop before this one left,
    /// [`NEEDS_LEFT`], with [`NEEDS_LEFT_DOWN`]). Called as each point is
    /// folded in and again as it is carried out (`turn_end_loop.rs`'s
    /// `task_edge_with`), so it
    /// goes up at the point itself, whatever back-off or grace the point's
    /// act waits on. Not under Autopilot Off (an Off park takes it down);
    /// and never beside a proven self-claim or behind another supervisor's
    /// claim ([`Session::needs_not_ours`]) — codex-alab raises its own, and
    /// this loop neither writes nor unsets that owner's entry there. A
    /// request the connection lost is asked again at the next point; any
    /// other reply is the set told.
    pub(super) fn sync_needs(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        if self.needs_not_ours() {
            return Ok(());
        }
        let seq = screen.seq;
        let key = self.turn_end.needs_key();
        if key == 0 {
            let why = if self.needs_told == Some(NEEDS_LEFT) {
                NEEDS_LEFT_DOWN
            } else {
                "none stands now"
            };
            return self.needs_down(seq, why, review);
        }
        if self.needs_told == Some(key) || self.autopilot(opts) == Autopilot::Off {
            return Ok(());
        }
        let needs = self.turn_end.standing_needs().to_vec();
        let program = self.reader(&screen.rows).program().name();
        let mut text = needs_attention_text(program, &needs);
        let owner = format!("owner={NEEDS_ATTENTION_OWNER}");
        let kind = format!("kind={}", aterm_types::needs::AttentionKind::Ask.word());
        let mut r = self.call(&["meta", "set", "attention", &owner, &kind, &text])?;
        if let Some(cap) = refused_cap(&r, &text) {
            text = fit_bytes(&text, cap);
            r = self.call(&["meta", "set", "attention", &owner, &kind, &text])?;
        }
        if r.lost() {
            return Ok(());
        }
        self.needs_told = Some(key);
        let first = needs.first().map_or("", String::as_str);
        review.note(&format!(
            "{NEEDS} seq={seq} count={} attention={} {}",
            needs.len(),
            reply_word(&r),
            clip_to(first, SUBJECT_CELLS)
        ));
        Ok(())
    }

    /// THE NEEDS ENTRY TAKEN DOWN, `why`: none stands now, an Off park, the
    /// loop's end — `meta unset attention owner=needs`, journaled `CLEARED
    /// seq=<n> needs attention=<reply> <why>` ([`CLEARED_NEEDS`]), which no
    /// reader takes for a badge of an escalation. Nothing this loop put up:
    /// nothing is said. Beside a proven self-claim or behind another
    /// supervisor's claim nothing is unset ([`Session::needs_not_ours`]:
    /// that owner's entry may be the self-claim's own, an Off park's
    /// given-back claim and the loop's end included): the loop only forgets
    /// its set.
    pub(super) fn needs_down(
        &mut self,
        seq: u64,
        why: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        if self.needs_told.is_none() {
            return Ok(());
        }
        if self.needs_not_ours() {
            self.needs_told = None;
            return Ok(());
        }
        let owner = format!("owner={NEEDS_ATTENTION_OWNER}");
        let r = self.call(&["meta", "unset", "attention", &owner])?;
        if r.lost() {
            return Ok(());
        }
        self.needs_told = None;
        review.note(&format!(
            "CLEARED seq={seq} {CLEARED_NEEDS} attention={} {why}",
            reply_word(&r)
        ));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reader these tests' Claude Code screens are named by.
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

    /// An rm-breaker box's attention (its command and the vendor's note as the
    /// reason) fits the server's 200 bytes for a keyed entry, its reason cut
    /// and closed.
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

    /// Claude Code's question tool (AskUserQuestion) as drawn: the question,
    /// its options, a rule, `4. Chat about this` and the footer. The badge
    /// names the question, not `4. Chat about this`. Negative control: a `?`
    /// row in the transcript above a user's message is never taken.
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

    /// A question aterm-phase reads whole is badged by its question, the `│`
    /// gutter stripped: never a transcript row, never `4. Chat about this`.
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

    /// The memory wall's escalation never cuts its resume line. Built the way
    /// the loop builds it (a long flag set, a 64-cell subject, and again
    /// behind a failed restart's prefix), the stored attention names the
    /// conversation whole or names no command; every `--resume` in it is
    /// followed by the whole id.
    ///
    /// Negative control: the cutting renderer alone ([`fit_reason`]) names the
    /// command and cuts its conversation away.
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
        let banner = format!("{} (140.4GB)", aterm_phase::anchor_text("wall.memory"));
        let flagged =
            format!("claude --dangerously-skip-permissions --model claude-opus-4-1 --resume {ID}");
        let reason = |line: &str| {
            format!(
                "memory critical: restart it{}{line}: {banner}",
                crate::harness::resume::THEN_RESUME_WITH
            )
        };
        // Every command named is whole: each `resume with claude` runs to
        // `--resume <the whole id>`.
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
        // The flagged line does not fit beside this label: the bare one does.
        let t = attention_text(CLAUDE, &point, &reason(&flagged));
        assert!(
            t.contains(&format!("then resume with claude --resume {ID}")),
            "{t}"
        );
        // Negative control: the cutting renderer alone cuts the command; it
        // is named, and its conversation is not.
        let label = point_label(CLAUDE, &point);
        let old = fit_reason(&label, &reason(&flagged));
        assert!(
            old.contains("then resume with claude --") && !old.contains(ID),
            "{old}"
        );
    }

    #[test]
    fn a_status_field_is_read_off_the_ok_line() {
        let st = "OK schema=1 sid=- hold=0 fabric=absent identity=-\n";
        assert_eq!(status_field(st, "fabric"), Some("absent"));
        assert_eq!(status_field(st, "hold"), Some("0"));
        assert_eq!(status_field(st, "identity"), None);
        assert_eq!(status_field(st, "nope"), None);
    }

    /// The rm breaker's statically-unresolvable form from a workflow subagent
    /// is badged `rm-breaker`, like the possibly-empty-variable form: the
    /// family, not one note's prefix. Control: a plain Bash box is badged
    /// `bash`.
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
    }

    /// A `meta` reply's `attention_kinds=` names the `needs` owner's entry
    /// by its owner alone, whatever its kind or place, an owner with a `:`
    /// read whole. NEGATIVE CONTROLS: other owners, an owner that only begins
    /// or ends with the word, and no entries at all name none.
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
