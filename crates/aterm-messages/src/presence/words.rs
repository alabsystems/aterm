// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The row's WORDS: the six slots composed from a [`Slot`] ([`words`]), laid
//! out at a width and shed from the right in the design's order
//! ([`Words::fit`], [`Words::pieces`]), and the vocabulary every free-text
//! value is held to ([`sanitize_token`]).

use std::fmt::Write as _;

use super::{AgentPhase, CTX_WARN_PCT, Hand, Host, Level, Link, Slot, StoryVerb, Tone};
use crate::{Duration, Instant};

/// The six slots, composed. Each is a whole string; [`Words::fit`] lays them
/// out at a width and sheds from the right in the design's order.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Words {
    /// The role slot (`meta role`, or `—`).
    pub role: String,
    /// The phase slot's word.
    pub phase: String,
    /// The `since` clauses, in the order they are printed; elided shortest
    /// first when the row is narrow.
    pub since: Vec<String>,
    /// The hand slot.
    pub hand: String,
    /// The hand without its detail (`◂ turn 41` for `◂ manager · turn 41`,
    /// `⊘ hold` for `⊘ hold <reason>`): what survives past the ctx slot on a
    /// narrow row, so the hand is never cut mid-word.
    pub hand_short: String,
    /// The mail slot — empty, and no slot on the row, with no mail to tell
    /// (ruling 366).
    pub mail: String,
    /// The mail counts alone (`✉2 ↑1`), without `kind←✓from`.
    pub mail_short: String,
    /// The context slot (empty without a reading).
    pub ctx: String,
    /// The fabric slot.
    pub fabric: String,
    /// The a11y sentence.
    pub sentence: String,
    /// The tone the phase slot is painted in.
    pub tone: Tone,
}

/// Two spaces between slots, one between a glyph and its value.
const SLOT_GAP: &str = "  ";
/// The joint between since-clauses.
const CLAUSE_SEP: &str = " \u{00b7} ";
/// The empty-slot mark.
const DASH: &str = "\u{2014}";

/// Which slot a laid-out piece belongs to, so the painter can colour it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotKind {
    /// The role.
    Role,
    /// The phase word.
    Phase,
    /// The since-clauses.
    Since,
    /// The hand.
    Hand,
    /// The mail.
    Mail,
    /// The context.
    Ctx,
    /// The fabric link.
    Fabric,
}

impl Words {
    /// Lay the slots out at `cols` cells. Elision right→left: the fabric slot
    /// (`rtt`), then since-clauses shortest first (the longest stop survives),
    /// then the role, then ctx; then the mail's `kind←from` and the hand's
    /// detail fold to their short forms, so hand, phase and mail survive to
    /// 24 columns — past which the line is cut.
    #[must_use]
    pub fn fit(&self, cols: usize) -> String {
        let pieces = self.pieces(cols);
        join_pieces(&pieces)
    }

