// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FOOTER'S FACTS, CLICKED (owner, 2026-10-19: "because I can click the
//! cruise icon for cruise versus land, I need to also be able to click the
//! other text on that line … likewise for model, effort, working directory,
//! branch"). The facts aterm sets in the composer's bottom rule
//! (`crate::claude_footer`) are targets as the chips beside them are
//! (`crate::claude_lights`): only where the last frame put them on the glass
//! ([`crate::claude_lights::FactHit`]), never under a band, all GLASS —
//! nothing is written into the grid, no row is re-gridded and Claude's own
//! ink is never covered.
//!
//! * A LEFT CLICK on the model, the effort, the working directory or the
//!   branch HIDES it: the rule shows it greyed with `off` after it, in
//!   Claude's rule gray and not bold (`◆ Opus 5.5 off xhigh`, `◆ Opus 5.5
//!   xhigh off`, `◆ Opus 5.5 off xhigh off` with both, `⌂ ~/aterm off`, `⎇
//!   main off`), as the top rule's key reads off, and a click on the stub
//!   shows it again.
//!   Nothing reaches the PTY. A stub gives way first in a rule short of room
//!   (`footer::fit_rule`).
//! * HIDDEN IS PER SESSION AND IN MEMORY ONLY ([`SessionFacts::off`]), for
//!   every window that shows the session: nothing is persisted, and every
//!   hide is undone by one click.
//! * Hovered or selected, a fact's glyphs are lit and its title stands in the
//!   lights' title slot where a block of chips is drawn ([`fact_title`]),
//!   built when the reading starts and when the facts move under it, never
//!   per frame (`App::note_fact_readings`).
//! * A RIGHT-CLICK (a ctrl-click on macOS; `↓`, shift+F10 or the Menu key on
//!   a selected fact) opens the fact's own glass menu (`crate::fact_menu`):
//!   copy the path or the branch, a new tab there, hide or show any fact —
//!   and for the model and the effort Claude's own `/model`, `/effort` and
//!   the top-effort key's session-only toggle, the only rows that type,
//!   through the facts' one typing path (`crate::claude_slash`).
//! * THE TOP RULE'S EFFORT KEY is a target too: Claude's own tag while the
//!   key is on, and — on evidence the key exists for the process, never over
//!   Claude's ink — aterm's greyed ` <key> off ` ghost in Claude's slot while
//!   it is off (`claude_footer`'s top rule). A left click types Claude's own
//!   session-only `/effort <key> on|off`, what it turns the key to read off
//!   the engine's top rule ([`App::press_claude_fact`]); Claude's refusal
//!   latches the ghost off for the process and its model.
//! * ctrl+shift+tab walks the chips, then the facts the glass shows, left to
//!   right, the top rule's key last; Return or Space is the selected fact's
//!   click.
//! * Where the accessibility tree is built each fact is a button named in
//!   words, and a reader's press is the click (`App::fact_spoken`).

use std::path::Path;

use aterm_agent::harness::footer::{FactKind, FactMask, FooterFacts};
use aterm_agent::harness::session_usage::{self, UsageFacts};

use crate::claude_lights::Fact;
use crate::{App, WindowId};

/// One session's clickable facts, kept by the App for every window that
/// shows the session (`App::claude_facts`) and dropped with it.
#[derive(Debug, Default, Clone)]
pub(crate) struct SessionFacts {
    /// The facts the person hid here by a click: per session, in memory
    /// only, never persisted (the top-effort key's own toggle is "this
    /// session only", and "likewise" gets the same lifetime).
    pub(crate) off: FactMask,
    /// The one slash press in flight for the session (`crate::claude_slash`).
    pub(crate) press: Option<crate::claude_slash::SlashPress>,
    /// How the last press ended, until its words' showing ends.
    pub(crate) ended: Option<(
        crate::claude_slash::Slash,
        crate::claude_slash::SlashEnd,
        std::time::Instant,
    )>,
    /// The top-effort key latched off by Claude's refusal.
    pub(crate) latch: Option<crate::claude_slash::KeyLatch>,
    /// The process (its group) whose top rule last showed Claude's
    /// top-effort tag, or that turned the key on at a click here: evidence
    /// the key exists for it, so its ghost may stand in for the tag while
    /// the key is off (`claude_footer`'s top rule).
    pub(crate) top_seen: Option<i32>,
    /// Why a fact menu's typed row was refused, said in the menu reopened
    /// over it: the fact and the words.
    pub(crate) menu_note: Option<(Fact, String)>,
}

