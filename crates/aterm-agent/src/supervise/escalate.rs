// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the loop hands a person, and taking it back: the worker's `attention`
//! (the menu bar badges it) and, while the fabric is connected, one post to
//! the manager's inbox.
//!
//! The attention text says what the person is asked, not merely that
//! something waits (audit SUP-8 measured `claude needs approval: Bash
//! command`): `<program> <kind>: <command, path or question, cut at 64
//! cells> (<why no rule answered it>)` — `claude rm-breaker: S=$PWD/tmp; for
//! p in a b; do set -- $p; rm -rf $S/$1; done (rm circuit breaker (newline
//! reading): …)`, within the server's 200 bytes for a keyed entry — the
//! reason cut with its closing `)` kept. A box that names no command,
//! path or question is named by its OWN first row ([`prompt_box_first_row`],
//! never a transcript row above it — main's round-25 fix, fed20fadf). Every
//! part is on one line and cut by terminal cells, a wide character two
//! ([`cut_cells`]). A limit keeps its own `limited: <message> reset=<when>`.
//!
//! Mail goes only where it can land: `kind=ask` (a box, a question) and
//! `kind=control` (a limit) are posted only while `status` says
//! `fabric=connected` (owner decision 4: the badge and its notification are
//! the human channel; with the fabric absent an `ask` is refused `no-bridge`
//! and a `control` only queues). Anything else is journaled `mail=skipped: no
//! fabric (fabric=<state>)`, never a posted `OK` that delivered nothing. An
//! `ask` is posted `--wait=0` — the loop is single-threaded and a default
//! `ask` waits up to 30 s for its record to land — and a reply that means
//! QUEUED is journaled `mail=queued id=<n> (…)` ([`post_word`]).
//!
//! The attention is KEYED (lane C): this loop writes and clears only its own
//! entry, `meta set attention owner=supervisor <text>` / `meta unset
//! attention owner=supervisor` — never the bare form, which is the human's
//! (owner `-`) — so a badge a person raised is never cleared by the loop,
//! and the loop's is never hidden by theirs past its own unset. The loop
//! knows its entry's text ([`Session::attention_ours`]) and unsets it only
//! when that text is the kind the closing point raised ([`is_ours`]). A
//! loop watching behind another supervisor's claim writes none.

use super::*;

/// The kinds an escalation's attention names, besides a limit's.
const KINDS: &[&str] = &[
    "bash",
    "read",
    "edit",
    "write",
    "workflow",
    "other",
    "rm-breaker",
    "trust",
    "question",
    "idle",
    "wall",
];
/// How much of the subject (command, path, question) the attention carries,
/// in terminal cells.
const SUBJECT_CELLS: usize = APPROVAL_LINE_CELLS;
/// How much of the reason the attention carries, in terminal cells.
const REASON_CELLS: usize = 120;

/// Whether a standing attention is one this loop writes: a limit's
/// (`limited: …`), a point's (`claude <kind>: …`), or the box badge an
/// earlier build wrote (`claude needs approval: …`).
pub(crate) fn is_ours(text: &str) -> bool {
    if text.starts_with(ATTENTION_PREFIX) || text.starts_with(&format!("{PROGRAM}{APPROVAL_MARK}"))
    {
        return true;
    }
    text.strip_prefix(PROGRAM)
        .and_then(|t| t.strip_prefix(' '))
        .and_then(|t| t.split_once(": "))
        .is_some_and(|(kind, _)| KINDS.contains(&kind))
}

/// Whether a standing attention is one of this loop's point badges (not a
/// limit's).
fn is_our_point(text: &str) -> bool {
    is_ours(text) && !text.starts_with(ATTENTION_PREFIX)
}

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

/// The question a dialog asks ABOVE the rows the box parser reads as the
/// box: Claude Code's question tool draws its options, a rule, then `4. Chat
/// about this` over the footer, and the box parser starts at that rule — so
/// the badge quoted `4. Chat about this` (the live E2E of 2026-09-24, D7).
/// The nearest row ending in `?` above `first`, over rules and option rows,
/// stopping at the transcript: a `⏺` row or a user's `❯` row that is not an
/// option's cursor.
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

