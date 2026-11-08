// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FOOTER'S FACTS, CLICKED.

use std::path::Path;

use aterm_agent::harness::footer::{self, FactKind, FactMask, FooterFacts};
use aterm_agent::harness::outlook;
use aterm_agent::harness::quota_view::QuotaView;
use aterm_agent::harness::session_usage::{self, UsageFacts};

use crate::claude_lights::Fact;
use crate::{App, WindowId};

/// One session's clickable facts.
#[derive(Debug, Default, Clone)]
pub(crate) struct SessionFacts {
    /// The facts the person hid here by a click: per session, in memory only, never persisted.
    /// `None` where they never chose: the program's starting mask applies
    /// ([`default_mask_for`]), however this entry came to exist.
    pub(crate) off: Option<FactMask>,
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
    /// The process (its group) whose top rule last showed Claude's top-effort tag.
    pub(crate) top_seen: Option<i32>,
    /// Why a fact menu's typed row was refused, said in the menu reopened over it.
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

/// The facts a session starts with hidden before the person chooses: Muse's own
/// composer status line prints its model, effort and working folder, so a Muse
/// session's rule starts with those three off (their stubs keep the restore click);
/// every other program's line does not, so nothing starts hidden.
#[must_use]
pub(crate) fn default_mask_for(program: Option<aterm_phase::Program>) -> FactMask {
    match program {
        Some(aterm_phase::Program::Muse) => FactMask::MODEL
            .union(FactMask::EFFORT)
            .union(FactMask::PATH),
        Some(
            aterm_phase::Program::Claude
            | aterm_phase::Program::Codex
            | aterm_phase::Program::Agy
            | aterm_phase::Program::Grok
            | aterm_phase::Program::Kimi
            | aterm_phase::Program::Generic,
        )
        | None => FactMask::default(),
    }
}

/// The facts a click on `fact` hides, or shows again where it is a stub: the next mask after `off`.
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

/// The pane's quota view while its profile is present: a transient profile miss may keep an old
/// view, and like line D's unit its words are said only while the profile is.
fn account_view(facts: &FooterFacts) -> Option<&QuotaView> {
    facts.subscription.as_ref()?;
    facts.quota_view()
}

/// The rotation's words (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §5.1), the account's
/// title's and speech's last clause, its next check on the glass's local clock (`spoken`: as a
/// screen reader says it), in UTC where the local zone does not read; `None` with no progress
/// read.
fn rotation_clause(facts: &FooterFacts, spoken: bool) -> Option<String> {
    let progress = facts.account_progress_view()?;
    let now = aterm_time::unix_secs()
        .and_then(|n| i64::try_from(n).ok())
        .unwrap_or(0);
    let offsets = outlook::Offsets::read_around(now, crate::presence::local_offset_at);
    Some(footer::rotation_words(&progress, |at| match &offsets {
        Some(o) if spoken => outlook::spoken_when(at, now, o),
        Some(o) => outlook::when(at, now, o),
        None => aterm_agent::harness::usage::rfc3339_utc(at),
    }))
}

/// The limit wall line D's `⧗` says, in words: `limit until 3pm`.
fn wall_clause(facts: &FooterFacts) -> Option<String> {
    let wall = facts.usage.as_ref()?.wall.as_ref()?;
    Some(format!("limit {}", session_usage::wall_text(wall)))
}

/// What a fact's title says, whole and short, hovered or selected (design §10.6.14).
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
                "{} {} \u{b7} click: hide \u{b7} right-click: compact",
                aterm_agent::harness::footer::CONTEXT_MARK,
                facts.context.as_deref().unwrap_or("")
            ),
            hide,
        ),
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
            // The wall, the quota row's title (§2.6) or the account's own words, then the
            // rotation's words last before the click's (§5.1).
            let mut clauses: Vec<String> = Vec::new();
            let sub = facts.subscription.as_ref();
            let gap = sub
                .filter(|sub| sub.weekly.is_none())
                .and_then(|sub| sub.weekly_gap)
                .map(|gap| gap.words().to_owned());
            clauses.extend(gap);
            clauses.extend(rotation_clause(facts, false));
            let clause = (!clauses.is_empty()).then(|| clauses.join(" \u{b7} "));
            let title = match account_view(facts) {
                Some(view) => view.title_with(clause.as_deref()),
                None => {
                    // Line D's unit's name, never the email (§2.5): the title is painted in
                    // line D's lights block, on the glass.
                    let mut parts: Vec<&str> = Vec::new();
                    parts.extend(footer::unit_name(facts));
                    if let Some(sub) = sub {
                        parts.extend(sub.plan.as_deref());
                        parts.extend(sub.weekly.as_deref());
                    }
                    let lead = if parts.is_empty() {
                        "\u{2386} accounts".to_owned()
                    } else {
                        format!("\u{2386} {}", parts.join(" \u{b7} "))
                    };
                    [Some(lead), clause, Some(QUOTA_CLICK.to_owned())]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" \u{b7} ")
                }
            };
            let title = match wall_clause(facts) {
                Some(wall) => format!("{wall} \u{b7} {title}"),
                None => title,
            };
            (title, QUOTA_CLICK.to_owned())
        }
    }
}

