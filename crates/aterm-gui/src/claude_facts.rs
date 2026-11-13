// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FOOTER'S FACTS, CLICKED.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_agent::harness::footer::{self, FactKind, FactMask, FooterFacts};
use aterm_agent::harness::outlook;
use aterm_agent::harness::profile::FablePlan;
use aterm_agent::harness::quota_view::QuotaView;
use aterm_agent::harness::session_usage::{self, UsageFacts};
use aterm_agent::harness::switch_top;

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
    /// The effort the session's slider was last set to, while it stands over the footer's own
    /// ([`EffortWish`]; per session, in memory only).
    pub(crate) effort_wish: Option<EffortWish>,
}

/// How long a typed effort pick stands as the slider's thumb once its press ended done: the
/// footer's next read says the session's own within it, and where Claude did not take the
/// level the thumb goes back to what the footer shows.
pub(crate) const EFFORT_SETTLE: Duration = Duration::from_secs(3);

/// A stop picked on the effort slider, the card's thumb until the footer says the session's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EffortWish {
    /// The level picked.
    pub(crate) level: &'static str,
    /// It is still to type: picked while another press was in flight, typed when that one ends,
    /// the latest pick alone.
    pub(crate) queued: bool,
    /// When it stops standing: set once its press ended done ([`EFFORT_SETTLE`] later); `None`
    /// while it is queued or being typed.
    pub(crate) until: Option<Instant>,
}

impl EffortWish {
    /// Whether the thumb is still this pick's at `now`.
    #[must_use]
    pub(crate) fn stands(&self, now: Instant) -> bool {
        self.until.is_none_or(|until| now < until)
    }
}

/// How long a read catalog stands before the resolver reads it again.
const CATALOG_RECHECK: Duration = Duration::from_secs(10);

/// The most builds whose catalog is held: the sessions of one machine run a few.
const CATALOGS_MAX: usize = 8;

/// Each Claude Code build's model catalog, as the footer's resolver thread last read it
/// ([`note_catalog`]): the main thread reads no file for a card. `None` is a build no sweep has
/// cached.
#[derive(Default)]
pub(crate) struct Catalogs(Vec<(Option<String>, Instant, Option<Arc<switch_top::Baked>>)>);

impl Catalogs {
    /// Whether `version`'s catalog is unread, or has stood [`CATALOG_RECHECK`] at `now`.
    #[must_use]
    pub(crate) fn due(&self, version: Option<&str>, now: Instant) -> bool {
        !self.0.iter().any(|(v, at, _)| {
            v.as_deref() == version && now.saturating_duration_since(*at) < CATALOG_RECHECK
        })
    }

    /// Hold `baked` as `version`'s, read at `now`.
    pub(crate) fn hold(
        &mut self,
        version: Option<&str>,
        now: Instant,
        baked: Option<switch_top::Baked>,
    ) {
        self.0.retain(|(v, _, _)| v.as_deref() != version);
        if self.0.len() >= CATALOGS_MAX {
            self.0.remove(0);
        }
        self.0
            .push((version.map(str::to_owned), now, baked.map(Arc::new)));
    }

    /// The catalog last read for `version`.
    #[must_use]
    pub(crate) fn get(&self, version: Option<&str>) -> Option<Arc<switch_top::Baked>> {
        self.0
            .iter()
            .find(|(v, _, _)| v.as_deref() == version)
            .and_then(|(_, _, baked)| baked.clone())
    }
}

static CATALOGS: Mutex<Catalogs> = Mutex::new(Catalogs(Vec::new()));

/// Read the catalog of the build a session on Claude Code `version` runs, off the main thread
/// (the footer's resolver, after it read the session's facts) and at most every
/// [`CATALOG_RECHECK`]: the build's cached catalog under the harness's state
/// ([`switch_top::catalog`]). No lock is held over the read.
pub(crate) fn note_catalog(version: Option<&str>, now: Instant) {
    if cfg!(test) {
        return;
    }
    let due = CATALOGS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .due(version, now);
    if !due {
        return;
    }
    let Some(state) = crate::harness_host::harness_state() else {
        return;
    };
    let baked = switch_top::catalog(&state, version);
    CATALOGS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .hold(version, now, baked);
}

/// The catalog last read for `version`; `None` before the resolver's first read, and for a
/// build no sweep has cached.
pub(crate) fn catalog_read_for(version: Option<&str>) -> Option<Arc<switch_top::Baked>> {
    CATALOGS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(version)
}

