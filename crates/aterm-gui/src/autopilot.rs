// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AUTOPILOT, the window's side (`docs/DESIGN-harness-autopilot-2026-10-15.md`
//! §2): the per-session harness control the footer's chip cycles, Settings ▸
//! Harness sets, and `aterm ctl @<sid> meta set autopilot` writes.
//!
//! **The modes** are aterm-agent's ([`Autopilot`]): `cruise` keeps the work
//! going, and nothing the agent says ends it (Autopilot Cruise), `land` lands
//! all of the work on the remote main branch and then types nothing until
//! someone's turn (Autopilot Land), `off` keeps the harness's hands off the
//! session (plain Claude Code, the owner's A/B baseline). The ENGINE reads
//! `land` at each point (its turn-end policy); the HOST implements `off`: it
//! stops the session's worker and starts none while the word holds
//! (`harness_host`'s `OffSessions`).
//!
//! **Where a session's mode comes from** (§2.1), the one resolver being
//! [`aterm_agent::supervise::SupervisorConfig::autopilot_for`]:
//!
//! 1. the master switch (`[harness] enabled = false`) wins over everything:
//!    nothing is supervised and the chip says so ([`AutopilotView::MasterOff`]);
//! 2. else the session's own word ([`SessionMeta::autopilot`]: the chip,
//!    Settings, `meta set autopilot`), carried across a restore and a seamless
//!    update;
//! 3. else the table's default, `[harness] autopilot` (`cruise` when absent).
//!
//! A window with no control socket has no host at all
//! ([`AutopilotView::Unhosted`]): nothing supervises it, whatever the words.
//!
//! **This module is the API the other slices build on**: the chip's glyphs,
//! words, tones and titles (§2.3, [`AutopilotView`]), what `status` and `ls`
//! print (`autopilot=`, [`wire_field`]), and the App's reader and writers
//! ([`App::session_autopilot`], [`App::set_session_autopilot`],
//! [`App::cycle_session_autopilot`], [`App::open_harness_settings`]). A write
//! from the window is the same store write `meta set autopilot` makes — no
//! control-socket round trip, nothing sent to the PTY, never a person's
//! keystroke — and rings the host (`session_timeline::apply_meta_value`).
//!
//! [`SessionMeta::autopilot`]: crate::session_timeline::SessionMeta::autopilot

use std::sync::OnceLock;

use aterm_agent::supervise::{Autopilot, SupervisorConfig};
use aterm_effects::harness_card::CardPalette;

use crate::{App, WindowId};

/// What the window's harness is, for every session's Autopilot at once: the
/// part of a session's effective mode that is not the session's own word.
/// Published by the App into the session registry
/// ([`crate::session_store::SessionStore::harness_posture`]) for the readers
/// that run off the main thread (the `sessions` roster), and computed in
/// place on it ([`App::harness_posture`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum HarnessPosture {
    /// No host: this instance has no control socket, so nothing supervises
    /// any session (`harness_host` starts only with one). The default, so a
    /// registry nobody published to says nothing is supervised.
    #[default]
    Unhosted,
    /// The policy supervises nothing here: `[harness] enabled = false` — or
    /// a headless instance under `[harness] headless = false`
    /// ([`crate::harness_host::would_supervise`]).
    MasterOff,
    /// The host supervises, and a session with no word of its own runs under
    /// this default (`[harness] autopilot`).
    On(Autopilot),
    /// The host runs, with some agent tables disabled. The mask is the live
    /// switches' encoding, derived from `SupervisorConfig::on_for`.
    Restricted(Autopilot, u8),
    /// The host runs, and this session's agent's OWN switch is off
    /// (`[harness.codex] enabled = false`, the master on): what
    /// [`Self::for_agent`] joins [`Self::Restricted`] to for a KNOWN agent
    /// the policy does not admit (§10.2.3, item 5). A session's posture
    /// alone — the window's is never this.
    AgentOff(aterm_phase::Program),
}

impl HarnessPosture {
    /// The posture of a window that has a host (`hosted`) running `cfg`, a
    /// headless instance where `headless`.
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

    /// Join the published program with the same policy gate the worker uses:
    /// an admitted agent runs under the table's default, a known one whose
    /// own switch is off reads [`Self::AgentOff`] (its chip and its row name
    /// the switch, §10.2.1). An agent not yet read keeps
    /// [`Self::MasterOff`]: an unresolved restricted projection cannot
    /// promise a carrying mode, and no chip is drawn before the program is
    /// read.
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

/// What an agent's own switch off says (§10.2.1): its Settings row's label
/// (`prefs::agent_switch_label`: `Supervise <product> sessions`, and Codex's
/// `Watch codex-alab sessions` since aterm supervises no Codex) and `is off`
/// — `Supervise Claude Code sessions is off` — so the chip, its row and the
/// menu name the switch a click turns on (`App::press_harness_switch`).
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

/// ONE MODE'S SENTENCE (`docs/DESIGN-harness-autopilot-2026-10-15.md`
/// §12.4.1, slice 4d), in the form each surface says it: the owner's own
/// words ("keep finding more to do", "wrap it up") around the engine's
/// truth, the remote main branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ModeWords {
    /// The mode the sentence says.
    pub(crate) mode: Autopilot,
    /// The Autopilot chip's title, after `autopilot: <mode> — `
    /// (`claude_lights::ChipFace::explanation`).
    pub(crate) chip: &'static str,
    /// The `Autopilot set to <mode>` record's line, after its tab
    /// (`message_reporters::autopilot_set`).
    pub(crate) record: &'static str,
    /// Settings ▸ Harness's mode button, as its description says what a
    /// press does (`native_settings_harness`'s `mode_description`).
    pub(crate) button: &'static str,
}

/// EVERY MODE'S SENTENCE, ONE TABLE (§12.4.1, slice 4d): the chip's title,
/// the records, Settings ▸ Harness and the manual (`aterm help harness`'s
/// AUTOPILOT) read it, so a mode is never said two ways. In
/// [`Autopilot::ALL`]'s order; [`mode_words`] looks one up.
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