/// The hideable facts, in the rule's order.
const HIDEABLE: [FactKind; 5] = [
    FactKind::Model,
    FactKind::Effort,
    FactKind::Path,
    FactKind::Branch,
    FactKind::Context,
];

/// `mask` with every fact of `shown` out of it.
fn without(mask: FactMask, shown: FactMask) -> FactMask {
    HIDEABLE
        .into_iter()
        .filter(|kind| shown.has(*kind))
        .fold(mask, |mask, kind| mask.with(kind, false))
}

/// The facts a click on `fact` hides, or shows again where it is a stub
/// (`restores`, the stub's own facts): the next mask after `off`. The usage
/// is never hidden by a click. Pure.
#[must_use]
pub(crate) fn mask_after(off: FactMask, fact: Fact, restores: FactMask) -> FactMask {
    if !restores.is_empty() {
        return without(off, restores);
    }
    fact.kind().map_or(off, |kind| off.with(kind, true))
}

/// The tokens a usage says in and out: every model's, merged.
fn usage_pair(usage: &UsageFacts) -> (u64, u64) {
    usage
        .models
        .iter()
        .fold((usage.more.input, usage.more.output), |(i, o), m| {
            (i.saturating_add(m.input), o.saturating_add(m.output))
        })
}

/// What a fact's title says, whole and short, hovered or selected
/// (design §10.6.14): the value and what a click does; a hidden fact's that a click
/// shows it again. `cwd` is the session's absolute working directory where
/// the resolver published one (`claude_footer::FactSide`), else the path
/// fact. Pure.
#[must_use]
pub(crate) fn fact_title(facts: &FooterFacts, cwd: Option<&Path>, fact: Fact) -> (String, String) {
    // Each stub shows its own fact (`◆ Opus 5.5 off xhigh off` is two).
    if fact.kind().is_some_and(|kind| facts.off.has(kind)) {
        return (
            format!("{} hidden \u{b7} click shows it", fact.word()),
            "click: show".to_owned(),
        );
    }
    let hide = "click: hide".to_owned();
    match fact {
        Fact::Model => (
            format!(
                "{} \u{b7} click: hide \u{b7} right-click: model menu",
                facts.model.as_deref().unwrap_or("")
            ),
            hide,
        ),
        Fact::Effort => (
            format!(
                "effort {} \u{b7} click: hide \u{b7} right-click: effort menu",
                facts.effort.as_deref().unwrap_or("")
            ),
            hide,
        ),
        Fact::Path => {
            let shown = cwd.map_or_else(
                || facts.path.clone().unwrap_or_default(),
                |cwd| cwd.display().to_string(),
            );
            (
                format!("{shown} \u{b7} click: hide \u{b7} right-click: copy, new tab"),
                hide,
            )
        }
        Fact::Branch => (
            format!(
                "\u{2387} {} \u{b7} click: hide \u{b7} right-click: copy",
                facts.branch.as_deref().unwrap_or("")
            ),
            hide,
        ),
        Fact::Context => (
            format!(
                "{} {} \u{b7} click: hide",
                aterm_agent::harness::footer::CONTEXT_MARK,
                facts.context.as_deref().unwrap_or("")
            ),
            hide,
        ),
        // The top rule's key: what its click turns it to, its state as the
        // facts last stated it (`App::fact_title` reads the ENGINE's).
        Fact::TopEffort => top_title(facts.top_effort.unwrap_or(false)),
        Fact::Usage => {
            let Some(usage) = facts.usage.as_ref() else {
                return ("\u{03a3}".to_owned(), "\u{03a3}".to_owned());
            };
            let (input, output) = usage_pair(usage);
            let tokens = format!(
                "\u{2191}{} \u{2193}{}",
                session_usage::tokens_short(input),
                session_usage::tokens_short(output)
            );
            let whole = match usage.runtime {
                Some(secs) => format!(
                    "\u{03a3} {} working \u{b7} {tokens} \u{b7} click: graph over time",
                    session_usage::runtime_short(secs)
                ),
                None => format!("\u{03a3} {tokens} \u{b7} click: graph over time"),
            };
            (whole, "click: graph".to_owned())
        }
        Fact::Account => {
            let mut parts: Vec<&str> = Vec::new();
            if let Some(sub) = facts.subscription.as_ref() {
                if let Some(email) = sub.email.as_deref() {
                    parts.push(email);
                }
                if let Some(plan) = sub.plan.as_deref() {
                    parts.push(plan);
                }
                if let Some(weekly) = sub.weekly.as_deref() {
                    parts.push(weekly);
                }
            }
            if parts.is_empty()
                && let Some(acct) = facts.account.as_deref()
            {
                parts.push(acct);
            }
            let summary = if parts.is_empty() {
                "accounts".to_owned()
            } else {
                parts.join(" \u{b7} ")
            };
            (
                format!("\u{2386} {summary} \u{b7} click: accounts"),
                "click: accounts".to_owned(),
            )
        }
    }
}

