// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AUTOPILOT, the window's side (`docs/DESIGN-harness-autopilot-2026-10-15.md` §2).

use std::sync::OnceLock;

use aterm_agent::supervise::{Autopilot, SupervisorConfig};
use aterm_effects::harness_card::CardPalette;

use crate::{App, WindowId};

/// What the window's harness is, for every session's Autopilot at once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum HarnessPosture {
    /// No host: this instance has no control socket, so nothing supervises any session.
    #[default]
    Unhosted,
    /// The policy supervises nothing here: `[harness] enabled = false`, or a headless instance
    /// under `[harness] headless = false`.
    MasterOff,
    /// The host supervises, and a session with no word of its own runs under this default.
    On(Autopilot),
    /// The host runs, with some agent tables disabled.
    Restricted(Autopilot, u8),
    /// The host runs, and this session's agent's OWN switch is off.
    AgentOff(aterm_phase::Program),
}

impl HarnessPosture {
    /// Posture of a window with host status `hosted`, running `cfg`, and `headless` state.
    #[must_use]
    pub(crate) fn of(hosted: bool, cfg: &SupervisorConfig, headless: bool) -> Self {
        if !hosted {
            Self::Unhosted
        } else if !crate::harness_host::would_supervise(cfg, headless) {
            Self::MasterOff
        } else if cfg.off.is_empty() {
            Self::On(cfg.autopilot)
        } else {
            Self::Restricted(cfg.autopilot, crate::harness_host::admitted_mask(cfg))
        }
    }

    /// The table default where a host runs, before joining an agent's gate.
    pub(crate) const fn default_mode(self) -> Option<Autopilot> {
        match self {
            Self::On(default) | Self::Restricted(default, _) => Some(default),
            Self::Unhosted | Self::MasterOff | Self::AgentOff(_) => None,
        }
    }

    /// Join the published program with the same policy gate the worker uses.
    pub(crate) fn for_agent(self, agent: Option<aterm_phase::Program>) -> Self {
        match (self, agent) {
            (Self::Restricted(default, admitted), Some(agent))
                if admitted & crate::harness_host::agent_bit(agent) != 0 =>
            {
                Self::On(default)
            }
            (Self::Restricted(..), Some(agent)) => Self::AgentOff(agent),
            (Self::Restricted(..), None) => Self::MasterOff,
            (posture, _) => posture,
        }
    }
}

/// What an agent's own switch off says (§10.2.1).
#[must_use]
pub(crate) const fn agent_switch_off(agent: aterm_phase::Program) -> &'static str {
    use aterm_phase::Program;
    match agent {
        Program::Claude => "Supervise Claude Code sessions is off",
        Program::Codex => "Watch codex-alab sessions is off",
        Program::Agy => "Supervise Antigravity CLI sessions is off",
        Program::Muse => "Supervise Muse Code sessions is off",
        Program::Gemini => "Supervise Gemini CLI sessions is off",
        Program::Generic => "Supervise the program sessions is off",
    }
}

/// One mode's sentence in the form each surface renders it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ModeWords {
    /// The mode the sentence says.
    pub(crate) mode: Autopilot,
    /// The Autopilot chip's title, after `autopilot: <mode> — `.
    pub(crate) chip: &'static str,
    /// The `Autopilot set to <mode>` record's line, after its tab.
    pub(crate) record: &'static str,
    /// Settings ▸ Harness's mode button, as its description says what a press does.
    pub(crate) button: &'static str,
}

/// EVERY MODE'S SENTENCE, ONE TABLE (§12.4.1).
pub(crate) const MODE_WORDS: [ModeWords; 3] = [
    ModeWords {
        mode: Autopilot::Cruise,
        chip: "keeps going for ever, finding more to do",
        record: "the harness keeps going for ever, finding more to do",
        button: "keep going for ever, finding more to do: nothing the agent says ends it",
    },
    ModeWords {
        mode: Autopilot::Land,
        chip: "wrapping up: landing everything on the remote main branch, then stopping",
        record: "the harness wraps it up: it lands everything on the remote main branch, then \
                 stops",
        button: "wrap it up: land everything on the remote main branch, then stop",
    },
    ModeWords {
        mode: Autopilot::Off,
        chip: "hands off",
        record: "the harness keeps its hands off",
        button: "the harness keeps its hands off this session",
    },
];

/// A TIMED CRUISE'S SENTENCES (§12.4.1's third row): Cruise until a time, then Land.
pub(crate) const TIMED_CRUISE_CHIP: &str = "keeps going until then, finding more to do";
/// The record of a timed Cruise set (`message_reporters::autopilot_timer_set`).
pub(crate) const TIMED_CRUISE_RECORD: &str =
    "the harness keeps going until that time, then wraps it up on the remote main branch";

/// `mode`'s sentences from [`MODE_WORDS`].
#[must_use]
pub(crate) const fn mode_words(mode: Autopilot) -> &'static ModeWords {
    match mode {
        Autopilot::Cruise => &MODE_WORDS[0],
        Autopilot::Land => &MODE_WORDS[1],
        Autopilot::Off => &MODE_WORDS[2],
    }
}

/// A session's EFFECTIVE Autopilot as the window shows it: the mode it runs under, or why the
/// harness runs none at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AutopilotView {
    /// Cruise: the work kept going; nothing the agent says ends it.
    Cruise,
    /// Land: all work landed on the remote main branch, then nothing typed until someone's turn.
    Land,
    /// Off: the harness keeps its hands off this session.
    Off,
    /// `[harness] enabled = false`: no session is supervised, whatever its word.
    MasterOff,
    /// No host (no control socket): nothing supervises any session ([`HarnessPosture::Unhosted`]).
    Unhosted,
    /// The master switch on and this agent's own switch off.
    AgentOff(aterm_phase::Program),
}

/// How a chip is tinted (§2.3): Cruise the accent, Land amber, the rest dim.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ChipTone {
    /// The theme's accent.
    Accent,
    /// Amber: the work is being wrapped up.
    Amber,
    /// Dim: the harness's hands are off.
    Dim,
}

/// The chip while a click's word is being written and read back (§2.3, "changing").
pub(crate) const PENDING_GLYPH: &str = "\u{25d0}";

/// Title suffix when no host runs a harness and a click opens Settings ▸ Harness.
#[cfg(test)]
macro_rules! open_harness_click {
    () => {
        "click: open Settings \u{25b8} Harness"
    };
}

/// Title suffix when an `aterm.toml` switch holds the harness off.
#[cfg(test)]
macro_rules! harness_on_click {
    () => {
        "click: turn the harness on"
    };
}

impl AutopilotView {
    /// Every view, the three modes in the chip's cycle order first (the tests' sweep of the table).
    #[cfg(test)]
    pub(crate) const ALL: [Self; 6] = [
        Self::Cruise,
        Self::Land,
        Self::Off,
        Self::MasterOff,
        Self::Unhosted,
        Self::AgentOff(aterm_phase::Program::Claude),
    ];

    /// The view of a session whose own word is `word` under `posture`.
    #[must_use]
    pub(crate) fn of(posture: HarnessPosture, word: Option<Autopilot>) -> Self {
        match posture {
            HarnessPosture::Unhosted => Self::Unhosted,
            HarnessPosture::MasterOff | HarnessPosture::Restricted(_, _) => Self::MasterOff,
            HarnessPosture::AgentOff(agent) => Self::AgentOff(agent),
            HarnessPosture::On(default) => Self::from_mode(word.unwrap_or(default)),
        }
    }