/// A TIMED CRUISE'S SENTENCES (§12.4.1's third row): Cruise until a time,
/// then Land. The chip's title follows its phrase (`autopilot: cruise until
/// 18:00, then land — `), which already names the time, so it keeps going
/// until then rather than for ever; the record set it says the same.
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

/// A session's EFFECTIVE Autopilot as the window shows it: the mode it runs
/// under, or why the harness runs none at all. What the chip draws (§2.3);
/// the session's own word is beside it ([`App::session_autopilot`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AutopilotView {
    /// Cruise: the work kept going; nothing the agent says ends it.
    Cruise,
    /// Land: all of the work landed on the remote main branch, then nothing
    /// typed until someone's turn.
    Land,
    /// Off: the harness keeps its hands off this session.
    Off,
    /// `[harness] enabled = false`: no session is supervised, whatever its
    /// word ([`HarnessPosture::MasterOff`]). A click on the chip turns the
    /// master switch on instead of cycling (`App::press_harness_switch`).
    MasterOff,
    /// No host (no control socket): nothing supervises any session
    /// ([`HarnessPosture::Unhosted`]). The footer draws no chip for it — the
    /// Autopilot chip needs a host (`crate::claude_lights`) — and the surfaces
    /// that do say it read it as the master switch's: `off` on the wire
    /// ([`Self::wire`]), no word a cycle writes ([`Self::next`]), and
    /// Settings ▸ Harness naming why.
    Unhosted,
    /// The master switch on and this agent's own switch off
    /// ([`HarnessPosture::AgentOff`], §10.2.3 item 5): read as the master
    /// switch's off everywhere — `off` on the wire, no word a cycle writes,
    /// a click turning that switch on — but NAMED as the agent's switch
    /// (`Supervise Claude Code sessions is off`, [`agent_switch_off`]).
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

/// The chip while a click's word is being written and read back (§2.3,
/// "changing"): `◐`, drawn alone.
pub(crate) const PENDING_GLYPH: &str = "\u{25d0}";

/// The clause v1's spelling of the Autopilot chip's title ends with where no
/// host runs a harness and a click opens Settings ▸ Harness
/// ([`AutopilotView::title`], which the face table's tests hold
/// `claude_lights::autopilot_title` to): the page by the name its rail row
/// shows (`SettingsRoute::Harness`). Test-only since the `⚙` chip, whose
/// title it also ended, went (owner, 2026-10-20: "remove the harness
/// settings button in the UX").
#[cfg(test)]
macro_rules! open_harness_click {
    () => {
        "click: open Settings \u{25b8} Harness"
    };
}

/// The clause v1's spelling of the title ends with where a switch of
/// aterm.toml holds the harness off — the master switch, the agent's own —
/// and a click turns it on, as the top rule's `on` does (owner, 2026-10-20:
/// "remove the harness settings button in the UX").
#[cfg(test)]
macro_rules! harness_on_click {
    () => {
        "click: turn the harness on"
    };
}

impl AutopilotView {
    /// Every view, the three modes in the chip's cycle order first (the
    /// tests' sweep of the table).
    #[cfg(test)]
    pub(crate) const ALL: [Self; 6] = [
        Self::Cruise,
        Self::Land,
        Self::Off,
        Self::MasterOff,
        Self::Unhosted,
        Self::AgentOff(aterm_phase::Program::Claude),
    ];

    /// The view of a session whose own word is `word` under `posture`:
    /// the posture's absence of a harness first, then the one resolver's
    /// rule (the word, else the table's default).
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

    /// The mode the harness runs the session under: `None` where it runs
    /// none ([`Self::MasterOff`], [`Self::Unhosted`], [`Self::AgentOff`]).
    #[must_use]
    pub(crate) const fn mode(self) -> Option<Autopilot> {
        match self {
            Self::Cruise => Some(Autopilot::Cruise),
            Self::Land => Some(Autopilot::Land),
            Self::Off => Some(Autopilot::Off),
            Self::MasterOff | Self::Unhosted | Self::AgentOff(_) => None,
        }
    }

    /// The view as a small number (`0..=10`), distinct for every view — an
    /// agent's own switch off by its agent — for the repaint keys that pack
    /// it (`claude_lights::WindowLights::autopilot_term`, bits 8–15; the
    /// harness card's).
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

    /// Whether the harness carries this session's work at all: Cruise or
    /// Land. Where the harness's acts are shown; the harness's glass (the
    /// card, the border) is [`Self::wears_glass`].
    #[must_use]
    pub(crate) const fn carries(self) -> bool {
        matches!(self, Self::Cruise | Self::Land)
    }

    /// The colours the harness's glass wears under this view (§3): Cruise's
    /// spectrum, Land's sunset, Off's moonbow (white); none where no harness
    /// runs ([`Self::MasterOff`], [`Self::Unhosted`], [`Self::AgentOff`]).
    #[must_use]
    pub(crate) const fn card_palette(self) -> Option<CardPalette> {
        match self {
            Self::Cruise => Some(CardPalette::Cruise),
            Self::Land => Some(CardPalette::Land),
            Self::Off => Some(CardPalette::Off),
            Self::MasterOff | Self::Unhosted | Self::AgentOff(_) => None,
        }
    }

    /// Whether, and in what colours, the harness's glass — the launch card,
    /// the composer's border — is drawn for a session under this view, where
    /// `supervised` says this window's host holds its live claim: Cruise and
    /// Land only under that claim; Off with none, since Off stops the
    /// session's worker, which gives the claim back (`harness_host`'s
    /// `OffSessions`), so Off rests on the view alone — the view says a host
    /// runs with the master switch and the agent's on. Never where no harness
    /// runs.
    #[must_use]
    pub(crate) const fn wears_glass(self, supervised: bool) -> Option<CardPalette> {
        match self {
            Self::Cruise | Self::Land if !supervised => None,
            _ => self.card_palette(),
        }
    }

    /// The word a click writes ([`App::cycle_session_autopilot`]): the next
    /// mode in [`Autopilot::ALL`]'s order (Cruise → Land → Off → Cruise).
    /// `None` where the harness runs no mode: the chip turns on the switch
    /// that holds it off there instead (`App::press_harness_switch`).
    #[must_use]
    pub(crate) fn next(self) -> Option<Autopilot> {
        let mode = self.mode()?;
        let at = Autopilot::ALL.iter().position(|m| *m == mode)?;
        Some(Autopilot::ALL[(at + 1) % Autopilot::ALL.len()])
    }