/// The top-effort key's title, whole and short (design §10.6.14), `on` as the
/// engine's top rule shows it: what a click turns it to. Pure.
#[must_use]
pub(crate) fn top_title(on: bool) -> (String, String) {
    let key = aterm_primer::CLAUDE_TOP_EFFORT_KEY;
    if on {
        (
            format!("{key} on \u{b7} click turns it off for this session"),
            "click: off".to_owned(),
        )
    } else {
        (
            format!("{key} off \u{b7} click turns it on for this session"),
            "click: on".to_owned(),
        )
    }
}

/// What a screen reader names the top-effort key, `on` as the engine's top
/// rule shows it, and says a press on it does (design §10.6.14: `<Key>: on`).
/// Pure.
#[cfg(any(a11y_tree, test))]
#[must_use]
pub(crate) fn top_spoken(on: bool) -> (String, String) {
    let (state, to) = if on { ("on", "off") } else { ("off", "on") };
    (
        format!("{}: {state}", crate::claude_slash::key_capitalised()),
        format!("Activate to turn it {to} for this session."),
    )
}

/// A token count as a screen reader says it: `940`, `1.2 thousand`, `20
/// million`, `3.1 billion` (the footer's grain, in words).
#[cfg(any(a11y_tree, test))]
#[must_use]
pub(crate) fn tokens_spoken(n: u64) -> String {
    const UNITS: [(u64, &str); 3] = [
        (1_000_000_000, "billion"),
        (1_000_000, "million"),
        (1_000, "thousand"),
    ];
    for (unit, word) in UNITS {
        if n >= unit {
            let whole = n / unit;
            return if whole < 10 && (n % unit) / (unit / 10) > 0 {
                format!("{whole}.{} {word}", (n % unit) / (unit / 10))
            } else {
                format!("{whole} {word}")
            };
        }
    }
    n.to_string()
}

/// A working time as a screen reader says it: `42 seconds`, `6 minutes 33
/// seconds`, `7 hours 52 minutes` (the footer's grain, in words).
#[cfg(any(a11y_tree, test))]
#[must_use]
pub(crate) fn runtime_spoken(secs: u64) -> String {
    let unit = |n: u64, one: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {one}s")
        }
    };
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    match secs {
        0..60 => unit(s, "second"),
        60..3600 => format!("{} {}", unit(m, "minute"), unit(s, "second")),
        _ => format!("{} {}", unit(h, "hour"), unit(m, "minute")),
    }
}