/// The kind and subject a point's attention names: a question's last row;
/// at an idle point the turn-end policy escalated, the wall it ended on
/// (`wall`) or the worker's last words (`idle`); a box's command, path or
/// question.
fn kind_and_subject(point: &Turn) -> (String, String) {
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
    let kind = if p
        .notes
        .join(" ")
        .starts_with(super::super::policy::approval::RM_BREAKER_NOTE)
    {
        "rm-breaker".to_string()
    } else {
        p.kind.name().to_string()
    };
    let subject = if !p.command.is_empty() {
        p.command.clone()
    } else if p.kind == PromptKind::Workflow && !p.description.is_empty() {
        p.description.clone()
    } else {
        // An unparsed box: its question row, the nearest row above the
        // options that asks one, else the box's OWN first row — never a
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

/// `<program> <kind>: <subject>`, the subject cut at 64 cells: what names a
/// point in its attention, whatever the reason.
pub(crate) fn point_label(point: &Turn) -> String {
    let (kind, subject) = kind_and_subject(point);
    format!(
        "{PROGRAM} {kind}: {}",
        or_dash(&clip_to(&subject, SUBJECT_CELLS))
    )
}

/// `<program> <kind>: <subject> (<reason>)` ([`point_label`]), the reason
/// cut at 120 cells and then to what the label leaves of the server's
/// [`ATTENTION_BYTES`] — `…` where it was cut and its `)` kept, so the whole
/// is always one entry the server takes.
pub(crate) fn attention_text(point: &Turn, reason: &str) -> String {
    let label = point_label(point);
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

/// The `<field>=` word of a `status` reply (`-` and absent are `None`).
pub(crate) fn status_field<'s>(stdout: &'s str, field: &str) -> Option<&'s str> {
    let line = stdout.lines().find(|l| l.starts_with("OK"))?;
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(field)?.strip_prefix('='))
        .filter(|v| *v != "-")
}