    /// The chip's glyph (§2.3): `∞` Cruise, `⤓` Land, `○` Off, `⊘` where no
    /// harness runs.
    #[must_use]
    pub(crate) const fn glyph(self) -> &'static str {
        match self {
            Self::Cruise => "\u{221e}",
            Self::Land => "\u{2913}",
            Self::Off => "\u{25cb}",
            Self::MasterOff | Self::Unhosted | Self::AgentOff(_) => "\u{2298}",
        }
    }

    /// The word drawn beside the glyph at rest: none for Cruise (its `∞`
    /// alone, the indicator the owner asked for), `land`, `off`, and `off`
    /// where no harness runs.
    #[must_use]
    pub(crate) const fn word(self) -> Option<&'static str> {
        match self {
            Self::Cruise => None,
            Self::Land => Some("land"),
            Self::Off | Self::MasterOff | Self::Unhosted | Self::AgentOff(_) => Some("off"),
        }
    }

    /// What the chip shows at rest: the glyph, then its word after a space
    /// where it has one (`∞`, `⤓ land`, `○ off`, `⊘ off`). The tests' spelling
    /// of it: the painter lays [`Self::glyph`] and [`Self::word`] out cell by
    /// cell (`claude_lights::face_of`), and builds no string for the face.
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

    /// The chip's title at rest (§2.3): what the mode does, then what a click
    /// does. The window composes it from the face table
    /// (`claude_lights::autopilot_title`), which gives exactly this for the
    /// default glass with no trail, then v2's ` · right-click: more` (the
    /// chip's menu, §9.3.2); kept here in v1's form, each mode's sentence
    /// read from [`MODE_WORDS`], and the face table's tests hold it to it.
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

    /// The word `status` and `ls` print for a supervisable agent session
    /// (`autopilot=`, [`wire_field`]): the mode, and `off` wherever the
    /// harness runs none — the master switch off, or no host.
    #[must_use]
    pub(crate) const fn wire(self) -> &'static str {
        match self {
            Self::Cruise => "cruise",
            Self::Land => "land",
            Self::Off | Self::MasterOff | Self::Unhosted | Self::AgentOff(_) => "off",
        }
    }
}

/// THE ONE THEME PREDICATE of the harness's persistent glass — the
/// composer's border, and the agents panel the harness paints: the session's
/// palette where its glass is worn ([`AutopilotView::wears_glass`]), or, in
/// a window with no host ([`AutopilotView::Unhosted`]) whose policy would
/// supervise the agent were there one (`unhosted_on`: the master switch and
/// the agent's own on, [`App::unhosted_glass`]) and whose session's own
/// `word` is not Off, the word's (Land's sunset, else Cruise's spectrum: the
/// border an unhosted window has always drawn). `None`: bare. The composer's
/// top rule wears it only where a harness switch stands
/// ([`HarnessSwitch::of`]): with no host it draws no harness border, and
/// the unhosted arm themes the agents panel alone.
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

/// THE HARNESS SWITCH (owner, 2026-10-20: "there needs to be an "on" button
/// drawn for the claude harness"; 2026-10-21: "alab harness on"): the one
/// on/off switch at the right of a hosted agent session's composer top rule,
/// ` alab harness ● on ` while the harness carries the session — the CLI's
/// lockup at the rule's left names the program. In every state the harness
/// does not carry it — the session's own Off, its agent's switch off, the
/// master switch off — the same switch is drawn off, with an `on` button:
/// ` alab harness ○ off ▐ on ▌ ` (`claude_footer`'s `switch_face`). A window
/// with no host has no switch: nothing there could turn a harness on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum HarnessSwitch {
    /// The harness carries the session (Cruise or Land): `● on` lit; a click
    /// turns the session Off ([`App::toggle_session_harness`]).
    On,
    /// The harness's hands are off, held so by the switch named: `○ off` lit
    /// and `on` drawn as a button; a click turns it on
    /// ([`App::switch_harness_on`]).
    Off(SwitchOff),
    /// A click turned a switch of aterm.toml on and the policy in force has
    /// not read it back yet: `◐ turning on`, and a click does nothing until
    /// it does or [`SWITCH_SETTLE`] passes (`claude_lights`).
    Turning,
}

/// Which switch holds a session's harness off ([`HarnessSwitch::Off`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SwitchOff {
    /// The session's own Autopilot word is `off` (or, with none, the
    /// table's default is).
    Session,
    /// Its agent's own switch, `[harness.<agent>] enabled = false`.
    Agent(aterm_phase::Program),
    /// The master switch, `[harness] enabled = false` (or a headless
    /// instance under `[harness] headless = false`).
    Master,
}

/// How long a click's switch write may take to read back before the switch
/// stops saying `◐ turning on` ([`HarnessSwitch::Turning`]): the config
/// lane's write, the watcher's next poll (half a second) and the host's
/// adoption, with room to spare. A write the lane refuses says so at once
/// and lets the switch go then.
pub(crate) const SWITCH_SETTLE: std::time::Duration = std::time::Duration::from_secs(5);

impl HarnessSwitch {
    /// The switch a session under `view` shows: `None` with no host. A
    /// click's write in flight is the window's to know (`claude_lights`),
    /// which shows [`Self::Turning`] over an off reading.
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

    /// Its state as a screen reader is told it (`Role::Switch`'s toggled
    /// state): on, off, or mixed while it turns on.
    #[must_use]
    pub(crate) const fn toggled(self) -> Option<bool> {
        match self {
            Self::On => Some(true),
            Self::Off(_) => Some(false),
            Self::Turning => None,
        }
    }