/// What a click on the account does, its title's last words.
const QUOTA_CLICK: &str = aterm_agent::harness::quota_view::CLICK;

/// The top-effort key's title.
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

/// What a screen reader names the top-effort key.
#[cfg(any(a11y_tree, test))]
#[must_use]
pub(crate) fn top_spoken(on: bool) -> (String, String) {
    let (state, to) = if on { ("on", "off") } else { ("off", "on") };
    (
        format!("{}: {state}", crate::claude_slash::key_capitalised()),
        format!("Activate to turn it {to} for this session."),
    )
}

/// A token count as a screen reader says it: `940`, `1.2 thousand`, `20 million`, `3.1 billion`.
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
/// seconds`, `1 minute`, `7 hours 52 minutes`, a zero second unit left unsaid.
#[must_use]
pub(crate) fn runtime_spoken(secs: u64) -> String {
    let unit = |n: u64, one: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {one}s")
        }
    };
    let both = |n: u64, one: &str, k: u64, other: &str| {
        if k == 0 {
            unit(n, one)
        } else {
            format!("{} {}", unit(n, one), unit(k, other))
        }
    };
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    match secs {
        0..60 => unit(s, "second"),
        60..3600 => both(m, "minute", s, "second"),
        _ => both(h, "hour", m, "minute"),
    }
}

/// What a screen reader names a fact and says a press on it does (design §10.6.14). A hidden
/// fact's greyed stub (`restores`) is named as the fact is, with `off` after it as the glass
/// draws it: `Branch: main off` for `⎇ main off`, though the folder's names its full working
/// directory where the glass draws the footer's short path.
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
    let hide = "Activate to hide it.".to_owned();
    let (label, description) = match fact {
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
            // The quota row's sentences (§2.6) or the account's own words, the wall first and
            // the rotation's words last (§5.1).
            let words = match account_view(facts) {
                Some(view) => view
                    .spoken
                    .strip_prefix("Accounts. ")
                    .unwrap_or(&view.spoken)
                    .trim_end_matches('.')
                    .to_owned(),
                None => {
                    // As the title says it: line D's unit's name, never the email (§2.5).
                    let mut parts: Vec<String> = Vec::new();
                    parts.extend(footer::unit_name(facts).map(str::to_owned));
                    if let Some(sub) = facts.subscription.as_ref() {
                        parts.extend(sub.plan.clone());
                        parts.extend(
                            sub.weekly
                                .as_deref()
                                .map(|words| words.replace('\u{2265}', "at least "))
                                .or_else(|| sub.weekly_gap.map(|gap| gap.spoken().to_owned())),
                        );
                    }
                    parts.join(", ")
                }
            };
            let mut sentences: Vec<String> = Vec::new();
            sentences.extend(wall_clause(facts).map(|wall| format!("Usage {wall}")));
            sentences.extend((!words.is_empty()).then_some(words));
            sentences.extend(rotation_clause(facts, true).map(|rotation| {
                let mut chars = rotation.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().chain(chars).collect()
                })
            }));
            let description = if facts.account_progress_view().is_some() {
                "Activate for account recovery details."
            } else {
                "Activate for the accounts menu."
            };
            (
                format!("{name}: {}", sentences.join(". ")),
                description.to_owned(),
            )
        }
        Fact::TopEffort => top_spoken(facts.top_effort.unwrap_or(false)),
    };
    if restores.is_empty() {
        return (label, description);
    }
    (
        format!("{label} {}", aterm_agent::harness::footer::OFF_STUB),
        "Activate to show it again.".to_owned(),
    )
}

