// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CLAUDE CODE LIGHTS — lights beside aterm's footer for the
//! settings the owner always expects on (owner direction, 2026-09-24): the
//! permission mode and fast mode. The effort tag Claude Code writes into its
//! own top rule already shows itself, so it has no light of ours (owner,
//! 2026-09-27: it "already has a UX element"); nor does thinking, which the
//! owner keeps on for good (owner, 2026-09-27: "that should always be on") —
//! a light that never changes says nothing.
//!
//! DEVIATIONS ONLY. A light is drawn only while its setting DIFFERS from
//! what the owner expects ([`Expect`], [`deviates`]; owner, 2026-09-27: "I
//! don't want to show the state of parameters that I always expect to be
//! on") — or while a toggle of it is in flight, or its answer is on show.
//! At rest, everything as expected, nothing is drawn beside the footer's
//! facts in the composer's bottom rule, and Claude's own rows are whole.
//! Hover a drawn light to read its title; click it to toggle.
//!
//! ONE PERMISSION-MODE LIGHT. Auto-approve (`bypass permissions`) and auto
//! mode are two positions of Claude Code's ONE permission mode, not two
//! settings: turning one on turns the other off, and a light per position
//! made "turn auto-approve on" mean "leave auto mode" (owner, 2026-09-27:
//! "when I toggle auto-approve, it turns off auto mode (what?!)"). The owner
//! expects EITHER ([`Mode::is_expected`]), so the one light is on in both and
//! its one action, from any other mode, is forward through Claude's own cycle
//! to the nearest of them — never away from one.
//!
//! Every light is read from what Claude Code ALREADY DRAWS and toggled
//! through what Claude Code ALREADY ACCEPTS — aterm keeps no second copy of a
//! vendor setting that could disagree with the vendor:
//!
//! | light | read from | toggled by |
//! |---|---|---|
//! | permission mode | the mode pill: `⏵⏵ bypass permissions on`, `⏸ plan mode on`, … | shift+tab, Claude's own mode cycle, forward to bypass or auto (from don't ask too) — aterm's own next presses only while Claude is idle |
//! | fast mode | the composer's top rule: `↯` or `fast mode`, and Claude's answer to `/fast` ([`fast_answers`]) | `/fast on` · `/fast off`, mid-turn too; a lasting refusal stops the asking ([`FastRefusal::lasting`]) |
//!
//! Measured on 2.1.282: the top rule's text is the vendor's `topBorderText`,
//! the join of its effort tag, its fast-mode tag and an internal tag; the
//! pill pairing is the vendor's mode table (`footer::PILLS`).
//!
//! VERSION DRIFT. A light whose indicator is not on the screen reads
//! [`LightState::Unknown`], never a guess, and an unknown light cannot be
//! clicked. A toggle is a [`Drive`] the host performs and then READS BACK:
//! each input must be answered within its own window ([`settle_ms`]), and a
//! next input goes only on an answer READ — a mode return presses again only
//! once the pill shows Claude took the last press, never on a timer — else
//! the host says on the light where it stopped. A vendor that renames a pill
//! or moves a tag therefore greys the light out; it can never make aterm type
//! into a composer it no longer understands.
//!
//! FAST MODE CAN COST AUTO MODE. Claude Code 2.1.283 turns auto mode off
//! while fast mode is on where its server flag says so
//! (`tengu_auto_mode_config.disableFastMode`: "auto mode unavailable while
//! fast mode is on · run /fast off", [`AUTO_OFF_FOR_FAST`]), moving an auto
//! session to manual. That flag cannot be read before the switch, so the
//! host reads the switch's answer: a `/fast on` that took an auto session out
//! of auto says so on the fast light, and is latched — the fast light stops
//! asking in auto mode ([`Expect::fast_costs_auto`]) and refuses a click
//! there, so the trade is never made twice without the person seeing it.

use crate::harness::footer;

/// How long ONE step of a toggle may take to show on Claude's screen before
/// the host calls it failed: one shift+tab's answer (a mode return refreshes
/// it on every press), or a pasted command's echo in the composer. The
/// ceiling, not the expectation — a pill redraw lands within a frame or two.
pub const SETTLE_MS: u64 = 2500;

/// How long `/fast` may take to ANSWER once submitted. Measured in the
/// 2.1.283 bundle: an idle Claude hides its composer while it asks the
/// server whether fast mode is available to this org, for up to 8 s (the
/// vendor's `Rdr = 8000`), then says so — so the old 2.5 s gave up on every
/// `/fast on` that went through. The org check's cap, the echo and the Enter.
pub const FAST_SETTLE_MS: u64 = 10_000;

/// How long `drive`'s answer may take ([`SETTLE_MS`] for a mode press,
/// [`FAST_SETTLE_MS`] for `/fast`).
#[must_use]
pub fn settle_ms(drive: &Drive) -> u64 {
    match drive {
        Drive::CycleMode => SETTLE_MS,
        Drive::Command { .. } => FAST_SETTLE_MS,
    }
}

/// How many shift+tab presses a mode toggle may spend on its way back to an
/// expected mode. Claude's own cycle (2.1.283, the vendor's `hbt`,
/// [`Mode::next_in_cycle`]) is default → accept edits → plan → bypass (when
/// the session allows it) → auto (when available) → default: three to five
/// modes; don't ask only ever leads on to default. The nearest expected mode
/// is at most three presses away from manual, four from don't ask. A search
/// is also stopped the moment the cycle comes back round to a mode it has
/// already shown (the host's lap check) — a cycle that holds neither bypass
/// nor auto is refused there, naming the mode it stopped in, never pressed
/// past. This bound is the backstop.
pub const MAX_MODE_PRESSES: u8 = 5;

/// One light.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Light {
    /// Claude's permission mode — ONE setting, whatever its pill names. On
    /// in an expected mode ([`Mode::is_expected`]: bypass or auto), off in
    /// any other.
    Mode,
    /// Fast mode: faster output from the same model.
    Fast,
}

impl Light {
    /// Every light, in the order the row draws them.
    pub const ALL: [Light; 2] = [Light::Mode, Light::Fast];

    /// The light's word on its chip (`○ fast`); the mode light's chip names
    /// its MODE instead ([`Mode::word`]), this only while none is known.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Light::Mode => "mode",
            Light::Fast => "fast",
        }
    }

    /// The title a hover or the keyboard selection shows.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Light::Mode => "Permission mode",
            Light::Fast => "Fast mode",
        }
    }

    /// The light's colour when it is on — each its own, so the row reads at a
    /// glance without its titles. Claude Code's own inks where it has one
    /// (auto mode's warning amber, fast mode's orange). The mode light draws
    /// in its MODE's own hue where it has one ([`Mode::hue`]); this is its
    /// hue when the mode is not known.
    #[must_use]
    pub fn hue(self) -> [u8; 3] {
        match self {
            Light::Mode => [0xE8, 0xB0, 0x3C],
            Light::Fast => [0xFF, 0x8A, 0x3D],
        }
    }
}

/// What a light shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LightState {
    /// The setting is on.
    On,
    /// The setting is off.
    Off,
    /// Claude's screen does not show it right now (a box covers the composer,
    /// or the vendor drew something this module does not know).
    Unknown,
}

/// Claude Code's permission mode, as its pill names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// `bypass permissions`.
    Bypass,
    /// `auto mode`.
    Auto,
    /// `accept edits`.
    AcceptEdits,
    /// `don't ask`.
    DontAsk,
    /// `plan mode`.
    Plan,
    /// `manual mode` — Claude's default mode.
    Manual,
}

impl Mode {
    /// The mode a pill's indicator (`auto mode`, [`footer::pill_indicator`])
    /// names.
    #[must_use]
    pub fn of_indicator(indicator: &str) -> Option<Mode> {
        Some(match indicator {
            "bypass permissions" => Mode::Bypass,
            "auto mode" => Mode::Auto,
            "accept edits" => Mode::AcceptEdits,
            "don't ask" => Mode::DontAsk,
            "plan mode" => Mode::Plan,
            "manual mode" => Mode::Manual,
            _ => return None,
        })
    }