    /// What the switch is, said after its name (the accessibility node's
    /// description): what holds it off, and what a click does.
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

/// The aterm.toml key of the headless gate (`[harness] headless`), which
/// turns supervision off in a headless instance alone.
pub(crate) const HARNESS_HEADLESS_KEY: &str = "harness.headless";

/// The aterm.toml key of `agent`'s own switch (`harness.<agent>.enabled`),
/// found among the switch rows Settings ▸ Harness draws
/// ([`crate::prefs::agent_switch`]): `None` for an agent with no table.
#[must_use]
pub(crate) fn agent_switch_key(agent: aterm_phase::Program) -> Option<&'static str> {
    crate::prefs::NESTED_LEAVES
        .iter()
        .map(|leaf| leaf.key)
        .find(|key| crate::prefs::agent_switch(key) == Some(agent))
}

/// What turning the harness on for one session writes
/// ([`App::switch_harness_on`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SwitchOnPlan {
    /// The aterm.toml switches that hold it off, each written `true`.
    pub(crate) keys: Vec<&'static str>,
    /// The session's own word is written `cruise`: it, or the table's
    /// default under no word, would leave the session Off with every switch
    /// on.
    pub(crate) cruise: bool,
}

/// [`SwitchOnPlan`] for a session of `agent` whose own word is `word`,
/// under `cfg` in an instance that is `headless` or not: every switch of
/// aterm.toml that holds it off — the master switch, the headless gate, the
/// agent's own switch — and its word where that would still leave it Off.
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

/// A session's own Autopilot word, read from its context (its metadata, a
/// leaf lock taken alone): `None` while it has none.
#[must_use]
pub(crate) fn word_of(ctx: &crate::SessionCtx) -> Option<Autopilot> {
    ctx.meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .autopilot_word()
}

/// This process's host's claim holder ([`crate::harness_host::holder`]),
/// built once: it names the process, which a frame never changes, and the
/// render path compares a session's claim against it
/// ([`App::session_autopilot`], [`App::harness_ab_arm`]).
pub(crate) fn own_holder() -> &'static str {
    static HOLDER: OnceLock<String> = OnceLock::new();
    HOLDER.get_or_init(crate::harness_host::holder)
}

/// One pass's reading of the metrics clock (`metrics::now_us`) for the
/// liveness of supervisor claims ([`App::session_autopilot_on`]): read at
/// most once, and only for a claim of this host's that lapses.
#[derive(Debug, Default)]
pub(crate) struct ClaimClock(std::cell::Cell<Option<u64>>);

impl ClaimClock {
    /// The pass's instant, read on first ask.
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

/// The `autopilot=` word `status` and the `sessions` roster print beside
/// `supervisor=`: the session's EFFECTIVE mode ([`AutopilotView::wire`]) when
/// it runs an agent the harness supervises (`supervisable`: its published
/// program by `crate::harness_host::agent_of`), joined with that agent's
/// policy gate, `-` otherwise — a shell, an
/// editor, an agent no reader is measured for.
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

/// The `land_at=` field `status`, `ls` and the bare `meta` print
/// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.3.5): the instant a
/// timed Cruise lands, in unix seconds, from the stored `land_at` (unix ms),
/// and `-` while none is stored.
#[must_use]
pub(crate) fn land_at_field(land_at: Option<i64>) -> String {
    land_at.map_or_else(|| "-".to_string(), |ms| ms.div_euclid(1000).to_string())
}

/// The `task=` word `status` and `ls` print (§9.4.1): the task's face as the
/// session's loop last told it (`open|landed|theirs`) for a supervisable
/// agent session (`agent`) whose face this window knows (`told_here`: its
/// host holds the live supervisor claim, or the live claim is a proven
/// self-claim whose own acts tell it, Autopilot Land), and `-` otherwise
/// — another process's supervisor sends no fact here, so a face it would
/// leave stale is never printed, and `-` too before anyone has told one.
#[must_use]
pub(crate) fn task_field(
    face: Option<aterm_agent::harness::acts::TaskFace>,
    told_here: bool,
    agent: Option<aterm_phase::Program>,
) -> &'static str {
    match face {
        Some(face) if told_here && agent.is_some() => face.status(),
        _ => "-",
    }
}

/// The `pause=` field `status` and `ls` print after `task=`
/// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §10.5.1): the unix second
/// the loop's pause after short turns ends, as the session's loop last told
/// this window's host, for a supervisable agent session (`agent`) whose live
/// claim that host holds (`claimed_here`) — and `-` otherwise, as `task=`.
/// A clock time, never a countdown.
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

/// THE WINDOW SURFACE A PERSON PRESSED to write a session's Autopilot pair
/// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §11.7.3, items 1 and 2):
/// stored with the writer
/// ([`crate::session_timeline::AutopilotSetBy`]), so the records, the
/// Autopilot menu, Settings ▸ Harness and `meta` can say whose hand it was.
/// Each of the window's writers names its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Surface {
    /// The footer's Autopilot chip ([`App::cycle_session_autopilot`]).
    Chip,
    /// The composer border's harness toggle
    /// ([`App::toggle_session_harness`]).
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

    /// Its word after `human:` in `meta`'s `autopilot_by=` and the restore
    /// leaf's `autopilot_by` (§11.7.2).
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

    /// How a line names it (§11.7.1): `from the chip`, `from Settings ▸
    /// Harness, every session`.
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

/// WHO SET A SESSION'S AUTOPILOT, IN WORDS (§11.7.3, item 4): composed
/// where it is shown — a record, the Autopilot menu's note, a SESSIONS row —
/// from the stored setter, never kept ([`App::autopilot_set_by_words`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SetByWords {
    /// Who, as a line ends: `over aterm ctl`, `from the chip`, `from tab 2
    /// (✳ Claude Code)`, `before aterm restarted`.
    pub(crate) by: String,
    /// When, as the local clock and day read it; `None` for a word carried
    /// from before a restart with no setter, or where the zone cannot be
    /// read.
    pub(crate) at: Option<crate::harness_glass::Stamp>,
}

