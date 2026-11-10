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
    /// The model fact's switch between the session's own model and [`SWITCH_MODEL`] at
    /// [`SWITCH_EFFORT`] (per session, in memory only).
    pub(crate) switch: ModelSwitch,
}

/// The model a click on the model fact or its effort switches to.
pub(crate) const SWITCH_MODEL: &str = "claude-opus-5-5";
/// The effort a click on the model fact or its effort switches to, with [`SWITCH_MODEL`].
pub(crate) const SWITCH_EFFORT: &str = "xhigh";

/// A model and effort a switch types back to: `None` for each is Claude's own default
/// (`/model default`, `/effort auto`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pick {
    pub(crate) model: Option<crate::claude_slash::ModelArg>,
    pub(crate) effort: Option<&'static str>,
}

impl Pick {
    /// The pick in words: `Sonnet 4.5 high`, `the default model at auto effort`.
    #[must_use]
    pub(crate) fn words(self) -> String {
        let model = self.model.map_or_else(
            || "the default model".to_owned(),
            |id| footer::model_display(id.as_str()),
        );
        match self.effort {
            Some(level) => format!("{model} {level}"),
            None => format!("{model} at auto effort"),
        }
    }
}

/// One session's model switch: a click on the model or its effort types `/model` then, once
/// that is answered, `/effort`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ModelSwitch {
    /// What the session ran before its last switch to [`SWITCH_MODEL`]: where a click back goes.
    pub(crate) back: Option<Pick>,
    /// The `/model` in flight and the `/effort` typed once it is answered.
    pub(crate) then: Option<(crate::claude_slash::Slash, crate::claude_slash::Slash)>,
    /// A switch began and its `/effort` has not been answered: the model shown may be the
    /// switch's half-way, not the session's own, so it is not remembered over [`Self::back`].
    pub(crate) half: bool,
}

/// What a click on the model or its effort does from what `facts` show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SwitchPlan {
    pub(crate) model: crate::claude_slash::Slash,
    pub(crate) effort: crate::claude_slash::Slash,
    /// What to remember as the session's own (a switch toward [`SWITCH_MODEL`]).
    pub(crate) remember: Option<Pick>,
    /// Where it goes.
    pub(crate) to: Pick,
    /// It goes back from [`SWITCH_MODEL`] at [`SWITCH_EFFORT`].
    pub(crate) back: bool,
}

impl SwitchPlan {
    /// Where it goes, in words: `switch to Opus 5.5 xhigh`, `back to Sonnet 4.5 high`.
    #[must_use]
    pub(crate) fn words(self) -> String {
        if self.back {
            format!("back to {}", self.to.words())
        } else {
            format!("switch to {}", self.to.words())
        }
    }
}

/// The context variant Claude Code spells after a model id: `claude-opus-4-6[1m]` runs with a
/// million tokens of context.
const LONG_CONTEXT: &str = "[1m]";

/// The id the session's shown model runs as: the usage tap's `model.id` where its display is
/// the one shown, which keeps the context variant ([`LONG_CONTEXT`]) the display name drops;
/// else the display name read back ([`footer::model_id_of_display`]), which cannot say it.
fn shown_model_id(facts: &FooterFacts) -> Option<String> {
    let shown = facts.model.as_deref()?;
    facts
        .subscription
        .as_ref()
        .and_then(|sub| sub.model_id.as_deref())
        .filter(|id| footer::model_display(id) == shown)
        .map(str::to_owned)
        .or_else(|| footer::model_id_of_display(shown))
}