    /// The view of a mode the harness runs.
    #[must_use]
    pub(crate) const fn from_mode(mode: Autopilot) -> Self {
        match mode {
            Autopilot::Cruise => Self::Cruise,
            Autopilot::Land => Self::Land,
            Autopilot::Off => Self::Off,
        }
    }

    /// The mode the harness runs the session under: `None` where it runs none.
    #[must_use]
    pub(crate) const fn mode(self) -> Option<Autopilot> {
        match self {
            Self::Cruise => Some(Autopilot::Cruise),
            Self::Land => Some(Autopilot::Land),
            Self::Off => Some(Autopilot::Off),
            Self::MasterOff | Self::Unhosted | Self::AgentOff(_) => None,
        }
    }

    /// The view as a small number (`0..=10`), distinct for every view.
    #[must_use]
    pub(crate) const fn code(self) -> u64 {
        match self {
            Self::Cruise => 0,
            Self::Land => 1,
            Self::Off => 2,
            Self::MasterOff => 3,
            Self::Unhosted => 4,
            Self::AgentOff(agent) => 5 + agent as u64,
        }
    }

    /// Whether the harness carries this session's work at all: Cruise or Land.
    #[must_use]
    pub(crate) const fn carries(self) -> bool {
        matches!(self, Self::Cruise | Self::Land)
    }

    /// The colours the harness's glass wears under this view (§3).
    #[must_use]
    pub(crate) const fn card_palette(self) -> Option<CardPalette> {
        match self {
            Self::Cruise => Some(CardPalette::Cruise),
            Self::Land => Some(CardPalette::Land),
            Self::Off => Some(CardPalette::Off),
            Self::MasterOff | Self::Unhosted | Self::AgentOff(_) => None,
        }
    }

    /// Whether, and in what colours, the harness's glass — the launch card, the composer's border —
    /// is drawn for a session under this view, where `supervised` says this window's host holds
    /// its live claim.
    #[must_use]
    pub(crate) const fn wears_glass(self, supervised: bool) -> Option<CardPalette> {
        match self {
            Self::Cruise | Self::Land if !supervised => None,
            _ => self.card_palette(),
        }
    }

    /// The word a click writes ([`App::cycle_session_autopilot`]).
    #[must_use]
    pub(crate) fn next(self) -> Option<Autopilot> {
        let mode = self.mode()?;
        let at = Autopilot::ALL.iter().position(|m| *m == mode)?;
        Some(Autopilot::ALL[(at + 1) % Autopilot::ALL.len()])
    }

    /// The chip's glyph (§2.3): `∞` Cruise, `⤓` Land, `○` Off, `⊘` where no harness runs.
    #[must_use]
    pub(crate) const fn glyph(self) -> &'static str {
        match self {
            Self::Cruise => "\u{221e}",
            Self::Land => "\u{2913}",
            Self::Off => "\u{25cb}",
            Self::MasterOff | Self::Unhosted | Self::AgentOff(_) => "\u{2298}",
        }
    }

    /// The word drawn beside the glyph at rest.
    #[must_use]
    pub(crate) const fn word(self) -> Option<&'static str> {
        match self {
            Self::Cruise => None,
            Self::Land => Some("land"),
            Self::Off | Self::MasterOff | Self::Unhosted | Self::AgentOff(_) => Some("off"),
        }
    }

    /// What the chip shows at rest: the glyph, then its word after a space where it has one.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn label(self) -> String {
        match self.word() {
            Some(word) => format!("{} {word}", self.glyph()),
            None => self.glyph().to_string(),
        }
    }

    /// The chip's tint (§2.3).
    #[must_use]
    pub(crate) const fn tone(self) -> ChipTone {
        match self {
            Self::Cruise => ChipTone::Accent,
            Self::Land => ChipTone::Amber,
            Self::Off | Self::MasterOff | Self::Unhosted | Self::AgentOff(_) => ChipTone::Dim,
        }
    }

    /// The chip's title at rest (§2.3): what the mode does, then what a click does.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn title(self) -> String {
        let mode = |mode: Autopilot, click: &str| {
            format!(
                "autopilot: {} \u{2014} {} \u{b7} click: {click}",
                mode.word(),
                mode_words(mode).chip
            )
        };
        match self {
            Self::Cruise => mode(Autopilot::Cruise, "land"),
            Self::Land => mode(Autopilot::Land, "off"),
            Self::Off => mode(Autopilot::Off, "cruise"),
            Self::MasterOff => {
                concat!("harness off in Settings \u{b7} ", harness_on_click!()).into()
            }
            Self::Unhosted => concat!(
                "no harness: this aterm has no control socket \u{b7} ",
                open_harness_click!()
            )
            .into(),
            Self::AgentOff(agent) => {
                use aterm_phase::Program;
                macro_rules! agent_off {
                    ($switch:literal) => {
                        concat!($switch, " is off \u{b7} ", harness_on_click!())
                    };
                }
                let off: &str = match agent {
                    Program::Claude => agent_off!("Supervise Claude Code sessions"),
                    Program::Codex => agent_off!("Watch codex-alab sessions"),
                    Program::Agy => agent_off!("Supervise Antigravity CLI sessions"),
                    Program::Muse => agent_off!("Supervise Muse Code sessions"),
                    Program::Gemini => agent_off!("Supervise Gemini CLI sessions"),
                    Program::Generic => agent_off!("Supervise the program sessions"),
                };
                off.into()
            }
        }
    }

    /// The word `status` and `ls` print for a supervisable agent session.
    #[must_use]
    pub(crate) const fn wire(self) -> &'static str {
        match self {
            Self::Cruise => "cruise",
            Self::Land => "land",
            Self::Off | Self::MasterOff | Self::Unhosted | Self::AgentOff(_) => "off",
        }
    }
}

/// THE ONE THEME PREDICATE of the harness's persistent glass.
#[must_use]
pub(crate) fn harness_theme(
    view: AutopilotView,
    supervised: bool,
    word: Option<Autopilot>,
    unhosted_on: bool,
) -> Option<CardPalette> {
    view.wears_glass(supervised).or(match (view, word) {
        (AutopilotView::Unhosted, _) if !unhosted_on => None,
        (AutopilotView::Unhosted, Some(Autopilot::Off)) => None,
        (AutopilotView::Unhosted, Some(Autopilot::Land)) => Some(CardPalette::Land),
        (AutopilotView::Unhosted, _) => Some(CardPalette::Cruise),
        _ => None,
    })
}

/// State of the harness toggle switch drawn for the active session's harness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum HarnessSwitch {
    /// The harness carries the session (Cruise or Land): `● on` lit; a click turns the session Off.
    On,
    /// The harness's hands are off, held so by the switch named.
    Off(SwitchOff),
    /// A click turned a switch of aterm.toml on and the policy in force has not read it back yet.
    Turning,
}

/// Which switch holds a session's harness off ([`HarnessSwitch::Off`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SwitchOff {
    /// The session's own Autopilot word is `off` (or, with none, the table's default is).
    Session,
    /// Its agent's own switch, `[harness.<agent>] enabled = false`.
    Agent(aterm_phase::Program),
    /// The master switch, `[harness] enabled = false` (or a headless instance under
    /// `[harness] headless = false`).
    Master,
}

/// Timeout for a switch write to read back before clearing `◐ turning on`.
pub(crate) const SWITCH_SETTLE: std::time::Duration = std::time::Duration::from_secs(5);