impl SetByWords {
    /// The words of a setter (the stored one: `None` for a word that names
    /// none) as the session's word stands (`has_word`), with
    /// `sid_place(sid)` naming the tab of a session that wrote over its edge
    /// connection (`None`: no live session has that sid). `None` where there
    /// is nothing to say: no setter and no word.
    #[must_use]
    pub(crate) fn of(
        set_by: Option<crate::session_timeline::AutopilotSetBy>,
        has_word: bool,
        sid_place: impl FnOnce(&str) -> Option<String>,
    ) -> Option<Self> {
        use aterm_slog::Actor;
        let Some(set_by) = set_by else {
            // A word carried from a leaf that names no setter (§11.7.3, item
            // 5: `restored`).
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

    /// The menu's note and a SESSIONS row's words (§11.7.1): `set 14:05 over
    /// aterm ctl`, `set Fri 14:05 from the chip` (clock times, never ages,
    /// the day named against `today` by [`crate::harness_glass::Stamp::words`]),
    /// `set before aterm restarted`.
    #[must_use]
    pub(crate) fn line(&self, today: Option<i64>) -> String {
        match self.at {
            Some(at) => format!("set {} {}", at.words(today), self.by),
            None => format!("set {}", self.by),
        }
    }

    /// A record's line (§11.7.1): `set over aterm ctl`, `set from the
    /// chip` — the record has its own time. The timer's fire names its own
    /// (`set at the 18:00 timer`, `App::autopilot_timer_landed`).
    #[must_use]
    pub(crate) fn record_line(&self) -> String {
        format!("set {}", self.by)
    }
}

impl App {
    /// The window's harness posture now: whether it has a host, and the
    /// policy in force (the config watcher's, `harness_policy`).
    #[must_use]
    pub(crate) fn harness_posture(&self) -> HarnessPosture {
        HarnessPosture::of(
            self.harness.is_some(),
            &self.harness_policy.get().policy,
            self.headless,
        )
    }

    /// Whether a window with no host wears the harness's glass for `agent`
    /// ([`harness_theme`]'s `unhosted_on`): the policy in force would
    /// supervise it were there a host — the master switch on, and the
    /// agent's own. Off either way, an unhosted window's border and panel
    /// stay bare, as a hosted one's do.
    #[must_use]
    pub(crate) fn unhosted_glass(&self, agent: Option<aterm_phase::Program>) -> bool {
        matches!(
            HarnessPosture::of(true, &self.harness_policy.get().policy, self.headless)
                .for_agent(agent),
            HarnessPosture::On(_)
        )
    }

    /// Publish [`Self::harness_posture`] into the session registry, for the
    /// readers off the main thread (the `sessions` roster). Called when the
    /// policy in force changes (`adopt_harness_policy`); the launch publishes
    /// its own before the first request is served.
    pub(crate) fn publish_harness_posture(&self) {
        let posture = self.harness_posture();
        self.store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .set_harness_posture(posture);
    }

    /// Session `local_id`'s Autopilot: `(word, view, supervised)` — its own
    /// word (`None`: unset, the table's default decides), the EFFECTIVE view
    /// under this window's posture ([`AutopilotView::of`]), and whether this
    /// window's host holds its live supervisor claim now (the claim's holder
    /// is [`crate::harness_host::holder`]): what the chip and the harness
    /// card draw. A session that is gone reads `(None, view of no word,
    /// false)`.
    ///
    /// Leaf locks only (the published program, then the session's metadata),
    /// so it is safe to call on
    /// the render path — and it is, ONCE a frame per Claude Code pane in the
    /// footer's repaint key and once in its splice, where the chips and the
    /// harness card share the reading ([`Self::session_autopilot_on`]). So
    /// it ALLOCATES NOTHING: the word is matched as stored
    /// ([`SessionMeta::autopilot_word`]), the claim's holder compared in
    /// place against this host's, built once ([`own_holder`]); and it reads
    /// the clock only for this host's own claim WITH A LAPSE (`ttl=`), whose
    /// liveness is the one question that needs it.
    ///
    /// [`SessionMeta::autopilot_word`]: crate::session_timeline::SessionMeta::autopilot_word
    #[must_use]
    pub(crate) fn session_autopilot(
        &self,
        local_id: u64,
    ) -> (Option<Autopilot>, AutopilotView, bool) {
        self.session_autopilot_on(local_id, &ClaimClock::default())
    }

    /// [`Self::session_autopilot`] on a pass's shared claim clock: a pass
    /// over every pane of a frame reads the clock at most once, and only
    /// where a claim of this host's lapses.
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

    /// The agent session `local_id` publishes that the host supervises or
    /// watches, if any ([`crate::harness_host::admitted`]: its timeline's
    /// program, never a vendor `codex`).
    pub(crate) fn session_agent(&self, local_id: u64) -> Option<aterm_phase::Program> {
        self.pool
            .get(local_id)
            .and_then(|session| crate::harness_host::admitted(&session.ctx))
    }

    /// Session `local_id`'s OWN posture: the window's, joined with its
    /// agent's switch ([`HarnessPosture::for_agent`]) — what its chip draws,
    /// what Settings ▸ Harness's buttons for it act under, and what a press
    /// of them is checked against.
    pub(crate) fn session_harness_posture(&self, local_id: u64) -> HarnessPosture {
        self.harness_posture()
            .for_agent(self.session_agent(local_id))
    }

    /// [`Self::session_autopilot_on`] for an `agent` the caller already read
    /// under its one timeline guard (the footer's), with a fourth answer only
    /// the composer's top line reads: whether the live claim is a PROVEN
    /// SELF-CLAIM (`SessionMeta::supervisor_proven`, [`crate::self_claim`]) —
    /// codex-alab carrying itself, whose harness is ALab's though this
    /// window's host holds no claim. Read in the same meta lock as the claim's
    /// holder, on the pass's clock, which it reads only for a proven claim
    /// with a lapse.
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
            // `SessionMeta::live_supervisor`'s rule, the clock read last.
            let live = |claim: &crate::session_timeline::SupervisorClaim| {
                claim.expires_us.is_none_or(|at| at > clock.now_us())
            };
            let supervised = meta
                .supervisor
                .as_ref()
                .is_some_and(|claim| claim.holder == own_holder() && live(claim));
            // `SessionMeta::supervisor_proven`'s rule, the same way.
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

    /// Write session `local_id`'s own Autopilot word (`None`: unset it, and the
    /// table's default decides) — the same store write `meta set|unset
    /// autopilot` makes, through the same typed ladder
    /// ([`Self::write_session_meta_field`]): no control-socket round trip,
    /// nothing sent to the PTY, never a person's keystroke (no `human_ms=`
    /// stamp). A write that moves the word rings the host
    /// (`session_timeline::apply_meta_value`), refreshes what a `meta set`
    /// refreshes, repaints the session's footer
    /// ([`crate::claude_footer::post_changed`]) and leaves a `harness` RECORD
    /// in the message log (never a row: nothing is asked of the person).
    /// Whether the word moved.
    ///
    /// A HAND WRITE: it drops any timer the session's Cruise held, the same
    /// word included (§9.3.6, item 1) —
    /// [`Self::set_session_autopilot_timed`] with no instant. `surface` is
    /// the window surface pressed, stored as the pair's setter with the
    /// person (§11.7.3, item 2).
    pub(crate) fn set_session_autopilot(
        &mut self,
        local_id: u64,
        word: Option<Autopilot>,
        surface: Surface,
    ) -> bool {
        self.set_session_autopilot_timed(local_id, word, None, surface)
    }

    /// THE WINDOW'S ONE WRITER OF THE AUTOPILOT PAIR (§9.3.6, item 1):
    /// session `local_id`'s own word and, beside `cruise` only, the instant
    /// its timed Cruise lands (`land_at`, unix ms; `None`: no timer) —
    /// `session_timeline::apply_autopilot`, the store write `meta set
    /// autopilot … [until=]` makes. Whether either half moved; a move is
    /// fanned out as [`Self::set_session_autopilot`] says, and the session's
    /// timer is indexed again from what is stored. Its setter is the person,
    /// at `surface` (§11.7.3, item 2).
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
        // The window's own chip and menu: the person's.
        let write = crate::session_timeline::apply_autopilot(
            &ctx,
            word.map(|w| w.word().to_string()),
            land_at,
            aterm_slog::Actor::Human,
            Some(surface),
        );
        self.autopilot_pair_written(local_id, write)
    }

    /// THE FAN-OUT of a window write of session `local_id`'s Autopilot pair
    /// (the window's writer, [`Self::set_session_autopilot_timed`]; the
    /// timer's cancel, `App::cancel_autopilot_timer`): the timer indexed
    /// again from what is stored, and an open Autopilot menu for the session
    /// built again (`App::refresh_autopilot_menu`) — both even where the
    /// write moved nothing (a wire write of the same pair whose wake is
    /// still in flight). Where it `moved`, what [`Self::set_session_autopilot`]
    /// says follows, its record naming the setter the write stored. Returns
    /// whether it moved.
    pub(crate) fn autopilot_pair_written(
        &mut self,
        local_id: u64,
        write: crate::session_timeline::AutopilotWrite,
    ) -> bool {
        self.autopilot_pair_fanned_out(local_id, write, true)
    }

    /// WRITE MANY SESSIONS' AUTOPILOT PAIRS AT ONCE (design §11.5.3, item
    /// 6; the `Every session` row): each `(local_id, word, land_at)` written
    /// as the window's one writer writes one
    /// ([`Self::set_session_autopilot_timed`]), with that write's own
    /// fan-out — the timer index, an open menu, the crash journal's owed
    /// words, the footer, the A/B arm and the keyed record — by `by` at
    /// `surface` (its setter, §11.7.3, item 2), but
    /// the page published ONCE, after the loop: each publish reads every
    /// session (`App::harness_state`), so N writes each publishing would
    /// read N² sessions on one click. Published whether or not a write
    /// moved, so the page converges on what is stored, as the timer's press
    /// does (`App::execute_harness_timer`). A session gone since the press
    /// was read is skipped. How many pairs moved.
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

    /// [`Self::autopilot_pair_written`]'s fan-out, the page's publish in it
    /// where `publish` (the batch publishes once, after its loop).
    fn autopilot_pair_fanned_out(
        &mut self,
        local_id: u64,
        write: crate::session_timeline::AutopilotWrite,
        publish: bool,
    ) -> bool {
        let moved = write.moved;
        // The timer the write may drop, read BEFORE the index is built again
        // from what is stored (§10.2.3, item 8): the record names it.
        let dropped = self.autopilot_timers.glass(local_id);
        // Unstamped: a moved pair stamps the tab chrome below, the hover's
        // timer words in it (§12.4.1), so each strip is stamped once.
        let timer_moved = self.index_autopilot_timer_unstamped(local_id, None);
        self.refresh_autopilot_menu(local_id);
        // The person's word about typing: the crash journal carries it at
        // once (`App::journal_owner_words`; a pair it already carries owes
        // nothing).
        self.journal_owner_words(local_id);
        if !moved {
            if timer_moved {
                self.restamp_for_hover(local_id);
            }
            return false;
        }
        // What a `meta set` from the window refreshes
        // (`App::write_session_meta_field`): the tab chrome, the needs border
        // and the menu-bar item, and the session's `events` watchers.
        self.refresh_meta_dependent_chrome(local_id);
        self.refresh_presence_session(local_id);
        self.mark_status_dirty();
        if let Some(session) = self.pool.get(local_id) {
            session.ctx.watchers.ring_watchers();
        }
        crate::claude_footer::post_changed(local_id);
        // The session's A/B arm moves with its mode (Settings ▸ Harness), and
        // the page, where a view shows it, lists the word.
        self.note_harness_arm(local_id);
        if publish {
            self.harness_screen_moved();
        }
        self.record_autopilot_change(local_id, dropped, write.revision);
        true
    }

    /// THE KEYED RECORD OF A MOVE OF SESSION `local_id`'S AUTOPILOT PAIR
    /// (§2, §9.3.2; §11.7.1): the window's own write
    /// ([`Self::autopilot_pair_written`]) and a write over the wire
    /// (`App::on_meta_changed`) alike — titled by what is stored now, its
    /// line led by the session's place, the timer the write dropped
    /// (`dropped`, read before the index was built again) said cancelled,
    /// and its last line who set it, from the setter the write stored
    /// ([`Self::autopilot_set_by_words`]). `revision` is the write's
    /// ([`crate::session_timeline::AutopilotWrite::revision`]): noted as
    /// recorded, so the wire's next wake for this session records it no
    /// second time.
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
        // The records' titles name no place; their lines lead with it,
        // the pane named where the tab holds two agents (§10.3.3, item 9).
        let place = self.session_place(&sid);
        // Who set it, composed now from the setter the write stored.
        let set_by = self
            .autopilot_set_by_words(local_id)
            .map(|words| words.record_line());
        let message = match self.autopilot_timers.glass(local_id) {
            // A timed Cruise says when it lands (§9.3.2).
            Some(until) => crate::message_reporters::autopilot_timer_set(
                &sid,
                &place,
                until,
                set_by.as_deref(),
            ),
            // A hand change says the timer it dropped is cancelled.
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

    /// THE STORED SETTER OF SESSION `local_id`'S AUTOPILOT, IN WORDS
    /// (§11.7.3, item 4; [`SetByWords::of`]), composed now: an edge writer's
    /// sid names its tab and title as they read now
    /// ([`Self::autopilot_setter_tab`]). `None` where the session is gone or
    /// there is nothing to say (no setter and no word).
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

    /// The tab of the live session whose sid is `sid`, as a setter's words
    /// name it (§11.7.3, item 4): `tab 2 (✳ Claude Code)` — its place
    /// ([`Self::session_place`]) and its label by the one namer
    /// ([`Self::tab_label_of_session`]), the label left out where it is the
    /// place's own words. `None` where no live session has that sid.
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

    /// A WRITE OVER THE WIRE moved session `local_id`'s Autopilot pair
    /// (`meta set|unset autopilot …` from an Owner client, the fabric or
    /// aterm's harness: `App::on_meta_changed`, §11.7.3, item 3): its keyed
    /// record, through the window's own builders
    /// ([`Self::record_autopilot_change`]), where the session's
    /// [`crate::session_timeline::SessionMeta::autopilot_revision`] is not
    /// the one last recorded — so a wake for another field, a write the
    /// window already recorded, and a session only restored here record
    /// nothing. `dropped`: the timer the index held before the wake indexed
    /// it again.
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

    /// The chip's click: session `local_id`'s EFFECTIVE mode steps to the
    /// next in [`Autopilot::ALL`]'s order (Cruise → Land → Off → Cruise,
    /// [`AutopilotView::next`]) and is written as its own word
    /// ([`Self::set_session_autopilot`]). Under [`AutopilotView::MasterOff`]
    /// (and [`AutopilotView::Unhosted`]) nothing is written: the view is
    /// returned unchanged, and the caller turns on the switch that holds the
    /// harness off (`App::press_harness_switch`) instead. The view after the
    /// click.
    pub(crate) fn cycle_session_autopilot(&mut self, local_id: u64) -> AutopilotView {
        let (_, view, _) = self.session_autopilot(local_id);
        let Some(next) = view.next() else {
            return view;
        };
        self.set_session_autopilot(local_id, Some(next), Surface::Chip);
        self.session_autopilot(local_id).1
    }

    /// THE HARNESS SWITCH, flipped (the composer top rule's switch,
    /// [`HarnessSwitch`]): a session the harness carries (Cruise or Land) is
    /// turned Off — its own word written `off`, which takes the whole
    /// harness off it (supervision, the launch card's rainbow kitty, the
    /// composer border's branding and the glass transcript rewrites) — and one
    /// it does not carry is turned on from whichever switch holds it off
    /// ([`Self::switch_harness_on`]). With no host there is no switch, and
    /// nothing is written. The view after the flip: a switch of aterm.toml
    /// turned on reads back when the watcher adopts the file, not here.
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

    /// THE SWITCH'S "ON" (owner, 2026-10-20: "there needs to be an "on"
    /// button drawn for the claude harness"): turn the harness on for session
    /// `local_id` in ONE click, from whichever switch holds it off
    /// ([`switch_on_plan`]) — the master switch, the session's agent's own
    /// switch, and the session's own word, as many of them as are off. The
    /// switches are aterm.toml's, written `true` on the config lane
    /// ([`Self::queue_harness_switch_write`]) and in force once the watcher
    /// adopts the file (`App::adopt_harness_policy`); the word is the
    /// store's, read back at once. What it wrote.
    ///
    /// The master switch is turned on, not opened in Settings (owner,
    /// 2026-10-20: "remove the harness settings button in the UX"): "turn the
    /// Claude harness on" from a pane the master switch holds off can only
    /// mean the switch that is off, and a session the switch would still
    /// leave off (its word, or the table's default, `off`) is carried too,
    /// so the one click is never a click that shows nothing.
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

    /// Open Settings ▸ Harness in window `wid` (the no-host chip, the
    /// Autopilot menu's `Harness settings…`, View ▸ Coding Agent Harness…,
    /// the palette): the one Settings tab, raised there at the
    /// Harness route, and landed at `focus` (§10.3.3, item 8) — a session's
    /// row, drawn selected until the next press or scroll, SUPERVISION, or
    /// [`HarnessFocus::Top`], the page's top (§10.3.1: View ▸ Coding Agent
    /// Harness… opens at the top). The landing is the page's own action
    /// ([`crate::harness_screen::HARNESS_LAND`]), so it holds before the
    /// page has been measured and after, and each open replaces the one
    /// before it: a landing still waiting for its row (a session the page
    /// does not list yet) never pulls a page opened since. Whether a tab
    /// shows it.
    ///
    /// [`HarnessFocus::Top`]: crate::harness_screen::HarnessFocus::Top
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

    /// THE `on` BUTTON'S PLAN ([`switch_on_plan`]): it writes exactly the
    /// switches that hold the session off — the master switch, the headless
    /// gate in a headless instance only, the session's agent's own switch
    /// and no other agent's — and the session's word `cruise` only where
    /// its word, or the table's default under none, would leave it Off with
    /// every switch on. For every case the posture after the plan carries
    /// the session. NEGATIVE CONTROL: nothing held off, nothing written.
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

    /// THE SWITCH A VIEW SHOWS ([`HarnessSwitch::of`]): lit while the
    /// harness carries the session, off — naming the switch that holds it —
    /// in every other hosted view, and none without a host; a screen reader
    /// is told on, off, or mixed while it turns on.
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

    /// §2.1's precedence: no host and the master switch win over every word;
    /// otherwise the word, else the table's default — the one resolver's rule,
    /// checked against it for every pair.
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

    /// The posture: no host first, then the policy's switch (and the
    /// headless instance's), then the table's default.
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

    /// The chip table of §2.3, glyph, word, tone and title; a click cycles
    /// the modes in `Autopilot::ALL`'s order, and does nothing where no
    /// harness runs.
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
        // The click's cycle visits every mode once and comes back.
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
        // The glass (§3): Cruise and Land under this host's claim, Off with
        // or without one (Off gives its claim back) in white, and nothing
        // where no harness runs. The border's theme is the glass's, but in a
        // window with no host, where the session's own word not Off gives
        // the border its colours. CONTROL: an unclaimed Cruise is bare.
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

    /// AN AGENT'S OWN SWITCH OFF IS NAMED (§10.2.1, §10.2.5): for each
    /// supervisable agent, its switch off under the master switch on joins to
    /// `AgentOff` — `⊘ off`, dim, titled with the switch's own Settings label
    /// and a click that turns it on, `off` on the wire, no word a cycle
    /// writes, no default — and counts as Off in the A/B where no claim is
    /// live, while every other agent of the same policy runs the table's
    /// default. An agent not yet read reads `MasterOff`, and the master switch
    /// off wins over the agent's. NEGATIVE CONTROL: with the agent's switch
    /// on, its sessions run the default.
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
            // NEGATIVE CONTROL: every other agent runs the default.
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
            // Not yet read: no carrying mode promised, no switch named.
            assert_eq!(posture.for_agent(None), HarnessPosture::MasterOff);
            // The master switch off wins over the agent's own.
            cfg.set("enabled", "false").unwrap();
            assert_eq!(
                HarnessPosture::of(true, &cfg, false).for_agent(Some(agent)),
                HarnessPosture::MasterOff,
                "{agent:?}: the master wins"
            );
        }
        // Every switch with a Settings row is named by that row's label —
        // Codex's too, a codex-alab tab's watch (aterm supervises no Codex).
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
        // Every view packs to its own code.
        let mut codes: Vec<u64> = AutopilotView::ALL.iter().map(|v| v.code()).collect();
        codes.extend(Program::ALL.map(|p| AutopilotView::AgentOff(p).code()));
        let unique: std::collections::BTreeSet<u64> = codes.iter().copied().collect();
        assert_eq!(unique.len(), 5 + Program::ALL.len(), "{codes:?}");
        assert!(codes.iter().all(|c| *c < 256), "bits 8-15 hold it");
    }