    /// The pieces [`Self::fit`] keeps at `cols`, in order, each tagged with
    /// its slot: a `Since` piece follows its `Phase` by one space, every other
    /// piece follows its predecessor by two.
    #[must_use]
    pub fn pieces(&self, cols: usize) -> Vec<(SlotKind, String)> {
        let mut since = self.since.clone();
        let mut fabric = true;
        let mut role = true;
        let mut ctx = true;
        let mut mail_full = true;
        let mut hand_full = true;
        loop {
            let pieces = self.compose(&since, role, ctx, fabric, hand_full, mail_full);
            if width(&join_pieces(&pieces)) <= cols {
                return pieces;
            }
            if fabric {
                fabric = false;
                continue;
            }
            // The since-clauses, shortest first — but the stop clause (the
            // longest stop) outlives everything below and goes last of all.
            let is_stop = |c: &String| {
                c.starts_with("limited") || c.starts_with("held") || c.starts_with('\u{2192}')
            };
            if let Some(i) = since
                .iter()
                .enumerate()
                .filter(|(_, c)| !is_stop(c))
                .min_by_key(|(_, c)| width(c))
                .map(|(i, _)| i)
            {
                since.remove(i);
                continue;
            }
            if role {
                role = false;
                continue;
            }
            if ctx {
                ctx = false;
                continue;
            }
            if mail_full && self.mail_short != self.mail {
                mail_full = false;
                continue;
            }
            if hand_full && self.hand_short != self.hand {
                hand_full = false;
                continue;
            }
            if !since.is_empty() {
                since.clear();
                continue;
            }
            // Nothing left to shed: cut the survivors at the width — the
            // MAIL keeps its cells (it is the rightmost survivor and the
            // shortest), the hand gives way first, then the phase, so all
            // three are on the row down to 13 columns.
            if let [
                (SlotKind::Phase, phase),
                (SlotKind::Hand, hand),
                (SlotKind::Mail, mail),
            ] = pieces.as_slice()
            {
                let (pw, hw, mw) = (width(phase), width(hand), width(mail));
                if pw + hw + mw + 4 > cols && cols >= mw + 4 + 3 + 4 {
                    let hand_room = (cols - mw - 4).saturating_sub(pw).max(3);
                    let hand_cut = truncate(hand, hand_room.min(hw));
                    let phase_room = cols - mw - 4 - width(&hand_cut);
                    let phase_cut = truncate(phase, phase_room.min(pw));
                    return vec![
                        (SlotKind::Phase, phase_cut),
                        (SlotKind::Hand, hand_cut),
                        (SlotKind::Mail, mail.clone()),
                    ];
                }
            }
            let mut out = Vec::new();
            let mut used = 0usize;
            for (i, (kind, text)) in pieces.into_iter().enumerate() {
                let gap = if i == 0 {
                    0
                } else if kind == SlotKind::Since {
                    1
                } else {
                    2
                };
                if used + gap >= cols {
                    break;
                }
                let room = cols - used - gap;
                let cut = truncate(&text, room);
                used += gap + width(&cut);
                out.push((kind, cut));
            }
            return out;
        }
    }

    #[allow(
        clippy::fn_params_excessive_bools,
        reason = "one switch per sheddable slot, in the shedding order `pieces` walks"
    )]
    fn compose(
        &self,
        since: &[String],
        role: bool,
        ctx: bool,
        fabric: bool,
        hand_full: bool,
        mail_full: bool,
    ) -> Vec<(SlotKind, String)> {
        let mut out = Vec::with_capacity(7);
        if role && !self.role.is_empty() {
            out.push((SlotKind::Role, self.role.clone()));
        }
        out.push((SlotKind::Phase, self.phase.clone()));
        if !since.is_empty() {
            out.push((SlotKind::Since, since.join(CLAUSE_SEP)));
        }
        out.push((
            SlotKind::Hand,
            if hand_full {
                &self.hand
            } else {
                &self.hand_short
            }
            .clone(),
        ));
        // No mail to tell is no slot (ruling 366): `✉0` read as jargon.
        if !self.mail.is_empty() {
            out.push((
                SlotKind::Mail,
                if mail_full {
                    &self.mail
                } else {
                    &self.mail_short
                }
                .clone(),
            ));
        }
        if ctx && !self.ctx.is_empty() {
            out.push((SlotKind::Ctx, self.ctx.clone()));
        }
        if fabric && !self.fabric.is_empty() {
            out.push((SlotKind::Fabric, self.fabric.clone()));
        }
        out
    }
}

/// Join laid-out pieces into the printed line: one space before a `Since`
/// piece, two before every other.
pub(super) fn join_pieces(pieces: &[(SlotKind, String)]) -> String {
    let mut out = String::new();
    for (i, (kind, text)) in pieces.iter().enumerate() {
        if i > 0 {
            out.push_str(if *kind == SlotKind::Since {
                " "
            } else {
                SLOT_GAP
            });
        }
        out.push_str(text);
    }
    out
}

/// Display width in cells (every glyph the row uses is one cell wide; the
/// text-presentation rule in the painter keeps ✓ and ⚠ that way).
fn width(s: &str) -> usize {
    s.chars().count()
}

fn truncate(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut t: String = s.chars().take(max - 1).collect();
    t.push('\u{2026}');
    t
}