/// Hold `baked` as the catalog of the build that says `version`, as the resolver would.
#[cfg(all(test, unix))]
pub(crate) fn hold_catalog(version: &str, baked: switch_top::Baked) {
    CATALOGS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .hold(Some(version), Instant::now(), Some(baked));
}

/// A model family the model card offers the newest of. aterm holds families, never a model's
/// version: each is resolved against the session's own build's catalog when the card opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ModelFamily {
    Fable,
    Opus,
}

impl ModelFamily {
    /// The model card's choices, in its order.
    pub(crate) const PICKS: [Self; 2] = [Self::Fable, Self::Opus];

    /// The family as a catalog names it, and as Claude Code's `/model` takes it for an alias.
    #[must_use]
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Fable => "fable",
            Self::Opus => "opus",
        }
    }

    /// The family's name where no catalog names its newest model.
    #[must_use]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Fable => "Fable",
            Self::Opus => "Opus",
        }
    }
}

/// One choice of the model card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelPick {
    pub(crate) family: ModelFamily,
    /// The catalog's display name of the family's newest model (`Opus 5.5`); the family's name
    /// where no catalog was read for the build.
    pub(crate) label: String,
    /// What `/model` is given: the model's id in the session's context variant, the family's
    /// alias where no catalog was read. `None` where a read catalog offers no model of the
    /// family: the choice is shown and cannot be made.
    pub(crate) arg: Option<crate::claude_slash::ModelArg>,
    /// The session runs it.
    pub(crate) current: bool,
    /// What the card says under it: how the plan bills it, or that the build does not offer it.
    pub(crate) note: Option<String>,
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

/// What the model card says under its Fable choice of how `plan` bills it, so a pick never
/// starts spending usage credits unsaid: nothing where the plan includes Fable.
fn fable_billing(plan: Option<&FablePlan>, now: i64) -> Option<String> {
    match plan {
        Some(FablePlan::Included) => None,
        Some(FablePlan::LimitsEnd(Some(at))) if *at > now => None,
        Some(FablePlan::CreditsOnly | FablePlan::LimitsEnd(_)) => {
            Some("billed to usage credits on this plan".to_owned())
        }
        Some(FablePlan::Unknown) | None => {
            Some("billing not read: may use usage credits".to_owned())
        }
    }
}

/// The model card's choices for the session `facts` are of ([`ModelFamily::PICKS`]), each the
/// newest of its family in `baked`, the session's own build's catalog, typed in the shown
/// model's context variant; with no catalog read, the family's alias under the family's name.
/// `now` is Unix seconds, for a plan whose Fable limits end at a date.
#[must_use]
pub(crate) fn model_picks(
    facts: &FooterFacts,
    baked: Option<&switch_top::Baked>,
    now: i64,
) -> Vec<ModelPick> {
    use crate::claude_slash::ModelArg;
    let own = shown_model_id(facts);
    let long = own.as_deref().is_some_and(|id| id.ends_with(LONG_CONTEXT));
    let own_base = own
        .as_deref()
        .map(|id| id.strip_suffix(LONG_CONTEXT).unwrap_or(id));
    let own_family = own_base
        .and_then(aterm_agent::harness::claude_model::family_version)
        .map(|(family, _)| family);
    let variant = |id: &str, native: bool| {
        if long && !native {
            ModelArg::new(&format!("{id}{LONG_CONTEXT}"))
        } else {
            ModelArg::new(id)
        }
    };
    let billing = fable_billing(
        facts
            .subscription
            .as_ref()
            .and_then(|sub| sub.fable.as_ref()),
        now,
    );
    ModelFamily::PICKS
        .into_iter()
        .map(|family| {
            let note = (family == ModelFamily::Fable)
                .then(|| billing.clone())
                .flatten();
            match baked.map(|baked| baked.latest_of(family.word())) {
                Some(Some(model)) => ModelPick {
                    family,
                    label: model.display_name.clone(),
                    arg: variant(&model.id, model.native_1m),
                    current: own_base == Some(model.id.as_str()),
                    note,
                },
                Some(None) => ModelPick {
                    family,
                    label: family.name().to_owned(),
                    arg: None,
                    current: false,
                    note: Some("not offered by this Claude Code build".to_owned()),
                },
                None => ModelPick {
                    family,
                    label: family.name().to_owned(),
                    arg: variant(family.word(), false),
                    current: own_family.as_deref() == Some(family.word()),
                    note,
                },
            }
        })
        .collect()
}