    /// Whether the owner EXPECTS this mode: bypass (auto-approve) or auto.
    /// Both are positions of the one permission mode, so a session in either
    /// is as expected, and neither is ever pressed away from — the light's
    /// only action runs forward from any OTHER mode to the nearest of them.
    #[must_use]
    pub fn is_expected(self) -> bool {
        matches!(self, Mode::Bypass | Mode::Auto)
    }

    /// The mode's short word, as the light names it: `bypass`, `auto`,
    /// `accept edits`, `don't ask`, `plan`, `manual`.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Mode::Bypass => "bypass",
            Mode::Auto => "auto",
            Mode::AcceptEdits => "accept edits",
            Mode::DontAsk => "don't ask",
            Mode::Plan => "plan",
            Mode::Manual => "manual",
        }
    }

    /// The pill glyph Claude draws before the mode (`footer::PILLS`): `⏸`
    /// for plan and manual, `⏵⏵` for the rest.
    #[must_use]
    pub fn glyph(self) -> &'static str {
        match self {
            Mode::Plan | Mode::Manual => "\u{23F8}",
            _ => "\u{23F5}\u{23F5}",
        }
    }

    /// The mode's own ink where Claude gives it one (bypass's red, auto's
    /// warning amber); `None` draws it in the row's plain ink.
    #[must_use]
    pub fn hue(self) -> Option<[u8; 3]> {
        match self {
            Mode::Bypass => Some([0xE0, 0x5A, 0x5A]),
            Mode::Auto => Some([0xE8, 0xB0, 0x3C]),
            _ => None,
        }
    }

    /// The mode one shift+tab moves to, in Claude's own cycle (2.1.283, the
    /// vendor's `hbt`) with bypass and auto both available: manual → accept
    /// edits → plan → bypass → auto → manual; don't ask only ever leads on to
    /// manual. A session that lacks bypass or auto skips it — the light never
    /// relies on this ring, it reads every answer back; tests and the derived
    /// model do.
    #[must_use]
    pub fn next_in_cycle(self) -> Mode {
        match self {
            Mode::Manual => Mode::AcceptEdits,
            Mode::AcceptEdits => Mode::Plan,
            Mode::Plan => Mode::Bypass,
            Mode::Bypass => Mode::Auto,
            Mode::Auto | Mode::DontAsk => Mode::Manual,
        }
    }
}

/// What one Claude Code screen says about the lights' settings — the one
/// parse the footer and the lights share per frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// The permission mode its pill names — every mode draws one on 2.1.283,
    /// default included (`⏸ manual mode on`, measured 2026-09-25). `None`
    /// when no pill this build knows is under the composer, and then the mode
    /// lights read Unknown rather than a guessed mode.
    pub mode: Option<Mode>,
    /// Claude is mid-turn (`aterm_phase::phase::busy_signal`, hard).
    pub busy: bool,
    /// The composer's top rule says fast mode is on: among the words Claude
    /// Code wrote into it ([`rule_tags`]), `↯` or `fast mode` (the vendor's
    /// narrow-terminal spelling, `fast mode (cooling down)` included).
    pub fast: bool,
}

/// Read `rows` (one pane, top to bottom). `None` without a composer frame.
#[must_use]
pub fn read_screen(rows: &[String]) -> Option<Screen> {
    let (top, bottom) = aterm_phase::phase::composer_rules(rows)?;
    let mode = footer::mode_row_under(rows, bottom)
        .and_then(|r| footer::pill_indicator(&rows[r]))
        .and_then(Mode::of_indicator);
    // A turn in flight, by the phase reader's own rule (the spinner, `Still
    // working`, `esc to interrupt` under the composer, …) — a background
    // monitor alone (`soft`) is not a turn.
    let busy = aterm_phase::phase::busy_signal(rows).is_some_and(|b| !b.soft);
    let mut before = "";
    let fast = rule_tags(&rows[top]).any(|tag| {
        let fast = tag == FAST_ICON || (before == "fast" && tag == "mode");
        before = tag;
        fast
    });
    Some(Screen { mode, busy, fast })
}

/// Whether Claude's composer holds EXACTLY `cmd` with the cursor right behind
/// it: the composer frame is drawn (no box over it), `cmd` is alone on the
/// draft's first line, no other draft line has text, and the terminal cursor
/// sits at the end of the caret row. The one shape where an Enter submits
/// `cmd` and nothing else — and where one ctrl+u (kill to line start) takes
/// back exactly what was typed. A cursor at the caret's start (a placeholder
/// suggestion, a homed caret), a character past the text or a second line
/// all fail it: the composer holds something aterm did not type.
#[must_use]
pub fn composer_holds(rows: &[String], cursor: (usize, usize), cmd: &str) -> bool {
    if aterm_phase::phase::composer_rules(rows).is_none() {
        return false;
    }
    let Some((caret, lines)) = aterm_phase::phase::composer_draft(rows) else {
        return false;
    };
    lines.first().is_some_and(|line| line == cmd)
        && lines.iter().skip(1).all(String::is_empty)
        && cursor == (caret, rows[caret].trim_end().chars().count())
}

/// The words inside a composer rule: `──── <tag> ↯ ─` → `<tag>`, `↯`.
/// A `·` between tags is a separator, not a word.
pub fn rule_tags(rule: &str) -> impl Iterator<Item = &str> {
    rule.split(['\u{2500}', '\u{00B7}'])
        .flat_map(str::split_whitespace)
}

/// What the lights show: the permission mode its pill names, and fast mode
/// as on, off or not known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Reading {
    /// The permission mode, where a pill this build knows is drawn.
    pub mode: Option<Mode>,
    /// Fast mode.
    pub fast: LightState,
}

impl Default for Reading {
    fn default() -> Self {
        Self::UNKNOWN
    }
}

impl Reading {
    /// Nothing known: no screen yet, or a box covering the composer.
    pub const UNKNOWN: Reading = Reading {
        mode: None,
        fast: LightState::Unknown,
    };

    /// `light`'s state: the mode light is On in an expected mode
    /// ([`Mode::is_expected`]) and Off in any other.
    #[must_use]
    pub fn state(&self, light: Light) -> LightState {
        match light {
            Light::Mode => self.mode.map_or(LightState::Unknown, |m| {
                if m.is_expected() {
                    LightState::On
                } else {
                    LightState::Off
                }
            }),
            Light::Fast => self.fast,
        }
    }

    /// Fold a newer SCREEN reading in, keeping what it does not know — so the
    /// row does not flicker grey under every dialog that covers the composer.
    pub fn fold(&mut self, newer: &Reading) {
        if newer.mode.is_some() {
            self.mode = newer.mode;
        }
        if newer.fast != LightState::Unknown {
            self.fast = newer.fast;
        }
    }
}

/// What the owner expects of the lights besides the permission mode, whose
/// expected set is fixed ([`Mode::is_expected`]) — the host's answer per
/// session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Expect {
    /// Fast mode is expected ON (owner direction, 2026-09-24) — unless the
    /// host has latched Claude's own lasting refusal of it for this Claude
    /// build and model ([`FastRefusal::lasting`]), which stops the light
    /// asking for what cannot be had.
    pub fast: bool,
    /// On this Claude build, turning fast mode on took a session out of auto
    /// mode (the host latched Claude's [`AUTO_OFF_FOR_FAST`]): in auto mode
    /// the fast light stops asking, so auto is not traded for fast unseen.
    pub fast_costs_auto: bool,
}

impl Default for Expect {
    fn default() -> Self {
        Self {
            fast: true,
            fast_costs_auto: false,
        }
    }
}