/// The row's OWN vocabulary — the glyphs that spell a hand, a hold, mail, the
/// link, a story, a trust verdict, a reset — and its two separators (two
/// spaces between slots, ` · ` between clauses). None of it may arrive inside
/// a free-text value: a `meta role` of `⊘ hold pause ·fleet🔒  ◂ manager` would
/// otherwise print as a hold and a hand the session does not have.
const BAND_GLYPHS: &[char] = &[
    '\u{25c2}',
    '\u{25b8}',
    '\u{2298}',
    '\u{2709}',
    '\u{21af}',
    '\u{2191}',
    '\u{27df}',
    '~',
    '\u{2715}',
    '\u{25c7}',
    '\u{2713}',
    '\u{2717}',
    '\u{26a0}',
    '\u{2190}',
    '\u{2192}',
    '\u{00b7}',
    '\u{1f512}',
];

/// A wire-safe token: printable, single-spaced, none of the row's own glyphs,
/// at most `cap` chars, cut with `…`. Every free-text value the row prints (a
/// role, a holder, a sender, a reason, a reset time, a told story's text)
/// passes here, so a control byte, a bidi override or the row's grammar in
/// an agent-chosen name can never reach the chrome or forge a slot.
#[must_use]
pub fn sanitize_token(s: &str, cap: usize) -> String {
    let mut clean = String::with_capacity(s.len());
    let mut at_space = true;
    for c in s.chars() {
        let c = if c.is_whitespace() { ' ' } else { c };
        if c.is_control()
            || matches!(
                c,
                '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
            )
            || BAND_GLYPHS.contains(&c)
        {
            continue;
        }
        if c == ' ' {
            if at_space {
                continue;
            }
            at_space = true;
        } else {
            at_space = false;
        }
        clean.push(c);
    }
    truncate(clean.trim_end(), cap)
}

/// A hold's reason as every human-facing surface prints and speaks it: the
/// wire token (`status hold=`'s pct-encoded form, `main%20broken`) DECODED to
/// its words ([`Host::pct_decode`]) and then held to [`sanitize_token`]'s
/// rule — a bridge-supplied reason can encode a control byte or a bidi
/// override, and the decode must not be the step that lets it through.
#[must_use]
pub fn hold_reason_words<V: Host>(reason: &str) -> String {
    sanitize_token(&V::pct_decode(reason), 32)
}

/// A duration as the row prints it: `12s`, `3m12s`, `2h05m`, `1d 22h`.
#[must_use]
pub fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else if s < 86_400 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d {}h", s / 86_400, (s % 86_400) / 3600)
    }
}

/// The short form of a session id for the hand slot: `s-1e91`.
#[must_use]
pub fn short_sid(sid: &str) -> String {
    let n = sid.chars().count().min(6);
    sid.chars().take(n).collect()
}

/// The trust glyph: ✓ for a verdict the receiver trusts, ✗ for one it refuses,
/// ? for anything unresolved. BEFORE the sender, always.
#[must_use]
pub(crate) fn trust_glyph(trust: &str) -> char {
    match trust {
        "agent" | "human" | "owner" | "operator" | "verified" => '\u{2713}',
        "forged-self" | "unreadable" | "observer" | "refused" | "spoof" => '\u{2717}',
        _ => '?',
    }
}

/// A box's kind in a person's words: the kind before the colon of
/// `kind[:verdict]` (`bash:not-read-only` → `bash`), the hyphenated kinds
/// spelled out (`plan-exit` → `plan`). `None` for a box of no named kind.
#[must_use]
pub fn prompt_kind_words(detail: Option<&str>) -> Option<&str> {
    let kind = detail
        .and_then(|d| d.split(':').next())
        .filter(|k| !k.is_empty() && *k != "other")?;
    Some(match kind {
        "plan-enter" => "plan mode",
        "plan-exit" => "plan",
        "held-message" => "held message",
        "goal-proposal" => "goal",
        "computer-use" => "computer use",
        "read-outside-setting" => "outside read",
        "model-switch" => "model switch",
        "rate-nudge" => "rate-limit model",
        "model-pick" => "model picker",
        "goal-resume" => "paused goal",
        "trust" => "folder trust",
        "powershell" => "PowerShell",
        other => other,
    })
}