    /// A host that supervises nothing (`Acts` inert, no sessions): what the
    /// App's posture needs to read `On`, shut down by the caller.
    fn idle_host() -> crate::harness_host::HostHandle {
        crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        )
    }

    /// The session's records of the window's writes, oldest first.
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

    /// THE WINDOW'S WRITE (§2.3: "A click on Autopilot writes the session's
    /// word in-process … it sends nothing to the PTY and is not a person's
    /// keystroke"): the word lands in the store `meta set autopilot` writes,
    /// as its one word, recorded as the ordinary `meta-change`, ringing the
    /// host, leaving a keyed `harness` record, and stamping no person's hand;
    /// a write that moves nothing does none of it; `None` hands the session
    /// back to the default. The chip's click cycles the EFFECTIVE mode in
    /// `Autopilot::ALL`'s order. NEGATIVE CONTROLS: with no host, and under
    /// the master switch off, a click writes nothing.
    #[test]
    fn the_window_writes_the_word_and_the_chip_cycles_it() {
        let mut app = App::headless_for_test();
        // A test App has no control socket: no host, nothing supervised.
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

        // The chip: Land → Off → Cruise → Land, each written as the word.
        assert_eq!(app.cycle_session_autopilot(0), AutopilotView::Off);
        assert_eq!(app.session_autopilot(0).0, Some(Autopilot::Off));
        assert_eq!(app.cycle_session_autopilot(0), AutopilotView::Cruise);
        assert_eq!(app.session_autopilot(0).0, Some(Autopilot::Cruise));
        assert_eq!(app.cycle_session_autopilot(0), AutopilotView::Land);

        // Unset: the table's default again, said as the default.
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

        // Supervised: only this host's live claim.
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
        // Its liveness still counts: a lease of this host's that lapsed (a
        // renewal to an instant long past) is no claim.
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            Some(1),
            now,
        )
        .unwrap();
        assert!(!app.session_autopilot(0).2, "this host's lapsed lease");

        // The master switch off: the cycle writes no word (the chip turns the
        // switch on instead, `App::press_harness_switch`).
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
        // The other agent: agy (a vendor `codex` is admitted to no worker
        // since the Codex screen lane's retirement, 2026-10-16).
        assert!(host.0.admits(aterm_phase::Program::Agy));
        let ctx = &app.pool.get(0).unwrap().ctx;
        // A still-visible claim is a fact, not permission for the card to carry.
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

    /// AN UNHOSTED WINDOW KEEPS THE SWITCHES (the cross-track review of
    /// 2026-10-19): with no control socket nothing supervises, but its
    /// border and panel wear the word's colours only where the policy in
    /// force would supervise the agent — the master switch off, or Claude
    /// Code's own switch off, leaves them bare, as a hosted window's are.
    /// CONTROL: the default policy lights them, and Claude's switch off
    /// leaves another agent's lit.
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
        // A vendor `codex` is shown, not supervised: no mode of the harness's.
        assert_eq!(read("codex"), "-", "a vendor codex claims no mode");
        assert_eq!(read("sh"), "-", "non-agent rows do not claim a mode");
    }

    /// The posture the App publishes into the registry is the one the
    /// `sessions` roster reads off the main thread.
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

    /// The Autopilot menu's `Harness settings…` page (and the no-host
    /// chip's): Settings ▸ Harness, opened in the window it was pressed in
    /// (View ▸ Coding Agent Harness… opens it in the front one).
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
        // The no-host title names the page the click opens, by its rail
        // name (under a switch off the click turns it on instead).
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

    /// `status`/`ls`: the effective mode for a supervisable agent, `off`
    /// wherever no harness runs, and `-` for anything else.
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