/// Whether `light` shows something the owner does NOT expect — the one
/// reason a light is drawn at rest (owner, 2026-09-27: "I don't want to show
/// the state of parameters that I always expect to be on"). The mode light
/// deviates in any mode but bypass and auto, fast mode while it is off (and
/// still expected — not in auto mode where fast mode is known to cost it). A
/// light this screen does not show never deviates: nothing is drawn on a
/// guess.
#[must_use]
pub fn deviates(light: Light, reading: &Reading, expect: &Expect) -> bool {
    match light {
        Light::Mode => reading.state(Light::Mode) == LightState::Off,
        Light::Fast => {
            expect.fast
                && reading.fast == LightState::Off
                && !(expect.fast_costs_auto && reading.mode == Some(Mode::Auto))
        }
    }
}

/// What `screen` says of each light. Without a screen nothing is known
/// ([`Reading::UNKNOWN`]) — the host keeps showing the last known states
/// while a box covers the composer.
#[must_use]
pub fn read(screen: Option<&Screen>) -> Reading {
    let Some(s) = screen else {
        return Reading::UNKNOWN;
    };
    Reading {
        mode: s.mode,
        fast: if s.fast {
            LightState::On
        } else {
            LightState::Off
        },
    }
}

/// Claude Code's fast-mode icon, `↯` (2.1.284's `mre`): on the composer's
/// top rule while fast mode is on, and at the head of its `Fast mode ON`
/// answers ([`fast_answer_of`]).
const FAST_ICON: &str = "\u{21AF}";

/// How to toggle a light: Claude Code's own inputs, and what must hold before
/// the host may send them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Drive {
    /// Press shift+tab, read the pill back, and press again until the mode
    /// is one the owner expects ([`Mode::is_expected`]) — the NEAREST of
    /// bypass and auto forward in this session's own cycle, which may hold
    /// either or both — giving up once the cycle laps (see
    /// [`MAX_MODE_PRESSES`]). Safe with a draft in the composer: the mode
    /// cycle does not touch it. Never from an expected mode: there is no
    /// gesture of aterm's that leaves one.
    CycleMode,
    /// Submit this slash command. ONLY into an EMPTY composer
    /// (`upgrade::composer_is_empty`): typed over a draft it would be sent
    /// with the draft.
    Command {
        /// The command, with its argument (`/fast on`): a bare `/fast` opens
        /// Claude's picker instead of switching.
        cmd: &'static str,
        /// Claude runs it MID-TURN, with the composer still up — so a turn in
        /// flight neither refuses the click nor holds its Enter. Measured on
        /// 2.1.283: `/fast` is `{type:"local-jsx", name:"fast",
        /// argumentHint:"[on|off]", immediate:!0}`, and the prompt's submit
        /// runs an immediate local-jsx command mid-turn with `hidesPrompt:!1`
        /// (its `on`/`off` argument parsed by the vendor's `PVo`).
        immediate: bool,
    },
}

/// shift+tab, as a terminal sends it.
pub const SHIFT_TAB: &[u8] = b"\x1b[Z";

/// How to switch `light` from what `reading` shows. `None` for an unknown
/// light — nothing is typed into a screen this module cannot read back — and
/// for the mode light in an expected mode (its one action is back to bypass
/// or auto, and it is there).
#[must_use]
pub fn drive(light: Light, reading: &Reading) -> Option<Drive> {
    match light {
        Light::Mode => reading
            .mode
            .filter(|m| !m.is_expected())
            .map(|_| Drive::CycleMode),
        Light::Fast => match reading.fast {
            LightState::On => Some(Drive::Command {
                cmd: "/fast off",
                immediate: true,
            }),
            LightState::Off => Some(Drive::Command {
                cmd: "/fast on",
                immediate: true,
            }),
            LightState::Unknown => None,
        },
    }
}

/// Claude's answer to `/fast`, in its own words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FastAnswer {
    /// `Fast mode ON` (or `Kept Fast mode ON`).
    On,
    /// `Fast mode ON · model set to …`: the model was not fast-capable, and
    /// Claude moved the session to one that is.
    OnModelMoved,
    /// `Fast mode OFF` (or `Kept Fast mode OFF`).
    Off,
    /// Claude refused, and said why.
    Refused(FastRefusal),
}

/// Why Claude refused fast mode, from its own words (the 2.1.283 bundle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FastRefusal {
    /// `Fast mode is not available` (`PVo`'s early return: fast mode is off
    /// in this Claude, `CLAUDE_CODE_DISABLE_FAST_MODE` or a build without it).
    NotAvailable,
    /// `Fast mode is only available when using the Anthropic API directly`.
    ApiOnly,
    /// `… is not in your organization's allowed models`.
    ModelNotAllowed,
    /// `Fast mode is not available in the Agent SDK`.
    NotInSdk,
    /// `Checking fast mode availability` — asked again too soon.
    Checking,
    /// `Fast mode requires a paid subscription`, or `Fast mode unavailable
    /// during evaluation. Please purchase credits.`
    NeedsPaidPlan,
    /// `Fast mode has been disabled by your organization`.
    DisabledByOrg,
    /// `Fast mode requires usage credits` (its ` · …` tail varies).
    NeedsCredits,
    /// `Fast mode unavailable due to network connectivity issues`.
    Network,
    /// `Fast mode is currently unavailable`.
    Unavailable,
    /// `Fast mode unchanged (cancelled)`.
    Cancelled,
    /// Any other refusal Claude wrapped as `Fast mode unavailable: …`, or a
    /// `Fast mode was not …` / `(use /model to switch, then /fast)`.
    Other,
}

impl FastRefusal {
    /// What the fast light's title says of it.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            FastRefusal::NotAvailable => "not available in this Claude",
            FastRefusal::ApiOnly => "Anthropic API only",
            FastRefusal::ModelNotAllowed => "model not allowed by your org",
            FastRefusal::NotInSdk => "not in the Agent SDK",
            FastRefusal::Checking => "Claude is still checking; try again",
            FastRefusal::NeedsPaidPlan => "needs a paid plan",
            FastRefusal::DisabledByOrg => "disabled by your organization",
            FastRefusal::NeedsCredits => "needs usage credits",
            FastRefusal::Network => "network check failed; try again",
            FastRefusal::Unavailable => "currently unavailable",
            FastRefusal::Cancelled => "cancelled",
            FastRefusal::Other => "Claude did not switch (see its message)",
        }
    }

    /// Whether a later try may well succeed (a check still running, the
    /// network, a cancel).
    #[must_use]
    pub fn transient(self) -> bool {
        matches!(
            self,
            FastRefusal::Checking
                | FastRefusal::Network
                | FastRefusal::Unavailable
                | FastRefusal::Cancelled
        )
    }

    /// Whether the refusal is the account's or the build's own answer, which
    /// a retry will not change — the host then stops asking for fast mode on
    /// that Claude build and model (in memory: until aterm restarts). Neither
    /// a transient refusal nor [`FastRefusal::Other`] is: a reason this build
    /// does not know, and Claude's "(use /model to switch, then /fast)"
    /// advice, are no ground to stop asking on a guess. `ModelNotAllowed` is
    /// lasting for its MODEL only, which is why the host keys the latch by
    /// build and model.
    #[must_use]
    pub fn lasting(self) -> bool {
        !self.transient() && self != FastRefusal::Other
    }
}

/// The words Claude Code 2.1.283 says when turning fast mode on took the
/// session out of auto mode (the vendor's `i8`, case `fast-mode`, under the
/// server flag `tengu_auto_mode_config.disableFastMode`): "auto mode
/// unavailable while fast mode is on · run /fast off".
pub const AUTO_OFF_FOR_FAST: &str = "auto mode unavailable while fast mode is on";

/// Whether Claude's screen says auto mode is off because fast mode is on
/// ([`AUTO_OFF_FOR_FAST`], anywhere on the screen: its layout is not
/// measured, its words are the vendor's).
#[must_use]
pub fn says_auto_off_for_fast(rows: &[String]) -> bool {
    rows.iter().any(|row| row.contains(AUTO_OFF_FOR_FAST))
}