/// A box in the row's words: `bash approval`, `plan approval`; the agent's
/// question tool is a `question`, never an approval; a box of no named kind
/// is `approval`.
#[must_use]
pub fn prompt_band_word(detail: Option<&str>) -> String {
    match prompt_kind_words(detail) {
        Some("question") => "question".to_string(),
        Some(kind) => format!("{kind} approval"),
        None => "approval".to_string(),
    }
}

/// The phase slot for a published input stall — `(phase, since clauses,
/// spoken)`, the three things [`words`] composes a phase from:
///
/// ```text
/// frozen 2m03s · not reading input     frozen, not reading input for 2m03s; restart it
/// frozen 2m03s · survived its restart  frozen, still running after its restart signal; end it with signal kill
/// stopped 41s · input queued           stopped with input queued for 41s; resume it
/// ```
///
/// It takes the slot AHEAD of typed attention (2026-09-24): the incident's
/// band read the supervisor's "answer this box" for 2h41m over a program that
/// could read no answer. The duration counts from the oldest unread byte
/// (`since`).
#[must_use]
pub(crate) fn stall_words(
    since: Instant,
    stopped: bool,
    survived: bool,
    now: Instant,
) -> (String, Vec<String>, String) {
    let dur = fmt_dur(now.saturating_duration_since(since));
    if stopped {
        return (
            "stopped".to_string(),
            vec![dur.clone(), "input queued".to_string()],
            format!("stopped with input queued for {dur}; resume it"),
        );
    }
    if survived {
        // The program lived through its restart signal.
        return (
            "frozen".to_string(),
            vec![dur, "survived its restart".to_string()],
            "frozen, still running after its restart signal; end it with signal kill".to_string(),
        );
    }
    (
        "frozen".to_string(),
        vec![dur.clone(), "not reading input".to_string()],
        format!("frozen, not reading input for {dur}; restart it"),
    )
}