impl<C: Ctl> Session<'_, C> {
    /// The fabric's state as the worker's `status` says it (`connected`,
    /// `absent`, …), `unknown` when the host does not say.
    fn fabric_state(&mut self) -> Result<String, Fail> {
        let r = self.call(&["status"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("status failed: {}", r.stderr.trim())));
        }
        Ok(if r.ok() {
            status_field(&r.stdout, "fabric").unwrap_or("unknown")
        } else {
            "unknown"
        }
        .to_string())
    }

    /// The escalation: `meta set attention <text>` on the worker (cut to the
    /// server's keyed cap, and cut again to the cap a `too long (max <n>
    /// bytes)` refusal names, once — never a badge silently lost to a
    /// length), the same text posted from the worker's session
    /// as `kind=<kind>` to the manager's ([`Self::set_manager`]) — only while
    /// the fabric is connected — and one journal line, `ESCALATED seq=<n>
    /// attention=<reply> mail=<reply>`, with the server's first word for
    /// each; a refusal is recorded, never the loop's end. The same episode
    /// opened again (`again`: the seq of the escalation it keeps, [`Retry`])
    /// sets the attention and posts nothing: `mail=skipped: the retry hit
    /// the wall again, the episode of seq=<m>`; so does one adopted from a
    /// previous watcher (`mail=skipped: adopted …`). With no manager, or the
    /// fabric not connected: `mail=skipped: no --inbox and no
    /// $ATERM_PARENT_SESSION_ID`, `mail=skipped: no fabric (fabric=<state>)`.
    /// An `ask` is posted `--wait=0` and its reply journaled by
    /// [`post_word`] (a queued post says `queued id=<n> (…)`).
    pub(super) fn escalate(
        &mut self,
        seq: u64,
        text: &str,
        again: Option<u64>,
        kind: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        if let Some(holder) = self.claim.watching_behind() {
            review.note(&format!(
                "ESCALATED seq={seq} attention=skipped: another supervisor ({holder}) answers \
                 this session mail=skipped: the same"
            ));
            return Ok(());
        }
        let mut attention = fit_bytes(text, ATTENTION_BYTES);
        let owner = format!("owner={ATTENTION_OWNER}");
        let mut r = self.call(&["meta", "set", "attention", &owner, &attention])?;
        if let Some(cap) = refused_cap(&r, &attention) {
            attention = fit_bytes(&attention, cap);
            r = self.call(&["meta", "set", "attention", &owner, &attention])?;
        }
        if r.ok() {
            self.attention_ours = Some(attention.clone());
        }
        let attention_said = reply_word(&r);
        let adopted = kind == "control" && std::mem::take(&mut self.adopted_limit);
        let mail_said = match (again, self.manager.clone()) {
            (Some(of), _) => {
                format!("skipped: the retry hit the wall again, the episode of seq={of}")
            }
            _ if adopted => "skipped: adopted from a previous watcher".to_string(),
            (None, None) => "skipped: no --inbox and no $ATERM_PARENT_SESSION_ID".to_string(),
            (None, Some(to)) => match self.fabric_state()?.as_str() {
                "connected" => {
                    let to = format!("to={to}");
                    let kind_arg = format!("kind={kind}");
                    // An `ask` WAITS by default — up to 30 s for its record to
                    // land on the bus — and this loop is single-threaded: a
                    // starting or stalled bridge held every look behind the
                    // escalation (round-25 review). `--wait=0` returns at once;
                    // the row is recorded before any wait, so the post is
                    // queued whatever the reply ([`post_word`]).
                    let mut args = vec!["post", to.as_str(), kind_arg.as_str()];
                    if kind == "ask" {
                        args.push("--wait=0");
                    }
                    args.push(text);
                    let r = self.call(&args)?;
                    post_word(&r)
                }
                state => format!("skipped: no fabric (fabric={state})"),
            },
        };
        review.note(&format!(
            "ESCALATED seq={seq} attention={attention_said} mail={mail_said}"
        ));
        Ok(())
    }

    /// A box the loop did not approve, a question the worker asked, or a
    /// point the turn-end policy escalates (`reason`: why), escalated ONCE
    /// PER REVIEW POINT: the worker's `attention` set to [`attention_text`] —
    /// `reason`, else the reason the approval policy gave for a box
    /// ([`Session::box_reason`]) — and one `kind=ask` posted to the manager
    /// ([`Self::escalate`]). It is called only where the loop hands a NEW
    /// point over — the same point on a later look posts nothing again — so
    /// the same box back after the worker worked (or after an outage) is a
    /// new point, a new `EVENT prompt` AND a new ask (main's round-25 fix,
    /// fed20fadf: deduping on the box's key alone left a cleared badge gone).
    /// The one exception is the point a previous watcher's badge was
    /// ADOPTED for at the start ([`Self::reconcile`]): its first review
    /// posts nothing, the ask having gone already. The manager's own
    /// `key`/`turn` that answers it is the screen change the loop already
    /// waits on ([`Self::wait_for_next`]), and the badge goes when the point
    /// has ([`Self::close_box`]).
    pub(super) fn escalate_point(
        &mut self,
        point: &Turn,
        allow: &[String],
        reason: Option<&str>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let key = review_key(point, allow);
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
            (None, Phase::Question, _) => "the worker asked a question".to_string(),
            (None, _, Some((k, why))) if *k == key => why.clone(),
            _ => "no approval rule answers this box".to_string(),
        };
        let text = attention_text(point, &reason);
        self.escalate(point.screen.seq, &text, None, "ask", review)?;
        self.box_ask = Some(BoxAsk { key });
        Ok(())
    }

    /// This loop's own attention entry unset (`meta unset attention
    /// owner=supervisor`) when `ours` says its text is the kind being
    /// closed: the unset's reply word, `kept` when the entry holds another
    /// kind (a box badge over a limit's: the box's close clears it), `none`
    /// when the loop holds no entry. Another writer's entry is never
    /// touched, whatever it says. `meta set attention ''` is a usage error
    /// on the wire (measured), so the clear is `meta unset`.
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
            Some(_) => "kept".to_string(),
            None => "none".to_string(),
        })
    }

    /// The escalated point has left the screen: its badge cleared IF IT IS
    /// STILL OURS ([`Self::unset_if_ours`]) and journaled `CLEARED seq=<n>
    /// box attention=<said>`.
    pub(super) fn close_box(&mut self, seq: u64, review: &mut dyn Review) -> Result<(), Fail> {
        self.adopted_box = false;
        if self.box_ask.take().is_none() {
            return Ok(());
        }
        let said = self.unset_if_ours(is_our_point)?;
        review.note(&format!("CLEARED seq={seq} box attention={said}"));
        Ok(())
    }

    /// The episode is over: the attention cleared IF IT IS STILL OURS — a
    /// `limited:` text; another writer may have raised its own badge over
    /// ours while the episode was open, and that stays — and journaled as
    /// `CLEARED seq=<n> attention=<said> <why>`, `seq` the read it closed on.
    pub(super) fn close_episode(
        &mut self,
        seq: u64,
        why: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        if self.limit.take().is_none() {
            return Ok(());
        }
        let said = self.unset_if_ours(|t| t.starts_with(ATTENTION_PREFIX))?;
        review.note(&format!("CLEARED seq={seq} attention={said} {why}"));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            &turn(Phase::Prompt, rows),
            "rm circuit breaker (newline reading): unresolved $1",
        );
        assert_eq!(
            t,
            "claude rm-breaker: S=$PWD/tmp; for p in a b; do set -- $p; rm -rf $S/$1; done \
             (rm circuit breaker (newline reading): unresolved $1)"
        );
        assert!(is_ours(&t) && is_our_point(&t));
        let trust: Vec<String> = include_str!("policy/fixtures/cap-trust.txt")
            .lines()
            .map(str::to_string)
            .collect();
        let t = attention_text(&turn(Phase::Prompt, trust), "x");
        assert!(
            t.starts_with("claude trust: /private/tmp/claude-502/scratch/work1 (x)"),
            "{t}"
        );
    }

    /// The live E2E's D2 (2026-09-24): an rm-breaker box's attention —
    /// its command and the vendor's note as the reason — ran past the
    /// server's 200 bytes for a keyed entry, the server refused it, and no
    /// one was told. It fits now, its reason cut and closed.
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
        let t = attention_text(&turn(Phase::Prompt, rows), why);
        assert!(t.len() <= 200, "{} bytes: {t}", t.len());
        assert!(t.ends_with("…)"), "{t}");
        assert!(t.starts_with("claude rm-breaker: "), "{t}");
    }

    /// The live E2E's D7: Claude Code's question tool (AskUserQuestion) as
    /// drawn — the question, its options, a rule, `4. Chat about this` and
    /// the footer. The badge names the question, not `4. Chat about this`.
    /// NEGATIVE CONTROL: a `?` row in the transcript above a user's message
    /// is never taken.
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
        let t = attention_text(&turn(Phase::Prompt, rows.clone()), "no rule");
        assert!(t.contains("What should I do with a.txt?"), "{t}");
        assert!(!t.contains("Chat about this"), "{t}");
        rows[6] = "Pick one.".to_string();
        rows[0] = "⏺ Which one should I do?".to_string();
        let t = attention_text(&turn(Phase::Prompt, rows), "no rule");
        assert!(!t.contains("Which one should I do?"), "{t}");
    }

    #[test]
    fn a_long_subject_and_reason_are_clipped_and_the_whole_fits_256_bytes() {
        let mut rows = aterm_phase::prompt::fixtures::bash_one_row();
        let at = rows
            .iter()
            .position(|r| r == "   git log --oneline -5")
            .expect("row");
        rows[at] = format!("   echo {}", "é".repeat(300));
        let t = attention_text(&turn(Phase::Prompt, rows), &"why ".repeat(100));
        assert!(t.len() <= ATTENTION_BYTES, "{}", t.len());
        assert!(t.starts_with("claude bash: echo é"), "{t}");
        assert!(t.contains('…'));
    }

    /// Negative controls: another writer's badge, and look-alikes, are not
    /// ours.
    #[test]
    fn only_this_loops_texts_are_ours() {
        assert!(is_ours("limited: Weekly limit reached reset=Sep 20"));
        assert!(is_ours("claude needs approval: Bash command"));
        assert!(is_ours(
            "claude question: which one? (the worker asked a question)"
        ));
        assert!(!is_ours("aterm harness: model bucket"));
        assert!(!is_ours("claude teapot: x"));
        assert!(!is_ours("codex bash: ls"));
        assert!(!is_our_point("limited: x reset=-"));
    }

    #[test]
    fn a_status_field_is_read_off_the_ok_line() {
        let st = "OK schema=1 sid=- hold=0 fabric=absent identity=-\n";
        assert_eq!(status_field(st, "fabric"), Some("absent"));
        assert_eq!(status_field(st, "hold"), Some("0"));
        assert_eq!(status_field(st, "identity"), None);
        assert_eq!(status_field(st, "nope"), None);
    }
}