impl HarnessSwitch {
    /// The switch a session under `view` shows: `None` with no host.
    #[must_use]
    pub(crate) const fn of(view: AutopilotView) -> Option<Self> {
        match view {
            AutopilotView::Cruise | AutopilotView::Land => Some(Self::On),
            AutopilotView::Off => Some(Self::Off(SwitchOff::Session)),
            AutopilotView::AgentOff(agent) => Some(Self::Off(SwitchOff::Agent(agent))),
            AutopilotView::MasterOff => Some(Self::Off(SwitchOff::Master)),
            AutopilotView::Unhosted => None,
        }
    }

    /// Whether the switch reads on.
    #[must_use]
    pub(crate) const fn is_on(self) -> bool {
        matches!(self, Self::On)
    }

    /// Its state as a screen reader is told it (`Role::Switch`'s toggled state): on, off, or
    /// mixed while it turns on.
    #[must_use]
    pub(crate) const fn toggled(self) -> Option<bool> {
        match self {
            Self::On => Some(true),
            Self::Off(_) => Some(false),
            Self::Turning => None,
        }
    }

    /// What the switch is, said after its name (the accessibility node's description).
    #[must_use]
    pub(crate) fn described(self, product: &str) -> String {
        match self {
            Self::On => format!(
                "The aterm harness carries this {product} session. Click to turn it off for this \
                 session."
            ),
            Self::Turning => "The aterm harness is turning on.".to_owned(),
            Self::Off(SwitchOff::Session) => format!(
                "The aterm harness is off for this {product} session. Click on to turn it on."
            ),
            Self::Off(SwitchOff::Agent(agent)) => format!(
                "{}. Click on to turn it on.",
                crate::autopilot::agent_switch_off(agent)
            ),
            Self::Off(SwitchOff::Master) => {
                "Supervise coding agents is off. Click on to turn it on.".to_owned()
            }
        }
    }
}

/// The aterm.toml key of the headless gate (`[harness] headless`).
pub(crate) const HARNESS_HEADLESS_KEY: &str = "harness.headless";

/// The `aterm.toml` key of `agent`'s own switch (`harness.<agent>.enabled`).
#[must_use]
pub(crate) fn agent_switch_key(agent: aterm_phase::Program) -> Option<&'static str> {
    crate::prefs::NESTED_LEAVES
        .iter()
        .map(|leaf| leaf.key)
        .find(|key| crate::prefs::agent_switch(key) == Some(agent))
}

/// What turning the harness on for one session writes ([`App::switch_harness_on`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SwitchOnPlan {
    /// The aterm.toml switches that hold it off, each written `true`.
    pub(crate) keys: Vec<&'static str>,
    /// The session's own word is written `cruise`.
    pub(crate) cruise: bool,
}

/// [`SwitchOnPlan`] for a session of `agent` with mode `word` under `cfg` and `headless`.
#[must_use]
pub(crate) fn switch_on_plan(
    cfg: &SupervisorConfig,
    headless: bool,
    agent: Option<aterm_phase::Program>,
    word: Option<Autopilot>,
) -> SwitchOnPlan {
    let mut keys = Vec::new();
    if !cfg.enabled {
        keys.push(crate::prefs::EDIT_HARNESS_ENABLED);
    }
    if headless && !cfg.headless {
        keys.push(HARNESS_HEADLESS_KEY);
    }
    if let Some(agent) = agent
        && cfg.off.contains(&agent)
        && let Some(key) = agent_switch_key(agent)
    {
        keys.push(key);
    }
    SwitchOnPlan {
        keys,
        cruise: cfg.autopilot_for(word) == Autopilot::Off,
    }
}

/// A session's own Autopilot word, read from its context (its metadata, a leaf lock taken alone).
#[must_use]
pub(crate) fn word_of(ctx: &crate::SessionCtx) -> Option<Autopilot> {
    ctx.meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .autopilot_word()
}

/// This process's host's claim holder ([`crate::harness_host::holder`]), built once.
pub(crate) fn own_holder() -> &'static str {
    static HOLDER: OnceLock<String> = OnceLock::new();
    HOLDER.get_or_init(crate::harness_host::holder)
}

/// Metrics clock reading (`metrics::now_us`) for supervisor claim liveness.
#[derive(Debug, Default)]
pub(crate) struct ClaimClock(std::cell::Cell<Option<u64>>);

impl ClaimClock {
    fn now_us(&self) -> u64 {
        if let Some(now) = self.0.get() {
            return now;
        }
        #[cfg(test)]
        crate::work_counts::harness_clock_read();
        let now = crate::metrics::now_us();
        self.0.set(Some(now));
        now
    }
}

/// The `autopilot=` word `status` and the `sessions` roster print beside `supervisor=`.
#[must_use]
pub(crate) fn wire_field(
    posture: HarnessPosture,
    word: Option<Autopilot>,
    agent: Option<aterm_phase::Program>,
) -> &'static str {
    if agent.is_some() {
        AutopilotView::of(posture.for_agent(agent), word).wire()
    } else {
        "-"
    }
}

/// The `land_at=` field `status`, `ls` and the bare `meta` print.
#[must_use]
pub(crate) fn land_at_field(land_at: Option<i64>) -> String {
    land_at.map_or_else(|| "-".to_string(), |ms| ms.div_euclid(1000).to_string())
}

/// The `task=` word `status` and `ls` print (§9.4.1).
#[must_use]
pub(crate) const fn task_field(
    face: Option<aterm_agent::harness::acts::TaskFace>,
    told_here: bool,
    agent: Option<aterm_phase::Program>,
) -> &'static str {
    match face {
        Some(face) if told_here && agent.is_some() => face.status(),
        _ => "-",
    }
}

/// The `pause=` field `status` and `ls` print after `task=`.
#[must_use]
pub(crate) fn pause_field(
    pause: Option<crate::PausePlan>,
    claimed_here: bool,
    agent: Option<aterm_phase::Program>,
) -> String {
    match pause {
        Some(p) if claimed_here && agent.is_some() => p.at_unix.to_string(),
        _ => "-".to_string(),
    }
}

/// THE WINDOW SURFACE A PERSON PRESSED to write a session's Autopilot pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Surface {
    /// The footer's Autopilot chip ([`App::cycle_session_autopilot`]).
    Chip,
    /// The composer border's harness toggle ([`App::toggle_session_harness`]).
    Border,
    /// The Autopilot menu: a mode, a timed Cruise, its cancel.
    Menu,
    /// A SESSIONS row on Settings ▸ Harness: a mode, its Timer, its Cancel.
    Settings,
    /// SESSIONS' `Every session` row (§11.5).
    EverySession,
}

impl Surface {
    /// Every surface, in the order the design lists them.
    pub(crate) const ALL: [Self; 5] = [
        Self::Chip,
        Self::Border,
        Self::Menu,
        Self::Settings,
        Self::EverySession,
    ];

    /// Its word after `human:` in `meta`'s `autopilot_by=` and the restore leaf's `autopilot_by`.
    #[must_use]
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Chip => "chip",
            Self::Border => "border",
            Self::Menu => "menu",
            Self::Settings => "settings",
            Self::EverySession => "every",
        }
    }

    /// The surface [`Self::word`] names; `None` for any other word.
    #[must_use]
    pub(crate) fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|surface| surface.word() == word)
    }

    /// How a line names it (§11.7.1): `from the chip`, `from Settings ▸ Harness, every session`.
    #[must_use]
    pub(crate) const fn phrase(self) -> &'static str {
        match self {
            Self::Chip => "from the chip",
            Self::Border => "from the composer border",
            Self::Menu => "from the Autopilot menu",
            Self::Settings => "from Settings \u{25b8} Harness",
            Self::EverySession => "from Settings \u{25b8} Harness, every session",
        }
    }
}