/// Where a click on the model fact or its effort switches `facts`' session: to [`SWITCH_MODEL`]
/// at [`SWITCH_EFFORT`] (in the shown model's context variant), remembering the model and
/// effort shown as the session's own, or, where those are shown, back to the remembered pick
/// (Claude's defaults where none is).
#[must_use]
pub(crate) fn switch_plan(facts: &FooterFacts, switch: &ModelSwitch) -> SwitchPlan {
    use crate::claude_slash::{ModelArg, Slash};
    let effort = facts.effort.as_deref().and_then(footer::effort_level);
    let at = facts.model.as_deref() == Some(footer::model_display(SWITCH_MODEL).as_str())
        && effort == Some(SWITCH_EFFORT);
    let slashes = |pick: Pick| {
        (
            Slash::Model(pick.model),
            Slash::Effort(pick.effort.unwrap_or("auto")),
        )
    };
    if at {
        let to = switch.back.unwrap_or_default();
        let (model, effort) = slashes(to);
        return SwitchPlan {
            model,
            effort,
            remember: None,
            to,
            back: true,
        };
    }
    let own = shown_model_id(facts);
    let long = own.as_deref().is_some_and(|id| id.ends_with(LONG_CONTEXT));
    let to = Pick {
        model: if long {
            ModelArg::new(&format!("{SWITCH_MODEL}{LONG_CONTEXT}"))
        } else {
            ModelArg::new(SWITCH_MODEL)
        },
        effort: Some(SWITCH_EFFORT),
    };
    let (model, effort_slash) = slashes(to);
    let shown = Pick {
        model: own.as_deref().and_then(ModelArg::new),
        effort,
    };
    SwitchPlan {
        model,
        effort: effort_slash,
        remember: (!(switch.half && switch.back.is_some())).then_some(shown),
        to,
        back: false,
    }
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
/// The model and its effort are never hidden by a click ([`App::switch_model`]); their stubs
/// (a Muse session's starting mask) show them again.
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
    // Its accounts named as the title's lead names this one, by email (§2.5).
    let progress = match crate::accounts_outlook::snapshot().and_then(|s| s.outlook.clone()) {
        Some(outlook) => {
            std::borrow::Cow::Owned(progress.named(|label| outlook.name_of(label).to_owned()))
        }
        None => progress,
    };
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

/// What a fact's title says, whole and short, hovered or selected (design §10.6.14), for a
/// session with no model switch made ([`fact_title_with`]).
#[cfg(test)]
#[must_use]
pub(crate) fn fact_title(facts: &FooterFacts, cwd: Option<&Path>, fact: Fact) -> (String, String) {
    fact_title_with(facts, cwd, fact, Some(&ModelSwitch::default()))
}

/// What a fact's title says, whole and short, the model's and the effort's naming where a click
/// switches the session ([`switch_plan`] of `switch`): `None` where the session is no Claude
/// Code, whose model and effort a click hides.
#[must_use]
pub(crate) fn fact_title_with(
    facts: &FooterFacts,
    cwd: Option<&Path>,
    fact: Fact,
    switch: Option<&ModelSwitch>,
) -> (String, String) {
    // Each stub shows its own fact (`◆ Opus 5.5 off xhigh off` is two).
    if fact.kind().is_some_and(|kind| facts.off.has(kind)) {
        return (
            format!("{} hidden \u{b7} click shows it", fact.word()),
            "click: show".to_owned(),
        );
    }
    let hide = "click: hide".to_owned();
    let switch = || {
        switch.map_or_else(
            || "click: hide".to_owned(),
            |switch| format!("click: {}", switch_plan(facts, switch).words()),
        )
    };
    match fact {
        Fact::Model => (
            format!(
                "{} \u{b7} {} \u{b7} right-click: model menu",
                facts.model.as_deref().unwrap_or(""),
                switch()
            ),
            switch(),
        ),
        Fact::Effort => (
            format!(
                "effort {} \u{b7} {} \u{b7} right-click: effort menu",
                facts.effort.as_deref().unwrap_or(""),
                switch()
            ),
            switch(),
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
            // Where the login file contradicts the roster, the hover says both.
            clauses.extend(facts.account_detail.clone());
            clauses.extend(rotation_clause(facts, false));
            let clause = (!clauses.is_empty()).then(|| clauses.join(" \u{b7} "));
            let title = match account_view(facts) {
                Some(view) => view.title_with(clause.as_deref()),
                None => {
                    // Line D's unit's name: a rostered login's email, else its label (§2.5).
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
#[cfg(test)]
#[must_use]
pub(crate) fn fact_spoken_of(
    facts: &FooterFacts,
    cwd: Option<&Path>,
    fact: Fact,
    restores: FactMask,
) -> (String, String) {
    fact_spoken_with(facts, cwd, fact, restores, Some(&ModelSwitch::default()))
}

/// [`fact_spoken_of`] for a session whose model switch stands at `switch` (`None`: no Claude
/// Code, whose model and effort a press hides).
#[cfg(any(a11y_tree, test))]
#[must_use]
pub(crate) fn fact_spoken_with(
    facts: &FooterFacts,
    cwd: Option<&Path>,
    fact: Fact,
    restores: FactMask,
    switch: Option<&ModelSwitch>,
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
    let switch = || {
        switch.map_or_else(
            || "Activate to hide it.".to_owned(),
            |switch| {
                let plan = switch_plan(facts, switch);
                let way = if plan.back { "back to" } else { "to" };
                format!("Activate to switch {way} {}.", plan.to.words())
            },
        )
    };
    let (label, description) = match fact {
        Fact::Model => (
            format!("{name}: {}", facts.model.as_deref().unwrap_or("")),
            switch(),
        ),
        Fact::Effort => (
            format!("{name}: {}", facts.effort.as_deref().unwrap_or("")),
            switch(),
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
                    // As the title says it: line D's unit's name (§2.5).
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
        // The model and its effort switch a Claude Code session (a stub shows its fact again);
        // elsewhere, where nothing types `/model`, a click hides them.
        if matches!(fact, Fact::Model | Fact::Effort)
            && restores.is_empty()
            && self.model_switch(session).is_some()
        {
            if let Err(why) = self.switch_model(wid, session) {
                self.fact_press_refused(wid, session, fact, why);
            }
            return;
        }
        let next = mask_after(self.fact_mask(session), fact, restores);
        self.set_fact_mask(session, next);
    }

    /// `session`'s model switch as it stands; `None` where the session is no Claude Code, the
    /// one program aterm types `/model` and `/effort` into ([`App::press_slash`]).
    #[must_use]
    pub(crate) fn model_switch(&self, session: u64) -> Option<ModelSwitch> {
        let claude = self.pool.get(session).is_some_and(|entry| {
            let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            crate::harness_host::agent_of(timeline.agent()) == Some(aterm_phase::Program::Claude)
        });
        claude.then(|| {
            self.claude_facts
                .get(&session)
                .map(|f| f.switch.clone())
                .unwrap_or_default()
        })
    }

    /// Where a click on `session`'s model or effort switches it ([`switch_plan`]); `None` where
    /// it does not ([`Self::model_switch`]).
    #[must_use]
    pub(crate) fn switch_plan(&self, session: u64) -> Option<SwitchPlan> {
        let switch = self.model_switch(session)?;
        let facts = self
            .shown_facts(session)
            .map(|(facts, _)| facts)
            .unwrap_or_default();
        Some(switch_plan(&facts, &switch))
    }

    /// THE MODEL SWITCH: type `session`'s [`switch_plan`]'s `/model` through the facts' one
    /// typing path ([`App::press_slash`]), its `/effort` following once Claude answers the first
    /// (`crate::claude_slash`'s `end_slash`). The model and effort shown are remembered as the
    /// session's own only once the first press begins.
    pub(crate) fn switch_model(
        &mut self,
        wid: WindowId,
        session: u64,
    ) -> Result<(), crate::claude_slash::SlashRefusal> {
        let Some(plan) = self.switch_plan(session) else {
            return Err(crate::claude_slash::SlashRefusal::NotReady);
        };
        self.press_slash(wid, session, plan.model)?;
        let switch = &mut self.claude_facts.entry(session).or_default().switch;
        if let Some(pick) = plan.remember {
            switch.back = Some(pick);
        }
        switch.then = Some((plan.model, plan.effort));
        switch.half = true;
        Ok(())
    }

    /// The `/effort` a switch types once Claude answered `done`, its `/model`: begun here, or
    /// the switch dropped where `done` was not that `/model` or did not end done.
    pub(crate) fn switch_after(
        &mut self,
        wid: WindowId,
        session: u64,
        done: crate::claude_slash::Slash,
        end: crate::claude_slash::SlashEnd,
    ) {
        let Some(facts) = self.claude_facts.get_mut(&session) else {
            return;
        };
        // Any end but a done `/model` with its `/effort` still to type ends the switch, and
        // with it the half-way: the model shown is the session's own again.
        let next = match facts.switch.then.take() {
            Some((after, next)) if end == crate::claude_slash::SlashEnd::Done && after == done => {
                next
            }
            _ => {
                facts.switch.half = false;
                return;
            }
        };
        if self.press_slash(wid, session, next).is_err()
            && let Some(facts) = self.claude_facts.get_mut(&session)
        {
            facts.switch.half = false;
        }
    }

    /// Say why a click on `fact` typed nothing: in the lights' title slot ([`App::press_slash`]
    /// already did), and, where no block covers the lights, in `fact`'s menu opened over it.
    fn fact_press_refused(
        &mut self,
        wid: WindowId,
        session: u64,
        fact: Fact,
        why: crate::claude_slash::SlashRefusal,
    ) {
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
        let Some(anchor) = self.fact_anchor(wid, session, fact) else {
            return;
        };
        self.claude_facts.entry(session).or_default().menu_note = Some((fact, why.says("", None)));
        self.open_fact_menu_keeping_note(wid, session, fact, anchor, false);
    }

    /// THE KEY'S CLICK (design §10.6.14): what it turns the key to is read off the
    /// ENGINE's top rule, never the painted cells — off while Claude draws
    /// its tag, on where it does not — and typed as Claude's own `/effort
    /// <key> on|off` through the facts' one typing path
    /// ([`App::press_slash`]), which says a refusal in the lights' title
    /// slot.
    fn press_top_effort(&mut self, wid: WindowId, session: u64) {
        let want = !self.engine_top_tag(session).unwrap_or(false);
        if let Err(why) =
            self.press_slash(wid, session, crate::claude_slash::Slash::TopEffort(want))
        {
            self.fact_press_refused(wid, session, Fact::TopEffort, why);
        }
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
        let switch = self.model_switch(session);
        self.shown_facts(session)
            .map(|(facts, cwd)| fact_title_with(&facts, cwd.as_deref(), fact, switch.as_ref()))
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
        let switch = self.model_switch(session);
        self.shown_facts(session)
            .map(|(facts, cwd)| {
                fact_spoken_with(&facts, cwd.as_deref(), fact, restores, switch.as_ref())
            })
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