/// What a screen reader names a fact and says a press on it does (design
/// §10.6.14), from the facts the rule shows and the session's absolute
/// working directory. Pure.
#[cfg(any(a11y_tree, test))]
#[must_use]
pub(crate) fn fact_spoken_of(
    facts: &FooterFacts,
    cwd: Option<&Path>,
    fact: Fact,
    restores: FactMask,
) -> (String, String) {
    let name = match fact {
        Fact::Model => "Model",
        Fact::Effort => "Effort",
        Fact::Path => "Working directory",
        Fact::Branch => "Branch",
        Fact::Context => "Context left",
        Fact::Usage => "Token traffic",
        Fact::Account => "Account",
        Fact::TopEffort => return top_spoken(facts.top_effort.unwrap_or(false)),
    };
    if !restores.is_empty() {
        return (
            format!("{name}: hidden"),
            "Activate to show it again.".to_owned(),
        );
    }
    let hide = "Activate to hide it.".to_owned();
    match fact {
        Fact::Model => (
            format!("{name}: {}", facts.model.as_deref().unwrap_or("")),
            hide,
        ),
        Fact::Effort => (
            format!("{name}: {}", facts.effort.as_deref().unwrap_or("")),
            hide,
        ),
        Fact::Path => {
            let shown = cwd.map_or_else(
                || facts.path.clone().unwrap_or_default(),
                |cwd| cwd.display().to_string(),
            );
            (
                format!("{name}: {shown}"),
                format!("{hide} Its menu copies it or opens a new tab there."),
            )
        }
        Fact::Branch => (
            format!("{name}: {}", facts.branch.as_deref().unwrap_or("")),
            format!("{hide} Its menu copies it."),
        ),
        Fact::Context => (
            format!("{name}: {}", facts.context.as_deref().unwrap_or("")),
            hide,
        ),
        Fact::Usage => {
            let words = facts.usage.as_ref().map_or_else(String::new, |usage| {
                let (input, output) = usage_pair(usage);
                let tokens = format!("{} in, {} out", tokens_spoken(input), tokens_spoken(output));
                match usage.runtime {
                    Some(secs) => format!("{} working, {tokens}", runtime_spoken(secs)),
                    None => tokens,
                }
            });
            (
                format!("{name}: {words}"),
                "Activate for a graph over time.".to_owned(),
            )
        }
        Fact::Account => {
            let mut parts: Vec<&str> = Vec::new();
            if let Some(sub) = facts.subscription.as_ref() {
                if let Some(email) = sub.email.as_deref() {
                    parts.push(email);
                }
                if let Some(plan) = sub.plan.as_deref() {
                    parts.push(plan);
                }
                if let Some(weekly) = sub.weekly.as_deref() {
                    parts.push(weekly);
                }
            }
            if parts.is_empty()
                && let Some(acct) = facts.account.as_deref()
            {
                parts.push(acct);
            }
            let words = parts.join(", ");
            (
                format!("{name}: {words}"),
                "Activate for the accounts menu.".to_owned(),
            )
        }
        Fact::TopEffort => top_spoken(facts.top_effort.unwrap_or(false)),
    }
}

impl App {
    /// The facts the person hid in `session` ([`SessionFacts::off`]).
    #[must_use]
    pub(crate) fn fact_mask(&self, session: u64) -> FactMask {
        self.claude_facts
            .get(&session)
            .map_or_else(FactMask::default, |f| f.off)
    }

    /// WHAT A CLICK ON `session`'s `fact` DOES — the pointer's, Return or
    /// Space on it selected, a reader's press — where it shows `restores`
    /// again for a stub: the model, the effort, the working directory and
    /// the branch hide behind their greyed `off`, and a stub shows its facts
    /// again ([`mask_after`]). In process; nothing reaches the PTY.
    pub(crate) fn press_claude_fact(
        &mut self,
        wid: WindowId,
        session: u64,
        fact: Fact,
        restores: FactMask,
    ) {
        // The top rule's key toggles for the session through Claude's own
        // command ([`Self::press_top_effort`]).
        if fact == Fact::TopEffort {
            self.press_top_effort(wid, session);
            return;
        }
        // The usage and account are never hidden: their click opens the
        // Traffic card or Accounts menu (`crate::fact_menu`).
        if matches!(fact, Fact::Usage | Fact::Account) {
            if let Some(anchor) = self.fact_anchor(wid, session, fact) {
                self.open_fact_menu(wid, session, fact, anchor, false);
            }
            return;
        }
        let next = mask_after(self.fact_mask(session), fact, restores);
        self.set_fact_mask(session, next);
    }