/// Claude's refusals of `/fast`, by the words they open with, in match
/// order (the Agent SDK's before the plain "not available").
const FAST_REFUSALS: [(&str, FastRefusal); 11] = [
    (
        "Fast mode is not available in the Agent SDK",
        FastRefusal::NotInSdk,
    ),
    ("Fast mode is not available", FastRefusal::NotAvailable),
    (
        "Fast mode is only available when using the Anthropic API directly",
        FastRefusal::ApiOnly,
    ),
    ("Checking fast mode availability", FastRefusal::Checking),
    (
        "Fast mode requires a paid subscription",
        FastRefusal::NeedsPaidPlan,
    ),
    (
        "Fast mode unavailable during evaluation",
        FastRefusal::NeedsPaidPlan,
    ),
    (
        "Fast mode has been disabled by your organization",
        FastRefusal::DisabledByOrg,
    ),
    (
        "Fast mode requires usage credits",
        FastRefusal::NeedsCredits,
    ),
    (
        "Fast mode unavailable due to network connectivity issues",
        FastRefusal::Network,
    ),
    (
        "Fast mode is currently unavailable",
        FastRefusal::Unavailable,
    ),
    ("Fast mode unchanged (cancelled)", FastRefusal::Cancelled),
];

/// What one line of Claude's says about fast mode, if it is an answer to
/// `/fast`. Claude opens every `Fast mode ON` answer — `/fast on`'s and the
/// picker's, and `Kept Fast mode ON` — with its fast-mode icon `↯` and a
/// space (2.1.284: `${icon} Fast mode ON…`, the icon in its theme's
/// fast-mode colour), so the answer is read the way the vendor's own
/// classifier reads its result: colour sequences stripped, then the icon.
/// The vendor wraps its availability refusals as `Fast mode unavailable:
/// <reason>`; the prefix is taken off first, and a wrapped reason this build
/// does not know is still a refusal ([`FastRefusal::Other`]).
#[must_use]
pub fn fast_answer_of(text: &str) -> Option<FastAnswer> {
    let text = crate::harness::upgrade_models::strip_sgr(text);
    let text = text.trim();
    let text = text.strip_prefix(FAST_ICON).map_or(text, str::trim_start);
    let (text, wrapped) = match text.strip_prefix("Fast mode unavailable: ") {
        Some(rest) => (rest.trim(), true),
        None => (text, false),
    };
    let on = aterm_phase::anchor("fast.on");
    if text.starts_with(on)
        || text
            .strip_prefix("Kept ")
            .is_some_and(|t| t.starts_with(on))
    {
        return Some(if text.contains(aterm_phase::anchor("fast.model_set")) {
            FastAnswer::OnModelMoved
        } else {
            FastAnswer::On
        });
    }
    if text.starts_with("Fast mode OFF") || text.starts_with("Kept Fast mode OFF") {
        return Some(FastAnswer::Off);
    }
    if let Some((_, why)) = FAST_REFUSALS
        .iter()
        .find(|(words, _)| text.starts_with(words))
    {
        return Some(FastAnswer::Refused(*why));
    }
    if text.contains("is not in your organization's allowed models") {
        return Some(FastAnswer::Refused(FastRefusal::ModelNotAllowed));
    }
    if wrapped
        || text.starts_with("Fast mode was not")
        || text.contains("(use /model to switch, then /fast)")
    {
        return Some(FastAnswer::Refused(FastRefusal::Other));
    }
    None
}

/// Claude's answers to one command on one screen ([`fast_answers`]), in the
/// two places it answers — kept apart, because only the transcript's are
/// tied to the command and can be COUNTED.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FastAnswers {
    /// How many echoes of the command in the transcript carry an answer.
    pub echoed: usize,
    /// The last echo's answer.
    pub echo: Option<FastAnswer>,
    /// The notification under the composer, if one answering `/fast` is up
    /// — ONE slot, which a newer notification replaces in place; it names no
    /// command, and a count cannot tell a replacement from the one before.
    pub notice: Option<FastAnswer>,
}

impl FastAnswers {
    /// Claude's newest word on screen: the notification where one is up
    /// (it is drawn after the transcript), else the last echo's answer.
    #[must_use]
    pub fn newest(&self) -> Option<FastAnswer> {
        self.notice.or(self.echo)
    }
}