/// WHO SET A SESSION'S AUTOPILOT, IN WORDS (§11.7.3, item 4): composed where it is shown.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SetByWords {
    /// Trailing attribution phrase (`over aterm ctl`, `from the chip`, or tab origin).
    pub(crate) by: String,
    /// When, as the local clock and day read it.
    pub(crate) at: Option<crate::harness_glass::Stamp>,
}

impl SetByWords {
    /// Attribution words for a stored setter as the session's Autopilot mode stands.
    #[must_use]
    pub(crate) fn of(
        set_by: Option<crate::session_timeline::AutopilotSetBy>,
        has_word: bool,
        sid_place: impl FnOnce(&str) -> Option<String>,
    ) -> Option<Self> {
        use aterm_slog::Actor;
        let Some(set_by) = set_by else {
            return has_word.then(|| Self {
                by: "before aterm restarted".to_string(),
                at: None,
            });
        };
        let by = match set_by.actor {
            Actor::Human => set_by
                .surface
                .map_or("from the window", Surface::phrase)
                .to_string(),
            Actor::Ctl => "over aterm ctl".to_string(),
            Actor::Harness => "by aterm's harness".to_string(),
            Actor::Bridge => "over the fabric".to_string(),
            Actor::Aterm => "by its timer".to_string(),
            Actor::Sid(sid) => sid_place(sid.as_str()).map_or_else(
                || {
                    format!(
                        "from session {}",
                        aterm_messages::presence::short_sid(sid.as_str())
                    )
                },
                |place| format!("from {place}"),
            ),
            Actor::Vendor | Actor::Nobody => "over aterm ctl (writer unknown)".to_string(),
        };
        Some(Self {
            by,
            at: crate::harness_glass::Stamp::at(set_by.since_ms.div_euclid(1000)),
        })
    }

    /// The menu's note and a SESSIONS row's words (§11.7.1).
    #[must_use]
    pub(crate) fn line(&self, today: Option<i64>) -> String {
        match self.at {
            Some(at) => format!("set {} {}", at.words(today), self.by),
            None => format!("set {}", self.by),
        }
    }

    /// A record's line (§11.7.1): `set over aterm ctl`, `set from the chip`.
    #[must_use]
    pub(crate) fn record_line(&self) -> String {
        format!("set {}", self.by)
    }
}

impl App {
    /// The window's harness posture now: whether it has a host, and the policy in force.
    #[must_use]
    pub(crate) fn harness_posture(&self) -> HarnessPosture {
        HarnessPosture::of(
            self.harness.is_some(),
            &self.harness_policy.get().policy,
            self.headless,
        )
    }

    /// Whether a window with no host wears the harness's glass for `agent`.
    #[must_use]
    pub(crate) fn unhosted_glass(&self, agent: Option<aterm_phase::Program>) -> bool {
        matches!(
            HarnessPosture::of(true, &self.harness_policy.get().policy, self.headless)
                .for_agent(agent),
            HarnessPosture::On(_)
        )
    }

    /// Publish [`Self::harness_posture`] into the session registry.
    pub(crate) fn publish_harness_posture(&self) {
        let posture = self.harness_posture();
        self.store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .set_harness_posture(posture);
    }

    /// Session `local_id`'s Autopilot: `(word, view, supervised)`.
    #[must_use]
    pub(crate) fn session_autopilot(
        &self,
        local_id: u64,
    ) -> (Option<Autopilot>, AutopilotView, bool) {
        self.session_autopilot_on(local_id, &ClaimClock::default())
    }

    /// [`Self::session_autopilot`] on a pass's shared claim clock.
    #[must_use]
    pub(crate) fn session_autopilot_on(
        &self,
        local_id: u64,
        clock: &ClaimClock,
    ) -> (Option<Autopilot>, AutopilotView, bool) {
        let (word, view, supervised, _) =
            self.session_autopilot_for(local_id, self.session_agent(local_id), clock);
        (word, view, supervised)
    }

    /// The agent session `local_id` publishes that the host supervises or watches, if any.
    pub(crate) fn session_agent(&self, local_id: u64) -> Option<aterm_phase::Program> {
        self.pool
            .get(local_id)
            .and_then(|session| crate::harness_host::admitted(&session.ctx))
    }

    /// Session `local_id`'s OWN posture.
    pub(crate) fn session_harness_posture(&self, local_id: u64) -> HarnessPosture {
        self.harness_posture()
            .for_agent(self.session_agent(local_id))
    }

    /// [`Self::session_autopilot_on`] for an `agent` already read under the timeline guard.
    pub(crate) fn session_autopilot_for(
        &self,
        local_id: u64,
        agent: Option<aterm_phase::Program>,
        clock: &ClaimClock,
    ) -> (Option<Autopilot>, AutopilotView, bool, bool) {
        #[cfg(test)]
        crate::work_counts::autopilot_read();
        let posture = self.harness_posture().for_agent(agent);
        let Some(session) = self.pool.get(local_id) else {
            return (None, AutopilotView::of(posture, None), false, false);
        };
        let (word, supervised, self_proven) = {
            let meta = session.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            let live = |claim: &crate::session_timeline::SupervisorClaim| {
                claim.expires_us.is_none_or(|at| at > clock.now_us())
            };
            let supervised = meta
                .supervisor
                .as_ref()
                .is_some_and(|claim| claim.holder == own_holder() && live(claim));
            let self_proven = meta
                .supervisor
                .as_ref()
                .is_some_and(|claim| claim.proven.is_some() && live(claim));
            (meta.autopilot_word(), supervised, self_proven)
        };
        (
            word,
            AutopilotView::of(posture, word),
            supervised,
            self_proven,
        )
    }

    /// Write session `local_id`'s own Autopilot word.
    pub(crate) fn set_session_autopilot(
        &mut self,
        local_id: u64,
        word: Option<Autopilot>,
        surface: Surface,
    ) -> bool {
        self.set_session_autopilot_timed(local_id, word, None, surface)
    }

    /// THE WINDOW'S ONE WRITER OF THE AUTOPILOT PAIR (§9.3.6, item 1).
    pub(crate) fn set_session_autopilot_timed(
        &mut self,
        local_id: u64,
        word: Option<Autopilot>,
        land_at: Option<i64>,
        surface: Surface,
    ) -> bool {
        let Some(ctx) = self.pool.get(local_id).map(|s| s.ctx.clone()) else {
            return false;
        };
        let write = crate::session_timeline::apply_autopilot(
            &ctx,
            word.map(|w| w.word().to_string()),
            land_at,
            aterm_slog::Actor::Human,
            Some(surface),
        );
        self.autopilot_pair_written(local_id, write)
    }

    /// THE FAN-OUT of a window write of session `local_id`'s Autopilot pair.
    pub(crate) fn autopilot_pair_written(
        &mut self,
        local_id: u64,
        write: crate::session_timeline::AutopilotWrite,
    ) -> bool {
        self.autopilot_pair_fanned_out(local_id, write, true)
    }

    /// WRITE MANY SESSIONS' AUTOPILOT PAIRS AT ONCE.
    pub(crate) fn write_autopilot_batch(
        &mut self,
        writes: &[(u64, Option<Autopilot>, Option<i64>)],
        by: aterm_slog::Actor,
        surface: Option<Surface>,
    ) -> usize {
        let mut moved = 0;
        for &(local_id, word, land_at) in writes {
            let Some(ctx) = self.pool.get(local_id).map(|s| s.ctx.clone()) else {
                continue;
            };
            let write = crate::session_timeline::apply_autopilot(
                &ctx,
                word.map(|w| w.word().to_string()),
                land_at,
                by,
                surface,
            );
            if self.autopilot_pair_fanned_out(local_id, write, false) {
                moved += 1;
            }
        }
        self.harness_screen_moved();
        moved
    }