impl App {
    /// The facts the person hid in `session` ([`SessionFacts::off`]). Where they never
    /// chose, a Muse session starts with the model, effort and path hidden: Muse's own
    /// composer status line already prints the three, so the rule showing them again is
    /// pure repetition. The off stubs stay, one click from restoring each fact, and any
    /// choice the person makes replaces this starting point whole.
    ///
    /// Callers already holding the session's timeline lock must use
    /// [`Self::fact_mask_known`] with the program that lock gives them: this reads the
    /// program under that same lock where no choice is stored yet.
    #[must_use]
    pub(crate) fn fact_mask(&self, session: u64) -> FactMask {
        if let Some(off) = self.chosen_mask(session) {
            return off;
        }
        default_mask_for(self.session_program(session))
    }

    /// The mask the person chose in `session`, where they chose one.
    fn chosen_mask(&self, session: u64) -> Option<FactMask> {
        self.claude_facts.get(&session).and_then(|f| f.off)
    }

    /// [`Self::fact_mask`] where the caller already knows `session`'s program (it holds
    /// the timeline lock): no lock is taken here.
    #[must_use]
    pub(crate) fn fact_mask_known(
        &self,
        session: u64,
        program: Option<aterm_phase::Program>,
    ) -> FactMask {
        self.chosen_mask(session)
            .unwrap_or_else(|| default_mask_for(program))
    }

    fn session_program(&self, session: u64) -> Option<aterm_phase::Program> {
        self.pool.get(session).and_then(|entry| {
            let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline
                .agent()
                .program
                .as_deref()
                .and_then(aterm_phase::program_of)
        })
    }

    /// WHAT A CLICK ON `session`'s `fact` DOES.
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
    /// slot.
    fn press_top_effort(&mut self, wid: WindowId, session: u64) {
        let want = !self.engine_top_tag(session).unwrap_or(false);
        let Err(why) = self.press_slash(wid, session, crate::claude_slash::Slash::TopEffort(want))
        else {
            return;
        };
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

    /// Hide `session`'s facts in `off` and show the rest, in every window that shows it.
    /// The choice is kept even where it only restates the starting mask: from here it is
    /// the person's, not the program's.
    pub(crate) fn set_fact_mask(&mut self, session: u64, off: FactMask) {
        let before = self.fact_mask(session);
        self.claude_facts.entry(session).or_default().off = Some(off);
        if before == off {
            return;
        }
        self.refresh_fact_readings(session);
        self.refresh_fact_menu(session);
        self.redraw_claude_footer(session);
    }

    /// `session`'s facts as its rule shows them (the kept launch card's fill,
    /// the person's hidden facts laid in) and its absolute working directory
    /// where the resolver published one: what a title and a reader's words
    /// are made of.
    pub(crate) fn shown_facts(
        &self,
        session: u64,
    ) -> Option<(FooterFacts, Option<std::path::PathBuf>)> {
        let entry = self.pool.get(session)?;
        let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        let mut facts = timeline
            .claude_footer()?
            .filled_from(timeline.claude_card());
        facts.off = self.fact_mask_known(
            session,
            timeline
                .agent()
                .program
                .as_deref()
                .and_then(aterm_phase::program_of),
        );
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

    /// What a screen reader names `session`'s `fact` and says a press on it does.
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

    /// The on/off a screen reader is told of `session`'s `fact` where it is a
    /// two-state toggle (`Role::Switch`, as the harness switch is): the top
    /// rule's key, on where Claude draws its tag, as [`top_spoken`] names it.
    #[cfg(a11y_tree)]
    #[must_use]
    pub(crate) fn fact_toggled(
        &self,
        session: u64,
        fact: Fact,
    ) -> Option<aterm_a11y::model::Toggled> {
        (fact == Fact::TopEffort).then(|| self.engine_top_tag(session).unwrap_or(false).into())
    }
}

#[cfg(test)]
#[path = "claude_facts_tests.rs"]
mod tests;