/// Compose the six slots for `slot` as seen from a window whose story
/// watermark is `watermark`. Pure; allocates (it is called on CHANGE, never per
/// frame).
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "the six slots in their printed order, each beside its spoken twin; the story and hand arms are already helpers, and a further split would part the order from the twins"
)]
pub fn words<V: Host>(slot: &Slot<V>, now: Instant, watermark: u64) -> Words {
    // The row's words follow the level it is shown at (ruling 280): an
    // attention another place tells must not hide the story under it.
    let level = slot.shown_level(watermark);
    let tone = slot.tone(now, watermark);
    let role = slot
        .role
        .as_deref()
        .map_or_else(|| DASH.to_string(), |r| sanitize_token(r, 64));

    // phase + since. A TOLD point (`ctl story`) takes the slot for three
    // seconds ahead of everything: it is the watcher's decision, and the
    // phase it interrupts is still one keystroke of patience away.
    let (phase, mut since, mut spoken_phase) = if let Some((verb, text)) = slot.told_now(now)
        && let Some((glyph, word)) = verb.told_words()
    {
        let since = if text.is_empty() {
            Vec::new()
        } else {
            vec![text.to_string()]
        };
        let teller = verb.teller();
        let spoken = if text.is_empty() {
            format!("{word} by {teller}")
        } else {
            format!("{word} by {teller}, {text}")
        };
        (format!("{glyph} {word}"), since, spoken)
    } else if let Some(fact) = &slot.input_stall {
        stall_words(
            V::stall_since(fact),
            V::stall_stopped(fact),
            V::stall_survived(fact),
            now,
        )
    } else if let Some(text) = slot
        .attention
        .as_ref()
        .filter(|_| !slot.attention_told_elsewhere)
    {
        let t = sanitize_token(text, 48);
        (t.clone(), Vec::new(), format!("attention, {t}"))
    } else if level == Level::Story {
        story_phase(slot, now, watermark)
    } else if let Some(agent) = slot
        .agent
        .as_ref()
        // A phase the reader could not name is no phase to print (ruling
        // 366): `unknown 3m35s` told a person nothing but that aterm could
        // not read the screen. The row falls through to the shell's word or
        // the dash; `status phase=unknown` keeps the fact.
        .filter(|a| !matches!(a.phase, AgentPhase::Unknown))
    {
        let word = agent.phase.band_word();
        let mut clauses = Vec::new();
        let mut spoken = word.to_string();
        match &agent.phase {
            AgentPhase::Prompt { detail } => {
                let word = prompt_band_word(detail.as_deref());
                clauses.push(fmt_dur(now.saturating_duration_since(slot.agent_since)));
                spoken.clone_from(&word);
                (word, clauses, spoken)
            }
            AgentPhase::Wall { kind, reset, until } => {
                // `limited → 19:30 · 1d 22h`: the reset the notice named, then
                // the time TO it (design §1; the mock counts down) — never the
                // time since the limit began, which is the story's to tell.
                // A wall the harness retries prints its NEXT TRY the same way
                // (`can't reach the API → 14:05 · 3m`) and speaks it as one.
                if let Some(r) = reset {
                    clauses.push(format!("\u{2192} {r}"));
                    spoken = if V::wall_retries(*kind) {
                        format!("{word}, next try {r}")
                    } else {
                        format!("{word}, resets {r}")
                    };
                }
                if let Some(left) = until
                    .map(|u| u.saturating_duration_since(now))
                    .filter(|d| !d.is_zero())
                {
                    clauses.push(fmt_dur(left));
                }
                (word.to_string(), clauses, spoken)
            }
            _ => {
                clauses.push(fmt_dur(now.saturating_duration_since(slot.agent_since)));
                (word.to_string(), clauses, spoken)
            }
        }
    } else if let Some((word, at)) = slot.shell {
        (
            word.to_string(),
            vec![fmt_dur(now.saturating_duration_since(at))],
            word.to_string(),
        )
    } else {
        (DASH.to_string(), Vec::new(), String::new())
    };

    // hand (and its short form for a narrow row)
    let (hand, hand_short, spoken_hand) = hand_words(slot);
    if slot.hold.is_some() {
        // Under a hold the stop clause rides `since` so the summary law and the
        // live row agree on where a duration is printed — but not behind a
        // told word (`✓ approved 0s ⊘ hold review` read as an approval's age).
        if let Some((_, began)) = slot.stop_in_progress()
            && since.is_empty()
            && slot.told_now(now).is_none()
        {
            since.push(fmt_dur(now.saturating_duration_since(began)));
        }
    }

    // mail — no slot at all while there is none to tell (ruling 366): a
    // `✉0` on every row was a count of nothing, in a glyph a person has to
    // learn.
    let m = &slot.mail;
    let mut mail = if m.unread > 0 || m.pending > 0 || m.dropped > 0 || m.queued > 0 {
        format!("\u{2709}{}", m.unread)
    } else {
        String::new()
    };
    if m.pending > 0 {
        let _ = write!(mail, " \u{00b7}{}", m.pending);
    }
    if m.dropped > 0 {
        let _ = write!(mail, " \u{21af}{}", m.dropped);
    }
    if m.queued > 0 {
        let _ = write!(mail, " \u{2191}{}", m.queued);
    }
    let mail_short = mail.clone();
    let mut spoken_mail = String::new();
    if m.unread > 0 {
        spoken_mail = format!("{} unread", m.unread);
        if let Some(last) = &m.last {
            let kind = sanitize_token(&last.kind, 12);
            let from = sanitize_token(&last.from, 24);
            let _ = write!(mail, " {kind}\u{2190}{}{from}", trust_glyph(&last.trust));
            let _ = write!(spoken_mail, ", {kind} from {from}, {}", last.trust);
        }
    }

    // ctx
    // The context LEFT (`<n>% until auto-compact`, `<n>% context left`).
    let ctx = match slot.agent.as_ref().and_then(|a| a.context_pct) {
        Some(pct) if pct <= CTX_WARN_PCT => format!("ctx {pct}% left \u{26a0}"),
        Some(pct) => format!("ctx {pct}% left"),
        None => String::new(),
    };
    let spoken_ctx = slot
        .agent
        .as_ref()
        .and_then(|a| a.context_pct)
        .map(|p| format!("context {p} percent left"));

    // fabric
    let (fabric, spoken_fabric) = match slot.link {
        Link::Absent => ("\u{00b7}".to_string(), None),
        Link::Connected { rtt_ms: Some(ms) } => (format!("\u{27df} {ms}ms"), None),
        Link::Connected { rtt_ms: None } => ("\u{27df}".to_string(), None),
        // A stall with a date prints its age; one without (a bridge still
        // dialing since it attached) prints the glyph alone — never a figure
        // the model made up.
        Link::Stalled { age_ms: Some(a) } => {
            let s = a / 1000;
            (
                format!("~ {s}s"),
                Some(format!("bridge stalled {s} seconds")),
            )
        }
        Link::Stalled { age_ms: None } => ("~".to_string(), Some("bridge stalled".to_string())),
        Link::Disconnected => ("\u{2715} lost".to_string(), Some("bridge lost".to_string())),
    };

    if spoken_phase.is_empty() {
        spoken_phase = "quiet".to_string();
    }
    let mut sentence = Vec::new();
    if !spoken_hand.is_empty() {
        sentence.push(spoken_hand);
    }
    sentence.push(spoken_phase);
    if !spoken_mail.is_empty() {
        sentence.push(spoken_mail);
    }
    if let Some(c) = spoken_ctx.filter(|_| !ctx.is_empty()) {
        sentence.push(c);
    }
    if let Some(f) = spoken_fabric {
        sentence.push(f);
    }

    Words {
        role,
        phase,
        since,
        hand,
        hand_short,
        mail,
        mail_short,
        ctx,
        fabric,
        sentence: sentence.join(", "),
        tone,
    }
}