    fn autopilot_pair_fanned_out(
        &mut self,
        local_id: u64,
        write: crate::session_timeline::AutopilotWrite,
        publish: bool,
    ) -> bool {
        let moved = write.moved;
        let dropped = self.autopilot_timers.glass(local_id);
        let timer_moved = self.index_autopilot_timer_unstamped(local_id, None);
        self.refresh_autopilot_menu(local_id);
        self.journal_owner_words(local_id);
        if !moved {
            if timer_moved {
                self.restamp_for_hover(local_id);
            }
            return false;
        }
        self.refresh_meta_dependent_chrome(local_id);
        self.refresh_presence_session(local_id);
        self.mark_status_dirty();
        if let Some(session) = self.pool.get(local_id) {
            session.ctx.watchers.ring_watchers();
        }
        crate::claude_footer::post_changed(local_id);
        self.note_harness_arm(local_id);
        if publish {
            self.harness_screen_moved();
        }
        self.record_autopilot_change(local_id, dropped, write.revision);
        true
    }

    /// THE KEYED RECORD OF A MOVE OF SESSION `local_id`'S AUTOPILOT PAIR (§2, §9.3.2; §11.7.1).
    pub(crate) fn record_autopilot_change(
        &mut self,
        local_id: u64,
        dropped: Option<crate::claude_lights::LandAtGlass>,
        revision: u64,
    ) {
        self.autopilot_recorded.insert(local_id, revision);
        let Some((sid, has_word)) = self.pool.get(local_id).map(|s| {
            let has_word = s
                .ctx
                .meta
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .autopilot
                .is_some();
            (s.ctx.self_id.as_str().to_string(), has_word)
        }) else {
            return;
        };
        let (_, view, _) = self.session_autopilot(local_id);
        let place = self.session_place(&sid);
        let set_by = self
            .autopilot_set_by_words(local_id)
            .map(|words| words.record_line());
        let message = match self.autopilot_timers.glass(local_id) {
            Some(until) => crate::message_reporters::autopilot_timer_set(
                &sid,
                &place,
                until,
                set_by.as_deref(),
            ),
            None => crate::message_reporters::autopilot_set(
                &sid,
                &place,
                view,
                has_word,
                dropped,
                set_by.as_deref(),
            ),
        };
        self.record_message(message);
    }

    /// Stored setter of session `local_id`'s Autopilot in words ([`SetByWords::of`]).
    pub(crate) fn autopilot_set_by_words(&self, local_id: u64) -> Option<SetByWords> {
        let (set_by, has_word) = {
            let meta = self
                .pool
                .get(local_id)?
                .ctx
                .meta
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            (meta.autopilot_set_by, meta.autopilot.is_some())
        };
        SetByWords::of(set_by, has_word, |sid| self.autopilot_setter_tab(sid))
    }

    /// The tab of the live session whose sid is `sid`, as a setter's words name it.
    pub(crate) fn autopilot_setter_tab(&self, sid: &str) -> Option<String> {
        let local = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_sid(&aterm_session::SessionId::new(sid))
            .map(|h| h.local_id)?;
        self.pool.get(local)?;
        let place = self.session_place(sid);
        let place = place.strip_prefix("in ").unwrap_or(&place).to_string();
        let label = self.tab_label_of_session(local);
        Some(if label == place {
            place
        } else {
            format!("{place} ({label})")
        })
    }

    /// A WRITE OVER THE WIRE moved session `local_id`'s Autopilot pair.
    pub(crate) fn record_wire_autopilot(
        &mut self,
        local_id: u64,
        dropped: Option<crate::claude_lights::LandAtGlass>,
    ) {
        let Some(revision) = self.pool.get(local_id).map(|s| {
            s.ctx
                .meta
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .autopilot_revision
        }) else {
            return;
        };
        if self.autopilot_recorded.get(&local_id).copied().unwrap_or(0) == revision {
            return;
        }
        self.record_autopilot_change(local_id, dropped, revision);
    }

    /// Advance session `local_id`'s effective Autopilot mode to the next in [`Autopilot::ALL`].
    pub(crate) fn cycle_session_autopilot(&mut self, local_id: u64) -> AutopilotView {
        let (_, view, _) = self.session_autopilot(local_id);
        let Some(next) = view.next() else {
            return view;
        };
        self.set_session_autopilot(local_id, Some(next), Surface::Chip);
        self.session_autopilot(local_id).1
    }

    /// THE HARNESS SWITCH, flipped (the composer top rule's switch, [`HarnessSwitch`]).
    pub(crate) fn toggle_session_harness(&mut self, local_id: u64) -> AutopilotView {
        let (_, view, _) = self.session_autopilot(local_id);
        match HarnessSwitch::of(view) {
            Some(HarnessSwitch::On) => {
                self.set_session_autopilot(local_id, Some(Autopilot::Off), Surface::Border);
            }
            Some(HarnessSwitch::Off(_)) => {
                self.switch_harness_on(local_id);
            }
            Some(HarnessSwitch::Turning) | None => {}
        }
        self.session_autopilot(local_id).1
    }

    /// Turn the harness switch on for session `local_id` and record the pending toggle.
    pub(crate) fn switch_harness_on(&mut self, local_id: u64) -> SwitchOnPlan {
        let (word, _, _) = self.session_autopilot(local_id);
        let plan = switch_on_plan(
            &self.harness_policy.get().policy,
            self.headless,
            self.session_agent(local_id),
            word,
        );
        if plan.cruise {
            self.set_session_autopilot(local_id, Some(Autopilot::Cruise), Surface::Border);
        }
        for key in &plan.keys {
            self.queue_harness_switch_write(key);
        }
        plan
    }

    /// Open Settings ▸ Harness in window `wid`.
    pub(crate) fn open_harness_settings(
        &mut self,
        wid: WindowId,
        focus: crate::harness_screen::HarnessFocus,
    ) -> bool {
        if !self.open_settings_tab_in_window(wid, crate::native_settings::SettingsRoute::Harness) {
            return false;
        }
        self.dispatch_settings_action_in_window(
            wid,
            crate::harness_screen::HARNESS_LAND,
            Some(crate::native_app::SemanticInput::Text(focus.wire())),
        )
    }
}