    /// THE KEY'S CLICK (design §10.6.14): what it turns the key to is read off the
    /// ENGINE's top rule, never the painted cells — off while Claude draws
    /// its tag, on where it does not — and typed as Claude's own `/effort
    /// <key> on|off` through the facts' one typing path
    /// ([`App::press_slash`]), which says a refusal in the lights' title
    /// slot. Where no block of chips shows that slot, the effort menu opens
    /// hung from the key instead, the refusal its note and its typed rows
    /// disabled. The gesture's own work alone: nothing is typed later.
    fn press_top_effort(&mut self, wid: WindowId, session: u64) {
        let want = !self.engine_top_tag(session).unwrap_or(false);
        let Err(why) = self.press_slash(wid, session, crate::claude_slash::Slash::TopEffort(want))
        else {
            return;
        };
        // A press already in flight speaks for itself (a double-click's
        // second press): no menu opens over the pane while aterm types.
        if why == crate::claude_slash::SlashRefusal::InFlight {
            return;
        }
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.claude_lights.shows_block(session))
        {
            return;
        }
        let Some(anchor) = self.fact_anchor(wid, session, Fact::TopEffort) else {
            return;
        };
        self.claude_facts.entry(session).or_default().menu_note =
            Some((Fact::TopEffort, why.says("", None)));
        self.open_fact_menu_keeping_note(wid, session, Fact::TopEffort, anchor, false);
    }

    /// Hide `session`'s facts in `off` and show the rest, in every window
    /// that shows it: its rule is composed anew on the next frame, and a
    /// title someone reads of them is built again.
    pub(crate) fn set_fact_mask(&mut self, session: u64, off: FactMask) {
        if self.fact_mask(session) == off {
            return;
        }
        self.claude_facts.entry(session).or_default().off = off;
        self.refresh_fact_readings(session);
        // An open fact menu's Hide and Show rows follow (`crate::fact_menu`).
        self.refresh_fact_menu(session);
        self.redraw_claude_footer(session);
    }

    /// `session`'s facts as its rule shows them (the kept launch card's fill,
    /// the person's hidden facts laid in) and its absolute working directory
    /// where the resolver published one: what a title and a reader's words
    /// are made of. One timeline lock; an event's read, never a frame's.
    fn shown_facts(&self, session: u64) -> Option<(FooterFacts, Option<std::path::PathBuf>)> {
        let entry = self.pool.get(session)?;
        let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        let mut facts = timeline
            .claude_footer()?
            .filled_from(timeline.claude_card());
        facts.off = self.fact_mask(session);
        let cwd = timeline.claude_side().and_then(|side| side.cwd.clone());
        Some((facts, cwd))
    }

    /// What `session`'s `fact`'s title says, whole and short ([`fact_title`]).
    #[must_use]
    pub(crate) fn fact_title(&self, session: u64, fact: Fact) -> (String, String) {
        if fact == Fact::TopEffort {
            return top_title(self.engine_top_tag(session).unwrap_or(false));
        }
        self.shown_facts(session)
            .map(|(facts, cwd)| fact_title(&facts, cwd.as_deref(), fact))
            .unwrap_or_default()
    }

    /// What a screen reader names `session`'s `fact` (standing for
    /// `restores`, a stub's) and says a press on it does ([`fact_spoken_of`]).
    #[cfg(a11y_tree)]
    #[must_use]
    pub(crate) fn fact_spoken(
        &self,
        session: u64,
        fact: Fact,
        restores: FactMask,
    ) -> (String, String) {
        if fact == Fact::TopEffort {
            return top_spoken(self.engine_top_tag(session).unwrap_or(false));
        }
        self.shown_facts(session)
            .map(|(facts, cwd)| fact_spoken_of(&facts, cwd.as_deref(), fact, restores))
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[path = "claude_facts_tests.rs"]
mod tests;