/// Every answer to `cmd` on this screen. Claude answers a `/fast` in one of
/// two places: at idle, the `⎿` line(s) under the transcript's echo of the
/// command (`❯ /fast on`), above the composer; mid-turn, a notification on
/// the row above the composer's top rule (measured) or under its bottom rule. The ECHO COUNT is what makes a
/// transcript answer this toggle's: the host notes it at the click, and only
/// an echo beyond it — never one already on screen from an earlier try —
/// resolves the toggle. The notification is judged apart (the host notes the
/// one up at the click, and only a notification other than that one, or one
/// that comes after it went, is new). Without a composer on screen (Claude
/// applying fast mode) the whole screen is transcript.
#[must_use]
pub fn fast_answers(rows: &[String], cmd: &str) -> FastAnswers {
    let rules = aterm_phase::phase::composer_rules(rows);
    let (transcript, below) = match rules {
        Some((top, bottom)) => (&rows[..top], &rows[(bottom + 1).min(rows.len())..]),
        None => (rows, &rows[rows.len()..]),
    };
    let mut count = 0;
    let mut newest = None;
    for (i, row) in transcript.iter().enumerate() {
        let echo = row.trim_end();
        if echo.strip_prefix("\u{276F} ").or(echo.strip_prefix("> ")) != Some(cmd) {
            continue;
        }
        // The answer: the `⎿` row under the echo (a blank row may sit
        // between), with its indented continuation rows.
        let mut rest = transcript[i + 1..]
            .iter()
            .skip_while(|r| r.trim().is_empty());
        let Some(first) = rest
            .next()
            .and_then(|r| r.trim_start().strip_prefix('\u{23BF}'))
        else {
            continue;
        };
        let mut text = first.trim().to_owned();
        for more in rest {
            let t = more.trim();
            if t.is_empty() || !more.starts_with("    ") || t.starts_with('\u{23BF}') {
                break;
            }
            text.push(' ');
            text.push_str(t);
        }
        if let Some(answer) = fast_answer_of(&text) {
            count += 1;
            newest = Some(answer);
        }
    }
    // Mid-turn Claude draws its notification on the row directly ABOVE the
    // composer's top rule (measured 2.1.283, 2026-09-27: right-aligned at
    // 144 columns, from column 2 and cut to fit at 80), for about 8 s; under
    // the composer only where the layout differs. Only that one row above
    // the rule, and only its vendor words, count.
    let above = match rules {
        Some((top, _)) if top > 0 => &rows[top - 1..top],
        _ => &rows[..0],
    };
    let heads = ["Kept Fast mode", "Fast mode", "Checking fast mode"];
    let above_notice = above.iter().find_map(|row| {
        let t = row.trim();
        heads
            .iter()
            .any(|head| t.starts_with(head))
            .then(|| fast_answer_of(t))
            .flatten()
    });
    let notice = above_notice.or_else(|| {
        below.iter().find_map(|row| {
            heads
                .iter()
                .filter_map(|head| row.find(head))
                .min()
                .and_then(|at| fast_answer_of(&row[at..]))
        })
    });
    FastAnswers {
        echoed: count,
        echo: newest,
        notice,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(rule: &str, mode_row: &str) -> Vec<String> {
        vec![
            "\u{25CF} done".into(),
            String::new(),
            rule.into(),
            "\u{276F} ".into(),
            "\u{2500}".repeat(60),
            mode_row.into(),
        ]
    }

    const BYPASS: &str = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle) \u{00B7} \u{2190} for agents";

    /// The owner's own screen, measured 2.1.282: bypass, with an effort tag
    /// on the rule (`workspace` stands in for the vendor's word) that no
    /// light reads.
    #[test]
    fn the_lights_read_claudes_own_indicators() {
        let rule = format!("{} workspace \u{2500}", "\u{2500}".repeat(50));
        let rows = screen(&rule, BYPASS);
        let s = read_screen(&rows).expect("a composer");
        assert_eq!(s.mode, Some(Mode::Bypass));
        assert!(!s.busy);
        let r = read(Some(&s));
        assert_eq!(
            Light::ALL.map(|l| r.state(l)),
            [LightState::On, LightState::Off]
        );
        let rule = format!("{} workspace \u{21AF} \u{2500}", "\u{2500}".repeat(50));
        let auto = "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)";
        let s = read_screen(&screen(&rule, auto)).unwrap();
        let r = read(Some(&s));
        assert_eq!(r.mode, Some(Mode::Auto));
        assert_eq!(
            Light::ALL.map(|l| r.state(l)),
            [LightState::On, LightState::On],
            "auto is as expected as bypass: the one mode light is on in both"
        );
    }

    #[test]
    fn fast_mode_spelled_out_is_fast_mode() {
        let rule = format!(
            "{} fast mode (cooling down) \u{2500}",
            "\u{2500}".repeat(40)
        );
        let s = read_screen(&screen(&rule, BYPASS)).unwrap();
        assert_eq!(read(Some(&s)).fast, LightState::On);
    }

    /// Claude's default mode draws its own pill (2.1.283, measured
    /// 2026-09-25): manual, and the mode light is off — as it is in every
    /// mode but bypass and auto.
    #[test]
    fn the_manual_pill_is_the_default_mode() {
        let manual =
            "  \u{23F8} manual mode on \u{00B7} ? for shortcuts \u{00B7} \u{2190} for agents";
        let s = read_screen(&screen(&"\u{2500}".repeat(60), manual)).unwrap();
        assert_eq!(s.mode, Some(Mode::Manual));
        assert_eq!(read(Some(&s)).state(Light::Mode), LightState::Off);
        for mode in [Mode::AcceptEdits, Mode::Plan, Mode::DontAsk, Mode::Manual] {
            assert!(!mode.is_expected(), "{mode:?}");
        }
        assert!(Mode::Bypass.is_expected() && Mode::Auto.is_expected());
    }

    /// A row under the composer that names no pill this build knows is NOT
    /// read as some mode: the mode lights are Unknown, so nothing is pressed
    /// on the strength of a guess (a renamed pill). A hint a narrow pane cut
    /// is no guess: the next test.
    #[test]
    fn a_pill_this_build_does_not_know_is_unknown() {
        for row in ["  ? for shortcuts", "  \u{23F5}\u{23F5} turbo mode on"] {
            let s = read_screen(&screen(&"\u{2500}".repeat(60), row)).unwrap();
            assert_eq!(s.mode, None, "{row:?}");
            assert_eq!(
                read(Some(&s)).state(Light::Mode),
                LightState::Unknown,
                "{row:?}"
            );
        }
    }

    /// A narrow pane cuts the `(shift+tab to cycle)` hint between words
    /// (2.1.284, footer's NARROW PANES): the pill still names the mode, so the
    /// lights read it, and a plan-mode pill is still one the light cycles
    /// away from.
    #[test]
    fn a_narrow_panes_cut_hint_still_names_the_mode() {
        let rule = "\u{2500}".repeat(38);
        let cases = [
            (
                "  \u{23F5}\u{23F5} bypass permissions on (shift+tab",
                Mode::Bypass,
            ),
            ("  \u{23F5}\u{23F5} auto mode on (shift+tab to", Mode::Auto),
            ("  \u{23F8} plan mode on (shift+tab to", Mode::Plan),
        ];
        for (row, mode) in cases {
            let s = read_screen(&screen(&rule, row)).expect("a composer");
            assert_eq!(s.mode, Some(mode), "{row:?}");
            let r = read(Some(&s));
            let cycle = (!mode.is_expected()).then_some(Drive::CycleMode);
            assert_eq!(drive(Light::Mode, &r), cycle, "{row:?}");
        }
    }

    /// Claude mid-turn: `esc to interrupt` under the composer.
    #[test]
    fn a_turn_in_flight_reads_busy() {
        let busy = "  \u{23F5}\u{23F5} auto mode on \u{00B7} esc to interrupt";
        assert!(
            read_screen(&screen(&"\u{2500}".repeat(60), busy))
                .unwrap()
                .busy
        );
    }

    /// A box over the composer: nothing is known, and nothing may be typed.
    #[test]
    fn no_composer_no_lights_and_no_drive() {
        assert_eq!(read_screen(&["$ ls".to_owned()]), None);
        assert_eq!(read(None), Reading::UNKNOWN);
        for light in Light::ALL {
            assert_eq!(drive(light, &Reading::UNKNOWN), None);
        }
    }

    /// The mode light's one action is FORWARD to bypass or auto, and only
    /// from another mode: in either expected mode there is nothing to press —
    /// the owner's complaint of 2026-09-27 was a click that left auto mode.
    #[test]
    fn each_toggle_is_claudes_own_input() {
        let at = |mode: Mode| Reading {
            mode: Some(mode),
            ..Reading::UNKNOWN
        };
        for mode in [Mode::Manual, Mode::AcceptEdits, Mode::Plan, Mode::DontAsk] {
            assert_eq!(
                drive(Light::Mode, &at(mode)),
                Some(Drive::CycleMode),
                "{mode:?}"
            );
        }
        for mode in [Mode::Bypass, Mode::Auto] {
            assert_eq!(
                drive(Light::Mode, &at(mode)),
                None,
                "{mode:?}: never pressed away from"
            );
        }
        let fast_on = Reading {
            fast: LightState::On,
            ..Reading::UNKNOWN
        };
        assert_eq!(
            drive(Light::Fast, &fast_on),
            Some(Drive::Command {
                cmd: "/fast off",
                immediate: true
            })
        );
        let fast = drive(Light::Fast, &Reading::UNKNOWN);
        assert_eq!(fast, None, "an unknown fast light types nothing");
        assert_eq!(settle_ms(&Drive::CycleMode), SETTLE_MS);
        assert_eq!(
            settle_ms(&Drive::Command {
                cmd: "/fast on",
                immediate: true
            }),
            FAST_SETTLE_MS,
            "past the vendor's 8 s org check"
        );
    }

    /// The vendor's ring, bypass and auto both available: every non-expected
    /// mode reaches an expected one within three presses, and no press from
    /// an expected mode is ever needed.
    #[test]
    fn every_mode_is_at_most_three_presses_from_an_expected_one() {
        for start in [Mode::Manual, Mode::AcceptEdits, Mode::Plan, Mode::DontAsk] {
            let mut at = start;
            let mut presses = 0;
            while !at.is_expected() {
                at = at.next_in_cycle();
                presses += 1;
                assert!(presses <= 4, "{start:?}");
            }
            assert_eq!(at, Mode::Bypass, "{start:?}: bypass comes first");
            assert!(presses <= 4 && (start == Mode::DontAsk || presses <= 3));
        }
        assert_eq!(Mode::Bypass.next_in_cycle(), Mode::Auto);
        assert_eq!(Mode::Auto.next_in_cycle(), Mode::Manual);
        assert_eq!(Mode::Plan.glyph(), "\u{23F8}");
        assert_eq!(Mode::Auto.glyph(), "\u{23F5}\u{23F5}");
        assert_eq!(Mode::AcceptEdits.word(), "accept edits");
        assert!(Mode::Bypass.hue().is_some() && Mode::Plan.hue().is_none());
    }

    /// At rest nothing is drawn: bypass or auto, fast on. Each light deviates
    /// on its own — and an unknown one never does.
    #[test]
    fn only_what_the_owner_does_not_expect_deviates() {
        let expect = Expect::default();
        let rest = |mode: Mode| Reading {
            mode: Some(mode),
            fast: LightState::On,
        };
        for mode in [Mode::Bypass, Mode::Auto] {
            for light in Light::ALL {
                assert!(!deviates(light, &rest(mode), &expect), "{mode:?} {light:?}");
            }
        }
        assert!(deviates(Light::Mode, &rest(Mode::Plan), &expect));
        let slow = Reading {
            fast: LightState::Off,
            ..rest(Mode::Auto)
        };
        assert!(deviates(Light::Fast, &slow, &expect));
        assert!(
            !deviates(
                Light::Fast,
                &slow,
                &Expect {
                    fast: false,
                    ..expect
                }
            ),
            "a latched refusal stops asking"
        );
        let costs_auto = Expect {
            fast_costs_auto: true,
            ..expect
        };
        assert!(
            !deviates(Light::Fast, &slow, &costs_auto),
            "where fast mode costs auto mode, auto is not asked to trade"
        );
        assert!(
            deviates(
                Light::Fast,
                &Reading {
                    mode: Some(Mode::Bypass),
                    ..slow
                },
                &costs_auto
            ),
            "control: bypass loses nothing to fast mode"
        );
        for light in Light::ALL {
            assert!(!deviates(light, &Reading::UNKNOWN, &expect), "{light:?}");
        }
    }

    /// EVERY RECORDED CLAUDE CODE SCREEN in `aterm-phase`'s fixtures (measured
    /// sessions and the 2.1.280 box captures): a screen with a composer gives
    /// a definite state for every light it shows, and one with a box up gives
    /// no screen at all — so a vendor change that moves the composer shows up
    /// here as a corpus failure, not as lights that quietly lie.
    #[test]
    fn every_recorded_screen_reads_definitely_or_not_at_all() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-phase/src/fixtures");
        let mut readable = 0;
        for entry in std::fs::read_dir(&dir).expect("the phase fixtures") {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let rows: Vec<String> = text.lines().map(str::to_owned).collect();
            let Some(s) = read_screen(&rows) else {
                continue;
            };
            let got = read(Some(&s));
            // Fast mode's rule tag is always readable under a composer; the mode
            // light is definite exactly when a pill this build knows is there.
            let pill = footer::mode_row(&rows).is_some();
            assert!(
                got.fast != LightState::Unknown
                    && (got.state(Light::Mode) != LightState::Unknown) == pill,
                "{}: {got:?}",
                path.display()
            );
            if rows
                .iter()
                .any(|r| r.contains("\u{23F5}\u{23F5} auto mode on"))
            {
                assert_eq!(s.mode, Some(Mode::Auto), "{}", path.display());
            }
            readable += 1;
        }
        assert!(
            readable >= 10,
            "the corpus must exercise the reader: {readable}"
        );
    }

    /// THE CURRENT VENDOR, LIVE: every 2.1.283 capture (measured 2026-09-25,
    /// `aterm ctl text` of a private headless aterm, the owner's settings)
    /// reads as the state it was captured in — mode and turn in flight, with
    /// the effort tag on the rule (`workspace` stands in for the vendor's
    /// word) read by no light. A vendor build that draws any of these differently fails
    /// HERE, by name, before it can grey a light or press a wrong key.
    /// THE DRIFT CANARY. Every recorded FOOTER screen of every Claude Code
    /// build in the corpus (`claude-<version>-footer-*.txt` in aterm-phase's
    /// fixtures, recorded by `tools/capture-claude-screens.sh`) has a composer
    /// this build's reader sees, a mode row under it, and a pill the footer
    /// AND the lights read as a mode. So a new Claude Code, captured before
    /// sessions run on it, that draws its mode row differently fails here —
    /// by file and build — where the running window's lights could only step
    /// aside and name it in aterm.log. The directory is read, not listed: a new
    /// build's captures are judged the moment they land.
    #[test]
    fn every_captured_footer_screen_of_every_build_is_read() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-phase/src/fixtures");
        let mut builds = std::collections::BTreeSet::new();
        let mut screens = 0;
        for entry in std::fs::read_dir(&dir).expect("the phase fixtures") {
            let path = entry.expect("a fixture").path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_owned();
            let Some(version) = name
                .strip_prefix("claude-")
                .and_then(|rest| rest.split_once("-footer-"))
                .map(|(version, _)| version.to_owned())
            else {
                continue;
            };
            let text = std::fs::read_to_string(&path).expect("a readable fixture");
            let rows: Vec<String> = text
                .lines()
                .skip_while(|line| line.starts_with('#'))
                .map(str::to_owned)
                .collect();
            let screen = read_screen(&rows)
                .unwrap_or_else(|| panic!("{name}: no composer this build's reader sees"));
            let row = footer::mode_row(&rows)
                .unwrap_or_else(|| panic!("{name}: no mode row the lights read"));
            assert!(
                footer::pill_indicator(&rows[row]).is_some(),
                "{name}: the mode pill is not one this build reads: {:?}",
                rows[row]
            );
            assert!(
                screen.mode.is_some(),
                "{name}: the lights read no mode from {:?}",
                rows[row]
            );
            builds.insert(version);
            screens += 1;
        }
        assert!(
            screens >= 8 && builds.contains("2.1.283"),
            "the canary must judge the measured corpus: {screens} screens of {builds:?}"
        );
    }

    #[test]
    fn the_2_1_283_screens_read_as_measured() {
        let cases: [(&str, &str, Mode, bool); 8] = [
            (
                "auto-idle",
                include_str!(
                    "../../../aterm-phase/src/fixtures/claude-2.1.283-footer-auto-idle.txt"
                ),
                Mode::Auto,
                false,
            ),
            (
                "auto-draft",
                include_str!(
                    "../../../aterm-phase/src/fixtures/claude-2.1.283-footer-auto-draft.txt"
                ),
                Mode::Auto,
                false,
            ),
            (
                "manual",
                include_str!("../../../aterm-phase/src/fixtures/claude-2.1.283-footer-manual.txt"),
                Mode::Manual,
                false,
            ),
            (
                "accept-edits",
                include_str!(
                    "../../../aterm-phase/src/fixtures/claude-2.1.283-footer-accept-edits.txt"
                ),
                Mode::AcceptEdits,
                false,
            ),
            (
                "plan",
                include_str!("../../../aterm-phase/src/fixtures/claude-2.1.283-footer-plan.txt"),
                Mode::Plan,
                false,
            ),
            (
                "busy",
                include_str!("../../../aterm-phase/src/fixtures/claude-2.1.283-footer-busy.txt"),
                Mode::Auto,
                true,
            ),
            (
                "after-turn",
                include_str!(
                    "../../../aterm-phase/src/fixtures/claude-2.1.283-footer-after-turn.txt"
                ),
                Mode::Auto,
                false,
            ),
            (
                "bypass-idle",
                include_str!(
                    "../../../aterm-phase/src/fixtures/claude-2.1.283-footer-bypass-idle.txt"
                ),
                Mode::Bypass,
                false,
            ),
        ];
        for (name, text, mode, busy) in cases {
            let rows: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
            let s = read_screen(&rows).unwrap_or_else(|| panic!("{name}: a composer"));
            assert_eq!(s.mode, Some(mode), "{name}");
            assert_eq!(s.busy, busy, "{name}");
            let (top, _) = aterm_phase::phase::composer_rules(&rows)
                .unwrap_or_else(|| panic!("{name}: the composer's rules"));
            assert!(
                rule_tags(&rows[top]).any(|t| t == "workspace"),
                "{name}: the effort tag is read off the rule"
            );
            assert_eq!(
                read(Some(&s)).fast,
                LightState::Off,
                "{name}: an effort tag is not fast mode"
            );
            let rule = aterm_phase::phase::composer_bottom(&rows)
                .unwrap_or_else(|| panic!("{name}: the composer's bottom rule"));
            assert!(
                footer::mode_row(&rows).is_some_and(|r| r > rule),
                "{name}: the mode row is read under the rule the footer writes into"
            );
        }
    }

    /// Only the exact shape: the command alone, the cursor right behind it.
    #[test]
    fn the_composer_holds_the_command_only_exactly() {
        let rule = "\u{2500}".repeat(40);
        let screen = |caret: &str, second: &str| -> Vec<String> {
            let mut rows = vec![rule.clone(), format!("\u{276F} {caret}")];
            if !second.is_empty() {
                rows.push(format!("  {second}"));
            }
            rows.push(rule.clone());
            rows.push("  \u{23F5}\u{23F5} auto mode on".into());
            rows
        };
        let cmd = "/fast on";
        assert!(composer_holds(&screen(cmd, ""), (1, 10), cmd));
        for (rows, cursor, why) in [
            (
                screen("/fast onh", ""),
                (1, 11),
                "a character typed after it",
            ),
            (screen(cmd, "hello"), (1, 10), "a second line"),
            (screen(cmd, ""), (1, 2), "a placeholder or a homed caret"),
            (screen(cmd, ""), (1, 11), "a space typed after it"),
            (screen("/fast", ""), (1, 7), "only part of it"),
        ] {
            assert!(!composer_holds(&rows, cursor, cmd), "{why}");
        }
        assert!(
            !composer_holds(&[rule.clone(), format!("\u{276F} {cmd}")], (1, 10), cmd),
            "no frame: a box over the composer"
        );
    }

    /// Claude's answers to `/fast`, word for word — the strings of the
    /// 2.1.283 bundle — each read as what it means; text that is not an
    /// answer reads as none.
    #[test]
    fn claudes_fast_answers_read_in_its_own_words() {
        use FastAnswer::{Off, On, OnModelMoved, Refused};
        use FastRefusal as R;
        for (text, want) in [
            ("Fast mode ON", On),
            ("Kept Fast mode ON", On),
            ("Fast mode ON \u{00B7} model set to Opus 5.5", OnModelMoved),
            ("Fast mode OFF", Off),
            ("Kept Fast mode OFF", Off),
            ("Fast mode is not available", Refused(R::NotAvailable)),
            (
                "Fast mode is only available when using the Anthropic API directly",
                Refused(R::ApiOnly),
            ),
            (
                "Fast mode unavailable: claude-opus-5-5 is not in your organization's allowed models",
                Refused(R::ModelNotAllowed),
            ),
            (
                "Fast mode is not available in the Agent SDK",
                Refused(R::NotInSdk),
            ),
            (
                "Checking fast mode availability\u{2026}",
                Refused(R::Checking),
            ),
            (
                "Fast mode unavailable: Fast mode requires a paid subscription",
                Refused(R::NeedsPaidPlan),
            ),
            (
                "Fast mode unavailable during evaluation. Please purchase credits.",
                Refused(R::NeedsPaidPlan),
            ),
            (
                "Fast mode has been disabled by your organization",
                Refused(R::DisabledByOrg),
            ),
            (
                "Fast mode requires usage credits \u{00B7} /usage-credits to add",
                Refused(R::NeedsCredits),
            ),
            (
                "Fast mode unavailable due to network connectivity issues",
                Refused(R::Network),
            ),
            (
                "Fast mode is currently unavailable",
                Refused(R::Unavailable),
            ),
            ("Fast mode unchanged (cancelled)", Refused(R::Cancelled)),
            ("Fast mode unavailable: something new", Refused(R::Other)),
            (
                "Fast mode was not enabled (use /model to switch, then /fast)",
                Refused(R::Other),
            ),
        ] {
            assert_eq!(fast_answer_of(text), Some(want), "{text:?}");
        }
        for not in [
            "fast mode (cooling down)",
            "Turning fast mode on\u{2026}",
            "done",
        ] {
            assert_eq!(fast_answer_of(not), None, "{not:?}");
        }
        assert!(R::Checking.transient() && R::Network.transient());
        assert!(!R::NotAvailable.transient() && !R::DisabledByOrg.transient());
        // Only the account's or the build's own answer is lasting: never a
        // retry-later, and never a reason this build does not know.
        assert!(R::NotAvailable.lasting() && R::ModelNotAllowed.lasting());
        assert!(!R::Checking.lasting() && !R::Other.lasting());
    }

    /// Claude's own words when fast mode took auto mode off, found wherever
    /// they are drawn. Control: auto mode's other reasons are not it.
    #[test]
    fn fast_mode_taking_auto_off_is_read_in_claudes_words() {
        let rows = |line: &str| vec!["\u{276F} ".to_owned(), line.to_owned()];
        assert!(says_auto_off_for_fast(&rows(
            "  auto mode unavailable while fast mode is on \u{00B7} run /fast off"
        )));
        for other in [
            "  auto mode unavailable for this model",
            "  auto mode disabled by settings",
            "  \u{23F5}\u{23F5} auto mode on",
        ] {
            assert!(!says_auto_off_for_fast(&rows(other)), "{other:?}");
        }
    }

    /// MEASURED mid-turn (2026-09-27, 80 columns): the refusal is no echo but
    /// a notification on the row directly ABOVE the composer's top rule, for
    /// about 8 s, cut to fit — read as a refusal. CONTROLS: the
    /// 144-column capture, read after the notice had gone, has none; a
    /// notice-worded row two rows above the rule (worker prose) is none.
    #[test]
    fn the_measured_mid_turn_fast_refusal_is_the_row_above_the_rule() {
        use aterm_phase::prompt::fixtures::{
            FAST_REFUSED_BUSY_80_MEASURED, FAST_REFUSED_BUSY_MEASURED, screen,
        };
        let rows = screen(FAST_REFUSED_BUSY_80_MEASURED);
        let got = fast_answers(&rows, "/fast on");
        assert_eq!(
            (got.echoed, got.echo, got.notice),
            // Cut to fit at 80 columns (`… connectivity is…`): the vendor's
            // reason no longer reads, so it is the generic refusal.
            (0, None, Some(FastAnswer::Refused(FastRefusal::Other)))
        );
        let gone = screen(FAST_REFUSED_BUSY_MEASURED);
        assert_eq!(fast_answers(&gone, "/fast on"), FastAnswers::default());
        let (top, _) = aterm_phase::phase::composer_rules(&rows).expect("a composer");
        let mut prose = rows.clone();
        prose.swap(top - 1, top - 2);
        assert_eq!(fast_answers(&prose, "/fast on"), FastAnswers::default());
    }

    /// MEASURED at 144 columns (2026-09-28, 2.1.284): mid-turn the refusal is
    /// on the row BELOW the mode row — under the composer, right-aligned —
    /// where the 80-column capture had it above the top rule. Both layouts
    /// read, and this one in full: the network refusal, its reason intact.
    /// CONTROL: no echo, so no transcript answer.
    #[test]
    fn the_measured_wide_mid_turn_fast_refusal_is_read_under_the_composer() {
        use aterm_phase::prompt::fixtures::{FAST_REFUSED_BUSY_UNDER_COMPOSER_MEASURED, screen};
        let rows = screen(FAST_REFUSED_BUSY_UNDER_COMPOSER_MEASURED);
        let got = fast_answers(&rows, "/fast on");
        assert_eq!(
            (got.echoed, got.echo, got.notice),
            (0, None, Some(FastAnswer::Refused(FastRefusal::Network)))
        );
        let s = read_screen(&rows).expect("a composer");
        assert!(s.busy, "{s:?}");
    }

    /// MEASURED (2026-09-27, a scratch Claude config whose base URL cannot
    /// resolve, so `/fast on` is refused and nothing is written): at idle the
    /// answer is the `⎿` row right under the echo — no blank row between —
    /// read as the network refusal through the vendor's doubled prefix, and
    /// at 80 columns its wrapped tail (`     issues`) is joined. A transient
    /// refusal: the light may ask again. The plan-mode screen of the same
    /// session reads plan, idle, fast off, with no tag on the rule.
    #[test]
    fn the_measured_idle_fast_refusal_is_read_under_its_echo() {
        use aterm_phase::prompt::fixtures::{
            FAST_REFUSED_IDLE_80_MEASURED, FAST_REFUSED_IDLE_MEASURED,
            FOOTER_PLAN_API_KEY_MEASURED, screen,
        };
        let network = Some(FastAnswer::Refused(FastRefusal::Network));
        for (name, text) in [
            ("144", FAST_REFUSED_IDLE_MEASURED),
            ("80", FAST_REFUSED_IDLE_80_MEASURED),
        ] {
            let rows = screen(text);
            let got = fast_answers(&rows, "/fast on");
            assert_eq!(
                (got.echoed, got.echo, got.notice),
                (1, network, None),
                "{name}"
            );
            assert!(!FastRefusal::Network.lasting(), "{name}");
            let s = read_screen(&rows).unwrap_or_else(|| panic!("{name}: a composer"));
            assert_eq!((s.mode, s.busy), (Some(Mode::Auto), false), "{name}");
            // NEGATIVE CONTROL: another command's echo carries no answer.
            assert_eq!(fast_answers(&rows, "/fast off"), FastAnswers::default());
        }
        let rows = screen(FOOTER_PLAN_API_KEY_MEASURED);
        let s = read_screen(&rows).expect("a composer");
        assert_eq!((s.mode, s.busy), (Some(Mode::Plan), false));
        let (top, _) = aterm_phase::phase::composer_rules(&rows).expect("the composer's rules");
        assert!(rule_tags(&rows[top]).next().is_none(), "{:?}", rows[top]);
        assert_eq!(read(Some(&s)).fast, LightState::Off);
    }

    /// SYNTHETIC SCREENS — assembled from the 2.1.283 vendor strings on the
    /// measured footer captures' geometry, NOT captured live (a live `/fast
    /// on` writes `fastMode` into the owner's shared settings). At idle the
    /// answer is the `⎿` row under the transcript's echo; mid-turn it is a
    /// notification under the composer. Only answers are counted, so an
    /// earlier try's answer on screen is told apart from a new one by count.
    #[test]
    fn the_answer_is_found_under_its_echo_or_under_the_composer() {
        let rule = "\u{2500}".repeat(60);
        let composer = |extra: &[&str]| -> Vec<String> {
            let mut rows: Vec<String> = extra.iter().map(|r| (*r).to_owned()).collect();
            rows.extend([
                rule.clone(),
                "\u{276F} ".to_owned(),
                rule.clone(),
                "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)".to_owned(),
            ]);
            rows
        };
        let cmd = "/fast on";
        let answers = |echoed, echo, notice| FastAnswers {
            echoed,
            echo,
            notice,
        };
        assert_eq!(fast_answers(&composer(&[]), cmd), FastAnswers::default());
        // Idle: the echo, a blank, the answer.
        let idle = composer(&[
            "\u{276F} /fast on",
            "  \u{23BF}  Fast mode is not available",
            "",
        ]);
        assert_eq!(
            fast_answers(&idle, cmd),
            answers(
                1,
                Some(FastAnswer::Refused(FastRefusal::NotAvailable)),
                None
            )
        );
        // A wrapped refusal, continued on the next row.
        let wrapped = composer(&[
            "\u{276F} /fast on",
            "",
            "  \u{23BF}  Fast mode unavailable: claude-opus-5-5 is not in your",
            "     organization's allowed models",
        ]);
        assert_eq!(
            fast_answers(&wrapped, cmd).echo,
            Some(FastAnswer::Refused(FastRefusal::ModelNotAllowed))
        );
        // A second try under the first: two answers, the newest last.
        let twice = composer(&[
            "\u{276F} /fast on",
            "  \u{23BF}  Checking fast mode availability\u{2026}",
            "\u{276F} /fast on",
            "  \u{23BF}  Fast mode ON",
        ]);
        assert_eq!(
            fast_answers(&twice, cmd),
            answers(2, Some(FastAnswer::On), None)
        );
        // Another command's echo is not this one's.
        let other = composer(&["\u{276F} /fast off", "  \u{23BF}  Fast mode OFF"]);
        assert_eq!(fast_answers(&other, cmd), FastAnswers::default());
        // Mid-turn: the notification under the composer, kept APART from
        // the echoes — it is one slot, not counted.
        let mut busy = composer(&[]);
        busy.push("                                             Fast mode ON".to_owned());
        assert_eq!(
            fast_answers(&busy, cmd),
            answers(0, None, Some(FastAnswer::On))
        );
        // Both up: the notification is the newest word.
        let mut both = twice.clone();
        both.push("        Checking fast mode availability\u{2026}".to_owned());
        let got = fast_answers(&both, cmd);
        assert_eq!(got.echoed, 2);
        assert_eq!(
            got.newest(),
            Some(FastAnswer::Refused(FastRefusal::Checking))
        );
        // Applying: no composer, the progress line is not an answer.
        let applying = vec![
            "\u{276F} /fast on".to_owned(),
            String::new(),
            "  Turning fast mode on\u{2026}".to_owned(),
        ];
        assert_eq!(fast_answers(&applying, cmd), FastAnswers::default());
    }

    /// Claude opens every `Fast mode ON` answer with its fast-mode icon
    /// (2.1.284: `${icon} Fast mode ON${model set to …} · <price>…` for
    /// `/fast on`, `${icon} Kept Fast mode ON` for the picker), so the idle
    /// ECHO's `⎿` row reads `↯ Fast mode ON · …`, and the transcript row
    /// carries the icon in its colour. Both are read as the vendor reads its
    /// own result: colour stripped, then the icon. NEGATIVE CONTROL: the
    /// row's text does not open with the phrase, which the reader before
    /// this took as no answer at all.
    #[test]
    fn a_fast_answer_led_by_claudes_icon_is_read() {
        use FastAnswer::{On, OnModelMoved};
        for (text, want) in [
            (
                "\u{21AF} Fast mode ON \u{00B7} model set to Opus 5.5 \u{00B7} $30/$150 per Mtok",
                OnModelMoved,
            ),
            ("\u{21AF} Fast mode ON \u{00B7} $30/$150 per Mtok", On),
            ("\u{21AF} Kept Fast mode ON", On),
            (
                "\u{1b}[38;2;255;106;0m\u{21AF}\u{1b}[39m Fast mode ON \u{00B7} $30/$150 per Mtok",
                On,
            ),
        ] {
            assert_eq!(fast_answer_of(text), Some(want), "{text:?}");
            assert!(
                !text.starts_with(aterm_phase::anchor("fast.on")),
                "the control: the phrase is not first: {text:?}"
            );
        }
        let rule = "\u{2500}".repeat(60);
        let rows: Vec<String> = [
            "\u{276F} /fast on",
            "  \u{23BF}  \u{21AF} Fast mode ON \u{00B7} model set to Opus 5.5 \u{00B7} $30/$150 per",
            "     Mtok",
            "",
            &rule,
            "\u{276F} ",
            &rule,
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        assert_eq!(
            fast_answers(&rows, "/fast on"),
            FastAnswers {
                echoed: 1,
                echo: Some(OnModelMoved),
                notice: None,
            },
            "the idle echo's answer, icon first"
        );
        // Only the icon is taken off: another glyph before the phrase is
        // still not an answer.
        assert_eq!(fast_answer_of("\u{2605} Fast mode ON"), None);
    }

    #[test]
    fn a_rule_holds_words_not_dashes() {
        assert_eq!(
            rule_tags("\u{2500}\u{2500}\u{2500} workspace \u{00B7} \u{21AF} \u{2500}")
                .collect::<Vec<_>>(),
            ["workspace", "\u{21AF}"]
        );
        assert!(rule_tags(&"\u{2500}".repeat(20)).next().is_none());
    }
}