/// The phase slot at [`Level::Story`]: the summary of what happened after the
/// watermark (≤6 clauses, zero counts omitted): turns · timed out · mails ·
/// approvals · choices · questions · the longest stop that ended since. A
/// `choice` is the harness answering a question box by policy — counted apart
/// from the watcher's approvals, and apart from `question` (a box that
/// WAITED).
fn story_phase<V: Host>(
    slot: &Slot<V>,
    now: Instant,
    watermark: u64,
) -> (String, Vec<String>, String) {
    let count = |verb: StoryVerb| {
        slot.story_since(watermark)
            .filter(|p| p.verb == verb)
            .count()
    };
    let timeouts = count(StoryVerb::TurnTimedOut);
    let turns = count(StoryVerb::Turn) + timeouts;
    let mut story = Vec::new();
    for (n, word) in [
        (turns, "turn"),
        (timeouts, "timed out"),
        (count(StoryVerb::Mail), "mail"),
        (count(StoryVerb::Approval), "approval"),
        (count(StoryVerb::Chose), "choice"),
        (count(StoryVerb::Question), "question"),
    ] {
        if n > 0 {
            let plural = if n == 1 || word == "timed out" {
                ""
            } else {
                "s"
            };
            story.push(format!("{n} {word}{plural}"));
        }
    }
    // The longest stop that ENDED since the watermark: its verb, how long
    // it lasted, and how long ago it lifted.
    let mut stops: Vec<(StoryVerb, Instant, Instant)> = Vec::new();
    let mut open: Option<(StoryVerb, Instant)> = None;
    // A stop that LIFTED but never began in this story — it was already
    // standing when the slot was minted, so its length is unknown — is
    // told as the resume alone.
    let mut unpaired_resume: Option<Instant> = None;
    for p in slot.story_since(watermark) {
        match p.verb {
            StoryVerb::Hold | StoryVerb::Limited => open = Some((p.verb, p.at)),
            StoryVerb::Resumed => match open.take() {
                Some((v, began)) => stops.push((v, began, p.at)),
                None => unpaired_resume = Some(p.at),
            },
            _ => {}
        }
    }
    if let Some((verb, began, ended)) = stops
        .into_iter()
        .max_by_key(|(_, b, e)| e.saturating_duration_since(*b))
    {
        let word = if verb == StoryVerb::Hold {
            "held"
        } else {
            "limited"
        };
        story.push(format!(
            "{word} {}, resumed {} ago",
            fmt_dur(ended.saturating_duration_since(began)),
            fmt_dur(now.saturating_duration_since(ended))
        ));
    } else if let Some(ended) = unpaired_resume {
        story.push(format!(
            "resumed {} ago",
            fmt_dur(now.saturating_duration_since(ended))
        ));
    }
    // A worker still RUNNING is not quiet, story or no story: the live
    // phase keeps the slot (`busy 31s`) and the story rides `since` after
    // it — so a turn that timed out on a busy worker never reads `◇ quiet`.
    let live = match &slot.agent {
        Some(a) => matches!(a.phase, AgentPhase::Busy).then_some(("busy", slot.agent_since)),
        None => slot.shell.filter(|(w, _)| *w == "running"),
    };
    if let Some((word, at)) = live {
        let mut since = vec![fmt_dur(now.saturating_duration_since(at))];
        since.extend(story.iter().cloned());
        let spoken = if story.is_empty() {
            word.to_string()
        } else {
            format!("{word}, {}", story.join(", "))
        };
        (word.to_string(), since, spoken)
    } else {
        let mut clauses = Vec::new();
        if let Some(at) = slot.story_since(watermark).next().map(|p| p.at) {
            clauses.push(format!(
                "since {}",
                fmt_dur(now.saturating_duration_since(at))
            ));
        }
        clauses.extend(story);
        let spoken = format!("quiet, {}", clauses.join(", "));
        ("\u{25c7} quiet".to_string(), clauses, spoken)
    }
}