#[cfg(test)]
#[path = "autopilot_set_by_tests.rs"]
mod set_by_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_on_button_writes_exactly_the_switches_that_hold_it_off() {
        use aterm_phase::Program;
        let cfg = |text: &str| crate::app_config::HarnessPolicy::read(text).policy;
        let claude = Some(Program::Claude);
        let plan = |text: &str, headless: bool, word: Option<Autopilot>| {
            switch_on_plan(&cfg(text), headless, claude, word)
        };
        assert_eq!(
            plan("[harness]\n", false, None),
            SwitchOnPlan::default(),
            "control: nothing holds it off"
        );
        assert_eq!(
            plan("[harness]\n", false, Some(Autopilot::Off)),
            SwitchOnPlan {
                keys: Vec::new(),
                cruise: true
            },
            "its own word off"
        );
        assert!(
            plan("[harness]\nautopilot = \"off\"\n", false, None).cruise,
            "the table's default off, no word"
        );
        assert!(
            !plan(
                "[harness]\nautopilot = \"off\"\n",
                false,
                Some(Autopilot::Land)
            )
            .cruise,
            "its own word carries it"
        );
        assert_eq!(
            plan("[harness]\nenabled = false\n", false, None).keys,
            [crate::prefs::EDIT_HARNESS_ENABLED]
        );
        assert_eq!(
            plan("[harness]\nheadless = false\n", false, None).keys,
            Vec::<&str>::new(),
            "the headless gate holds a windowed instance nothing"
        );
        assert_eq!(
            plan("[harness]\nheadless = false\n", true, None).keys,
            [HARNESS_HEADLESS_KEY]
        );
        assert_eq!(
            plan("[harness.claude]\nenabled = false\n", false, None).keys,
            [crate::prefs::EDIT_HARNESS_CLAUDE_ENABLED]
        );
        assert_eq!(
            plan("[harness.agy]\nenabled = false\n", false, None).keys,
            Vec::<&str>::new(),
            "another agent's switch is not Claude Code's"
        );
        assert_eq!(
            agent_switch_key(Program::Agy),
            Some(crate::prefs::EDIT_HARNESS_AGY_ENABLED)
        );
        assert_eq!(
            agent_switch_key(Program::Muse),
            Some(crate::prefs::EDIT_HARNESS_MUSE_ENABLED)
        );
    }

    #[test]
    fn every_hosted_view_has_a_switch_and_no_unhosted_one_does() {
        for view in AutopilotView::ALL {
            let switch = HarnessSwitch::of(view);
            match view {
                AutopilotView::Cruise | AutopilotView::Land => {
                    assert_eq!(switch, Some(HarnessSwitch::On));
                }
                AutopilotView::Off => {
                    assert_eq!(switch, Some(HarnessSwitch::Off(SwitchOff::Session)));
                }
                AutopilotView::MasterOff => {
                    assert_eq!(switch, Some(HarnessSwitch::Off(SwitchOff::Master)));
                }
                AutopilotView::AgentOff(agent) => {
                    assert_eq!(switch, Some(HarnessSwitch::Off(SwitchOff::Agent(agent))));
                }
                AutopilotView::Unhosted => assert_eq!(switch, None),
            }
            assert_eq!(
                switch.map(HarnessSwitch::is_on),
                switch.map(|_| view.carries())
            );
        }
        assert_eq!(HarnessSwitch::On.toggled(), Some(true));
        assert_eq!(HarnessSwitch::Off(SwitchOff::Master).toggled(), Some(false));
        assert_eq!(HarnessSwitch::Turning.toggled(), None);
    }

    #[test]
    fn the_view_is_the_master_switch_then_the_word_then_the_default() {
        for word in [
            None,
            Some(Autopilot::Cruise),
            Some(Autopilot::Land),
            Some(Autopilot::Off),
        ] {
            assert_eq!(
                AutopilotView::of(HarnessPosture::Unhosted, word),
                AutopilotView::Unhosted
            );
            assert_eq!(
                AutopilotView::of(HarnessPosture::MasterOff, word),
                AutopilotView::MasterOff,
                "a session ON never lifts a global off"
            );
            for default in Autopilot::ALL {
                let cfg = SupervisorConfig {
                    autopilot: default,
                    ..SupervisorConfig::default()
                };
                assert_eq!(
                    AutopilotView::of(HarnessPosture::On(default), word).mode(),
                    Some(cfg.autopilot_for(word)),
                    "{default:?} {word:?}"
                );
            }
        }
    }

    #[test]
    fn the_posture_reads_the_host_the_switch_and_the_default() {
        let on = SupervisorConfig::default();
        assert_eq!(
            HarnessPosture::of(false, &on, false),
            HarnessPosture::Unhosted
        );
        assert_eq!(
            HarnessPosture::of(true, &on, false),
            HarnessPosture::On(Autopilot::Cruise)
        );
        let mut off = SupervisorConfig::default();
        off.set("enabled", "false").unwrap();
        assert_eq!(
            HarnessPosture::of(true, &off, false),
            HarnessPosture::MasterOff
        );
        let mut quiet = SupervisorConfig::default();
        quiet.set("headless", "false").unwrap();
        assert_eq!(
            HarnessPosture::of(true, &quiet, true),
            HarnessPosture::MasterOff
        );
        let mut land = SupervisorConfig::default();
        land.set("autopilot", "land").unwrap();
        assert_eq!(
            HarnessPosture::of(true, &land, false),
            HarnessPosture::On(Autopilot::Land)
        );
        assert_eq!(HarnessPosture::default(), HarnessPosture::Unhosted);
    }

    #[test]
    fn the_chips_follow_the_designs_table_and_cycle_in_order() {
        let rows = [
            (
                AutopilotView::Cruise,
                "\u{221e}",
                ChipTone::Accent,
                Some(Autopilot::Land),
            ),
            (
                AutopilotView::Land,
                "\u{2913} land",
                ChipTone::Amber,
                Some(Autopilot::Off),
            ),
            (
                AutopilotView::Off,
                "\u{25cb} off",
                ChipTone::Dim,
                Some(Autopilot::Cruise),
            ),
            (
                AutopilotView::MasterOff,
                "\u{2298} off",
                ChipTone::Dim,
                None,
            ),
            (AutopilotView::Unhosted, "\u{2298} off", ChipTone::Dim, None),
            (
                AutopilotView::AgentOff(aterm_phase::Program::Claude),
                "\u{2298} off",
                ChipTone::Dim,
                None,
            ),
        ];
        for (view, label, tone, next) in rows {
            assert_eq!(view.label(), label, "{view:?}");
            assert_eq!(view.tone(), tone, "{view:?}");
            assert_eq!(view.next(), next, "{view:?}");
            assert!(view.title().contains("click: "), "{view:?}");
        }
        assert_eq!(
            AutopilotView::MasterOff.title(),
            "harness off in Settings \u{b7} click: turn the harness on"
        );
        let mut view = AutopilotView::Cruise;
        let mut seen = Vec::new();
        for _ in 0..Autopilot::ALL.len() {
            seen.push(view.mode().unwrap());
            view = AutopilotView::from_mode(view.next().unwrap());
        }
        assert_eq!(seen, Autopilot::ALL);
        assert_eq!(view, AutopilotView::Cruise);
        assert_eq!(PENDING_GLYPH, "\u{25d0}");
        assert!(AutopilotView::Cruise.carries() && AutopilotView::Land.carries());
        assert!(!AutopilotView::Off.carries() && !AutopilotView::MasterOff.carries());
        assert_eq!(AutopilotView::ALL.len(), 6);
        for view in AutopilotView::ALL {
            for supervised in [false, true] {
                let glass = match view {
                    AutopilotView::Cruise if supervised => Some(CardPalette::Cruise),
                    AutopilotView::Land if supervised => Some(CardPalette::Land),
                    AutopilotView::Off => Some(CardPalette::Off),
                    _ => None,
                };
                assert_eq!(view.wears_glass(supervised), glass, "{view:?} {supervised}");
                for (word, unhosted_on) in [
                    None,
                    Some(Autopilot::Cruise),
                    Some(Autopilot::Land),
                    Some(Autopilot::Off),
                ]
                .into_iter()
                .flat_map(|word| [(word, true), (word, false)])
                {
                    let theme = match (view, word) {
                        (AutopilotView::Unhosted, _) if !unhosted_on => None,
                        (AutopilotView::Unhosted, Some(Autopilot::Off)) => None,
                        (AutopilotView::Unhosted, Some(Autopilot::Land)) => Some(CardPalette::Land),
                        (AutopilotView::Unhosted, _) => Some(CardPalette::Cruise),
                        _ => glass,
                    };
                    assert_eq!(
                        harness_theme(view, supervised, word, unhosted_on),
                        theme,
                        "{view:?} {supervised} {word:?} {unhosted_on}"
                    );
                }
            }
        }
        assert_eq!(AutopilotView::Cruise.wears_glass(false), None, "CONTROL");
    }

    #[test]
    fn an_agents_own_switch_off_reads_agent_off_and_the_master_wins() {
        use crate::harness_screen::AbArm;
        use aterm_phase::Program;
        for agent in Program::ALL.into_iter().filter(|p| p.supervisable()) {
            let mut cfg = SupervisorConfig::default();
            cfg.set(&format!("{}.enabled", agent.name()), "false")
                .unwrap();
            let posture = HarnessPosture::of(true, &cfg, false);
            let joined = posture.for_agent(Some(agent));
            assert_eq!(joined, HarnessPosture::AgentOff(agent), "{agent:?}");
            assert_eq!(joined.default_mode(), None, "{agent:?}");
            for word in [None, Some(Autopilot::Cruise), Some(Autopilot::Land)] {
                let view = AutopilotView::of(joined, word);
                assert_eq!(view, AutopilotView::AgentOff(agent), "{agent:?} {word:?}");
                assert_eq!(view.mode(), None);
                assert_eq!(
                    view.next(),
                    None,
                    "a click turns the switch on, writing no word"
                );
                assert!(!view.carries());
                assert_eq!(view.wire(), "off");
                assert_eq!(view.label(), "\u{2298} off");
                assert_eq!(view.tone(), ChipTone::Dim);
                assert_eq!(
                    wire_field(posture, word, Some(agent)),
                    "off",
                    "{agent:?} {word:?}"
                );
            }
            let switch = format!("Supervise {} sessions is off", agent.product());
            assert_eq!(agent_switch_off(agent), switch);
            assert_eq!(
                switch,
                format!(
                    "{} is off",
                    crate::prefs::agent_switch_label(agent.product())
                ),
                "{agent:?}: the switch's Settings label"
            );
            assert_eq!(
                AutopilotView::AgentOff(agent).title(),
                format!("{switch} \u{b7} click: turn the harness on")
            );
            assert_eq!(
                AbArm::of(AutopilotView::AgentOff(agent), false, false),
                AbArm::Off,
                "{agent:?}: plain, the baseline"
            );
            assert_eq!(
                AbArm::of(AutopilotView::AgentOff(agent), false, true),
                AbArm::Unsupervised,
                "{agent:?}: a claim still live"
            );
            for other in Program::ALL
                .into_iter()
                .filter(|p| p.supervisable() && *p != agent)
            {
                assert_eq!(
                    posture.for_agent(Some(other)),
                    HarnessPosture::On(Autopilot::Cruise),
                    "{agent:?} off, {other:?} on"
                );
            }
            assert_eq!(posture.for_agent(None), HarnessPosture::MasterOff);
            cfg.set("enabled", "false").unwrap();
            assert_eq!(
                HarnessPosture::of(true, &cfg, false).for_agent(Some(agent)),
                HarnessPosture::MasterOff,
                "{agent:?}: the master wins"
            );
        }
        for agent in Program::ALL.into_iter().filter(|p| p.harness_table()) {
            assert_eq!(
                agent_switch_off(agent),
                format!(
                    "{} is off",
                    crate::prefs::agent_switch_label(agent.product())
                ),
                "{agent:?}"
            );
        }
        assert_eq!(
            agent_switch_off(Program::Codex),
            "Watch codex-alab sessions is off"
        );
        let mut codes: Vec<u64> = AutopilotView::ALL.iter().map(|v| v.code()).collect();
        codes.extend(Program::ALL.map(|p| AutopilotView::AgentOff(p).code()));
        let unique: std::collections::BTreeSet<u64> = codes.iter().copied().collect();
        assert_eq!(unique.len(), 5 + Program::ALL.len(), "{codes:?}");
        assert!(codes.iter().all(|c| *c < 256), "bits 8-15 hold it");
    }

    fn idle_host() -> crate::harness_host::HostHandle {
        crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        )
    }

    fn autopilot_records(app: &App) -> Vec<(String, Vec<String>)> {
        let key = format!(
            "harness.autopilot.{}",
            app.pool.get(0).unwrap().ctx.self_id.as_str()
        );
        app.messages
            .log()
            .records()
            .filter(|r| r.key.as_deref() == Some(key.as_str()))
            .map(|r| (r.title.clone(), r.detail.clone()))
            .collect()
    }

    #[test]
    fn the_window_writes_the_word_and_the_chip_cycles_it() {
        let mut app = App::headless_for_test();
        assert_eq!(app.harness_posture(), HarnessPosture::Unhosted);
        assert_eq!(
            app.session_autopilot(0),
            (None, AutopilotView::Unhosted, false)
        );
        assert_eq!(app.cycle_session_autopilot(0), AutopilotView::Unhosted);
        assert_eq!(app.session_autopilot(0).0, None, "no host: nothing written");

        let host = idle_host();
        app.harness = Some(host.clone());
        assert_eq!(app.harness_posture(), HarnessPosture::On(Autopilot::Cruise));
        let ctx = app.pool.get(0).unwrap().ctx.clone();
        let rings = crate::harness_host::rings_here();
        assert!(app.set_session_autopilot(
            0,
            Some(Autopilot::Land),
            crate::autopilot::Surface::Chip
        ));
        assert_eq!(
            crate::harness_host::rings_here(),
            rings + 1,
            "the host is rung"
        );
        assert_eq!(ctx.meta.lock().unwrap().autopilot.as_deref(), Some("land"));
        assert_eq!(
            app.session_autopilot(0),
            (Some(Autopilot::Land), AutopilotView::Land, false)
        );
        assert!(
            !app.set_session_autopilot(0, Some(Autopilot::Land), crate::autopilot::Surface::Chip),
            "a re-set moves nothing"
        );
        assert_eq!(crate::harness_host::rings_here(), rings + 1);
        assert_eq!(
            ctx.human_input.wire(crate::metrics::now_us()),
            "-",
            "never a person's keystroke"
        );
        let events: Vec<String> = ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == "meta-change")
            .map(|e| e.payload.to_string())
            .collect();
        assert_eq!(events, ["field=autopilot value=land"]);
        assert_eq!(
            autopilot_records(&app),
            [(
                "Autopilot set to land".to_string(),
                vec![
                    format!("Tab 1: {}", mode_words(Autopilot::Land).record),
                    "this session's own setting".to_string(),
                    "set from the chip".to_string()
                ]
            )]
        );

        assert_eq!(app.cycle_session_autopilot(0), AutopilotView::Off);
        assert_eq!(app.session_autopilot(0).0, Some(Autopilot::Off));
        assert_eq!(app.cycle_session_autopilot(0), AutopilotView::Cruise);
        assert_eq!(app.session_autopilot(0).0, Some(Autopilot::Cruise));
        assert_eq!(app.cycle_session_autopilot(0), AutopilotView::Land);

        assert!(app.set_session_autopilot(0, None, crate::autopilot::Surface::Chip));
        assert_eq!(
            app.session_autopilot(0),
            (None, AutopilotView::Cruise, false)
        );
        let records = autopilot_records(&app);
        assert_eq!(
            records.last().map(|(_, detail)| detail[1].as_str()),
            Some("back to the default, [harness] autopilot")
        );

        let now = crate::metrics::now_us();
        crate::session_timeline::claim_supervisor(&ctx, "someone-else", None, None, now).unwrap();
        assert!(!app.session_autopilot(0).2, "another holder's claim");
        crate::session_timeline::release_supervisor(&ctx, None, now, aterm_slog::Actor::Ctl);
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            now,
        )
        .unwrap();
        assert!(app.session_autopilot(0).2, "this host's claim");
        assert_eq!(own_holder(), crate::harness_host::holder(), "built once");
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            Some(1),
            now,
        )
        .unwrap();
        assert!(!app.session_autopilot(0).2, "this host's lapsed lease");

        app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
            crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
        );
        assert_eq!(app.harness_posture(), HarnessPosture::MasterOff);
        assert_eq!(app.cycle_session_autopilot(0), AutopilotView::MasterOff);
        assert_eq!(app.session_autopilot(0).0, None, "nothing written");
        host.shutdown_and_join();
    }

    struct StopHost(crate::harness_host::HostHandle);

    impl Drop for StopHost {
        fn drop(&mut self) {
            self.0.shutdown_and_join();
        }
    }

    fn agent_policy_app() -> (App, StopHost) {
        let mut app = App::headless_for_test();
        let host = idle_host();
        let policy = crate::app_config::HarnessPolicy::read(
            "[harness]\nheadless = true\n[harness.claude]\nenabled = false\n",
        );
        host.set_config(policy.policy.clone());
        app.harness = Some(host.clone());
        app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(policy);
        app.set_session_autopilot(0, Some(Autopilot::Cruise), crate::autopilot::Surface::Chip);
        (app, StopHost(host))
    }

    fn publish_program(app: &App, name: &str) {
        let ctx = &app.pool.get(0).unwrap().ctx;
        let mut timeline = ctx.timeline.lock().unwrap();
        timeline.note_foreground_group(7);
        timeline.set_program(7, Some(name.to_string()));
    }

    #[test]
    fn a_disabled_agent_shows_off_without_erasing_its_session_intent() {
        let (app, host) = agent_policy_app();
        publish_program(&app, "claude");
        assert!(!host.0.admits(aterm_phase::Program::Claude));
        assert!(host.0.admits(aterm_phase::Program::Agy));
        let ctx = &app.pool.get(0).unwrap().ctx;
        crate::session_timeline::claim_supervisor(
            ctx,
            own_holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .unwrap();
        let (word, view, supervised) = app.session_autopilot(0);
        assert_eq!(word, Some(Autopilot::Cruise), "the stored intent survives");
        assert!(supervised, "the actual claim remains visible");
        assert_eq!(view.wire(), "off", "Claude is denied by the host's policy");
        assert!(!view.carries(), "a stale claim does not admit a card");
        assert!(
            view.wears_glass(supervised).is_none(),
            "a stale claim does not admit a card"
        );
        assert_eq!(
            harness_theme(
                view,
                supervised,
                word,
                app.unhosted_glass(Some(aterm_phase::Program::Claude))
            ),
            None,
            "nor the border"
        );
        assert_eq!(app.harness_state().sessions[0].view.wire(), "off");

        publish_program(&app, "agy");
        assert_eq!(app.session_autopilot(0).1, AutopilotView::Cruise);
        assert_eq!(app.harness_state().sessions[0].view, AutopilotView::Cruise);
    }

    #[test]
    fn an_unhosted_window_keeps_the_master_and_agent_switches() {
        use aterm_phase::Program;
        let claude = Some(Program::Claude);
        for (toml, lit_claude, lit_agy) in [
            ("[harness]\nheadless = true\n", true, true),
            ("[harness]\nenabled = false\n", false, false),
            (
                "[harness]\nheadless = true\n[harness.claude]\nenabled = false\n",
                false,
                true,
            ),
        ] {
            let mut app = App::headless_for_test();
            assert!(app.harness.is_none(), "CONTROL: no host");
            app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read(toml),
            );
            assert_eq!(app.unhosted_glass(claude), lit_claude, "{toml:?}: Claude");
            assert_eq!(
                app.unhosted_glass(Some(Program::Agy)),
                lit_agy,
                "{toml:?}: agy"
            );
            let (word, view, supervised) = app.session_autopilot(0);
            assert_eq!(view, AutopilotView::Unhosted, "{toml:?}");
            assert_eq!(
                harness_theme(view, supervised, word, app.unhosted_glass(claude)).is_some(),
                lit_claude,
                "{toml:?}: the border"
            );
        }
    }

    #[test]
    fn the_published_posture_joins_the_rosters_actual_agent() {
        let (app, _host) = agent_policy_app();
        app.publish_harness_posture();
        let read = |name| {
            publish_program(&app, name);
            let roster = crate::control::cmd_sessions_store(&app.store, None);
            roster
                .lines()
                .nth(1)
                .expect("one session")
                .split_whitespace()
                .find_map(|field| field.strip_prefix("autopilot="))
                .expect("effective mode")
                .to_string()
        };
        assert_eq!(read("claude"), "off");
        assert_eq!(read("agy"), "cruise");
        assert_eq!(read("codex"), "-", "a vendor codex claims no mode");
        assert_eq!(read("sh"), "-", "non-agent rows do not claim a mode");
    }

    #[test]
    fn the_app_publishes_its_posture_to_the_registry() {
        let mut app = App::headless_for_test();
        let read = |app: &App| {
            app.store
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .harness_posture()
        };
        assert_eq!(read(&app), HarnessPosture::Unhosted, "unpublished");
        let host = idle_host();
        app.harness = Some(host.clone());
        app.publish_harness_posture();
        assert_eq!(read(&app), HarnessPosture::On(Autopilot::Cruise));
        host.shutdown_and_join();
    }

    #[test]
    fn the_harness_page_opens_in_the_window_asked() {
        use crate::native_settings::SettingsRoute;
        let mut app = App::headless_for_test();
        assert!(
            app.native_settings_route_view_target(SettingsRoute::Harness)
                .is_none()
        );
        assert!(app.open_harness_settings(WindowId(0), crate::harness_screen::HarnessFocus::Top));
        assert!(
            app.native_settings_route_view_target(SettingsRoute::Harness)
                .is_some()
        );
        let title = AutopilotView::Unhosted.title();
        assert!(
            title.ends_with(&format!(
                "Settings \u{25b8} {}",
                SettingsRoute::Harness.label()
            )),
            "{title}"
        );
        assert_eq!(
            SettingsRoute::from_path("/harness"),
            Some(SettingsRoute::Harness)
        );
    }

    #[test]
    fn the_wire_word_is_the_effective_mode_of_an_agent_session() {
        let on = HarnessPosture::On(Autopilot::Cruise);
        assert_eq!(
            wire_field(on, None, Some(aterm_phase::Program::Claude)),
            "cruise"
        );
        assert_eq!(
            wire_field(
                on,
                Some(Autopilot::Land),
                Some(aterm_phase::Program::Claude)
            ),
            "land"
        );
        assert_eq!(
            wire_field(on, Some(Autopilot::Off), Some(aterm_phase::Program::Claude)),
            "off"
        );
        assert_eq!(
            wire_field(
                HarnessPosture::On(Autopilot::Off),
                None,
                Some(aterm_phase::Program::Claude)
            ),
            "off"
        );
        assert_eq!(
            wire_field(
                HarnessPosture::On(Autopilot::Off),
                Some(Autopilot::Cruise),
                Some(aterm_phase::Program::Claude)
            ),
            "cruise",
            "a session's word opts it in under a default of off"
        );
        assert_eq!(
            wire_field(
                HarnessPosture::MasterOff,
                Some(Autopilot::Cruise),
                Some(aterm_phase::Program::Claude)
            ),
            "off"
        );
        assert_eq!(
            wire_field(
                HarnessPosture::Unhosted,
                None,
                Some(aterm_phase::Program::Claude)
            ),
            "off"
        );
        assert_eq!(
            wire_field(on, Some(Autopilot::Land), None),
            "-",
            "no agent: -"
        );
    }
}