/// The effort slider's stops, least first: the levels a pick moves a session between.
pub(crate) const EFFORT_STOPS: [&str; 3] = ["medium", "high", "xhigh"];

/// The effort slider of the session `facts` are of: its stops ([`EFFORT_STOPS`], of them the
/// ones the shown model takes where `baked`, the build's catalog, lists the model) and the
/// stop the thumb stands on: `wish`, a pick still standing over the footer's own, else the
/// effort shown; `None` where that is no stop (`low`, `max`) or is unread.
#[must_use]
pub(crate) fn effort_stops(
    facts: &FooterFacts,
    baked: Option<&switch_top::Baked>,
    wish: Option<&'static str>,
) -> (Vec<&'static str>, Option<usize>) {
    let offered = shown_model_id(facts)
        .as_deref()
        .and_then(|id| baked?.model(id))
        .map(|model| model.efforts.as_slice());
    let stops: Vec<&'static str> = EFFORT_STOPS
        .into_iter()
        .filter(|stop| offered.is_none_or(|efforts| efforts.iter().any(|e| e == stop)))
        .collect();
    let level = wish.or_else(|| facts.effort.as_deref().and_then(footer::effort_level));
    let at = level.and_then(|level| stops.iter().position(|stop| *stop == level));
    (stops, at)
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
/// session's rule starts with those three off. Kimi's status rows print those three,
/// its branch and its context, so its rule starts with all five off. Their stubs keep
/// the restore click; every other program's line does not, so nothing starts hidden.
#[must_use]
pub(crate) fn default_mask_for(program: Option<aterm_phase::Program>) -> FactMask {
    match program {
        Some(aterm_phase::Program::Muse) => FactMask::MODEL
            .union(FactMask::EFFORT)
            .union(FactMask::PATH),
        Some(aterm_phase::Program::Kimi) => FactMask::MODEL
            .union(FactMask::EFFORT)
            .union(FactMask::PATH)
            .union(FactMask::BRANCH)
            .union(FactMask::CONTEXT),
        Some(
            aterm_phase::Program::Claude
            | aterm_phase::Program::Codex
            | aterm_phase::Program::Agy
            | aterm_phase::Program::Grok
            | aterm_phase::Program::Generic,
        )
        | None => FactMask::default(),
    }
}

/// The facts a click on `fact` hides, or shows again where it is a stub: the next mask after `off`.
/// A Claude Code session's model and effort are never hidden by a click, which opens their card
/// ([`App::press_claude_fact`]); their stubs (a Muse session's starting mask) show them again.
/// A click hides only another program's model and effort: the folder, the branch and the
/// context have nothing to click (`Fact::clicks`), and only their stubs come here.
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
/// Claude Code session ([`fact_title_with`]).
#[cfg(test)]
#[must_use]
pub(crate) fn fact_title(facts: &FooterFacts, cwd: Option<&Path>, fact: Fact) -> (String, String) {
    fact_title_with(facts, cwd, fact, true)
}

/// What a fact's title says, whole and short. `picks`: the session is a Claude Code, where a
/// click on the model opens its choices and one on the effort its slider; elsewhere, where
/// nothing types `/model` or `/effort`, a click hides them. The folder, the branch and the
/// context say what they are and what their right-click's card holds: a click on them does
/// nothing, so their titles name none.
#[must_use]
pub(crate) fn fact_title_with(
    facts: &FooterFacts,
    cwd: Option<&Path>,
    fact: Fact,
    picks: bool,
) -> (String, String) {
    // Each stub shows its own fact (`◆ Opus 5.5 off xhigh off` is two).
    if fact.kind().is_some_and(|kind| facts.off.has(kind)) {
        return (
            format!("{} hidden \u{b7} click shows it", fact.word()),
            "click: show".to_owned(),
        );
    }
    let hide = "click: hide".to_owned();
    match fact {
        Fact::Model if picks => (
            format!(
                "{} \u{b7} click: choose model",
                facts.model.as_deref().unwrap_or("")
            ),
            "click: choose model".to_owned(),
        ),
        Fact::Effort if picks => (
            format!(
                "effort {} \u{b7} click: effort slider",
                facts.effort.as_deref().unwrap_or("")
            ),
            "click: effort slider".to_owned(),
        ),
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
                format!("{shown} \u{b7} right-click: copy, new tab"),
                "right-click: copy, new tab".to_owned(),
            )
        }
        Fact::Branch => (
            format!(
                "\u{2387} {} \u{b7} right-click: copy",
                facts.branch.as_deref().unwrap_or("")
            ),
            "right-click: copy".to_owned(),
        ),
        Fact::Context => (
            format!(
                "{} {} \u{b7} right-click: compact",
                aterm_agent::harness::footer::CONTEXT_MARK,
                facts.context.as_deref().unwrap_or("")
            ),
            "right-click: compact".to_owned(),
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
    fact_spoken_with(facts, cwd, fact, restores, true)
}

/// [`fact_spoken_of`] for a session that is a Claude Code (`picks`: a press opens the model's
/// choices and the effort's slider) or is not (a press hides them).
#[cfg(any(a11y_tree, test))]
#[must_use]
pub(crate) fn fact_spoken_with(
    facts: &FooterFacts,
    cwd: Option<&Path>,
    fact: Fact,
    restores: FactMask,
    picks: bool,
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
            if picks {
                "Activate to choose the model.".to_owned()
            } else {
                hide
            },
        ),
        Fact::Effort => (
            format!("{name}: {}", facts.effort.as_deref().unwrap_or("")),
            if picks {
                "Activate for the effort slider.".to_owned()
            } else {
                hide
            },
        ),
        Fact::Path => {
            let shown = cwd.map_or_else(
                || facts.path.clone().unwrap_or_default(),
                |cwd| cwd.display().to_string(),
            );
            (
                format!("{name}: {shown}"),
                "Activate for its menu: copy it or open a new tab there.".to_owned(),
            )
        }
        Fact::Branch => (
            format!("{name}: {}", facts.branch.as_deref().unwrap_or("")),
            "Activate for its menu: copy it.".to_owned(),
        ),
        Fact::Context => (
            format!("{name}: {}", facts.context.as_deref().unwrap_or("")),
            "Activate for its menu: compact it.".to_owned(),
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
                                .map(|words| words.replace('\u{2264}', "at most "))
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

    /// The facts `session`'s program starts with hidden ([`default_mask_for`]).
    #[must_use]
    pub(crate) fn starting_fact_mask(&self, session: u64) -> FactMask {
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

    /// WHAT ACTIVATING `session`'s `fact` DOES: a click on it, Return or Space on it selected, a
    /// screen reader's press. The folder, the branch and the context have nothing to click (the
    /// pointer's press on them never comes here, `App::press_claude_light`), so the keyboard's
    /// and a reader's activation opens their card, which they reach no other way.
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
        if !fact.clicks(!restores.is_empty()) {
            if let Some(anchor) = self.fact_anchor(wid, session, fact) {
                self.open_fact_menu(wid, session, fact, anchor, true);
            }
            return;
        }
        // A Claude Code session's model opens its choices and its effort its slider (a stub
        // shows its fact again); elsewhere, where nothing types `/model`, a click hides them.
        if matches!(fact, Fact::Model | Fact::Effort)
            && restores.is_empty()
            && self.session_picks(session)
        {
            if let Some(anchor) = self.fact_anchor(wid, session, fact) {
                self.open_fact_menu(wid, session, fact, anchor, false);
            }
            return;
        }
        let next = mask_after(self.fact_mask(session), fact, restores);
        self.set_fact_mask(session, next);
    }

    /// Whether `session` is a Claude Code, the one program aterm types `/model` and `/effort`
    /// into ([`App::press_slash`]): its model fact opens its choices and its effort its slider.
    #[must_use]
    pub(crate) fn session_picks(&self, session: u64) -> bool {
        self.pool.get(session).is_some_and(|entry| {
            let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            crate::harness_host::agent_of(timeline.agent()) == Some(aterm_phase::Program::Claude)
        })
    }

    /// The model card's choices for `session` ([`model_picks`] of the facts its rule shows and
    /// the catalog last read for its build); `None` where it is no Claude Code.
    #[must_use]
    pub(crate) fn model_picks(&self, session: u64) -> Option<Vec<ModelPick>> {
        if !self.session_picks(session) {
            return None;
        }
        let facts = self
            .shown_facts(session)
            .map(|(facts, _)| facts)
            .unwrap_or_default();
        let baked = catalog_read_for(facts.version.as_deref());
        let now = aterm_time::unix_secs()
            .and_then(|n| i64::try_from(n).ok())
            .unwrap_or(0);
        Some(model_picks(&facts, baked.as_deref(), now))
    }

    /// The effort slider of `session` ([`effort_stops`], the thumb on a pick still standing);
    /// `None` where it is no Claude Code.
    #[must_use]
    pub(crate) fn effort_stops(&self, session: u64) -> Option<(Vec<&'static str>, Option<usize>)> {
        self.effort_stops_at(session, Instant::now())
    }

    /// [`Self::effort_stops`] as of `now`: a pick stands over the footer's own until it has
    /// settled ([`EffortWish::stands`]).
    #[must_use]
    pub(crate) fn effort_stops_at(
        &self,
        session: u64,
        now: Instant,
    ) -> Option<(Vec<&'static str>, Option<usize>)> {
        if !self.session_picks(session) {
            return None;
        }
        let facts = self
            .shown_facts(session)
            .map(|(facts, _)| facts)
            .unwrap_or_default();
        let baked = catalog_read_for(facts.version.as_deref());
        let wish = self
            .claude_facts
            .get(&session)
            .and_then(|f| f.effort_wish)
            .filter(|wish| wish.stands(now))
            .map(|wish| wish.level);
        Some(effort_stops(&facts, baked.as_deref(), wish))
    }

    /// THE SLIDER'S PICK: set `session`'s effort to `level`, typed as Claude's own `/effort
    /// <level>` through the facts' one typing path ([`App::press_slash`]). The thumb stands on
    /// the pick at once ([`EffortWish`]) and follows the footer's next read. A pick made while a
    /// press is in flight is remembered, the latest alone, and typed when that press ends
    /// ([`Self::effort_after`]). A refusal is returned for the card to say in place.
    pub(crate) fn pick_effort(
        &mut self,
        wid: WindowId,
        session: u64,
        level: &'static str,
    ) -> Result<(), crate::claude_slash::SlashRefusal> {
        let queued = self.slash_in_flight(session);
        if !queued
            && let Err(why) =
                self.press_slash(wid, session, crate::claude_slash::Slash::Effort(level))
        {
            self.claude_facts.entry(session).or_default().effort_wish = None;
            return Err(why);
        }
        self.claude_facts.entry(session).or_default().effort_wish = Some(EffortWish {
            level,
            queued,
            until: None,
        });
        Ok(())
    }

    /// What the slider's pick does once the press `done` ended `end` at `now`: a pick that
    /// waited for it is typed, unless that press already typed its level; the pick that press
    /// typed settles ([`EFFORT_SETTLE`]) where it ended done and is dropped where it stopped,
    /// so the thumb is the footer's again. A waiting pick's refusal is the effort card's note. A
    /// press the person stopped drops a waiting pick too: nothing more is sent after them.
    pub(crate) fn effort_after(
        &mut self,
        wid: WindowId,
        session: u64,
        done: crate::claude_slash::Slash,
        end: crate::claude_slash::SlashEnd,
        now: Instant,
    ) {
        use crate::claude_slash::{Slash, SlashEnd};
        let Some(wish) = self
            .claude_facts
            .get(&session)
            .and_then(|facts| facts.effort_wish)
        else {
            return;
        };
        let typed = done == Slash::Effort(wish.level);
        let next =
            if wish.queued && end == SlashEnd::Stopped(crate::claude_slash::SlashRefusal::Person) {
                // The person stopped the press ("nothing more sent"): a pick waiting on it is
                // dropped, never typed over their cancel.
                None
            } else if wish.queued && !(typed && end == SlashEnd::Done) {
                match self.pick_effort(wid, session, wish.level) {
                    Ok(()) => return,
                    Err(why) => {
                        self.claude_facts.entry(session).or_default().menu_note =
                            Some((Fact::Effort, why.says("", None)));
                        None
                    }
                }
            } else if typed && end == SlashEnd::Done {
                crate::claude_lights::wake_after(session, EFFORT_SETTLE);
                Some(EffortWish {
                    queued: false,
                    until: Some(now + EFFORT_SETTLE),
                    ..wish
                })
            } else if typed {
                None
            } else {
                // Another press's end: a pick already settling keeps its own deadline.
                Some(wish)
            };
        self.claude_facts.entry(session).or_default().effort_wish = next;
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
        let picks = self.session_picks(session);
        self.shown_facts(session)
            .map(|(facts, cwd)| fact_title_with(&facts, cwd.as_deref(), fact, picks))
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
        let picks = self.session_picks(session);
        self.shown_facts(session)
            .map(|(facts, cwd)| fact_spoken_with(&facts, cwd.as_deref(), fact, restores, picks))
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