/// The hand slot, its short form for a narrow row, and its spoken twin.
fn hand_words<V: Host>(slot: &Slot<V>) -> (String, String, String) {
    match (&slot.hold, &slot.hand) {
        // aterm's own harness is named as aterm, never by its holder's pid
        // (ruling 313), on the rare row something else raised.
        (None, Hand::DrivenTurn { .. } | Hand::DrivenLease { .. }) if slot.aterm_hand => (
            "\u{25c2} aterm".to_string(),
            "\u{25c2} aterm".to_string(),
            "driven by aterm".to_string(),
        ),
        (Some(h), _) => {
            let reason = hold_reason_words::<V>(&h.reason);
            let fleet = if h.fleet {
                " \u{00b7}fleet\u{1f512}"
            } else {
                ""
            };
            (
                format!("\u{2298} hold {reason}{fleet}"),
                "\u{2298} hold".to_string(),
                format!(
                    "held, {reason}{}",
                    if h.fleet {
                        ", fleet, cannot be lifted here"
                    } else {
                        ""
                    }
                ),
            )
        }
        (
            None,
            Hand::DrivenTurn {
                id,
                holder: Some(h),
            },
        ) => {
            let h = sanitize_token(h, 32);
            (
                format!("\u{25c2} {h} \u{00b7} turn {id}"),
                format!("\u{25c2} turn {id}"),
                format!("driven by {h}, turn {id}"),
            )
        }
        (None, Hand::DrivenTurn { id, holder: None }) => (
            format!("\u{25c2} turn {id}"),
            format!("\u{25c2} turn {id}"),
            format!("driven, turn {id}"),
        ),
        (None, Hand::DrivenLease { holder }) => {
            let h = sanitize_token(holder, 32);
            (
                format!("\u{25c2} {h}"),
                format!("\u{25c2} {}", truncate(&h, 8)),
                format!("driven by {h}"),
            )
        }
        (None, Hand::Driving { sid }) => {
            let s = sanitize_token(sid, 12);
            (
                format!("\u{25b8} @{s}"),
                format!("\u{25b8} @{s}"),
                format!("driving session {s}"),
            )
        }
        (None, Hand::None) => (DASH.to_string(), DASH.to_string(), String::new()),
    }
}
