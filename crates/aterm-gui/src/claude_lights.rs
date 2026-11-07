// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code footer rule chips (permission mode light, [`Chip::Autopilot`], and [`Chip::Team`]).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::{Act, TaskFace};
use aterm_agent::harness::footer::{FactKind, FactMask};
use aterm_agent::harness::lights::{self, Goal, Light, LightState, Mode, ModeInk, Reading, Screen};
use aterm_agent::supervise::Autopilot;
use aterm_core::terminal::{RenderCell, UnderlineStyle};
use aterm_effects::glass_inks::{GlassInks, field};
use aterm_window::event::KeyEvent;
use aterm_window::keyboard::{Key, ModifiersState, NamedKey};

use crate::autopilot::{AutopilotView, ChipTone, PENDING_GLYPH};
use crate::{App, WindowId};

/// `chrome`'s `autopilot` lines.
#[path = "claude_lights_drawn.rs"]
mod drawn;

/// How long a toggle that did not take keeps saying so.
const REFUSAL_SHOWN: Duration = Duration::from_secs(4);

/// One chip of aterm's span in the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Chip {
    /// A light of Claude Code's (the permission mode).
    Light(Light),
    /// The session's Autopilot ([`AutopilotView`]).
    Autopilot,
    /// Who drives ([`TeamFace`]).
    Team,
}

impl Chip {
    /// Every chip in the row's order, left to right.
    fn row() -> impl Iterator<Item = Chip> {
        std::iter::once(Chip::Autopilot)
            .chain(Light::ALL.into_iter().map(Chip::Light))
            .chain(std::iter::once(Chip::Team))
    }
}

/// What the Team chip says.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum TeamFace {
    /// `✋ you`: the person has this tab.
    Yours,
    /// `⇠ lab-sweep`: a worker.
    Worker {
        /// The manager's local id, where it is here.
        manager: Option<u64>,
        /// The manager's name (`crate::team::Manager::label`).
        label: std::sync::Arc<str>,
        /// The worker's task state word and when it entered it.
        task: Option<(&'static str, Instant)>,
    },
    /// `⇉3`: a manager.
    Manager {
        /// `⇉<n>` (`crate::team::Team::workers_mark`).
        mark: std::sync::Arc<str>,
        /// How many workers name it.
        workers: u16,
    },
}

/// What a press of the Team chip does ([`TeamFace::act`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TeamAct {
    /// Go to the manager's tab and pane (its local id).
    GoTo(u64),
    /// Open the Overview at this manager, its workers listed under it.
    Overview,
    /// Hand the person's tab back.
    HandBack,
}

impl TeamFace {
    /// What a press does, `None` where it does nothing.
    const fn act(&self) -> Option<TeamAct> {
        match self {
            Self::Yours => Some(TeamAct::HandBack),
            Self::Worker {
                manager: Some(manager),
                ..
            } => Some(TeamAct::GoTo(*manager)),
            Self::Manager { .. } => Some(TeamAct::Overview),
            Self::Worker { manager: None, .. } => None,
        }
    }
}

impl From<Light> for Chip {
    fn from(light: Light) -> Self {
        Chip::Light(light)
    }
}

/// Where a chip block stands on its pane's rule, and the glass it wears there
/// (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §4.1): the pane column its first cell
/// takes and the pane's width, so each chip wears the letter of its own column
/// ([`GlassInks::letter`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChipAt {
    pub(crate) glass: GlassInks,
    pub(crate) col: usize,
    pub(crate) cols: usize,
}

impl ChipAt {
    /// A block whose width alone is read: no palette, at a pane's first column.
    fn bare(blank: RenderCell) -> Self {
        let pack = |c: [u8; 3]| u32::from_be_bytes([0, c[0], c[1], c[2]]);
        Self {
            glass: GlassInks::of(None, pack(blank.bg), pack(blank.fg), pack(blank.fg)),
            col: 0,
            cols: 1,
        }
    }

    /// The letter a chip `offset` cells into the block wears.
    fn letter(self, offset: usize) -> [u8; 3] {
        let c = self.glass.letter(field(self.col + offset, self.cols));
        [(c >> 16) as u8, (c >> 8) as u8, c as u8]
    }
}

#[cfg(test)]
impl ChipAt {
    /// The pane width a test's block is laid across.
    pub(crate) const TEST_COLS: usize = 80;

    /// A block under `palette` on `blank`'s row, laid where a footer's chips stand on an
    /// 80-column pane (a blank after the rule's lead).
    pub(crate) fn test(
        palette: Option<aterm_effects::harness_card::CardPalette>,
        blank: RenderCell,
    ) -> Self {
        let pack = |c: [u8; 3]| u32::from_be_bytes([0, c[0], c[1], c[2]]);
        Self {
            glass: GlassInks::of(palette, pack(blank.bg), pack(blank.fg), 0x0088_8888),
            col: aterm_agent::harness::footer::RULE_LEAD + 1,
            cols: Self::TEST_COLS,
        }
    }

    /// [`Self::test`] under Cruise.
    pub(crate) fn cruise(blank: RenderCell) -> Self {
        Self::test(
            Some(aterm_effects::harness_card::CardPalette::Cruise),
            blank,
        )
    }

    /// [`Self::test`] under Land.
    pub(crate) fn land(blank: RenderCell) -> Self {
        Self::test(Some(aterm_effects::harness_card::CardPalette::Land), blank)
    }

    /// The letter a chip `offset` cells into the block wears.
    pub(crate) fn letter_at(self, offset: usize) -> [u8; 3] {
        self.letter(offset)
    }

    /// The quiet ink: the off chips' and the quiet marks' ([`GlassInks::quiet`]).
    pub(crate) fn quiet(self) -> [u8; 3] {
        Inks::of(RenderCell::default(), self).quiet
    }

    /// The value ink: a title's and an expected mode's ([`GlassInks::value`]).
    pub(crate) fn value(self) -> [u8; 3] {
        Inks::of(RenderCell::default(), self).title
    }
}

/// The inks a block of chips is drawn in: the glass's tokens on its row
/// (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §4.1), the letter the chip being laid's
/// column wears ([`Inks::at`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Inks {
    /// The chip being laid's letter ([`GlassInks::letter`]).
    letter: [u8; 3],
    /// The row's own ink ([`GlassInks::lit`]).
    ink: [u8; 3],
    /// The quiet words and marks: the off chips, a worker's, a manager's, an unknown mode
    /// ([`GlassInks::quiet`]).
    quiet: [u8; 3],
    /// A chip's title and an expected mode's word ([`GlassInks::value`]).
    title: [u8; 3],
    ground: [u8; 3],
    at: ChipAt,
}

impl Inks {
    /// The inks of a row of `blank`'s, its block laid `at`.
    fn of(blank: RenderCell, at: ChipAt) -> Self {
        let rgb = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
        Self {
            letter: at.letter(0),
            ink: rgb(at.glass.lit()),
            quiet: rgb(at.glass.quiet()),
            title: rgb(at.glass.value()),
            ground: blank.bg,
            at,
        }
    }

    /// These inks for a chip `offset` cells into the block.
    fn at(self, offset: usize) -> Self {
        Self {
            letter: self.at.letter(offset),
            ..self
        }
    }

    /// A harness tone's ink.
    const fn of_tone(self, tone: ChipTone) -> [u8; 3] {
        match tone {
            ChipTone::Letter => self.letter,
            ChipTone::Quiet => self.quiet,
        }
    }
}

pub(crate) use crate::harness_glass::Stamp;

/// A local clock time, `HH:MM` in 24-hour time, as the tab's wait writes it (`→ 14:05`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Clock([u8; 5]);

impl Clock {
    /// The clock at `minute` of the day (`0..1440`; a larger one wraps).
    #[must_use]
    pub(crate) const fn of_minute(minute: u16) -> Self {
        /// The ASCII digit of `n`'s ones.
        const fn digit(n: u16) -> u8 {
            b'0' + (n % 10) as u8
        }
        let m = minute % 1440;
        let (h, mm) = (m / 60, m % 60);
        Self([digit(h / 10), digit(h), b':', digit(mm / 10), digit(mm)])
    }

    /// The minute of the day it names (`0..1440`).
    #[must_use]
    pub(crate) const fn minute(self) -> u16 {
        /// The value of ASCII digit `b`.
        const fn value(b: u8) -> u16 {
            (b.wrapping_sub(b'0') % 10) as u16
        }
        let [h1, h2, _, m1, m2] = self.0;
        ((value(h1) * 10 + value(h2)) * 60 + value(m1) * 10 + value(m2)) % 1440
    }

    /// `HH:MM`.
    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("--:--")
    }
}

/// A day of the week, named where a timer lands on another local date than the one it was set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

#[cfg_attr(
    not(any(test, a11y_tree)),
    allow(
        dead_code,
        reason = "the whole name is the accessibility tree's, built only under the `a11y_tree` \
                  cfg (Linux, macOS and Windows)"
    )
)]
impl Weekday {
    /// Every day, Monday first.
    pub(crate) const ALL: [Weekday; 7] = [
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
        Weekday::Sat,
        Weekday::Sun,
    ];

    /// The weekday of the local date `days` days after 1970-01-01, a Thursday.
    #[must_use]
    pub(crate) const fn of_days(days: i64) -> Self {
        Self::ALL[(days + 3).rem_euclid(7) as usize]
    }

    /// Its short name, as the title and the page write it (`Sat 16:00`).
    #[must_use]
    pub(crate) const fn short(self) -> &'static str {
        match self {
            Weekday::Mon => "Mon",
            Weekday::Tue => "Tue",
            Weekday::Wed => "Wed",
            Weekday::Thu => "Thu",
            Weekday::Fri => "Fri",
            Weekday::Sat => "Sat",
            Weekday::Sun => "Sun",
        }
    }

    /// Its whole name, as a screen reader says it.
    #[must_use]
    pub(crate) const fn long(self) -> &'static str {
        match self {
            Weekday::Mon => "Monday",
            Weekday::Tue => "Tuesday",
            Weekday::Wed => "Wednesday",
            Weekday::Thu => "Thursday",
            Weekday::Fri => "Friday",
            Weekday::Sat => "Saturday",
            Weekday::Sun => "Sunday",
        }
    }
}

/// An armed timer as the chip shows it.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Autopilot's timer (design §9.6.2) indexes it; until then nothing is armed"
    )
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct LandAtGlass {
    /// When it lands, local `HH:MM`.
    pub(crate) clock: Clock,
    /// The day it lands on, where that is not the day it was set.
    pub(crate) other_day: Option<Weekday>,
}

/// The harness's glass for one autopilot chip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct ChipGlass {
    /// The act lit for its dwell, drawn right of the chips in the chip's ink ([`act_label`]).
    pub(crate) lit: Option<Act>,
    /// The task's face.
    pub(crate) task: Option<TaskFace>,
    /// The local clock time and day the task's face moved there ([`Stamp`]), for the titles.
    pub(crate) task_at: Option<Stamp>,
    /// The armed timer: a timed Cruise reads `∞ ⤓18:00`.
    pub(crate) land_at: Option<LandAtGlass>,
    /// The trail's newest act and the clock time and day it arrived.
    pub(crate) last: Option<(Act, Stamp)>,
    /// Who did [`Self::last`] where it was a proven self-claim's act
    /// ([`crate::harness_glass::TrailEntry::by`]); `None` for aterm's own.
    pub(crate) last_by: Option<crate::harness_glass::Claimant>,
    /// The trail's generation.
    pub(crate) trail: u8,
    /// The loop's pause after short turns while one stands.
    pub(crate) pause: Option<crate::harness_glass::PauseGlass>,
}

/// Which face the autopilot chip wears.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ChipFace {
    /// `∞`: Cruise (`∞ cruise` for a moment after a click reaches it, [`ChipWord`]).
    Cruise,
    /// `∞ ⤓18:00`: Cruise until a time, then Land.
    CruiseUntil,
    /// `⤓`: Land, its task open or unknown (`⤓ land` for a moment after a click reaches it).
    Land,
    /// `✓ landed`: under Land, a clean `Nothing left:` defense, `LANDED` answered to the land
    /// check (or codex-alab's own check), and no repository check found work left to land; its
    /// hovered title says what aterm checked there in brief
    /// ([`aterm_agent::supervise::mode_words::landed_brief`]) and its description whole, with
    /// whether the land check was answered
    /// ([`aterm_agent::supervise::mode_words::landed_words`], §14.9 slice 9c).
    Landed,
    /// `⤓ quiet`: a person's own exchange.
    Theirs,
    /// `✕ failed`: under Land, the worker's task ended unfinished by the loop's own conclusion (its
    /// `Could not finish:` reply, or the same reply again with no work): nothing is being wrapped
    /// up, and the harness types nothing until someone else's turn.
    Failed,
    /// `○`: Off (`○ off` for a moment after a click reaches it).
    Off,
    /// `⊘ off`: the master switch is off.
    MasterOff,
    /// `⊘ off`, titled with the switch.
    AgentOff(aterm_phase::Program),
    /// `⊘ off`: no host.
    Unhosted,
}

/// How a block gives way in a rule short of room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keep {
    /// Every chip, the keyboard's reveal of the lights at rest included.
    Revealed,
    /// Every chip drawn without the reveal.
    Drawn,
    /// The light given up as well.
    NoLight,
    /// Team given up as well (who drives is on the tab's hover and its strip marks too).
    AutopilotOnly,
    /// Autopilot's glyph alone.
    GlyphOnly,
}

impl Keep {
    /// Every rung, in the order a rule short of room tries them.
    const LADDER: [Keep; 5] = [
        Keep::Revealed,
        Keep::Drawn,
        Keep::NoLight,
        Keep::AutopilotOnly,
        Keep::GlyphOnly,
    ];

    /// Whether this rung keeps `chip`, where it is drawn at all.
    const fn keeps(self, chip: Chip) -> bool {
        match chip {
            Chip::Autopilot => true,
            Chip::Light(_) => matches!(self, Keep::Revealed | Keep::Drawn),
            Chip::Team => !matches!(self, Keep::AutopilotOnly | Keep::GlyphOnly),
        }
    }
}

/// Why a toggle did not happen, as its light's title says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Refusal {
    /// The light is not on Claude's screen right now.
    NotShown,
    /// A toggle stopped short of its goal in this mode, which the light names.
    StoppedIn(Mode),
    /// A toggle stopped in this mode because Claude stayed mid-turn past the press deadline.
    StoppedMidTurn(Mode),
    /// The mode cycle came back round without the toggle's goal; it is in this mode.
    NotInCycle(Goal, Mode),
    /// The input seam did not accept the click's press.
    InputRejected,
    /// A footer fact's slash press is typing into the session (`crate::claude_slash`).
    Typing,
}

impl Refusal {
    /// Whether it names the mode a toggle stopped in.
    #[cfg(all(test, unix))]
    fn names_mode(self) -> bool {
        matches!(
            self,
            Refusal::StoppedIn(_) | Refusal::StoppedMidTurn(_) | Refusal::NotInCycle(..)
        )
    }

    /// What the light's title says of it: the reason, which a short title keeps whole.
    fn says(self, rewritten: bool) -> String {
        let named = |mode: Mode| mode.named_on_glass(rewritten);
        match self {
            Refusal::NotShown => "not on screen right now",
            Refusal::StoppedIn(mode) => return format!("stopped in {}", named(mode)),
            Refusal::StoppedMidTurn(mode) => {
                return format!("stopped in {} mid-turn", named(mode));
            }
            Refusal::NotInCycle(goal, mode) => {
                let goal = match goal {
                    Goal::On => goal.words(rewritten),
                    Goal::Off => named(Mode::Manual),
                };
                return format!("no {goal} in this cycle; in {}", named(mode));
            }
            Refusal::InputRejected => "input was not accepted; try again",
            Refusal::Typing => "aterm is already typing here",
        }
        .to_owned()
    }

    /// Where a click goes from the stop, which the whole title says after the reason: a stop
    /// short of the goal can be clicked on from the mode it names, a lap only laps again.
    const fn click(self) -> Option<Goal> {
        match self {
            Refusal::StoppedIn(mode) | Refusal::StoppedMidTurn(mode) => Some(Goal::of_mode(mode)),
            Refusal::NotShown
            | Refusal::NotInCycle(..)
            | Refusal::InputRejected
            | Refusal::Typing => None,
        }
    }
}

/// A toggle in flight: what was sent, and what the next frames must see.
#[derive(Debug, Clone)]
struct Pending {
    light: Light,
    /// Where the toggle goes: the state the light did not show when it was clicked.
    goal: Goal,
    /// The mode when the toggle began (a cycle that leaves it is done).
    started: Mode,
    /// The mode when the last shift+tab was sent.
    mode_at_press: Mode,
    /// Every mode the cycle has shown since it began.
    seen: Vec<Mode>,
    presses: u8,
    /// Past this, the toggle is given up.
    deadline: Instant,
    /// The loop's next look at the engine while one is armed.
    next_poll: Option<Instant>,
    /// Claude answered the last press but was mid-turn.
    held_mid_turn: bool,
}

/// How often a toggle in flight reads the engine for Claude's answer.
const MODE_POLL: Duration = Duration::from_millis(80);

/// Whether a press at `now` belongs to the gesture whose last press was at `*last`: within a
/// double click's interval of it ([`crate::multi_click_ms`]), its last instant included. A press
/// that belongs to it becomes its last, so a run of presses each within the interval of the one
/// before is one gesture however long the run, as the OS counts a run of clicks.
fn continues_gesture(last: &mut Option<Instant>, now: Instant) -> bool {
    let continues = last
        .is_some_and(|at| now.saturating_duration_since(at).as_millis() <= crate::multi_click_ms());
    if continues {
        *last = (*last).max(Some(now));
    }
    continues
}

/// What a toggle in flight does next, decided from the engine's reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Advance {
    /// Nothing yet: the last press is unanswered, or the composer is not on screen.
    Wait,
    /// Claude answered with `expect`, a mode short of the toggle's goal.
    Press { expect: Mode },
    /// The goal shows: done, never a press on from there.
    Arrived,
    /// The cycle came back round.
    Lapped,
}

/// One session's lights in one window.
#[derive(Debug, Clone, Default)]
struct SessionLights {
    /// The last state each light was seen in, kept while a box covers the composer.
    shown: Reading,
    pending: Option<Pending>,
    refused: Option<(Light, Refusal, Instant)>,
    /// A toggle that took: its light stays drawn, titled with its new state, until this instant.
    notice: Option<(Light, Instant)>,
    /// The session's Autopilot as the last frame that drew its footer read it.
    autopilot: Option<AutopilotView>,
    /// An Autopilot click in flight.
    autopilot_pending: Option<(Autopilot, Instant)>,
    /// A press of the harness switch that wrote a switch of aterm.toml: the side it turns to
    /// (`true` on, `false` off), and when `◌` stops waiting for the policy to read it back.
    switch_turning: Option<(bool, Instant)>,
    /// When the harness-switch gesture's last press came: a press that flipped the switch (either
    /// way) starts one, and a press of the switch or of the Autopilot chip within a double click's
    /// interval of its last press is the same gesture ([`WindowLights::switch_just_pressed`]).
    switch_pressed: Option<Instant>,
    /// When a press last reached the mode light: a press within a double click's interval of it
    /// is the same gesture ([`WindowLights::light_just_pressed`]).
    light_pressed: Option<Instant>,
    /// Why the owner's last press of the top rule's ⬆️ was refused, and until when its label's
    /// slot says so.
    up_refused: Option<(String, Instant)>,
    /// What the Team chip says, as the last frame that drew the footer read it.
    team: Option<TeamFace>,
    /// The harness's glass on the Autopilot chip as the same frame read it ([`App::chip_glass`]).
    glass: ChipGlass,
    /// Whether the glass rewrote Claude's mode row on this pane this frame, its pill dropped:
    /// the lights block is then the mode's one home ([`lights::Mode::word_on_glass`]).
    rewritten: bool,
    /// What a fact menu's row just did here (a copy, a new tab).
    fact_notice: Option<(FactNotice, Instant)>,
    /// The mode's name a click on the Autopilot chip brought up beside its glyph.
    word: Option<ChipWord>,
}

/// How long the mode's name a click brings up stands whole beside the Autopilot chip's glyph.
pub(crate) const WORD_HOLD: Duration = Duration::from_millis(1000);
/// How long the name then takes to fade into the ground ([`crate::motion::MotionEffect::ChipWord`]).
pub(crate) const WORD_FADE: Duration = Duration::from_millis(600);
/// The fade's steps, each a frame with the name's ink nearer the ground.
pub(crate) const WORD_STEPS: u32 = 6;

/// The mode's name a click on the Autopilot chip brings up (`∞ cruise`, `⤓ land`, `○ off`):
/// laid after the glyph, which the block's left edge keeps under the pointer for the next click;
/// held [`WORD_HOLD`], then faded into the ground over [`WORD_FADE`] where motion is allowed, else
/// gone at once when the hold ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChipWord {
    /// The mode it names: drawn only while that mode stands.
    mode: Autopilot,
    /// When the mode showed: the click that found it written, or the frame that read it back.
    from: Instant,
    /// Whether it fades ([`crate::motion::MotionEffect::ChipWord`]) or goes at once.
    fades: bool,
    /// The step a frame was last asked to draw ([`WindowLights::words_due`]).
    asked: u32,
}

impl ChipWord {
    /// Its step at `now`: `0` whole, `1..=WORD_STEPS` fading, `None` once gone.
    pub(crate) fn step(self, now: Instant) -> Option<u32> {
        let since = now.saturating_duration_since(self.from);
        let Some(into) = since.checked_sub(WORD_HOLD) else {
            return Some(0);
        };
        if !self.fades || into >= WORD_FADE {
            return None;
        }
        let steps = into.as_nanos() * u128::from(WORD_STEPS) / WORD_FADE.as_nanos();
        Some(1 + u32::try_from(steps).unwrap_or(WORD_STEPS - 1))
    }

    /// When its step next changes after `now`, its going included; `None` once gone.
    pub(crate) fn next_at(self, now: Instant) -> Option<Instant> {
        let step = self.step(now)?;
        let fade = if self.fades {
            WORD_FADE * step / WORD_STEPS
        } else {
            Duration::ZERO
        };
        Some(self.from + WORD_HOLD + fade)
    }

    /// The name's ink at `step`: `ink` whole, then a step nearer `ground` each step, never the
    /// ground itself.
    fn ink(step: u32, ink: [u8; 3], ground: [u8; 3]) -> [u8; 3] {
        let part = |n: u32| u16::try_from(n).map_or(f32::from(u16::MAX), f32::from);
        crate::chrome_band::mix3(ink, ground, part(step) / part(WORD_STEPS + 1))
    }
}

impl SessionLights {
    /// The mode's name a click brought up, while its mode stands and no click is in flight.
    fn active_word(&self) -> Option<(&'static str, ChipWord)> {
        let word = self.word.filter(|_| self.autopilot_pending.is_none())?;
        let view = self.autopilot?;
        if view.mode() != Some(word.mode) || ChipFace::of(view, &self.glass).standing() {
            return None;
        }
        Some((view.named()?, word))
    }

    /// The mode's name a click brought up, its ink's step at `now`, while its mode stands and no
    /// click is in flight.
    fn word_at(&self, now: Instant) -> Option<(&'static str, u32)> {
        let (named, word) = self.active_word()?;
        Some((named, word.step(now)?))
    }

    /// Whether the harness's chip, Autopilot, is drawn for this session.
    fn harness_chips(&self) -> bool {
        self.autopilot
            .is_some_and(|view| view != AutopilotView::Unhosted)
    }

    /// Whether a rule short of room for the Autopilot chip's face may keep its glyph alone.
    fn glyph_rung(&self) -> bool {
        self.autopilot_pending.is_none()
            && self
                .autopilot
                .is_some_and(|view| ChipFace::of(view, &self.glass).standing())
    }
}

/// One painted cell of the chips, in frame cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LightHit {
    frame_row: usize,
    /// The same row as a window grid row, for the chrome register.
    term_row: usize,
    col: usize,
    session: u64,
    /// The chip painted there.
    chip: Option<Chip>,
}

/// What of the chips' glass the pointer is over ([`App::track_claude_light_hover`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverChips {
    /// Not the chips': the motion is the pane's.
    No,
    /// A chip: a press acts on it.
    Chip,
    /// Glass a press does nothing on: the blank between two chips, or a top line's harness label
    /// (hover only, every button on it swallowed). The arrow cursor, and the motion is not the
    /// pane's.
    Gap,
}

impl OverChips {
    /// What [`App::claude_glass_at`] found.
    pub(crate) const fn of_glass(at: Option<&(u64, GlassAt)>) -> Self {
        match at {
            None | Some((_, GlassAt::Mark)) => Self::No,
            Some((_, GlassAt::Chip(None) | GlassAt::Label)) => Self::Gap,
            Some((
                _,
                GlassAt::Chip(Some(_)) | GlassAt::Switch(_) | GlassAt::Upgrade | GlassAt::Fact(..),
            )) => Self::Chip,
        }
    }
}

/// One of the facts in the composer's bottom rule a person acts on (`crate::claude_facts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Fact {
    /// The top-effort key on the composer's top rule.
    TopEffort,
    /// `◆ Opus 5.5`.
    Model,
    /// `xhigh`, the tail of the model's segment.
    Effort,
    /// `⌂ ~/aterm`.
    Path,
    /// `⎇ main`.
    Branch,
    /// `◐ ctx 84% left`.
    Context,
    /// `Σ 6m33s ↑20M ↓82k`.
    Usage,
    /// `⎆ sol-m27@example.test · max 20x · weekly 0% left`, one unit.
    Account,
}

impl Fact {
    /// Every fact a person acts on, each with its wire name: the bottom rule's
    /// in the order the rule sets them, then the top rule's key and the account group.
    pub(crate) const EVERY: [Fact; 8] = [
        Fact::Model,
        Fact::Effort,
        Fact::Path,
        Fact::Branch,
        Fact::Context,
        Fact::Usage,
        Fact::TopEffort,
        Fact::Account,
    ];

    /// Its wire name (`open fact-menu <name>`, `fact-menu/<name>/…`).
    #[must_use]
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Fact::Model => "model",
            Fact::Effort => "effort",
            Fact::Path => "path",
            Fact::Branch => "branch",
            Fact::Context => "context",
            Fact::Usage => "usage",
            Fact::TopEffort => "top-effort",
            Fact::Account => "account",
        }
    }

    /// The fact a wire name names ([`Self::key`]), of [`Self::EVERY`].
    #[must_use]
    pub(crate) fn parse(name: &str) -> Option<Self> {
        Self::EVERY.into_iter().find(|fact| fact.key() == name)
    }

    /// Its name heading its menu (`Folder · tab 1`).
    #[must_use]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Fact::Model => "Model",
            Fact::Effort => "Effort",
            Fact::Path => "Folder",
            Fact::Branch => "Branch",
            Fact::Context => "Context",
            Fact::Usage => "Traffic",
            Fact::TopEffort => "Effort",
            Fact::Account => "Accounts",
        }
    }

    /// The fact a span of the facts stands for (`footer::FactSpan::kind`).
    #[must_use]
    pub(crate) const fn of(kind: FactKind) -> Self {
        match kind {
            FactKind::Model => Fact::Model,
            FactKind::Effort => Fact::Effort,
            FactKind::Path => Fact::Path,
            FactKind::Branch => Fact::Branch,
            FactKind::Context => Fact::Context,
            FactKind::Usage => Fact::Usage,
            FactKind::Account => Fact::Account,
        }
    }

    /// The footer's own name for it.
    #[must_use]
    pub(crate) const fn kind(self) -> Option<FactKind> {
        Some(match self {
            Fact::Model => FactKind::Model,
            Fact::Effort => FactKind::Effort,
            Fact::Path => FactKind::Path,
            Fact::Branch => FactKind::Branch,
            Fact::Context => FactKind::Context,
            Fact::Usage => FactKind::Usage,
            Fact::Account => FactKind::Account,
            Fact::TopEffort => return None,
        })
    }

    /// Its word in a title or a menu row (`model hidden`, `Hide folder`).
    #[must_use]
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Fact::Model => "model",
            Fact::Effort => "effort",
            Fact::Path => "folder",
            Fact::Branch => "branch",
            Fact::Context => "context",
            Fact::Usage => "traffic",
            Fact::TopEffort => "key",
            Fact::Account => "account",
        }
    }
}

/// What the pointer or the keyboard is on in the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Target {
    Chip(Chip),
    Fact(Fact),
    /// The harness switch on the top rule ([`BorderSwitch`]), the keyboard's
    /// last stop in a pane whose rule draws one.
    Switch,
}

impl From<Chip> for Target {
    fn from(chip: Chip) -> Self {
        Target::Chip(chip)
    }
}

impl From<Fact> for Target {
    fn from(fact: Fact) -> Self {
        Target::Fact(fact)
    }
}

impl Target {
    /// The chip, where it is one.
    #[must_use]
    pub(crate) const fn chip(self) -> Option<Chip> {
        match self {
            Target::Chip(chip) => Some(chip),
            Target::Fact(_) | Target::Switch => None,
        }
    }

    /// The fact, where it is one.
    #[must_use]
    pub(crate) const fn fact(self) -> Option<Fact> {
        match self {
            Target::Fact(fact) => Some(fact),
            Target::Chip(_) | Target::Switch => None,
        }
    }
}

/// What a fact menu's row did, said for a moment in the lights' title slot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum FactNotice {
    Copied,
    NewTab,
    /// A slash press's words ([`App::note_fact_said`]).
    Said(std::sync::Arc<str>),
}

impl FactNotice {
    /// Its words, whole: a notice has no short form.
    #[must_use]
    pub(crate) fn text(&self) -> &str {
        match self {
            FactNotice::Copied => "\u{2713} copied",
            FactNotice::NewTab => "\u{2713} new tab",
            FactNotice::Said(words) => words,
        }
    }
}

/// How long a fact menu's notice stays in the title slot.
const FACT_NOTICE_SHOWN: Duration = Duration::from_secs(2);

/// Where one fact of the rule landed on the glass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FactHit {
    pub(crate) frame_row: usize,
    /// The same row as a window grid row, for the chrome register.
    pub(crate) term_row: usize,
    /// Its frame columns, `start..end`.
    pub(crate) cols: (usize, usize),
    pub(crate) session: u64,
    pub(crate) fact: Fact,
    /// What a click on it shows again.
    pub(crate) restores: FactMask,
}

/// Where a rule's title slot landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TitleHit {
    pub(crate) frame_row: usize,
    pub(crate) term_row: usize,
    pub(crate) cols: std::ops::Range<usize>,
    pub(crate) session: u64,
}

/// One top line's lockup the last frame put on the glass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MarkHit {
    pub(crate) frame_row: usize,
    pub(crate) term_row: usize,
    pub(crate) cols: std::ops::Range<usize>,
    pub(crate) session: u64,
}

/// What of the harness's glass is under a pixel ([`App::claude_glass_at`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GlassAt {
    /// A chip, or `None` for the blank between two ([`App::claude_chips_at`]).
    Chip(Option<Chip>),
    /// The top rule's harness switch (`border_toggles`), as the paint drew it.
    Switch(crate::autopilot::HarnessSwitch),
    /// A top line's lockup (`mark_hits`).
    Mark,
    /// A top line's harness label (`border_toggles`): a hover shows the CLI's whole build in the
    /// label's slot, only where the label shortens it (`label_whole`); every press on it is
    /// swallowed.
    Label,
    /// A top line's ⬆️ of a newer CLI build (`border_toggles`): a press asks for the move now.
    Upgrade,
    /// A fact of the bottom rule and what a click on it shows again.
    Fact(Fact, FactMask),
}

/// The part of a top line's harness `glass` is, as its paint lights it under the pointer
/// ([`WindowLights::part_hover`]); the switch has its own ([`WindowLights::switch_hover`]).
const fn part_of(glass: GlassAt) -> Option<crate::claude_footer::HoverAt> {
    match glass {
        GlassAt::Label => Some(crate::claude_footer::HoverAt::Label),
        GlassAt::Upgrade => Some(crate::claude_footer::HoverAt::Arrow),
        GlassAt::Chip(_) | GlassAt::Switch(_) | GlassAt::Mark | GlassAt::Fact(..) => None,
    }
}

/// One fact's title someone is reading (hovered or selected).
#[derive(Debug)]
struct FactReading {
    session: u64,
    fact: Fact,
    whole: String,
    short: String,
    /// The fit number taken when it was last built.
    laid: u64,
}

/// The titles a block may lay, whole and short, each borrowed from its reading.
type Titles<'a> = (
    Option<(&'a str, &'a str)>,
    Option<(&'a str, &'a str)>,
    Option<(&'a str, &'a str)>,
);

/// Which reading's title a block lays ([`WindowLights::lend_block`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TitleAt {
    Autopilot(usize),
    Fact(usize),
    Panel(usize),
}

/// The lights' state for one window (`WindowState::claude_lights`).
#[derive(Debug, Default)]
pub(crate) struct WindowLights {
    /// One per painted cell of every drawn chip.
    hits: Vec<LightHit>,
    /// The sessions whose composer rule this frame found on the glass (chip or none).
    footers: Vec<(u64, usize)>,
    hover: Option<(u64, Target)>,
    selected: Option<(u64, Target)>,
    /// A frame has been painted since the selection last moved.
    selection_painted: bool,
    sessions: HashMap<u64, SessionLights>,
    /// Painted blocks handed back, their buffers the next frame's (`App::claude_lights_block`).
    spare_blocks: Vec<Block>,
    /// The fitted blocks: each pane's chip block as the last frame fitted it.
    fitted: Vec<Fitted>,
    /// The last fit's number: each fit takes the next ([`Block::fit`]).
    fits: u64,
    /// The autopilot title someone reads here (hovered or selected), per session.
    readings: Vec<TitleReading>,
    /// The harness switches the last paint put on composer top rules.
    pub(crate) border_toggles: Vec<BorderSwitch>,
    /// The session whose harness switch the pointer is over ([`App::track_claude_light_hover`]).
    pub(crate) switch_hover: Option<u64>,
    /// The session whose top line's harness label (or arrow) the pointer rests on, and which.
    pub(crate) part_hover: Option<(u64, crate::claude_footer::HoverAt)>,
    /// The rules' title slots the last frame drew ([`TitleHit`]).
    pub(crate) title_hits: Vec<TitleHit>,
    /// The lockups the last paint put on top lines ([`MarkHit`]).
    pub(crate) mark_hits: Vec<MarkHit>,
    /// The session whose top line's lockup the pointer rests on ([`App::track_claude_light_hover`]).
    pub(crate) mark_hover: Option<u64>,
    /// The window pixel the last motion put the pointer on while it hovered a switch or a lockup.
    glass_hover_px: Option<(f64, f64)>,
    /// The grid the lists were laid.
    drawn_at: Option<crate::app_mouse::GlassGrid>,
    /// Where the last paint laid what a pointer hit-tests ([`WindowLights::places`]) and the
    /// hover it was laid under ([`App::settle_glass_hover`]).
    laid: Option<(u64, GlassHover)>,
    /// A paint after a re-grid moved the hover under a still pointer ([`App::settle_glass_hover`]).
    cursor_owed: bool,
    /// The facts on the glass.
    pub(crate) fact_hits: Vec<FactHit>,
    /// The fact titles someone reads here (hovered or selected).
    fact_readings: Vec<FactReading>,
    /// The agents panel's titles.
    panel_readings: Vec<PanelReading>,
}

/// One harness switch on a composer top rule as the paint drew it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BorderSwitch {
    pub(crate) frame_row: usize,
    /// The same row as a window grid row, for the chrome register.
    pub(crate) term_row: usize,
    /// The switch's cells (` ● on ` when lit, ` ○ off ` when off), its blanks included: a press
    /// anywhere on them turns it.
    pub(crate) cols: std::ops::Range<usize>,
    /// The harness label's cells (`alab v0.5 (v2.1.305)`): hover only.
    pub(crate) label: Option<std::ops::Range<usize>>,
    /// The ⬆️ of a newer CLI build with its side blanks: a press asks for the move now.
    pub(crate) arrow: Option<std::ops::Range<usize>>,
    pub(crate) session: u64,
    /// What the switch said.
    pub(crate) face: crate::autopilot::HarnessSwitch,
    /// The paint it was drawn with (`claude_footer::switch_words` rebuilds its words from it).
    pub(crate) paint: crate::claude_footer::SwitchPaint,
    pub(crate) program: aterm_phase::Program,
}

/// One panel row's title, `in Bash 4m12s` ([`WindowLights::panel_readings`]).
#[derive(Debug)]
struct PanelReading {
    session: u64,
    /// What it was built from: the tool, and the whole seconds in it.
    key: (aterm_agent::harness::flight::ToolName, u64),
    whole: String,
    short: String,
    /// The fit number taken when it was built ([`FitKey::title`]).
    laid: u64,
}

/// What a panel row waiting `secs` in `tool` is titled, whole and short.
fn wait_title(tool: &str, secs: u64) -> (String, String) {
    let span = match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, secs / 60 % 60),
    };
    (format!("in {tool} {span}"), format!("{tool} {span}"))
}

/// One Autopilot title someone is reading ([`WindowLights::readings`]).
#[derive(Debug)]
struct TitleReading {
    session: u64,
    /// The trail's generation [`Self::trail`] was built at.
    generation: u64,
    /// The trail as the title lists it (`harness_glass::trail_title`).
    trail: String,
    /// The local day the trail's wall read gave, which the face's clock is read against.
    today: Option<i64>,
    /// The chip's view and glass [`Self::whole`] and [`Self::short`] were built from.
    built: (AutopilotView, ChipGlass),
    /// The title, whole and short ([`autopilot_title`]).
    whole: String,
    short: String,
    /// The fit number ([`WindowLights::fits`]) taken when [`Self::whole`] and [`Self::short`].
    laid: u64,
}

/// Whether Autopilot's title names a clock.
const fn title_stamped(glass: &ChipGlass) -> bool {
    glass.task_at.is_some() || glass.pause.is_some()
}

impl TitleReading {
    /// Rebuild this title where its complete reading inputs moved.
    fn refresh(
        &mut self,
        generation: u64,
        built: (AutopilotView, ChipGlass),
        read_words: impl FnOnce() -> ReadWords,
        read_trail: impl FnOnce(bool, bool) -> (String, Option<i64>),
    ) -> bool {
        if self.generation == generation && self.built == built {
            return false;
        }
        self.generation = generation;
        self.built = built;
        let parts = compose_title(&built, read_words, read_trail);
        (self.trail, self.today, self.whole, self.short) =
            (parts.trail, parts.today, parts.whole, parts.short);
        true
    }
}

/// A chip title's parts as [`compose_title`] builds them.
pub(crate) struct TitleParts {
    /// The trail as the title lists it, and the day it was read on.
    pub(crate) trail: String,
    pub(crate) today: Option<i64>,
    /// The whole title and the short one.
    pub(crate) whole: String,
    pub(crate) short: String,
}

/// THE ONE COMPOSITION of an Autopilot chip's title under `built` (the view and glass a frame
/// noted), from the words and the trail its readers give: the hover's reading
/// ([`TitleReading::refresh`] and its first build) and `chrome`'s `title=` ([`title_parts`]).
/// While a read hold is shown under Land's `⤓`, the trail leaves out Land's own acts, which the
/// hold's `landing since 10:30, 2 nudges` already says (said once, §14.4).
fn compose_title(
    built: &(AutopilotView, ChipGlass),
    read_words: impl FnOnce() -> ReadWords,
    read_trail: impl FnOnce(bool, bool) -> (String, Option<i64>),
) -> TitleParts {
    let (hold, need, stopped, switch, receipt, said) = read_words();
    let held = ChipFace::of(built.0, &built.1) == ChipFace::Land
        && hold.is_some_and(crate::harness_glass::HeldGlass::read);
    let (trail, today) = read_trail(title_stamped(&built.1) || hold.is_some(), held);
    let (whole, short) = autopilot_title(
        built.0,
        &built.1,
        &trail,
        today,
        TitleWords {
            hold: hold.as_ref(),
            said: said.as_deref(),
            need: need.as_ref(),
            stopped,
            switch,
            receipt: receipt.as_deref(),
        },
    );
    TitleParts {
        trail,
        today,
        whole,
        short,
    }
}

/// Session `session`'s Autopilot chip title under `built`, with the hold, the need and the trail
/// it reads now ([`compose_title`]).
pub(crate) fn title_parts(
    harness_glass: &std::collections::HashMap<u64, crate::harness_glass::SessionGlass>,
    hold_said: &HoldSaidMap,
    pool: &crate::SessionPool,
    session: u64,
    built: &(AutopilotView, ChipGlass),
) -> TitleParts {
    compose_title(
        built,
        || title_words_of(harness_glass, hold_said, pool, session),
        |stamped, held| crate::harness_glass::trail_title(harness_glass, session, stamped, held),
    )
}

/// How many painted blocks a window keeps for reuse (`App::recycle_claude_lights_block`).
const SPARE_BLOCKS: usize = 8;

/// One pane's fitted block ([`WindowLights::fitted`]).
#[derive(Debug)]
struct Fitted {
    /// The pane's session and its leaf.
    session: u64,
    pane: usize,
    /// What the block was fitted from.
    key: FitKey,
    /// The fit's number ([`Block::fit`]).
    fit: u64,
    /// Whether a chip fit at all (`false`: no block is drawn).
    drawn: bool,
    /// The block, while it is not lent to a painter.
    block: Option<Block>,
    /// A frame asked for it since the last [`WindowLights::clear_hits`].
    used: bool,
}

/// Everything a chip block is fitted from ([`WindowLights::fit_into`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FitKey {
    blank: RenderCell,
    at: ChipAt,
    selected: Option<Target>,
    hover: Option<Target>,
    room: usize,
    /// Whether the mode's name a click brought up may be laid ([`Block::word`]).
    named: bool,
    /// The build of Autopilot's title ([`TitleReading::laid`]) or a fact's ([`FactReading::laid`]).
    title: Option<u64>,
    lights: LightsKey,
}

/// The subset of [`SessionLights`] read when laying a chip block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LightsKey {
    shown: Reading,
    pending: Option<(Light, Goal)>,
    refused: Option<(Light, Refusal)>,
    notice: Option<Light>,
    autopilot: Option<AutopilotView>,
    autopilot_pending: bool,
    glass: ChipGlass,
    /// The mode row's rewrite ([`SessionLights::rewritten`]).
    rewritten: bool,
    team: Option<u64>,
    /// A fact notice's hash: a slash press's words are text.
    fact_notice: Option<u64>,
    /// The mode's name a click brought up, and its fade's step ([`SessionLights::word_at`]).
    word: Option<(&'static str, u32)>,
}

impl SessionLights {
    /// This session's [`LightsKey`] at the instant `now` gives.
    fn block_key(&self, now: impl FnOnce() -> Instant) -> LightsKey {
        let active_word = self.active_word();
        let now = (self.refused.is_some()
            || self.notice.is_some()
            || self.fact_notice.is_some()
            || active_word.is_some())
        .then(now);
        let showing = |until: Instant| now.is_some_and(|now| until > now);
        LightsKey {
            word: active_word
                .zip(now)
                .and_then(|((named, word), now)| Some((named, word.step(now)?))),
            shown: self.shown,
            pending: self.pending.as_ref().map(|p| (p.light, p.goal)),
            refused: self
                .refused
                .filter(|(_, _, until)| showing(*until))
                .map(|(light, why, _)| (light, why)),
            notice: self
                .notice
                .filter(|(_, until)| showing(*until))
                .map(|(light, _)| light),
            autopilot: self.autopilot,
            autopilot_pending: self.autopilot_pending.is_some(),
            glass: self.glass,
            rewritten: self.rewritten,
            team: self.team.as_ref().map(|face| {
                use std::hash::{Hash as _, Hasher as _};
                let mut h = std::hash::DefaultHasher::new();
                face.hash(&mut h);
                h.finish()
            }),
            fact_notice: self
                .fact_notice
                .as_ref()
                .filter(|(_, until)| showing(*until))
                .map(|(notice, _)| {
                    use std::hash::{Hash as _, Hasher as _};
                    let mut h = std::hash::DefaultHasher::new();
                    notice.hash(&mut h);
                    h.finish()
                }),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// The fitted blocks are kept ([`WindowLights::fitted`]).
    static KEEP_FITTED: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Run `f` with every chip block fitted anew, none kept.
#[cfg(test)]
pub(crate) fn fit_anew_in<R>(f: impl FnOnce() -> R) -> R {
    let was = KEEP_FITTED.with(|k| k.replace(false));
    let out = f();
    KEEP_FITTED.with(|k| k.set(was));
    out
}

#[cfg(test)]
thread_local! {
    /// The instant a test pinned the lights' clock at ([`at_clock`]); `None`: the wall clock.
    static PINNED_NOW: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// Run `f` with the lights' clock ([`lights_now`]) pinned at `now`, so a frame's fit, key and
/// fingerprint read one instant however long the test takes. Its callers sit in Unix-only test
/// modules (`fit_tests`, `word_tests`), so it builds only there.
#[cfg(all(test, unix))]
pub(crate) fn at_clock<T>(now: Instant, f: impl FnOnce() -> T) -> T {
    let was = PINNED_NOW.with(|p| p.replace(Some(now)));
    let out = f();
    PINNED_NOW.with(|p| p.set(was));
    out
}

/// The lights' clock on the footer's per-frame paths.
pub(crate) fn lights_now() -> Instant {
    #[cfg(test)]
    {
        crate::work_counts::footer_clock_read();
        if let Some(now) = PINNED_NOW.with(std::cell::Cell::get) {
            return now;
        }
    }
    Instant::now()
}

/// The pointer's hover on the glass: a chip or fact, a switch, a top line's part, a lockup.
type GlassHover = (
    Option<(u64, Target)>,
    Option<u64>,
    Option<(u64, crate::claude_footer::HoverAt)>,
    Option<u64>,
);

impl WindowLights {
    /// The pointer's hover on the glass now ([`GlassHover`]).
    const fn glass_hover(&self) -> GlassHover {
        (
            self.hover,
            self.switch_hover,
            self.part_hover,
            self.mark_hover,
        )
    }

    /// A digest of where the last paint laid what a pointer hit-tests ([`App::claude_glass_at`]):
    /// the chips' cells, the switches with their labels and arrows, the facts and the lockups.
    fn places(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hits.len().hash(&mut h);
        for hit in &self.hits {
            (hit.frame_row, hit.term_row, hit.col, hit.session, hit.chip).hash(&mut h);
        }
        self.border_toggles.len().hash(&mut h);
        for b in &self.border_toggles {
            (
                b.frame_row,
                b.term_row,
                &b.cols,
                &b.label,
                &b.arrow,
                b.session,
            )
                .hash(&mut h);
        }
        self.fact_hits.len().hash(&mut h);
        for f in &self.fact_hits {
            (f.frame_row, f.term_row, f.cols, f.session, f.fact).hash(&mut h);
        }
        self.mark_hits.len().hash(&mut h);
        for m in &self.mark_hits {
            (m.frame_row, m.term_row, &m.cols, m.session).hash(&mut h);
        }
        h.finish()
    }

    /// Everything about the lights that is GUI state rather than grid content, for the repaint key.
    pub(crate) fn fingerprint(&self, now: impl Fn() -> Instant) -> u64 {
        let read = std::cell::Cell::new(None);
        let at = || {
            read.get().unwrap_or_else(|| {
                let at = now();
                read.set(Some(at));
                at
            })
        };
        let shows = |until: Instant| until > at();
        // A name that [`SessionLights::word_at`] does not draw (a click in flight, another mode,
        // or a standing face) neither reads the clock nor moves the repaint key.
        let step = |s: &SessionLights| {
            let (_, word) = s.active_word()?;
            Some((word.mode, word.step(at())?))
        };
        let mut active: Vec<_> = self
            .sessions
            .iter()
            .filter(|(_, s)| {
                s.pending.is_some()
                    || s.refused.is_some_and(|(_, _, until)| shows(until))
                    || s.notice.is_some_and(|(_, until)| shows(until))
                    || s.fact_notice
                        .as_ref()
                        .is_some_and(|(_, until)| shows(*until))
                    || step(s).is_some()
            })
            .collect();
        if self.hover.is_none() && self.selected.is_none() && active.is_empty() {
            return 0;
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hover.hash(&mut h);
        self.selected.hash(&mut h);
        active.sort_by_key(|(id, _)| **id);
        for (id, s) in active {
            id.hash(&mut h);
            if let Some(p) = &s.pending {
                (p.light, p.goal, p.presses).hash(&mut h);
            }
            if let Some((light, why, until)) = s.refused
                && shows(until)
            {
                (light, why).hash(&mut h);
            }
            if let Some((light, until)) = s.notice
                && shows(until)
            {
                (light, "notice").hash(&mut h);
            }
            if let Some((notice, until)) = &s.fact_notice
                && shows(*until)
            {
                notice.hash(&mut h);
            }
            if let Some(word) = step(s) {
                word.hash(&mut h);
            }
        }
        h.finish() | 1
    }

    /// Keep one poll armed for `session`'s toggle in flight.
    fn arm_poll(&mut self, session: u64, now: Instant, every: Duration) {
        if let Some(p) = self
            .sessions
            .get_mut(&session)
            .and_then(|s| s.pending.as_mut())
            && p.next_poll.is_none_or(|at| now >= at)
        {
            p.next_poll = Some(now + every);
            wake_after(session, every);
        }
    }

    /// Forget every painted light and footer rule.
    pub(crate) fn clear_hits(&mut self) {
        self.hits.clear();
        self.border_toggles.clear();
        self.title_hits.clear();
        self.fact_hits.clear();
        self.mark_hits.clear();
        self.footers.clear();
        self.selection_painted = true;
        if self.fitted.iter().any(|f| !f.used) {
            let spares = &mut self.spare_blocks;
            self.fitted.retain_mut(|f| {
                if !f.used
                    && let Some(block) = f.block.take()
                    && spares.len() < SPARE_BLOCKS
                {
                    spares.push(block);
                }
                f.used
            });
        }
        for f in &mut self.fitted {
            f.used = false;
        }
        let reading = [self.hover, self.selected];
        if !self.readings.is_empty() {
            self.readings
                .retain(|r| reading.contains(&Some((r.session, Target::Chip(Chip::Autopilot)))));
        }
        if !self.fact_readings.is_empty() {
            self.fact_readings
                .retain(|r| reading.contains(&Some((r.session, Target::Fact(r.fact)))));
        }
    }

    /// The fact the pointer or the keyboard is on in `session`'s rule, the keyboard's first.
    pub(crate) fn lit_fact(&self, session: u64) -> Option<FactKind> {
        let mine = |at: Option<(u64, Target)>| at.filter(|(id, _)| *id == session);
        match mine(self.selected).or(mine(self.hover)) {
            Some((_, Target::Fact(fact))) => fact.kind(),
            _ => None,
        }
    }

    /// How the top rule's key of `session` is read.
    #[must_use]
    pub(crate) fn top_effort_read(&self, session: u64) -> (bool, bool) {
        let at = Some((session, Target::Fact(Fact::TopEffort)));
        (self.hover == at, self.selected == at)
    }

    /// Whether this frame drew a block of chips for `session`.
    #[must_use]
    pub(crate) fn shows_block(&self, session: u64) -> bool {
        self.hits
            .iter()
            .any(|hit| hit.session == session && hit.chip.is_some())
    }

    /// `session`'s facts on this frame's glass, in glass order (left to right).
    fn facts_on_glass(&self, session: u64) -> Vec<Fact> {
        let mut hits: Vec<&FactHit> = self
            .fact_hits
            .iter()
            .filter(|h| h.session == session)
            .collect();
        hits.sort_by_key(|h| (h.fact == Fact::TopEffort, h.frame_row, h.cols.0));
        let mut out: Vec<Fact> = Vec::with_capacity(hits.len());
        for hit in hits {
            if !out.contains(&hit.fact) {
                out.push(hit.fact);
            }
        }
        out
    }

    /// What the keyboard walks in `session` while a selection lasts, in the
    /// row's order: the chips, the facts, then the top rule's harness switch.
    fn revealable(&self, session: u64) -> Vec<Target> {
        let s = self.sessions.get(&session);
        let shown = s.map(|s| s.shown).unwrap_or_default();
        let harness = s.is_some_and(SessionLights::harness_chips);
        let team = s.is_some_and(|s| s.team.is_some());
        Chip::row()
            .filter(|chip| match chip {
                Chip::Light(light) => revealed(*light, &shown),
                Chip::Autopilot => harness,
                Chip::Team => team,
            })
            .map(Target::Chip)
            .chain(self.facts_on_glass(session).into_iter().map(Target::Fact))
            .chain(self.switch_of(session).map(|_| Target::Switch))
            .collect()
    }

    /// `session`'s harness switch on this frame's glass.
    fn switch_of(&self, session: u64) -> Option<&BorderSwitch> {
        self.border_toggles.iter().find(|b| b.session == session)
    }

    /// Whether the pointer or the keyboard is on `session`'s harness switch.
    fn switch_lit(&self, session: u64) -> bool {
        self.switch_hover == Some(session) || self.selected == Some((session, Target::Switch))
    }

    /// Whether `target` of `session` is on this frame's glass.
    #[cfg(all(test, target_os = "macos"))]
    fn shows(&self, session: u64, target: Target) -> bool {
        self.shows_uncovered(session, target, |_| false)
    }

    /// Whether `target` of `session` is on this frame's glass and not on a row `covered` owns.
    fn shows_uncovered(
        &self,
        session: u64,
        target: Target,
        covered: impl Fn(usize) -> bool,
    ) -> bool {
        match target {
            Target::Chip(chip) => self
                .hits
                .iter()
                .any(|h| h.session == session && h.chip == Some(chip) && !covered(h.term_row)),
            Target::Fact(fact) => self
                .fact_hits
                .iter()
                .any(|h| h.session == session && h.fact == fact && !covered(h.term_row)),
            Target::Switch => self
                .border_toggles
                .iter()
                .any(|b| b.session == session && !covered(b.term_row)),
        }
    }

    /// Move the keyboard selection (a frame must paint it before it counts as off the glass).
    const fn select(&mut self, selected: Option<(u64, Target)>) {
        self.selected = selected;
        self.selection_painted = false;
    }

    /// Let `session`'s turning state go once `face` has reached the side it was turning toward.
    pub(crate) fn note_switch_face(
        &mut self,
        session: u64,
        face: Option<crate::autopilot::HarnessSwitch>,
    ) {
        if let Some(s) = self.sessions.get_mut(&session) {
            if let Some((on, _)) = s.switch_turning
                && !matches!(
                    (on, face),
                    (true, Some(crate::autopilot::HarnessSwitch::Off(_)))
                        | (false, Some(crate::autopilot::HarnessSwitch::On))
                )
            {
                s.switch_turning = None;
            }
            match face {
                Some(
                    crate::autopilot::HarnessSwitch::On
                    | crate::autopilot::HarnessSwitch::TurningOff,
                ) => {}
                Some(crate::autopilot::HarnessSwitch::Off(crate::autopilot::SwitchOff::Master)) => {
                    s.autopilot = Some(AutopilotView::MasterOff);
                    s.autopilot_pending = None;
                    s.rewritten = false;
                    s.word = None;
                }
                Some(crate::autopilot::HarnessSwitch::Off(crate::autopilot::SwitchOff::Agent(
                    agent,
                ))) => {
                    s.autopilot = Some(AutopilotView::AgentOff(agent));
                    s.autopilot_pending = None;
                    s.rewritten = false;
                    s.word = None;
                }
                Some(crate::autopilot::HarnessSwitch::Turning) => {
                    s.autopilot_pending = None;
                    s.rewritten = false;
                    s.word = None;
                }
                None => {
                    s.autopilot = Some(AutopilotView::Unhosted);
                    s.autopilot_pending = None;
                    s.rewritten = false;
                    s.word = None;
                }
            }
        }
    }

    /// Fold in `session`'s Autopilot as this frame's footer read it.
    fn note_autopilot_at(
        &mut self,
        session: u64,
        word: Option<Autopilot>,
        view: AutopilotView,
        glass: ChipGlass,
        now: impl FnOnce() -> Instant,
    ) {
        let s = self.sessions.entry(session).or_default();
        s.autopilot = Some(view);
        s.glass = glass;
        if let Some((want, until)) = s.autopilot_pending {
            if word == Some(want) {
                // The click's word read back: its name stands from here.
                s.autopilot_pending = None;
                let from = now();
                if let Some(named) = s.word.as_mut().filter(|named| named.mode == want) {
                    named.from = from;
                    named.asked = 0;
                }
            } else if until <= now() {
                s.autopilot_pending = None;
                s.word = None;
            }
        }
        self.note_switch_face(session, crate::autopilot::HarnessSwitch::of(view));
    }

    /// Whether a mode name a click brought up has reached a step no frame was asked for at `now`
    /// (its going included): each such name is marked asked, and the window owes a frame. A name
    /// whose click is still in flight waits for its read-back, which starts its hold.
    pub(crate) fn words_due(&mut self, now: Instant) -> bool {
        let mut due = false;
        for s in self.sessions.values_mut() {
            if s.autopilot_pending.is_some() {
                continue;
            }
            let drawn = s.active_word().is_some();
            let Some(named) = s.word.as_mut() else {
                continue;
            };
            match named.step(now) {
                Some(step) if !drawn || step == named.asked => {}
                Some(step) => {
                    named.asked = step;
                    due = true;
                }
                None => {
                    s.word = None;
                    due |= drawn;
                }
            }
        }
        due
    }

    /// When the next mode name a click brought up changes step after `now`; `now` itself for a
    /// name that went before `now` and is not let go yet (a look just before its end found its
    /// last step drawn), so the loop looks again at once and [`Self::words_due`] lets it go and
    /// asks the frame that clears it.
    pub(crate) fn word_wake(&self, now: Instant) -> Option<Instant> {
        self.sessions
            .values()
            .filter_map(|s| {
                let (_, word) = s.active_word()?;
                Some(word.next_at(now).unwrap_or(now))
            })
            .min()
    }

    /// Note the row `session`'s footer is drawn on this frame: whether the glass rewrote Claude's
    /// mode row.
    fn note_row(&mut self, session: u64, rewritten: bool) {
        self.sessions.entry(session).or_default().rewritten = rewritten;
    }

    /// [`Self::note_autopilot_at`] at `now`.
    #[cfg(test)]
    fn note_autopilot(
        &mut self,
        session: u64,
        word: Option<Autopilot>,
        view: AutopilotView,
        now: Instant,
    ) {
        self.note_autopilot_at(session, word, view, ChipGlass::default(), || now);
    }

    /// The harness chips' repaint term for `session`'s footer in this window.
    pub(crate) fn autopilot_term(
        &self,
        session: u64,
        word: Option<Autopilot>,
        view: AutopilotView,
        glass: ChipGlass,
    ) -> u64 {
        let pending = self
            .sessions
            .get(&session)
            .and_then(|s| s.autopilot_pending)
            .map(|(want, _)| want);
        let reading =
            [self.hover, self.selected].contains(&Some((session, Target::Chip(Chip::Autopilot))));
        let word_bits = |w: Option<Autopilot>| w.map_or(0, |w| 1 + w as u64);
        word_bits(word) | view.code() << 8 | word_bits(pending) << 16 | glass_bits(glass, reading)
    }

    /// What of `session`'s top line's harness the pointer or the keyboard is on.
    pub(crate) fn top_hover(&self, session: u64) -> crate::claude_footer::HoverAt {
        if self.switch_lit(session) {
            return crate::claude_footer::HoverAt::Switch;
        }
        self.part_hover
            .filter(|(s, _)| *s == session)
            .map_or(crate::claude_footer::HoverAt::None, |(_, part)| part)
    }

    /// The harness switch `session`'s top rule shows over `face`, and what of the harness the
    /// pointer or the keyboard is on.
    pub(crate) fn switch_shown(
        &self,
        session: u64,
        face: crate::autopilot::HarnessSwitch,
    ) -> (
        crate::autopilot::HarnessSwitch,
        crate::claude_footer::HoverAt,
    ) {
        use crate::autopilot::HarnessSwitch;
        let turning = self
            .sessions
            .get(&session)
            .and_then(|s| s.switch_turning)
            .map(|(on, _)| on);
        let face = match (turning, face) {
            (Some(true), HarnessSwitch::Off(_)) => HarnessSwitch::Turning,
            (Some(false), HarnessSwitch::On) => HarnessSwitch::TurningOff,
            _ => face,
        };
        (face, self.top_hover(session))
    }

    /// Whether this press still belongs to the last harness-switch gesture, which it then
    /// continues ([`continues_gesture`]).
    fn switch_just_pressed(&mut self, session: u64, now: Instant) -> bool {
        self.sessions
            .get_mut(&session)
            .is_some_and(|s| continues_gesture(&mut s.switch_pressed, now))
    }

    /// Whether this press still belongs to the last gesture that reached the mode light, which it
    /// then continues ([`continues_gesture`]): a later press of a double or triple click, which
    /// would turn back what the first turned over.
    fn light_just_pressed(&mut self, session: u64, now: Instant) -> bool {
        self.sessions
            .get_mut(&session)
            .is_some_and(|s| continues_gesture(&mut s.light_pressed, now))
    }

    /// The harness switch's repaint term for `session`'s footer in this window.
    pub(crate) fn switch_term(&self, session: u64, supervised: bool) -> u64 {
        let turning = self
            .sessions
            .get(&session)
            .and_then(|s| s.switch_turning)
            .map(|(on, _)| on);
        let refused = self
            .up_refused(session)
            .map_or(0, |words| aterm_hash::fnv1a_64(words.as_bytes()) | 1);
        (u64::from(supervised)
            | u64::from(turning.is_some()) << 1
            | u64::from(self.switch_lit(session)) << 2
            | u64::from(self.mark_hovered(session)) << 3
            | (self.top_hover(session) as u64) << 5
            | u64::from(turning == Some(false)) << 4)
            ^ refused.rotate_left(8)
    }

    /// The grid the lists were laid on, for a test to read or to forge.
    #[cfg(test)]
    pub(crate) fn drawn_at_mut(&mut self) -> &mut Option<crate::app_mouse::GlassGrid> {
        &mut self.drawn_at
    }

    /// Whether the pointer rests on `session`'s top line's lockup.
    pub(crate) fn mark_hovered(&self, session: u64) -> bool {
        self.mark_hover == Some(session)
    }

    /// The window pixel the pointer last hovered a switch or a lockup at, while it does.
    pub(crate) fn glass_hover_px(&self) -> Option<(f64, f64)> {
        self.glass_hover_px
    }

    /// Let every switch turning on or off go.
    pub(crate) fn let_switches_go(&mut self) -> bool {
        let mut any = false;
        for s in self.sessions.values_mut() {
            let went = s.switch_turning.take().is_some();
            #[cfg(test)]
            if went {
                SWITCHES_LET_GO.with(|n| n.set(n.get() + 1));
            }
            any |= went;
        }
        any
    }

    /// Read `rows` (`read`, inside the reader's panic fence) and fold the reading.
    #[cfg(test)]
    fn observe_rows(
        &mut self,
        session: u64,
        rows: &[String],
        read: impl FnOnce(&[String]) -> Option<Reading>,
    ) -> Step {
        let read =
            crate::reader_guard::read_or_none(session, "Claude Code lights", rows, || read(rows));
        self.observe_read(session, read)
    }

    /// Fold a frame's reading of `session`'s rows in ([`Self::observe`]).
    fn observe_read(&mut self, session: u64, read: crate::claude_footer::LightsRead) -> Step {
        match read {
            Some(reading) => self.observe(session, reading),
            None => Step::default(),
        }
    }

    /// Fold one frame's reading of `session`'s screen.
    fn observe(&mut self, session: u64, reading: Option<Reading>) -> Step {
        let s = self.sessions.entry(session).or_default();
        s.shown.fold(&reading.unwrap_or(Reading::UNKNOWN));
        let Some(p) = s.pending.as_ref() else {
            return Step::default();
        };
        Step {
            wake: reading.is_some_and(|r| r.mode.is_some_and(|m| m != p.mode_at_press)),
        }
    }

    /// Move `session`'s toggle along from the engine's `screen` (`None` without a composer).
    fn advance(&mut self, session: u64, screen: Option<&Screen>, now: Instant) -> Advance {
        let Some(s) = self.sessions.get_mut(&session) else {
            return Advance::Wait;
        };
        let Some(p) = s.pending.as_mut() else {
            return Advance::Wait;
        };
        let Some((mode, busy)) = screen.and_then(|screen| Some((screen.mode?, screen.busy))) else {
            return Advance::Wait;
        };
        s.shown.mode = Some(mode);
        if p.goal.reached(mode) {
            s.pending = None;
            s.refused = None;
            s.notice = Some((Light::Mode, now + REFUSAL_SHOWN));
            return Advance::Arrived;
        }
        if p.deadline <= now || mode == p.mode_at_press {
            return Advance::Wait;
        }
        if mode == p.started || p.seen.contains(&mode) || p.presses >= lights::MAX_MODE_PRESSES {
            let goal = p.goal;
            s.pending = None;
            s.refused = Some((
                Light::Mode,
                Refusal::NotInCycle(goal, mode),
                now + REFUSAL_SHOWN,
            ));
            return Advance::Lapped;
        }
        if busy {
            p.held_mid_turn = true;
            return Advance::Wait;
        }
        p.presses += 1;
        p.seen.push(mode);
        p.mode_at_press = mode;
        p.held_mid_turn = false;
        p.deadline = now + Duration::from_millis(lights::SETTLE_MS);
        Advance::Press { expect: mode }
    }

    /// Give up on every toggle past its deadline.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_mode_toggle",
            action = "Expire",
            project = "claude_lights::gesture_tests::project_mode_toggle"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_mode_toggle",
            action = "Rest",
            project = "claude_lights::gesture_tests::project_mode_toggle"
        )
    )]
    fn expire(&mut self, now: Instant) -> (bool, Vec<u64>) {
        let mut any = false;
        let mut refused = Vec::new();
        for (session, s) in &mut self.sessions {
            if let Some(p) = &s.pending
                && p.deadline <= now
            {
                let why = match s.shown.mode.unwrap_or(p.mode_at_press) {
                    at if p.goal.reached(at) => None,
                    at if p.held_mid_turn => Some(Refusal::StoppedMidTurn(at)),
                    at => Some(Refusal::StoppedIn(at)),
                };
                match why {
                    Some(why) => s.refused = Some((p.light, why, now + REFUSAL_SHOWN)),
                    None => s.notice = Some((p.light, now + REFUSAL_SHOWN)),
                }
                s.pending = None;
                refused.push(*session);
                any = true;
            }
            if s.refused.is_some_and(|(_, _, until)| until <= now) {
                s.refused = None;
                any = true;
            }
            if s.notice.is_some_and(|(_, until)| until <= now) {
                s.notice = None;
                any = true;
            }
            if s.fact_notice
                .as_ref()
                .is_some_and(|(_, until)| *until <= now)
            {
                s.fact_notice = None;
                any = true;
            }
            if s.autopilot_pending.is_some_and(|(_, until)| until <= now) {
                s.autopilot_pending = None;
                s.word = None;
                any = true;
            }
            if s.autopilot_pending.is_none()
                && s.word.is_some_and(|named| named.step(now).is_none())
            {
                s.word = None;
                any = true;
            }
            if s.switch_turning.is_some_and(|(_, until)| until <= now) {
                s.switch_turning = None;
                any = true;
            }
            if s.up_refused
                .as_ref()
                .is_some_and(|(_, until)| *until <= now)
            {
                s.up_refused = None;
                any = true;
            }
        }
        (any, refused)
    }

    /// Why the owner's last press of `session`'s ⬆️ was refused, while its label's slot says so
    /// ([`Self::refuse_upgrade`]); `expire` lets it go.
    pub(crate) fn up_refused(&self, session: u64) -> Option<&str> {
        self.sessions
            .get(&session)?
            .up_refused
            .as_ref()
            .map(|(words, _)| words.as_str())
    }

    /// Show `words`, why the owner's press of `session`'s ⬆️ was refused, in its label's slot
    /// until `until`.
    pub(crate) fn refuse_upgrade(&mut self, session: u64, words: String, until: Instant) {
        self.sessions.entry(session).or_default().up_refused = Some((words, until));
    }

    /// Pane `pane`'s chip block of `session` ([`App::claude_lights_block`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "the fit's inputs: the pane, its ground, where it stands, the two chips in focus, the \
                  room and Autopilot's title"
    )]
    fn lend_block(
        &mut self,
        session: u64,
        pane: usize,
        blank: RenderCell,
        chips_at: ChipAt,
        (selected, hover): (Option<Target>, Option<Target>),
        (room, named): (usize, bool),
        title: Option<TitleAt>,
    ) -> Option<Block> {
        let mut now = None;
        let mut clock = || *now.get_or_insert_with(lights_now);
        let session_key = match self.sessions.get(&session) {
            Some(s) => s.block_key(&mut clock),
            None => SessionLights::default().block_key(&mut clock),
        };
        let key = FitKey {
            blank,
            at: chips_at,
            selected,
            hover,
            room,
            named,
            title: title.map(|at| match at {
                TitleAt::Autopilot(i) => self.readings[i].laid,
                TitleAt::Fact(i) => self.fact_readings[i].laid,
                TitleAt::Panel(i) => self.panel_readings[i].laid,
            }),
            lights: session_key,
        };
        #[cfg(test)]
        let keep = KEEP_FITTED.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let keep = true;
        let entry = self
            .fitted
            .iter()
            .position(|f| f.session == session && f.pane == pane);
        if let Some(i) = entry {
            let f = &mut self.fitted[i];
            f.used = true;
            if keep && f.key == key {
                if !f.drawn {
                    return None;
                }
                if let Some(block) = f.block.take() {
                    return Some(block);
                }
            }
        }
        #[cfg(test)]
        crate::work_counts::footer_block_fitted();
        if self.spare_blocks.capacity() < SPARE_BLOCKS {
            self.spare_blocks.reserve_exact(SPARE_BLOCKS);
        }
        let mut block = entry
            .and_then(|i| self.fitted[i].block.take())
            .or_else(|| self.spare_blocks.pop())
            .unwrap_or_default();
        let autopilot = match title {
            Some(TitleAt::Autopilot(i)) => {
                let r = &self.readings[i];
                Some((r.whole.as_str(), r.short.as_str()))
            }
            _ => None,
        };
        let fact = match title {
            Some(TitleAt::Fact(i)) => {
                let r = &self.fact_readings[i];
                Some((r.whole.as_str(), r.short.as_str()))
            }
            _ => None,
        };
        let panel = match title {
            Some(TitleAt::Panel(i)) => {
                let r = &self.panel_readings[i];
                Some((r.whole.as_str(), r.short.as_str()))
            }
            _ => None,
        };
        let drawn = self.fit_into(
            session,
            blank,
            now.unwrap_or_else(lights_now),
            chips_at,
            selected.and_then(Target::chip),
            hover.and_then(Target::chip),
            (room, named),
            (autopilot, fact, panel),
            &mut block,
        );
        self.fits += 1;
        let fit = Fitted {
            session,
            pane,
            key,
            fit: self.fits,
            drawn,
            block: None,
            used: true,
        };
        match entry {
            Some(i) => self.fitted[i] = fit,
            None => self.fitted.push(fit),
        }
        if drawn {
            block.fit = self.fits;
            Some(block)
        } else {
            if self.spare_blocks.len() < SPARE_BLOCKS {
                self.spare_blocks.push(block);
            }
            None
        }
    }

    /// A painted block back from the painter ([`App::recycle_claude_lights_block`]).
    fn give_back(&mut self, block: Block) {
        if block.fit != 0
            && let Some(f) = self
                .fitted
                .iter_mut()
                .find(|f| f.fit == block.fit && f.drawn && f.block.is_none())
        {
            f.block = Some(block);
        } else if self.spare_blocks.len() < SPARE_BLOCKS {
            self.spare_blocks.push(block);
        }
    }

    /// [`Self::block_into`] with the window's own selection and hover.
    #[cfg(test)]
    fn block(
        &self,
        session: u64,
        blank: RenderCell,
        now: Instant,
        chips_at: ChipAt,
        keep: Keep,
    ) -> Block {
        let mine = |at: Option<(u64, Target)>| {
            at.filter(|(id, _)| *id == session)
                .and_then(|(_, t)| t.chip())
        };
        self.block_with(
            session,
            blank,
            now,
            chips_at,
            mine(self.selected),
            mine(self.hover),
            keep,
            None,
        )
    }

    /// The block ([`Self::block_into`]) `room` cells hold, down [`Keep::LADDER`].
    #[allow(
        clippy::too_many_arguments,
        reason = "the block's inputs: the session, its ground, the clock, where it stands, the \
                  pointer's and the keyboard's chips and the room"
    )]
    fn fitted(
        &self,
        session: u64,
        blank: RenderCell,
        now: Instant,
        chips_at: ChipAt,
        selected: Option<Chip>,
        hover: Option<Chip>,
        room: usize,
    ) -> Option<Block> {
        let mut block = Block::default();
        self.fit_into(
            session,
            blank,
            now,
            chips_at,
            selected,
            hover,
            (room, true),
            (None, None, None),
            &mut block,
        )
        .then_some(block)
    }

    /// [`Self::fitted`] laid into `block` (its buffers reused, cleared first).
    #[allow(
        clippy::too_many_arguments,
        reason = "fitted's own inputs, Autopilot's title as its reading built it, and the block \
                  it lays into"
    )]
    fn fit_into(
        &self,
        session: u64,
        blank: RenderCell,
        now: Instant,
        chips_at: ChipAt,
        selected: Option<Chip>,
        hover: Option<Chip>,
        (room, named): (usize, bool),
        titles: Titles<'_>,
        block: &mut Block,
    ) -> bool {
        let at_rest = SessionLights::default();
        let s = self.sessions.get(&session).unwrap_or(&at_rest);
        let inks = Inks::of(blank, chips_at);
        let glyph_rung = s.glyph_rung();
        let width_at = |keep: Keep, named: Option<(&'static str, u32)>| {
            let drawn = drawn_at(s, now, selected, hover, keep);
            chips_width_at(s, Chip::row().filter(|c| drawn(*c)), keep, inks, named)
        };
        let Some(keep) = Keep::LADDER
            .into_iter()
            .filter(|keep| *keep != Keep::Revealed || selected.is_some())
            .filter(|keep| *keep != Keep::GlyphOnly || glyph_rung)
            .find(|keep| (1..=room).contains(&width_at(*keep, None)))
        else {
            return false;
        };
        // The rung is chosen at rest: the mode's name a click brought up takes only room the rest
        // leaves, and never a chip's place.
        let named = s
            .word_at(now)
            .filter(|_| named && matches!(keep, Keep::Revealed | Keep::Drawn))
            .filter(|_| drawn_at(s, now, selected, hover, keep)(Chip::Autopilot))
            .filter(|&word| width_at(keep, Some(word)) <= room);
        self.block_into(
            session,
            blank,
            now,
            chips_at,
            selected,
            hover,
            (keep, named),
            titles,
            block,
        );
        if !matches!(keep, Keep::Revealed | Keep::Drawn) {
            block.holds = room;
        }
        true
    }

    /// [`Self::block_into`] into a fresh block, as the tests read it.
    #[cfg(test)]
    #[allow(
        clippy::too_many_arguments,
        reason = "block_into's own inputs, less the block it lays into"
    )]
    fn block_with(
        &self,
        session: u64,
        blank: RenderCell,
        now: Instant,
        chips_at: ChipAt,
        selected: Option<Chip>,
        hover: Option<Chip>,
        keep: Keep,
        autopilot: Option<(&str, &str)>,
    ) -> Block {
        let mut block = Block::default();
        self.block_into(
            session,
            blank,
            now,
            chips_at,
            selected,
            hover,
            (
                keep,
                self.sessions
                    .get(&session)
                    .and_then(|s| s.word_at(now))
                    .filter(|_| matches!(keep, Keep::Revealed | Keep::Drawn)),
            ),
            (autopilot, None, None),
            &mut block,
        );
        block
    }

    /// `session`'s chip block at `keep`.
    #[allow(
        clippy::too_many_arguments,
        reason = "the block's inputs: the session, its ground, the clock, where it stands, the \
                  pointer's and the keyboard's chips, how much of the row is kept, Autopilot's \
                  title as its reading built it, and the block it lays into"
    )]
    fn block_into(
        &self,
        session: u64,
        blank: RenderCell,
        now: Instant,
        chips_at: ChipAt,
        selected: Option<Chip>,
        hover: Option<Chip>,
        (keep, named): (Keep, Option<(&'static str, u32)>),
        (autopilot, fact, panel): Titles<'_>,
        out: &mut Block,
    ) {
        let at_rest = SessionLights::default();
        let s = self.sessions.get(&session).unwrap_or(&at_rest);
        let focus = selected.or(hover);
        let refused = s.refused.filter(|(_, _, until)| *until > now);
        let noticed = s
            .notice
            .filter(|(_, until)| *until > now)
            .map(|(light, _)| light);
        let pending = s.pending.as_ref().map(|p| p.light);
        let fact_noticed = s
            .fact_notice
            .as_ref()
            .filter(|(_, until)| *until > now)
            .map(|(notice, _)| (notice.text(), matches!(notice, FactNotice::Said(_))));
        let drawn = drawn_at(s, now, selected, hover, keep);
        let titled = [
            focus,
            pending.map(Chip::Light),
            refused.map(|(l, _, _)| Chip::Light(l)),
            noticed.map(Chip::Light),
        ]
        .into_iter()
        .flatten()
        .find(|chip| drawn(*chip))
        .map(ChipTitle::Chip)
        .or_else(|| {
            s.glass
                .lit
                .filter(|_| drawn(Chip::Autopilot))
                .map(ChipTitle::Act)
        });
        let inks = Inks::of(blank, chips_at);
        let title_ink = inks.title;
        let Block {
            title,
            lean,
            short_title,
            reason,
            lights: cells,
            cols,
            holds,
            word,
            fit: _,
        } = out;
        title.clear();
        for form in lean.iter_mut() {
            form.clear();
        }
        short_title.clear();
        cells.clear();
        cols.clear();
        *reason = false;
        *word = 0;
        let lay = |text: &str, ink: [u8; 3], out: &mut Vec<RenderCell>| {
            out.extend(text.chars().chain("  ".chars()).map(|ch| {
                let mut c = blank;
                c.ch = ch;
                c.fg = ink;
                c
            }));
        };
        let lay_lean =
            |whole: &str, out: &mut [Vec<RenderCell>; LEAN_FORMS]| {
                for ((head, tail), form) in lean_titles(whole).into_iter().zip(out.iter_mut()) {
                    if !head.is_empty() {
                        form.extend(head.chars().chain(tail.chars()).chain("  ".chars()).map(
                            |ch| {
                                let mut c = blank;
                                c.ch = ch;
                                c.fg = title_ink;
                                c
                            },
                        ));
                    }
                }
            };
        let titled = titled.filter(|_| fact.is_none());
        let mut act_tone = None;
        let focus_titled = matches!(titled, Some(ChipTitle::Chip(chip)) if focus == Some(chip));
        let fact_noticed = fact_noticed
            .filter(|(_, said)| fact.is_none() || *said)
            .map(|(text, _)| text);
        match titled {
            _ if !focus_titled && let Some(text) = fact_noticed => {
                lay(text, title_ink, title);
                lay(text, title_ink, short_title);
            }
            None if let Some((whole, short)) = fact => {
                lay(whole, title_ink, title);
                lay(short, title_ink, short_title);
            }
            None if let Some((whole, short)) = panel => {
                lay(whole, title_ink, title);
                lay(short, title_ink, short_title);
            }
            Some(ChipTitle::Chip(Chip::Autopilot))
                if let Some((whole, short)) =
                    autopilot.filter(|_| s.autopilot_pending.is_none()) =>
            {
                lay(whole, title_ink, title);
                lay_lean(whole, lean);
                lay(short, title_ink, short_title);
            }
            Some(ChipTitle::Chip(chip)) => {
                let words = title_of(s, chip, refused);
                *reason = words.reason;
                lay(&words.whole, title_ink, title);
                match (chip, &words.lean) {
                    (Chip::Autopilot, _) => lay_lean(&words.whole, lean),
                    (_, Some(form)) => lay(form, title_ink, &mut lean[LEAN_FORMS - 1]),
                    (_, None) => {}
                }
                lay(&words.short, title_ink, short_title);
            }
            Some(ChipTitle::Act(act)) => {
                let view = s.autopilot.unwrap_or(AutopilotView::Unhosted);
                let (glyph, word, tone) = act_label(view, act);
                act_tone = Some(tone);
                let ink = inks.of_tone(tone);
                title.extend(
                    glyph
                        .chars()
                        .chain(std::iter::once(' '))
                        .chain(word.chars())
                        .chain("  ".chars())
                        .map(|ch| {
                            let mut c = blank;
                            c.ch = ch;
                            c.fg = ink;
                            c
                        }),
                );
            }
            None => {}
        }
        let mut before = None;
        for chip in Chip::row().filter(|c| drawn(*c)) {
            if !cells.is_empty() {
                cells.push(blank);
                if before.is_some_and(|(chip, worded)| gap_after(chip, worded) > 1) {
                    let mut rule = blank;
                    rule.ch = CHIP_RULE;
                    rule.fg = inks.quiet;
                    cells.push(rule);
                    cells.push(blank);
                }
            }
            let face = face_of(
                chip,
                s,
                keep,
                inks.at(cells.len()),
                named.filter(|_| chip == Chip::Autopilot),
            );
            before = Some((chip, face.worded()));
            let start = cells.len();
            for (ch, ink) in face.cells() {
                let mut c = blank;
                c.ch = ch;
                c.fg = ink;
                if focus == Some(chip) {
                    c.bold = true;
                    c.underline = UnderlineStyle::Single;
                }
                cells.push(c);
                if aterm_grapheme::char_width(ch) == 2 {
                    let mut tail = c;
                    tail.ch = ' ';
                    tail.wide = true;
                    cells.push(tail);
                }
            }
            cols.push((start..cells.len(), chip));
        }
        // An act's label stands after the chips and the two blanks the rule lays before a title
        // ([`crate::claude_footer`]'s `compose_rule_spans`): its letter is that column's.
        if let Some(tone) = act_tone {
            let ink = inks.at(cells.len() + 2).of_tone(tone);
            for c in title.iter_mut() {
                c.fg = ink;
            }
        }
        *holds = cells.len();
        if named.is_some() {
            let rest = chips_width_at(s, Chip::row().filter(|c| drawn(*c)), keep, inks, None);
            *word = cells.len().saturating_sub(rest);
        }
    }
}

/// Whether the keyboard's reveal draws and walks `light` in a session that shows `shown`.
fn revealed(light: Light, shown: &Reading) -> bool {
    shown.state(light) != LightState::Unknown
}

/// Whether `s`'s block draws a chip at `keep`.
fn drawn_at(
    s: &SessionLights,
    now: Instant,
    selected: Option<Chip>,
    hover: Option<Chip>,
    keep: Keep,
) -> impl Fn(Chip) -> bool + '_ {
    let focus = selected.or(hover);
    let refused = s
        .refused
        .filter(|(_, _, until)| *until > now)
        .map(|(light, _, _)| light);
    let noticed = s
        .notice
        .filter(|(_, until)| *until > now)
        .map(|(light, _)| light);
    let pending = s.pending.as_ref().map(|p| p.light);
    let harness = s.harness_chips();
    let team = s.team.is_some();
    // The mode's one home on a rewritten row: its light is kept at every rung, the pill being
    // gone (a row too narrow for the whole pill is never rewritten).
    let home = s.rewritten;
    move |chip: Chip| {
        (keep.keeps(chip) || (home && matches!(chip, Chip::Light(_))))
            && match chip {
                Chip::Autopilot => harness,
                Chip::Team => team,
                Chip::Light(light) => {
                    lights::deviates(light, &s.shown)
                        || (s.rewritten && s.shown.mode.is_some())
                        || pending == Some(light)
                        || refused == Some(light)
                        || noticed == Some(light)
                        || focus == Some(chip)
                        || (keep == Keep::Revealed
                            && selected.is_some()
                            && revealed(light, &s.shown))
                }
            }
    }
}

/// The cells `chips` take in the row at `keep`, each its face ([`face_of`]: at rest, or the
/// Autopilot chip worded by `named`, the mode's name a click brought up) and set off from the
/// next by [`gap_after`] it.
fn chips_width_at(
    s: &SessionLights,
    chips: impl Iterator<Item = Chip>,
    keep: Keep,
    inks: Inks,
    named: Option<(&'static str, u32)>,
) -> usize {
    let mut width = 0;
    let mut before: Option<(Chip, bool)> = None;
    for chip in chips {
        let face = face_of(
            chip,
            s,
            keep,
            inks,
            named.filter(|_| chip == Chip::Autopilot),
        );
        width += before.map_or(0, |(chip, worded)| gap_after(chip, worded)) + face.width();
        before = Some((chip, face.worded()));
    }
    width
}

/// The `─` that sets a worded Autopilot chip off from the next ([`gap_after`]): the rule's own,
/// which the rule's layout draws in its ink ([`Block::rule_gaps`]).
pub(crate) const CHIP_RULE: char = '\u{2500}';

/// The cells between `chip` and the next: a blank, the rule's `─` and a blank after an Autopilot
/// chip with words after its glyph (`⊘ off ─ yolo`, `✓ landed ─ yolo`, a click's mode name
/// `⤓ land ─ yolo`), which would otherwise read as one phrase with the mode's word; one blank
/// after a bare glyph (`∞ yolo`, `⤓ yolo`) and after the mode's chip, whose word the next chip's
/// glyph sets off (`∞ yolo ⇠ lab-sweep`).
fn gap_after(chip: Chip, worded: bool) -> usize {
    if worded && chip == Chip::Autopilot {
        3
    } else {
        1
    }
}

/// A chip's title: whole, a leaner form where it has one, short, and whether it is a light's
/// refusal (its reason outranks the facts, `footer::fit_rule`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct TitleText {
    whole: String,
    /// The light's one lean form, laid in the last lean slot ([`LEAN_FORMS`]); the Autopilot
    /// chip's lean forms are cut from its whole ([`lean_titles`]).
    lean: Option<String>,
    short: String,
    reason: bool,
}

/// What the mode light's title and its accessible description say ([`light_title`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct LightTitle {
    text: TitleText,
    /// The same as sentences, for a screen reader (no ` — ` or ` · ` read aloud).
    spoken: String,
}

/// `words` as a sentence: its first letter capitalized and a full stop after it.
fn sentence(words: &str) -> String {
    let words = words.trim_end_matches('\u{2026}');
    let mut chars = words.chars();
    let first = chars.next().map(|c| c.to_uppercase().collect::<String>());
    format!("{}{}.", first.unwrap_or_default(), chars.as_str())
}

/// What `light`'s title says: what its state does (the mode's gist; Claude Code's own name for the
/// mode where the chip wears aterm's word for it, `yolo` or `lame`), then where a click goes
/// (`· click: lame`); a toggle in flight says where it is going, a refusal its reason. The chip
/// already shows the state's word, so the title explains it rather than repeating it.
fn light_title(
    s: &SessionLights,
    light: Light,
    refused: Option<(Light, Refusal, Instant)>,
) -> LightTitle {
    let rewritten = s.rewritten;
    let click = |goal: Goal| format!("{TITLE_CLICK}{}", goal.words(rewritten));
    let spoken_click = |goal: Goal| format!(" Click for {}.", goal.words(rewritten));
    if let Some((l, why, _)) = refused
        && l == light
    {
        let short = why.says(rewritten);
        let goal = why.click();
        return LightTitle {
            spoken: format!(
                "{}{}",
                sentence(&short),
                goal.map_or_else(String::new, spoken_click)
            ),
            text: TitleText {
                whole: format!("{short}{}", goal.map_or_else(String::new, click)),
                lean: None,
                short,
                reason: true,
            },
        };
    }
    if let Some(p) = s.pending.as_ref().filter(|p| p.light == light) {
        let to = p.goal.words(rewritten);
        return LightTitle {
            spoken: format!("Switching to {to}."),
            text: TitleText {
                whole: format!("switching to {to}\u{2026}"),
                lean: None,
                short: "switching\u{2026}".to_owned(),
                reason: false,
            },
        };
    }
    let Some(mode) = s.shown.mode else {
        let words = "not on screen right now";
        return LightTitle {
            spoken: sentence(words),
            text: TitleText {
                whole: words.to_owned(),
                lean: None,
                short: words.to_owned(),
                reason: false,
            },
        };
    };
    let goal = Goal::of_mode(mode);
    let gist = mode.gist();
    let named = (mode.word_on_glass(rewritten) != mode.word()).then(|| mode.claude_name());
    let lean = format!("{gist}{}", click(goal));
    let spoken = match named {
        Some(name) => format!("{} ({name}).", sentence(gist).trim_end_matches('.')),
        None => sentence(gist),
    };
    LightTitle {
        spoken: format!("{spoken}{}", spoken_click(goal)),
        text: TitleText {
            whole: named.map_or_else(
                || lean.clone(),
                |name| format!("{gist}{TITLE_EXPLAINS}{name}{}", click(goal)),
            ),
            lean: named.map(|_| lean),
            short: gist.to_owned(),
            reason: false,
        },
    }
}

/// What `chip`'s title says ([`TitleText`], each form without its trailing gap).
fn title_of(
    s: &SessionLights,
    chip: Chip,
    refused: Option<(Light, Refusal, Instant)>,
) -> TitleText {
    let plain = |(whole, short): (String, String)| TitleText {
        whole,
        lean: None,
        short,
        reason: false,
    };
    match chip {
        Chip::Light(light) => light_title(s, light, refused).text,
        Chip::Autopilot if s.autopilot_pending.is_some() => plain((
            "autopilot: switching\u{2026}".to_owned(),
            "switching\u{2026}".to_owned(),
        )),
        Chip::Autopilot => {
            let view = s.autopilot.unwrap_or(AutopilotView::Unhosted);
            plain(autopilot_title(
                view,
                &s.glass,
                "",
                None,
                TitleWords::default(),
            ))
        }
        Chip::Team => plain(team_title(s.team.as_ref(), lights_now())),
    }
}

/// The Team chip's title, whole and short.
fn team_title(face: Option<&TeamFace>, now: Instant) -> (String, String) {
    let Some(face) = face else {
        return (String::new(), String::new());
    };
    let short = match face {
        TeamFace::Yours => "you have this tab".to_owned(),
        TeamFace::Worker { label, task, .. } => {
            let task = task.map_or_else(String::new, |(state, since)| {
                format!(
                    " \u{b7} task {state} {}",
                    crate::presence::fmt_dur(now.saturating_duration_since(since))
                )
            });
            format!("worker of {label}{task}")
        }
        TeamFace::Manager { workers, .. } => format!("manages {workers}"),
    };
    match face.act() {
        Some(act) => {
            let act = match act {
                TeamAct::GoTo(_) => "click to go there",
                TeamAct::Overview => "click to see them",
                TeamAct::HandBack => "click to hand it back",
            };
            (format!("{short} \u{2014} {act}"), short)
        }
        None => (short.clone(), short),
    }
}

/// A harness chip's title cut to its head before the first ` — ` or ` · `.
fn short_title(title: &str) -> &str {
    [" \u{2014} ", " \u{b7} "]
        .iter()
        .filter_map(|cut| title.find(cut))
        .min()
        .map_or(title, |at| &title[..at])
}

/// A chip's face as the painter lays it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Face<'a> {
    glyph: &'a str,
    glyph_ink: [u8; 3],
    word: Option<&'a str>,
    clock: Option<Clock>,
    word_ink: [u8; 3],
}

impl<'a> Face<'a> {
    /// A face of one ink.
    const fn plain(glyph: &'a str, word: Option<&'a str>, ink: [u8; 3]) -> Self {
        Self {
            glyph,
            glyph_ink: ink,
            word,
            clock: None,
            word_ink: ink,
        }
    }

    /// Whether anything follows the glyph.
    const fn worded(&self) -> bool {
        self.word.is_some() || self.clock.is_some()
    }

    /// The cells it takes: two for a wide character (`✋`, a cjk name). A face with no glyph
    /// (on a rewritten row `yolo`, `auto`, `lame`) is its words alone, no blank before them.
    fn width(&self) -> usize {
        cells_of(self.glyph)
            + if self.worded() {
                usize::from(!self.glyph.is_empty())
                    + self.word.map_or(0, cells_of)
                    + self.clock.map_or(0, |_| 5)
            } else {
                0
            }
    }

    /// Its characters, each with its ink, in the order they are laid.
    fn cells(self) -> impl Iterator<Item = (char, [u8; 3])> {
        let worded = self.worded();
        let word_ink = self.word_ink;
        let clock = self.clock.map(|c| c.0);
        let blank = worded && !self.glyph.is_empty();
        self.glyph
            .chars()
            .map(move |ch| (ch, self.glyph_ink))
            .chain(
                blank
                    .then_some(' ')
                    .into_iter()
                    .chain(self.word.into_iter().flat_map(str::chars))
                    .chain(clock.into_iter().flatten().map(char::from))
                    .map(move |ch| (ch, word_ink)),
            )
    }
}

/// A chip's face at `keep` ([`Face`]); the Autopilot chip's worded by `named`, the mode's name a
/// click brought up and its fade's step ([`SessionLights::word_at`]), where the block lays it
/// (`⤓ land`: after the glyph, which keeps its cell under the pointer).
fn face_of<'a>(
    chip: Chip,
    s: &'a SessionLights,
    keep: Keep,
    inks: Inks,
    named: Option<(&'static str, u32)>,
) -> Face<'a> {
    let ink_of = |tone: ChipTone| inks.of_tone(tone);
    let ink = inks.ink;
    match chip {
        Chip::Light(light) => {
            let (glyph, word, ink) = light_face(light, s, inks);
            Face::plain(glyph, Some(word), ink)
        }
        Chip::Autopilot => {
            let view = s.autopilot.unwrap_or(AutopilotView::Unhosted);
            if s.autopilot_pending.is_some() {
                return Face::plain(PENDING_GLYPH, None, ink_of(view.tone()));
            }
            let face = ChipFace::of(view, &s.glass);
            let (glyph_tone, word_tone) = face.tones();
            if keep == Keep::GlyphOnly {
                return Face::plain(face.rung(), None, ink_of(glyph_tone));
            }
            let glyph = Face::plain(face.glyph(), None, ink_of(glyph_tone));
            match named {
                Some((name, step)) => Face {
                    word: Some(name),
                    word_ink: ChipWord::ink(step, ink_of(word_tone), inks.ground),
                    ..glyph
                },
                None => Face {
                    word: face.word(),
                    clock: face.clock(&s.glass),
                    word_ink: ink_of(word_tone),
                    ..glyph
                },
            }
        }
        Chip::Team => match &s.team {
            Some(TeamFace::Yours) => Face::plain("\u{270b}", Some("you"), ink),
            Some(TeamFace::Worker { label, .. }) => {
                Face::plain("\u{21e0}", Some(&**label), inks.quiet)
            }
            Some(TeamFace::Manager { mark, .. }) => Face::plain(mark, None, inks.quiet),
            None => Face::plain("", None, inks.quiet),
        },
    }
}

/// The grid cells `text` takes, one per character but two for a wide one (`✋`, a cjk name).
fn cells_of(text: &str) -> usize {
    text.chars()
        .map(|ch| aterm_grapheme::char_width(ch).clamp(1, 2))
        .sum()
}

/// What the block's title slot holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChipTitle {
    /// The chip's title, whole and short.
    Chip(Chip),
    /// The act's label, whole.
    Act(Act),
}

/// A light's glyph, word and ink: on a rewritten row `yolo`, `auto` and `lame` stand as words
/// alone ([`Mode::glyph_on_glass`]), `◌` stands for the glyph while a toggle is in flight, and the
/// ink is the mode's ([`Mode::ink`]): an expected mode in the value ink, the rest in the row's.
fn light_face(
    light: Light,
    s: &SessionLights,
    inks: Inks,
) -> (&'static str, &'static str, [u8; 3]) {
    let switching = s.pending.as_ref().is_some_and(|p| p.light == light);
    let glyph = |own: &'static str| if switching { PENDING_GLYPH } else { own };
    match s.shown.mode {
        Some(mode) => (
            glyph(mode.glyph_on_glass(s.rewritten)),
            mode.word_on_glass(s.rewritten),
            match mode.ink() {
                ModeInk::Value => inks.title,
                ModeInk::Row => inks.ink,
            },
        ),
        None => (glyph("\u{25CC}"), light.word(), inks.quiet),
    }
}

impl ChipFace {
    /// Whether the harness types under this face (`∞`, `∞ ⤓18:00`, `⤓`): the faces a stop is
    /// said on (§14.3), never an Off, landed or quiet one.
    #[must_use]
    pub(crate) const fn carries(self) -> bool {
        matches!(
            self,
            ChipFace::Cruise | ChipFace::CruiseUntil | ChipFace::Land
        )
    }

    /// Every face, in order.
    #[cfg(test)]
    pub(crate) const ALL: [ChipFace; 10] = [
        ChipFace::Cruise,
        ChipFace::CruiseUntil,
        ChipFace::Land,
        ChipFace::Landed,
        ChipFace::Theirs,
        ChipFace::Failed,
        ChipFace::Off,
        ChipFace::MasterOff,
        ChipFace::AgentOff(aterm_phase::Program::Claude),
        ChipFace::Unhosted,
    ];

    /// The face a chip under `view` wears with `glass`.
    #[must_use]
    pub(crate) const fn of(view: AutopilotView, glass: &ChipGlass) -> Self {
        match view {
            AutopilotView::Cruise if glass.land_at.is_some() => ChipFace::CruiseUntil,
            AutopilotView::Cruise => ChipFace::Cruise,
            AutopilotView::Land => match glass.task {
                Some(TaskFace::Landed) => ChipFace::Landed,
                Some(TaskFace::Theirs) => ChipFace::Theirs,
                Some(TaskFace::Failed) => ChipFace::Failed,
                Some(TaskFace::Open) | None => ChipFace::Land,
            },
            AutopilotView::Off => ChipFace::Off,
            AutopilotView::MasterOff => ChipFace::MasterOff,
            AutopilotView::AgentOff(agent) => ChipFace::AgentOff(agent),
            AutopilotView::Unhosted => ChipFace::Unhosted,
        }
    }

    /// Whether it is a standing face.
    #[must_use]
    pub(crate) const fn standing(self) -> bool {
        matches!(
            self,
            ChipFace::CruiseUntil | ChipFace::Landed | ChipFace::Theirs | ChipFace::Failed
        )
    }

    /// The chip's glyph: `∞`, `⤓`, `✓`, `✕`, `○` or `⊘`.
    #[must_use]
    pub(crate) const fn glyph(self) -> &'static str {
        match self {
            ChipFace::Cruise | ChipFace::CruiseUntil => AutopilotView::Cruise.glyph(),
            ChipFace::Land | ChipFace::Theirs => AutopilotView::Land.glyph(),
            ChipFace::Landed => "\u{2713}",
            ChipFace::Failed => FAILED_GLYPH,
            ChipFace::Off => AutopilotView::Off.glyph(),
            ChipFace::MasterOff => AutopilotView::MasterOff.glyph(),
            ChipFace::AgentOff(agent) => AutopilotView::AgentOff(agent).glyph(),
            ChipFace::Unhosted => AutopilotView::Unhosted.glyph(),
        }
    }

    /// The chip's [`Keep::GlyphOnly`] rung.
    #[must_use]
    pub(crate) const fn rung(self) -> &'static str {
        self.glyph()
    }

    /// The word drawn a space after the glyph.
    #[must_use]
    pub(crate) const fn word(self) -> Option<&'static str> {
        match self {
            ChipFace::Cruise => AutopilotView::Cruise.word(),
            ChipFace::CruiseUntil => Some(AutopilotView::Land.glyph()),
            ChipFace::Land => AutopilotView::Land.word(),
            ChipFace::Landed => Some("landed"),
            ChipFace::Theirs => Some("quiet"),
            ChipFace::Failed => Some("failed"),
            ChipFace::Off => AutopilotView::Off.word(),
            ChipFace::MasterOff => AutopilotView::MasterOff.word(),
            ChipFace::AgentOff(agent) => AutopilotView::AgentOff(agent).word(),
            ChipFace::Unhosted => AutopilotView::Unhosted.word(),
        }
    }

    /// The clock drawn right after the word, with no space (`⤓18:00`).
    #[must_use]
    pub(crate) fn clock(self, glass: &ChipGlass) -> Option<Clock> {
        match self {
            ChipFace::CruiseUntil => glass.land_at.map(|at| at.clock),
            _ => None,
        }
    }

    /// The glyph's and word's tones.
    #[must_use]
    pub(crate) const fn tones(self) -> (ChipTone, ChipTone) {
        match self {
            ChipFace::Cruise | ChipFace::CruiseUntil | ChipFace::Land | ChipFace::Landed => {
                (ChipTone::Letter, ChipTone::Letter)
            }
            ChipFace::Theirs
            | ChipFace::Failed
            | ChipFace::Off
            | ChipFace::MasterOff
            | ChipFace::AgentOff(_)
            | ChipFace::Unhosted => (ChipTone::Quiet, ChipTone::Quiet),
        }
    }

    /// The mode phrase: the title's head and the whole short title.
    #[must_use]
    pub(crate) fn phrase(
        self,
        glass: &ChipGlass,
        today: Option<i64>,
    ) -> std::borrow::Cow<'static, str> {
        let fixed = match self {
            ChipFace::Cruise => "autopilot: cruise",
            ChipFace::CruiseUntil => {
                let Some(at) = glass.land_at else {
                    return "autopilot: cruise".into();
                };
                return format!("autopilot: cruise until {}, then land", timer_words(at)).into();
            }
            ChipFace::Land => "autopilot: land",
            ChipFace::Landed => {
                let phrase = "autopilot: land, landed";
                return match glass.task_at {
                    Some(at) => format!("{phrase} {}", at.words(today)).into(),
                    None => phrase.into(),
                };
            }
            ChipFace::Theirs => "autopilot: land, quiet",
            ChipFace::Failed => "autopilot: land, failed",
            ChipFace::Off => "autopilot: off",
            ChipFace::MasterOff => "harness off in Settings",
            ChipFace::AgentOff(agent) => crate::autopilot::agent_switch_off(agent),
            ChipFace::Unhosted => "no harness: this aterm has no control socket",
        };
        fixed.into()
    }

    /// What the mode does, after the phrase and ` — ` in the whole title.
    #[must_use]
    pub(crate) fn explanation(
        self,
        _glass: &ChipGlass,
        words: TitleWords<'_>,
        today: Option<i64>,
    ) -> Option<std::borrow::Cow<'static, str>> {
        use crate::autopilot::mode_words;
        use aterm_agent::supervise::Autopilot;
        if let Some(stopped) = words.stop_under(self) {
            let stop = crate::harness_glass::stop_words(
                stopped,
                crate::harness_glass::HoldForm::Title,
                words.switch_under(self),
            );
            let held = words
                .hold
                .filter(|_| self == ChipFace::Land)
                .and_then(|hold| {
                    crate::harness_glass::hold_words(
                        hold,
                        words.need,
                        crate::harness_glass::HoldForm::Title,
                        today,
                        words.said,
                    )
                });
            return Some(match held {
                Some(held) => format!("{stop}{}{held}", crate::native_ui::FACT_SEPARATOR).into(),
                None => stop.into(),
            });
        }
        let fixed = match self {
            ChipFace::Cruise => mode_words(Autopilot::Cruise).chip,
            ChipFace::CruiseUntil => crate::autopilot::TIMED_CRUISE_CHIP,
            ChipFace::Land => {
                let sentence = mode_words(Autopilot::Land).chip;
                let Some(hold) = words.hold else {
                    return Some(sentence.into());
                };
                let held = crate::harness_glass::hold_words(
                    hold,
                    words.need,
                    crate::harness_glass::HoldForm::Title,
                    today,
                    words.said,
                );
                return Some(match held {
                    Some(held) if hold.read() => held.into(),
                    Some(since) => {
                        format!("{sentence}{}{since}", crate::native_ui::FACT_SEPARATOR).into()
                    }
                    None => sentence.into(),
                });
            }
            ChipFace::Landed => {
                return Some(
                    aterm_agent::supervise::mode_words::landed_brief(words.receipt).into(),
                );
            }
            ChipFace::Theirs => {
                "a person's own exchange; the harness types nothing until someone else's turn ends"
            }
            ChipFace::Failed => FAILED_EXPLAINS,
            ChipFace::Off => "hands off",
            ChipFace::MasterOff | ChipFace::AgentOff(_) | ChipFace::Unhosted => return None,
        };
        Some(fixed.into())
    }

    /// What a click does (`click: …`).
    #[must_use]
    pub(crate) const fn click(self, _glass: &ChipGlass) -> &'static str {
        match self {
            ChipFace::Cruise => "land",
            ChipFace::CruiseUntil => "land now",
            ChipFace::Land | ChipFace::Landed | ChipFace::Theirs | ChipFace::Failed => "off",
            ChipFace::Off => "cruise",
            ChipFace::MasterOff | ChipFace::AgentOff(_) => "turn the harness on",
            ChipFace::Unhosted => "open Settings \u{25b8} Harness",
        }
    }

    /// The chip's accessibility label.
    #[must_use]
    pub(crate) fn spoken(self, glass: &ChipGlass, today: Option<i64>) -> String {
        let lands = || match glass.land_at {
            Some(LandAtGlass {
                clock,
                other_day: Some(day),
            }) => format!(" Lands {} at {}.", day.long(), clock.as_str()),
            Some(LandAtGlass { clock, .. }) => format!(" Lands at {}.", clock.as_str()),
            None => String::new(),
        };
        match self {
            ChipFace::Cruise => "Autopilot: cruise.".to_owned(),
            ChipFace::CruiseUntil => format!("Autopilot: cruise.{}", lands()),
            ChipFace::Land => "Autopilot: land.".to_owned(),
            ChipFace::Landed => match glass.task_at {
                Some(stamp) => format!("Autopilot: land. Landed {}.", stamp.spoken(today)),
                None => "Autopilot: land. Landed.".to_owned(),
            },
            ChipFace::Theirs => {
                "Autopilot: land. Quiet: a person is driving the session.".to_owned()
            }
            ChipFace::Failed => "Autopilot: land. Failed: the worker could not finish.".to_owned(),
            ChipFace::Off => "Autopilot: off.".to_owned(),
            ChipFace::MasterOff => "Autopilot: off. The harness is off in Settings.".to_owned(),
            ChipFace::AgentOff(agent) => format!(
                "Autopilot: off. {} in Settings.",
                crate::autopilot::agent_switch_off(agent)
            ),
            ChipFace::Unhosted => "Autopilot: off. This aterm has no control socket.".to_owned(),
        }
    }
}

/// [`ChipFace::Failed`]'s glyph, the Workers row's mark for a failed worker
/// (`native_workers::RowState::Failed`).
pub(crate) const FAILED_GLYPH: &str = "\u{2715}";

/// What [`ChipFace::Failed`] says the mode does now.
pub(crate) const FAILED_EXPLAINS: &str =
    "the worker could not finish; the harness types nothing until someone else's turn";

/// An armed timer's time as the title says it.
fn timer_words(at: LandAtGlass) -> String {
    match at.other_day {
        Some(day) => format!("{} {}", day.short(), at.clock.as_str()),
        None => at.clock.as_str().to_owned(),
    }
}

/// The Autopilot title's parts after its mode phrase ([`autopilot_title`]).
const TITLE_EXPLAINS: &str = " \u{2014} ";
const TITLE_PAUSE: &str = " \u{b7} pausing until ";
const TITLE_LAST: &str = " \u{b7} last: ";
const TITLE_CLICK: &str = " \u{b7} click: ";
const TITLE_MORE: &str = " \u{b7} right-click: more";

/// How many leaner forms the Autopilot title has ([`lean_titles`], `footer::LightsWidth::lean`).
pub(crate) const LEAN_FORMS: usize = aterm_agent::harness::footer::LEAN_FORMS;

/// What the autopilot chip's title says beside its glass.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TitleWords<'a> {
    /// Land's hold as the loop told it, whatever the face.
    pub(crate) hold: Option<&'a crate::harness_glass::HeldGlass>,
    /// The hold's own words, where the loop told them (slice 4b, [`crate::App::hold_said`]).
    pub(crate) said: Option<&'a aterm_agent::harness::hold_said::HoldSaid>,
    /// The session's need, where the published one is a harness's own ask.
    pub(crate) need: Option<&'a crate::needs::HarnessAsk>,
    /// The arm that stopped the typing, as the loop told it, whatever the face (§14.3).
    pub(crate) stopped: Option<aterm_agent::harness::acts::HeldBy>,
    /// The person's switch to Land would ask once more over the stop, as the loop told it
    /// (`crate::harness_glass::SessionGlass::stop_switch`): said only under Cruise
    /// ([`Self::switch_under`]).
    pub(crate) switch: bool,
    /// What aterm checked at the landing, beside a landed face (§14.9, slice 9c): the landed
    /// title's explanation says it in brief (`mode_words::landed_brief`) and its screen-reader
    /// description whole (`mode_words::landed_words`); `None` says aterm has no record of checking it.
    pub(crate) receipt: Option<&'a aterm_agent::harness::acts::LandReceipt>,
}

/// What a title reads of a session beside its glass, copied out: Land's hold, the need, the
/// stop, whether the person's switch would ask over it, the landing's receipt, and the hold's
/// own words
/// ([`TitleWords`]).
pub(crate) type ReadWords = (
    Option<crate::harness_glass::HeldGlass>,
    Option<crate::needs::HarnessAsk>,
    Option<aterm_agent::harness::acts::HeldBy>,
    bool,
    Option<std::sync::Arc<aterm_agent::harness::acts::LandReceipt>>,
    Option<std::sync::Arc<aterm_agent::harness::hold_said::HoldSaid>>,
);

/// The Land holds' own words beside their glass ([`crate::App::hold_said`]).
pub(crate) type HoldSaidMap =
    std::collections::HashMap<u64, std::sync::Arc<aterm_agent::harness::hold_said::HoldSaid>>;

impl<'a> TitleWords<'a> {
    /// The need said on its own part of the title (`· needs you: …; carrying on`).
    #[must_use]
    pub(crate) fn need_under(self, face: ChipFace) -> Option<&'a crate::needs::HarnessAsk> {
        let explained = !matches!(
            face,
            ChipFace::MasterOff | ChipFace::AgentOff(_) | ChipFace::Unhosted
        );
        let held = face == ChipFace::Land
            && self.hold.is_some_and(|hold| {
                hold.read() && hold.held.holds(aterm_agent::harness::acts::HoldLeg::Needs)
            });
        self.need.filter(|_| explained && !held)
    }

    /// The hold `face` says: under Land's `⤓` alone.
    #[must_use]
    pub(crate) fn hold_under(self, face: ChipFace) -> Option<&'a crate::harness_glass::HeldGlass> {
        self.hold.filter(|_| face == ChipFace::Land)
    }

    /// The stop `face` says (§14.3): under a face the harness types under alone
    /// ([`ChipFace::carries`]). While it stands it takes the place of the mode's own promise, and
    /// the lapsed pause and the trail are not said.
    #[must_use]
    pub(crate) fn stop_under(self, face: ChipFace) -> Option<aterm_agent::harness::acts::HeldBy> {
        self.stopped.filter(|_| face.carries())
    }

    /// Whether the stop said under `face` names the person's switch to Land: where it would ask
    /// once more, and only on a face that can switch to Land
    /// ([`crate::harness_glass::switch_face`]).
    #[must_use]
    pub(crate) fn switch_under(self, face: ChipFace) -> bool {
        self.switch && crate::harness_glass::switch_face(face)
    }
}

/// The words a title reads of session `session` ([`TitleWords`]), copied out.
fn title_words_of(
    glass: &std::collections::HashMap<u64, crate::harness_glass::SessionGlass>,
    said: &HoldSaidMap,
    pool: &crate::SessionPool,
    session: u64,
) -> ReadWords {
    let own = glass.get(&session);
    let hold = own.and_then(crate::harness_glass::SessionGlass::hold);
    let stopped = own.and_then(crate::harness_glass::SessionGlass::stopped);
    let (need, receipt) = with_title_reads(pool, session, |need, receipt| {
        (need.cloned(), receipt.cloned())
    });
    (
        hold,
        need,
        stopped,
        own.is_some_and(crate::harness_glass::SessionGlass::stop_switch),
        receipt,
        said.get(&session).cloned().filter(|_| hold.is_some()),
    )
}

/// Session `session`'s published need where it is a harness's ask, and what aterm checked at its
/// landing beside a landed face (§14.9, slice 9c), read under the one lock of its timeline.
fn with_title_reads<R>(
    pool: &crate::SessionPool,
    session: u64,
    f: impl FnOnce(
        Option<&crate::needs::HarnessAsk>,
        Option<&std::sync::Arc<aterm_agent::harness::acts::LandReceipt>>,
    ) -> R,
) -> R {
    match pool.get(session) {
        Some(s) => {
            let timeline = s
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let receipt = timeline
                .task_face()
                .filter(|(face, _)| *face == aterm_agent::harness::acts::TaskFace::Landed)
                .and_then(|_| timeline.land_receipt());
            f(timeline.need().harness_ask(), receipt)
        }
        None => f(None, None),
    }
}

/// The autopilot chip's title under `view` with `glass`, whole and short.
#[must_use]
pub(crate) fn autopilot_title(
    view: AutopilotView,
    glass: &ChipGlass,
    trail: &str,
    today: Option<i64>,
    words: TitleWords<'_>,
) -> (String, String) {
    #[cfg(test)]
    crate::work_counts::autopilot_title_built();
    let face = ChipFace::of(view, glass);
    let phrase = face.phrase(glass, today);
    let explanation = face.explanation(
        glass,
        TitleWords {
            hold: words.hold_under(face),
            ..words
        },
        today,
    );
    let click = face.click(glass);
    const PAUSE_LEAD: &str = " \u{b7} ";
    let need = words.need_under(face).map(crate::harness_glass::need_title);
    let stopped = words.stop_under(face).is_some();
    let pause = glass
        .pause
        .filter(|_| !stopped)
        .map(|p| p.title_words(today));
    let trail = if stopped { "" } else { trail };
    let mut whole = String::with_capacity(
        phrase.len()
            + explanation
                .as_ref()
                .map_or(0, |e| TITLE_EXPLAINS.len() + e.len())
            + need.as_ref().map_or(0, |n| PAUSE_LEAD.len() + n.len())
            + pause.as_ref().map_or(0, |p| PAUSE_LEAD.len() + p.len())
            + if trail.is_empty() {
                0
            } else {
                TITLE_LAST.len() + trail.len()
            }
            + TITLE_CLICK.len()
            + click.len()
            + TITLE_MORE.len(),
    );
    whole.push_str(&phrase);
    if let Some(explanation) = explanation {
        whole.push_str(TITLE_EXPLAINS);
        whole.push_str(&explanation);
    }
    if let Some(need) = need {
        whole.push_str(PAUSE_LEAD);
        whole.push_str(&need);
    }
    if let Some(pause) = pause {
        whole.push_str(PAUSE_LEAD);
        whole.push_str(&pause);
    }
    if !trail.is_empty() {
        whole.push_str(TITLE_LAST);
        whole.push_str(trail);
    }
    whole.push_str(TITLE_CLICK);
    whole.push_str(click);
    whole.push_str(TITLE_MORE);
    let short = short_title(&whole).to_owned();
    (whole, short)
}

/// The Autopilot title's leaner forms, between its whole and its short form, widest first.
#[must_use]
pub(crate) fn lean_titles(whole: &str) -> [(&str, &str); LEAN_FORMS] {
    let Some((more, click)) = whole
        .strip_suffix(TITLE_MORE)
        .and_then(|rest| Some((rest.len(), rest.rfind(TITLE_CLICK)?)))
    else {
        return [("", ""); LEAN_FORMS];
    };
    let trail_at = whole[..click].find(TITLE_LAST);
    let head_end = trail_at.unwrap_or(click);
    let without_trail = match trail_at {
        Some(at) => (&whole[..at], &whole[click..]),
        None => ("", ""),
    };
    let v1_end = whole[..head_end].find(TITLE_PAUSE).unwrap_or(head_end);
    let v1 = (&whole[..v1_end], &whole[click..more]);
    let (clicks, click_only) = match whole[..v1_end].find(TITLE_EXPLAINS) {
        Some(phrase) => {
            let explained = whole[phrase..v1_end].chars().count();
            let menu = whole[more..].chars().count();
            let clicks = if explained > menu {
                (&whole[..phrase], &whole[click..])
            } else {
                ("", "")
            };
            (clicks, (&whole[..phrase], &whole[click..more]))
        }
        None => (("", ""), ("", "")),
    };
    [without_trail, v1, clicks, click_only]
}

/// The act's label: its glyph and its word a space apart (`✓ approved`).
#[must_use]
pub(crate) const fn act_label(
    view: AutopilotView,
    act: Act,
) -> (&'static str, &'static str, ChipTone) {
    (act.glyph(), act.word(), view.tone())
}

/// The chip's accessibility description.
#[cfg_attr(
    not(any(test, a11y_tree)),
    allow(
        dead_code,
        reason = "the chips' accessibility nodes describe it; the tree is built only under the \
                  `a11y_tree` cfg (Linux, macOS and Windows)"
    )
)]
/// Its newest act, named for the one who did it where that is a proven self-claim's holder
/// (`by`: `Last: codex-alab approved at 14:05.`), so a screen reader never hears another
/// agent's act as aterm's.
#[must_use]
pub(crate) fn spoken_last(
    by: Option<&str>,
    act: Act,
    clock: Clock,
    day: Option<Weekday>,
) -> String {
    let who = by.map_or_else(String::new, |by| format!("{by} "));
    match day {
        None => format!("Last: {who}{} at {}.", act.spoken(), clock.as_str()),
        Some(day) => format!(
            "Last: {who}{} on {} at {}.",
            act.spoken(),
            day.long(),
            clock.as_str()
        ),
    }
}

/// The glass's part of [`WindowLights::autopilot_term`].
pub(crate) fn glass_bits(glass: ChipGlass, reading: bool) -> u64 {
    let act = glass.lit.map_or(0, |act| 1 + act as u64);
    let face = match glass.task {
        None | Some(TaskFace::Open) => 0,
        Some(TaskFace::Landed) => 1,
        Some(TaskFace::Failed) => 2,
        Some(TaskFace::Theirs) => 4,
    };
    let timer = glass
        .land_at
        .map_or(0, |at| 1 + u64::from(at.clock.minute()));
    let other_day = glass.land_at.is_some_and(|at| at.other_day.is_some());
    let weekday = glass
        .land_at
        .and_then(|at| at.other_day)
        .map_or(0, |day| 1 + day as u64);
    let trail = if reading { u64::from(glass.trail) } else { 0 };
    act << 24 | face << 32 | timer << 36 | u64::from(other_day) << 47 | trail << 48 | weekday << 56
}

/// What one frame's reading of a pane asks of the host.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Step {
    /// Claude answered a toggle's input.
    wake: bool,
}

/// Row `r` of `t`'s live screen as text, trailing blanks trimmed.
pub(crate) fn row_text(t: &aterm_core::terminal::Terminal, r: usize) -> String {
    let mut line = t
        .get_line_text(i32::try_from(r).unwrap_or(i32::MAX), None)
        .unwrap_or_default();
    line.truncate(line.trim_end().len());
    line
}

/// `read` inside the reader's panic fence for `session`'s live screen.
fn fenced_screen_now<T>(
    session: u64,
    rows: &[String],
    read: impl FnOnce() -> Option<T>,
) -> Option<T> {
    crate::reader_guard::read_or_none(session, "Claude Code live screen", rows, read).flatten()
}

#[cfg(test)]
thread_local! {
    /// The windows [`App::redraw_autopilot_title_readers`] asked for a frame on this thread.
    pub(crate) static TITLE_READERS_ASKED: std::cell::Cell<u32> =
        const { std::cell::Cell::new(0) };
    /// The harness switches a refused write let go of `◌ turning on` or `off` on this thread.
    pub(crate) static SWITCHES_LET_GO: std::cell::Cell<u32> =
        const { std::cell::Cell::new(0) };
}

/// Ask `ws`'s window, where it has one, for a frame.
pub(crate) fn redraw(ws: &crate::WindowState) {
    if let Some(w) = ws.os_window.as_ref() {
        w.request_redraw();
    }
}

/// Post the footer wake for `session` after `delay`.
pub(crate) fn wake_after(session: u64, delay: Duration) {
    #[cfg(test)]
    crate::work_counts::footer_wake_armed();
    let _ = crate::qos::spawn_lane_thread(
        "aterm-claude-light-timer",
        crate::qos::Role::Background,
        move || {
            std::thread::sleep(delay);
            crate::claude_footer::post_changed(session);
        },
    );
}

/// What the painter needs for one pane.
#[derive(Debug, Default)]
pub(crate) struct Block {
    /// The mode light's title ([`light_title`]: what its state does, Claude Code's name for the
    /// mode where the chip wears aterm's word, then `· click: <goal>`), or a harness chip's title.
    pub(crate) title: Vec<RenderCell>,
    /// The whole title with parts given up, widest first.
    pub(crate) lean: [Vec<RenderCell>; LEAN_FORMS],
    /// The title's head alone (the light's gist, a refusal's reason or `switching…`; a harness
    /// chip's short form), for a pane with no room for the whole title.
    pub(crate) short_title: Vec<RenderCell>,
    /// The title is a reason (a refusal's, [`light_title`]), which outranks the branch and the path
    /// in the rule.
    pub(crate) reason: bool,
    /// The chips, one blank apart, a worded Autopilot chip ` ─ ` ([`Block::rule_gaps`]); the
    /// rule's layout pads the block.
    pub(crate) lights: Vec<RenderCell>,
    /// The cells the block holds in the rule (`footer::LightsWidth`'s chips).
    pub(crate) holds: usize,
    /// The cells of [`Self::holds`] the mode's name a click brought up adds (`⤓ land ─ yolo`
    /// over `⤓ yolo`: ` land` and the ` ─` that sets it off): the rule lays them only where they
    /// leave its fit as it stands without them.
    pub(crate) word: usize,
    cols: Vec<(std::ops::Range<usize>, Chip)>,
    /// The fit that laid it ([`WindowLights::fitted`]), unique in its window.
    fit: u64,
}

impl Block {
    /// Synthetic private-dev input through the production lights reading and chip composition.
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    pub(crate) fn layout_scene(blank: RenderCell, at: ChipAt) -> Block {
        let mut lights = WindowLights::default();
        let rows = [
            "─".repeat(80),
            "❯ ".to_owned(),
            "─".repeat(80),
            "  ⏸ plan mode on (shift+tab to cycle)".to_owned(),
        ];
        lights.observe(0, lights::read_lights(&rows));
        let now = Instant::now();
        let view = AutopilotView::Land;
        lights.note_autopilot_at(0, view.mode(), view, ChipGlass::default(), || now);
        // Land just reached by a click: its name worded on the chip (`⤓ land ─ ⏸ plan`).
        let s = lights.sessions.entry(0).or_default();
        s.word = Some(ChipWord {
            mode: Autopilot::Land,
            from: now,
            fades: true,
            asked: 0,
        });
        let named = s.word_at(now);
        let mut block = Block::default();
        lights.block_into(
            0,
            blank,
            now,
            at,
            None,
            None,
            (Keep::Revealed, named),
            (None, None, None),
            &mut block,
        );
        block
    }

    /// Whether `chip` is drawn.
    pub(crate) fn shows(&self, chip: impl Into<Chip>) -> bool {
        let chip = chip.into();
        self.cols.iter().any(|(_, c)| *c == chip)
    }

    /// The fit that laid it, unique in its window (`0`: none numbered it).
    pub(crate) const fn fit(&self) -> u64 {
        self.fit
    }

    /// The cells of [`Self::lights`] that set a worded Autopilot chip off from the next
    /// ([`CHIP_RULE`]), which the rule's layout draws as its own rule.
    pub(crate) fn rule_gaps(&self) -> impl Iterator<Item = usize> + '_ {
        self.lights.iter().enumerate().filter_map(|(at, cell)| {
            (cell.ch == CHIP_RULE && !self.cols.iter().any(|(cols, _)| cols.contains(&at)))
                .then_some(at)
        })
    }
}

#[cfg(test)]
impl Block {
    /// A block of `lights` (the chips' cells, as one chip) and its titles.
    pub(crate) fn of_chips(
        lights: Vec<RenderCell>,
        title: Vec<RenderCell>,
        short_title: Vec<RenderCell>,
        reason: bool,
    ) -> Block {
        Block {
            cols: vec![(0..lights.len(), Chip::Light(Light::Mode))],
            title,
            lean: Default::default(),
            short_title,
            reason,
            holds: lights.len(),
            word: 0,
            lights,
            fit: 0,
        }
    }

    /// The block a session at rest draws under `view`.
    pub(crate) fn of_autopilot(view: AutopilotView, blank: RenderCell, at: ChipAt) -> Block {
        let mut w = WindowLights::default();
        let now = Instant::now();
        w.note_autopilot(0, view.mode(), view, now);
        w.block(0, blank, now, at, Keep::Drawn)
    }
}

impl App {
    /// Fold one Claude Code pane's reading of its screen into its lights.
    pub(crate) fn observe_claude_lights(
        &mut self,
        wid: WindowId,
        session: u64,
        read: crate::claude_footer::LightsRead,
    ) {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let step = ws.claude_lights.observe_read(session, read);
        if step.wake {
            crate::claude_footer::post_changed(session);
        }
    }

    /// The harness's glass on session `local_id`'s Autopilot chip ([`ChipGlass`]).
    #[must_use]
    pub(crate) fn chip_glass(&self, local_id: u64) -> ChipGlass {
        let mut glass = self
            .harness_glass
            .get(&local_id)
            .map_or_else(ChipGlass::default, crate::harness_glass::SessionGlass::chip);
        if !self.autopilot_timers.is_empty() {
            glass.land_at = self.autopilot_timers.glass(local_id);
        }
        if glass.lit.is_some() && !self.harness_acts_shown() {
            glass.lit = None;
        }
        glass
    }

    /// Fold in `session`'s Autopilot for the footer this frame draws.
    pub(crate) fn note_claude_autopilot(
        &mut self,
        wid: WindowId,
        session: u64,
        (word, view): (Option<Autopilot>, AutopilotView),
        rewritten: bool,
    ) {
        let glass = self.chip_glass(session);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.claude_lights
                .note_autopilot_at(session, word, view, glass, || {
                    #[cfg(test)]
                    crate::work_counts::harness_clock_read();
                    Instant::now()
                });
            ws.claude_lights.note_row(session, rewritten);
        }
    }

    /// Fold in `session`'s Team chip for the footer this frame draws.
    pub(crate) fn note_claude_team(&mut self, wid: WindowId, session: u64) {
        let face = self.team_face(session);
        if let Some(ws) = self.windows.get_mut(&wid) {
            let s = ws.claude_lights.sessions.entry(session).or_default();
            if s.team != face {
                s.team = face;
            }
        }
    }

    /// The Team chip's click, what its title promises ([`TeamFace::act`]).
    fn press_team_chip(&mut self, wid: WindowId, session: u64) {
        let act = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.claude_lights.sessions.get(&session))
            .and_then(|s| s.team.as_ref())
            .and_then(TeamFace::act);
        match act {
            Some(TeamAct::GoTo(manager)) => {
                let _ = self.focus_session_preferring(Some(wid), manager);
            }
            Some(TeamAct::Overview) => {
                let at = crate::app_overview::OverviewAt::Manager(session);
                if let Err(e) = self.open_overview_raised(wid, at) {
                    aterm_log::info!("team chip {session}: {e}");
                }
            }
            Some(TeamAct::HandBack) => self.hand_back_person(session),
            None => {}
        }
    }

    /// The chip block to paint in pane `pane` (its leaf) of `session`, in its composer rule: in
    /// `room`'s cells, standing at `chips_at`, with the mode's name a click brought up where its flag lets
    /// it be laid.
    pub(crate) fn claude_lights_block(
        &mut self,
        wid: WindowId,
        session: u64,
        pane: usize,
        blank: RenderCell,
        chips_at: ChipAt,
        room: (usize, bool),
    ) -> Option<Block> {
        let ws = self.windows.get_mut(&wid)?;
        let waits = crate::claude_panel::pointed_wait(ws.claude_card.panels(), session);
        let lights = &mut ws.claude_lights;
        let mine = |at: Option<(u64, Target)>| at.filter(|(id, _)| *id == session).map(|(_, t)| t);
        let (selected, hover) = (mine(lights.selected), mine(lights.hover));
        let at = lights.readings.iter().position(|r| r.session == session);
        let at = if [selected, hover].contains(&Some(Target::Chip(Chip::Autopilot))) {
            let generation = self
                .harness_glass
                .get(&session)
                .map_or(0, crate::harness_glass::SessionGlass::trail_gen);
            let built = lights
                .sessions
                .get(&session)
                .map_or((AutopilotView::Unhosted, ChipGlass::default()), |s| {
                    (s.autopilot.unwrap_or(AutopilotView::Unhosted), s.glass)
                });
            match at {
                Some(i) => {
                    let reading = &mut lights.readings[i];
                    if reading.refresh(
                        generation,
                        built,
                        || {
                            title_words_of(
                                &self.harness_glass,
                                &self.hold_said,
                                &self.pool,
                                session,
                            )
                        },
                        |stamped, held| {
                            crate::harness_glass::trail_title(
                                &self.harness_glass,
                                session,
                                stamped,
                                held,
                            )
                        },
                    ) {
                        lights.fits += 1;
                        reading.laid = lights.fits;
                    }
                    Some(i)
                }
                None => {
                    let TitleParts {
                        trail,
                        today,
                        whole,
                        short,
                    } = title_parts(
                        &self.harness_glass,
                        &self.hold_said,
                        &self.pool,
                        session,
                        &built,
                    );
                    lights.fits += 1;
                    lights.readings.push(TitleReading {
                        session,
                        generation,
                        trail,
                        today,
                        built,
                        whole,
                        short,
                        laid: lights.fits,
                    });
                    Some(lights.readings.len() - 1)
                }
            }
        } else {
            if let Some(i) = at {
                lights.readings.swap_remove(i);
            }
            None
        };
        let panel_at = lights
            .panel_readings
            .iter()
            .position(|r| r.session == session);
        let panel_at = match waits.filter(|_| selected.or(hover).is_none()) {
            Some(key) => match panel_at {
                Some(i) if lights.panel_readings[i].key == key => Some(i),
                found => {
                    let (whole, short) = wait_title(key.0.as_str(), key.1);
                    lights.fits += 1;
                    let reading = PanelReading {
                        session,
                        key,
                        whole,
                        short,
                        laid: lights.fits,
                    };
                    match found {
                        Some(i) => {
                            lights.panel_readings[i] = reading;
                            Some(i)
                        }
                        None => {
                            lights.panel_readings.push(reading);
                            Some(lights.panel_readings.len() - 1)
                        }
                    }
                }
            },
            None => {
                if let Some(i) = panel_at {
                    lights.panel_readings.swap_remove(i);
                }
                None
            }
        };
        let title = match selected.or(hover) {
            Some(Target::Fact(fact)) => lights
                .fact_readings
                .iter()
                .position(|r| r.session == session && r.fact == fact)
                .map(TitleAt::Fact),
            None => panel_at.map(TitleAt::Panel),
            _ => at.map(TitleAt::Autopilot),
        };
        lights.lend_block(
            session,
            pane,
            blank,
            chips_at,
            (selected, hover),
            room,
            title,
        )
    }

    /// The WINDOWS reading `session`'s autopilot title.
    pub(crate) fn redraw_autopilot_title_readers(&self, session: u64) {
        for ws in self.windows.values() {
            let reading = [ws.claude_lights.hover, ws.claude_lights.selected]
                .contains(&Some((session, Target::Chip(Chip::Autopilot))));
            if !reading {
                continue;
            }
            #[cfg(test)]
            TITLE_READERS_ASKED.with(|asked| asked.set(asked.get() + 1));
            if let Some(window) = ws.os_window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// The windows owing a frame for a mode name a click brought up on an Autopilot chip, whose
    /// fade reached its next step or whose hold ended at `now`.
    pub(crate) fn claude_chip_words_due(&mut self, now: Instant) -> Vec<WindowId> {
        self.windows
            .iter_mut()
            .filter(|(_, ws)| ws.claude_lights.sessions.values().any(|s| s.word.is_some()))
            .filter_map(|(wid, ws)| ws.claude_lights.words_due(now).then_some(*wid))
            .collect()
    }

    /// The loop's next wake for a mode name a click brought up
    /// (`metrics::DeadlineOwner::ChipWord`).
    pub(crate) fn claude_chip_word_wake(&self, now: Instant) -> Option<Instant> {
        self.windows
            .values()
            .filter_map(|ws| ws.claude_lights.word_wake(now))
            .min()
    }

    /// Hand a painted block back to window `wid` ([`Self::claude_lights_block`]).
    pub(crate) fn recycle_claude_lights_block(&mut self, wid: WindowId, block: Block) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.claude_lights.give_back(block);
        }
    }

    /// Every window repaints: a `[claude_footer]` switch changed what their Claude Code rules say.
    pub(crate) fn redraw_every_window(&self) {
        for ws in self.windows.values() {
            redraw(ws);
        }
    }

    /// `session`'s composer rule is on the glass this frame (chip or none, facts or none).
    pub(crate) fn note_claude_footer_row(&mut self, wid: WindowId, session: u64, room: usize) {
        if let Some(ws) = self.windows.get_mut(&wid)
            && !ws.claude_lights.footers.iter().any(|(s, _)| *s == session)
        {
            ws.claude_lights.footers.push((session, room));
        }
    }

    /// Record where `block`'s chips landed.
    pub(crate) fn note_claude_light_hits(
        &mut self,
        wid: WindowId,
        session: u64,
        frame_row: usize,
        term_row: usize,
        col: usize,
        block: &Block,
    ) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            let chips = block
                .cols
                .iter()
                .map(|(span, chip)| (span.clone(), Some(*chip)));
            let gaps = block
                .cols
                .windows(2)
                .map(|pair| (pair[0].0.end..pair[1].0.start, None));
            ws.claude_lights
                .hits
                .extend(chips.chain(gaps).flat_map(|(span, chip)| {
                    span.map(move |at| LightHit {
                        frame_row,
                        term_row,
                        col: col + at,
                        session,
                        chip,
                    })
                }));
        }
    }

    /// What of the chips' glass is under window pixel `(x, y)`.
    pub(crate) fn claude_chips_at(
        &self,
        wid: WindowId,
        x: f64,
        y: f64,
    ) -> Option<(u64, Option<Chip>)> {
        if self.windows.get(&wid)?.claude_lights.hits.is_empty() {
            return None;
        }
        self.chips_at_cell(wid, self.glass_cell_at(wid, x, y)?)
    }

    /// [`Self::claude_chips_at`] at frame cell `(row, col)` of the grid the
    /// lists were laid on.
    fn chips_at_cell(
        &self,
        wid: WindowId,
        (row, col): (usize, usize),
    ) -> Option<(u64, Option<Chip>)> {
        let hit = self
            .windows
            .get(&wid)?
            .claude_lights
            .hits
            .iter()
            .find(|h| h.frame_row == row && h.col == col)?;
        (!self.chrome_owns_terminal_row(wid, hit.term_row)).then_some((hit.session, hit.chip))
    }

    /// The frame cell under window pixel `(x, y)` on the grid the glass's
    /// lists were laid on.
    pub(crate) fn glass_cell_at(&self, wid: WindowId, x: f64, y: f64) -> Option<(usize, usize)> {
        let drawn = self.windows.get(&wid)?.claude_lights.drawn_at;
        self.laid_cell_at(wid, drawn, x, y)
    }

    /// The pointer left the window.
    pub(crate) fn clear_claude_light_hover(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid)
            && (ws.claude_lights.hover.take().is_some()
                | ws.claude_lights.switch_hover.take().is_some()
                | ws.claude_lights.part_hover.take().is_some()
                | ws.claude_lights.mark_hover.take().is_some()
                | ws.claude_lights.glass_hover_px.take().is_some())
        {
            redraw(ws);
        }
    }

    /// The glass's hover follows the glass, with no motion.
    pub(crate) fn settle_glass_hover(
        &mut self,
        wid: WindowId,
        grid: Option<crate::app_mouse::GlassGrid>,
    ) {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let regridded = std::mem::replace(&mut ws.claude_lights.drawn_at, grid) != grid;
        let lights = &mut ws.claude_lights;
        let was = lights.glass_hover();
        let laid = (lights.places(), was);
        // Glass laid elsewhere under the same hover moved under a still pointer (a re-laid rule,
        // a newly drawn ⬆️): track it again. A paint under a hover that changed since the last
        // paint (a motion's, or a track's here) only records its places, so glass a hover
        // itself re-lays never moves the hover again.
        let moved = lights
            .laid
            .replace(laid)
            .is_some_and(|(places, hover)| hover == was && places != laid.0);
        if (regridded || moved) && ws.pointer_position_known {
            let (x, y) = ws.last_cursor_px;
            let _ = self.track_claude_light_hover(wid, x, y);
            if let Some(ws) = self.windows.get_mut(&wid) {
                let lights = &mut ws.claude_lights;
                lights.cursor_owed |= lights.glass_hover() != was;
            }
        }
        let Some(lights) = self.windows.get(&wid).map(|ws| &ws.claude_lights) else {
            return;
        };
        if lights.switch_hover.is_none()
            && lights.part_hover.is_none()
            && lights.mark_hover.is_none()
        {
            return;
        }
        let at = lights
            .glass_hover_px
            .and_then(|(x, y)| self.claude_glass_at(wid, x, y));
        let switch =
            at.and_then(|(session, glass)| matches!(glass, GlassAt::Switch(_)).then_some(session));
        let part = at.and_then(|(session, glass)| Some((session, part_of(glass)?)));
        let mark = at.and_then(|(session, glass)| (glass == GlassAt::Mark).then_some(session));
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let lights = &mut ws.claude_lights;
        let mut gone = false;
        if lights.switch_hover.is_some() && lights.switch_hover != switch {
            lights.switch_hover = None;
            gone = true;
        }
        if lights.part_hover.is_some() && lights.part_hover != part {
            lights.part_hover = None;
            gone = true;
        }
        if lights.mark_hover.is_some() && lights.mark_hover != mark {
            lights.mark_hover = None;
            gone = true;
        }
        if lights.switch_hover.is_none()
            && lights.part_hover.is_none()
            && lights.mark_hover.is_none()
        {
            lights.glass_hover_px = None;
        }
        if gone {
            redraw(ws);
        }
    }

    /// The cursor follows the glass's hover.
    pub(crate) fn refresh_glass_cursor_for_frame(&mut self, wid: WindowId) {
        let owed = self.windows.get_mut(&wid).is_some_and(|ws| {
            std::mem::take(&mut ws.claude_lights.cursor_owed) && ws.pointer_position_known
        });
        if owed {
            self.update_hover_cursor(wid);
        }
    }

    /// The pointer on `at`.
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn hover_claude_chip(&mut self, wid: WindowId, at: Option<(u64, Chip)>) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.claude_lights.hover = at.map(|(session, chip)| (session, Target::Chip(chip)));
        }
    }

    /// The pointer on `at`.
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn hover_claude_fact(&mut self, wid: WindowId, at: Option<(u64, Fact)>) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.claude_lights.hover = at.map(|(session, fact)| (session, Target::Fact(fact)));
        }
        self.note_fact_readings(wid);
    }

    /// A mouse press anywhere lets a keyboard selection go.
    pub(crate) fn clear_claude_light_selection(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid)
            && ws.claude_lights.selected.take().is_some()
        {
            redraw(ws);
        }
    }

    /// Pointer motion: update hover state and return [`OverChips`].
    pub(crate) fn track_claude_light_hover(&mut self, wid: WindowId, x: f64, y: f64) -> OverChips {
        let at = self.claude_glass_at(wid, x, y);
        let over = at.and_then(|(session, glass)| match glass {
            GlassAt::Chip(chip) => Some((session, Target::Chip(chip?))),
            GlassAt::Fact(fact, _) => Some((session, Target::Fact(fact))),
            GlassAt::Switch(_) | GlassAt::Mark | GlassAt::Label | GlassAt::Upgrade => None,
        });
        let switch =
            at.and_then(|(session, glass)| matches!(glass, GlassAt::Switch(_)).then_some(session));
        let part = at.and_then(|(session, glass)| Some((session, part_of(glass)?)));
        let mark = at.and_then(|(session, glass)| (glass == GlassAt::Mark).then_some(session));
        let Some(ws) = self.windows.get_mut(&wid) else {
            return OverChips::No;
        };
        if ws.selecting {
            return OverChips::No;
        }
        ws.claude_lights.glass_hover_px =
            (switch.is_some() || part.is_some() || mark.is_some()).then_some((x, y));
        if ws.claude_lights.hover != over
            || ws.claude_lights.switch_hover != switch
            || ws.claude_lights.part_hover != part
            || ws.claude_lights.mark_hover != mark
        {
            ws.claude_lights.hover = over;
            ws.claude_lights.switch_hover = switch;
            ws.claude_lights.part_hover = part;
            ws.claude_lights.mark_hover = mark;
            redraw(ws);
            if over.is_some_and(|(_, t)| t.fact().is_some()) {
                self.note_fact_readings(wid);
            }
        }
        OverChips::of_glass(at.as_ref())
    }

    /// What of the harness's glass is under window pixel `(x, y)`.
    pub(crate) fn claude_glass_at(&self, wid: WindowId, x: f64, y: f64) -> Option<(u64, GlassAt)> {
        let ws = self.windows.get(&wid)?;
        let lights = &ws.claude_lights;
        if lights.hits.is_empty()
            && lights.border_toggles.is_empty()
            && lights.fact_hits.is_empty()
            && lights.mark_hits.is_empty()
        {
            return None;
        }
        let (row, col) = self.glass_cell_at(wid, x, y)?;
        if let Some((session, chip)) = self.chips_at_cell(wid, (row, col)) {
            return Some((session, GlassAt::Chip(chip)));
        }
        if let Some(b) = lights.border_toggles.iter().find(|b| {
            b.frame_row == row
                && b.arrow.as_ref().is_some_and(|cols| cols.contains(&col))
                && !self.chrome_owns_terminal_row(wid, b.term_row)
        }) {
            return Some((b.session, GlassAt::Upgrade));
        }
        if let Some(b) = lights.border_toggles.iter().find(|b| {
            b.frame_row == row
                && b.cols.contains(&col)
                && !self.chrome_owns_terminal_row(wid, b.term_row)
        }) {
            return Some((b.session, GlassAt::Switch(b.face)));
        }
        if let Some(b) = lights.border_toggles.iter().find(|b| {
            b.frame_row == row
                && b.label.as_ref().is_some_and(|cols| cols.contains(&col))
                && !self.chrome_owns_terminal_row(wid, b.term_row)
        }) {
            return Some((b.session, GlassAt::Label));
        }
        if let Some(h) = lights.fact_hits.iter().find(|h| {
            h.frame_row == row
                && (h.cols.0..h.cols.1).contains(&col)
                && !self.chrome_owns_terminal_row(wid, h.term_row)
        }) {
            return Some((h.session, GlassAt::Fact(h.fact, h.restores)));
        }
        lights
            .mark_hits
            .iter()
            .find(|m| {
                m.frame_row == row
                    && m.cols.contains(&col)
                    && !self.chrome_owns_terminal_row(wid, m.term_row)
            })
            .map(|m| (m.session, GlassAt::Mark))
    }

    /// Build again every fact title someone reads in window `wid` (hovered or selected).
    pub(crate) fn note_fact_readings(&mut self, wid: WindowId) {
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        let focus: Vec<(u64, Fact)> = [ws.claude_lights.selected, ws.claude_lights.hover]
            .into_iter()
            .flatten()
            .filter_map(|(session, t)| Some((session, t.fact()?)))
            .collect();
        let built: Vec<(u64, Fact, (String, String))> = focus
            .into_iter()
            .map(|(session, fact)| (session, fact, self.fact_title(session, fact)))
            .collect();
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let lights = &mut ws.claude_lights;
        lights.fact_readings.retain(|r| {
            built
                .iter()
                .any(|(s, f, _)| (*s, *f) == (r.session, r.fact))
        });
        let mut moved = false;
        for (session, fact, (whole, short)) in built {
            match lights
                .fact_readings
                .iter_mut()
                .find(|r| (r.session, r.fact) == (session, fact))
            {
                Some(r) if r.whole == whole && r.short == short => {}
                Some(r) => {
                    lights.fits += 1;
                    (r.whole, r.short, r.laid) = (whole, short, lights.fits);
                    moved = true;
                }
                None => {
                    lights.fits += 1;
                    let laid = lights.fits;
                    lights.fact_readings.push(FactReading {
                        session,
                        fact,
                        whole,
                        short,
                        laid,
                    });
                    moved = true;
                }
            }
        }
        if moved {
            redraw(ws);
        }
    }

    /// [`Self::note_fact_readings`] in every window that reads a title of `session`'s facts.
    pub(crate) fn refresh_fact_readings(&mut self, session: u64) {
        let windows: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, ws)| {
                ws.claude_lights
                    .fact_readings
                    .iter()
                    .any(|r| r.session == session)
            })
            .map(|(wid, _)| *wid)
            .collect();
        for wid in windows {
            self.note_fact_readings(wid);
        }
    }

    /// A fact menu's row did `notice` for `session` in window `wid` (a copy, a new tab).
    pub(crate) fn note_fact_notice(&mut self, wid: WindowId, session: u64, notice: FactNotice) {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        ws.claude_lights
            .sessions
            .entry(session)
            .or_default()
            .fact_notice = Some((notice, Instant::now() + FACT_NOTICE_SHOWN));
        redraw(ws);
        wake_after(session, FACT_NOTICE_SHOWN);
    }

    /// A slash press's `words` for `session` in window `wid` (`crate::claude_slash`).
    pub(crate) fn note_fact_said(
        &mut self,
        wid: WindowId,
        session: u64,
        words: String,
        shown: Duration,
    ) -> bool {
        let said = self.set_fact_said(wid, session, words, shown);
        if said {
            wake_after(session, shown);
        }
        said
    }

    /// [`Self::note_fact_said`] with no wake of its own.
    pub(crate) fn set_fact_said(
        &mut self,
        wid: WindowId,
        session: u64,
        words: String,
        shown: Duration,
    ) -> bool {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        ws.claude_lights
            .sessions
            .entry(session)
            .or_default()
            .fact_notice = Some((FactNotice::Said(words.into()), Instant::now() + shown));
        redraw(ws);
        true
    }

    /// Let go of a slash press's words for `session` in window `wid`.
    pub(crate) fn clear_fact_said(&mut self, wid: WindowId, session: u64) {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if let Some(s) = ws.claude_lights.sessions.get_mut(&session)
            && matches!(s.fact_notice, Some((FactNotice::Said(_), _)))
        {
            s.fact_notice = None;
            redraw(ws);
        }
    }

    /// Whether a mode toggle of the lights is in flight for `session` in any window.
    #[must_use]
    pub(crate) fn mode_toggle_in_flight(&self, session: u64) -> bool {
        self.windows.values().any(|ws| {
            ws.claude_lights
                .sessions
                .get(&session)
                .is_some_and(|s| s.pending.is_some())
        })
    }

    /// A press, at `at`, of session `session`'s harness switch in window `wid`.
    pub(crate) fn press_harness_switch(
        &mut self,
        wid: WindowId,
        session: u64,
        shown: crate::autopilot::HarnessSwitch,
        at: Instant,
    ) {
        use crate::autopilot::HarnessSwitch;
        if self
            .windows
            .get_mut(&wid)
            .is_some_and(|ws| ws.claude_lights.switch_just_pressed(session, at))
        {
            return;
        }
        // `at` places the press in a run of presses; the settle deadline runs on the clock the
        // frames that end it read.
        let now = Instant::now();
        let (_, view, _) = self.session_autopilot(session);
        let turning = |app: &mut Self, on: bool| {
            if let Some(ws) = app.windows.get_mut(&wid) {
                ws.claude_lights
                    .sessions
                    .entry(session)
                    .or_default()
                    .switch_turning = Some((on, now + crate::autopilot::SWITCH_SETTLE));
                wake_after(session, crate::autopilot::SWITCH_SETTLE);
            }
        };
        let flipped = match (shown, HarnessSwitch::of(view)) {
            (HarnessSwitch::On, Some(HarnessSwitch::On)) => {
                turning(self, false);
                self.toggle_session_harness(session);
                true
            }
            (HarnessSwitch::Off(_), Some(HarnessSwitch::Off(_))) => {
                let writes = !crate::autopilot::switch_on_plan(
                    &self.harness_policy.get().policy,
                    self.headless,
                    self.session_agent(session),
                )
                .keys
                .is_empty();
                if writes {
                    turning(self, true);
                }
                self.switch_harness_on(session);
                true
            }
            _ => false,
        };
        if let Some(ws) = self.windows.get_mut(&wid) {
            if flipped {
                ws.claude_lights
                    .sessions
                    .entry(session)
                    .or_default()
                    .switch_pressed = Some(at);
            }
            redraw(ws);
        }
    }

    /// A press of `session`'s harness switch showing `face` as a click on it
    /// makes one, from the pointer, a screen reader or the keyboard: a tab
    /// name being edited settles first, then the switch turns.
    fn press_switch_as_clicked(
        &mut self,
        wid: WindowId,
        session: u64,
        face: crate::autopilot::HarnessSwitch,
        at: Instant,
    ) {
        if self.inline_rename_edit(wid).is_some() {
            self.clear_strip_press(wid);
            self.settle_rename_edit(wid);
        }
        self.press_harness_switch(wid, session, face, at);
    }

    /// A left press at `at` on a chip acts on it ([`Self::press_claude_chip`]).
    pub(crate) fn press_claude_light(&mut self, wid: WindowId, at: Instant) -> bool {
        let (x, y) = self
            .windows
            .get(&wid)
            .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
        let Some((session, glass)) = self.claude_glass_at(wid, x, y) else {
            if let Some((session, url)) = self.claude_glass_hit_at(wid, x, y) {
                return self.press_claude_glass_hit(wid, session, url, at);
            }
            // A cell a rewritten row vacated takes the press: nothing of Claude's is under it.
            return self.glass_swallows_at(wid, x, y);
        };
        let secondary = cfg!(target_os = "macos")
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.mods.control_key());
        match glass {
            GlassAt::Chip(chip) => {
                if secondary && self.open_autopilot_menu_at(wid, x, y) {
                    return true;
                }
                self.press_visible_claude_chip(wid, session, chip, at)
            }
            // A secondary click (control-click on macOS) is a right-click: on the switch or the
            // ⬆️ it turns nothing and asks for no move, as a right-click there does.
            GlassAt::Switch(_) | GlassAt::Upgrade if secondary => true,
            GlassAt::Switch(face) => {
                self.press_switch_as_clicked(wid, session, face, at);
                true
            }
            GlassAt::Upgrade => self.press_upgrade_arrow(wid, session),
            GlassAt::Fact(..) if secondary => {
                self.open_fact_menu_at(wid, x, y);
                true
            }
            GlassAt::Fact(fact, restores) => {
                self.press_visible_claude_fact(wid, session, fact, restores)
            }
            // The label is hover only: its press, and its release, go nowhere. The lockup's is the
            // pane's, as before the rule wore one.
            GlassAt::Label => true,
            GlassAt::Mark => false,
        }
    }

    /// Show `words`, why the owner's Upgrade Now for `session` was refused, in the label slot of
    /// its top rule's ⬆️ in every window that draws it, for [`REFUSAL_SHOWN`].
    pub(crate) fn show_upgrade_refusal(&mut self, session: u64, words: &str) {
        let until = Instant::now() + REFUSAL_SHOWN;
        let mut shown = false;
        for ws in self.windows.values_mut() {
            if ws
                .claude_lights
                .border_toggles
                .iter()
                .any(|b| b.session == session)
            {
                ws.claude_lights
                    .refuse_upgrade(session, words.to_string(), until);
                redraw(ws);
                shown = true;
            }
        }
        if shown {
            wake_after(session, REFUSAL_SHOWN);
        }
    }

    /// A press of `session`'s ⬆️ in `wid`: for Claude Code a one-shot restart of its own
    /// (`upgrade_host::WordAsk::pressed`), which moves past its Autopilot or its harness switched
    /// off and the agent's own background work and waits only for what a restart would lose or
    /// cannot pass (`upgrade::press_gate`); for another CLI the tab menu's Upgrade Now for the
    /// build it offers (`App::send_upgrade_word`), which skips the hold clock and keeps every
    /// safety gate. An asked move's `◌` takes no second press, and a held one asks nothing and
    /// says why it cannot move (another supervisor holds it, or another CLI's harness or
    /// Autopilot is off, [`crate::claude_footer::UpState::Held`]). A tab name being edited settles
    /// first.
    pub(crate) fn press_upgrade_arrow(&mut self, wid: WindowId, session: u64) -> bool {
        use crate::claude_footer::UpState;
        let Some((up, face)) = self.windows.get(&wid).and_then(|ws| {
            ws.claude_lights
                .border_toggles
                .iter()
                .find(|b| {
                    b.session == session
                        && b.arrow.is_some()
                        && !self.chrome_owns_terminal_row(wid, b.term_row)
                })
                .map(|b| (b.paint.up, b.paint.face))
        }) else {
            return false;
        };
        if self.inline_rename_edit(wid).is_some() {
            self.clear_strip_press(wid);
            self.settle_rename_edit(wid);
        }
        if up.state == UpState::Held {
            // The slot says why for a few seconds where the rule has a label; the messages row
            // says it at every width, with what lets the move be asked for.
            self.show_upgrade_refusal(session, up.why);
            let place = self
                .session_sid(session)
                .map(|tab| self.upgrade_place(&tab))
                .unwrap_or_default();
            let _ = self.post_message(crate::message_reporters::agent_upgrade_held(
                up.agent,
                up.to.as_str(),
                up.why,
                crate::claude_footer::held_then(up.why),
                &place,
            ));
            return true;
        }
        if up.state != UpState::Ready {
            return true;
        }
        let Some(tab) = self.session_sid(session) else {
            return true;
        };
        let to = up.to.as_str().to_string();
        let ask = if up.agent.presses() {
            // With the harness or the Autopilot off, the restart types only its relaunch line.
            let aside_off = self.upgrade_view.row_for(&tab).is_some_and(|r| {
                use aterm_agent::harness::upgrade_drive::{AGENT_OFF, AUTOPILOT_OFF};
                matches!(r.aside.as_str(), AGENT_OFF | AUTOPILOT_OFF)
            });
            crate::upgrade_host::WordAsk::pressed(tab, to, face.is_off() || aside_off)
        } else {
            crate::upgrade_host::WordAsk::new(tab, to, aterm_messages::UpgradeWord::Now)
        };
        self.send_upgrade_word(ask);
        true
    }

    /// Click a visible `fact` on `session`'s rule, settling any open in-grid rename first.
    pub(crate) fn press_visible_claude_fact(
        &mut self,
        wid: WindowId,
        session: u64,
        fact: Fact,
        restores: FactMask,
    ) -> bool {
        let shown = self.windows.get(&wid).is_some_and(|ws| {
            ws.claude_lights.fact_hits.iter().any(|hit| {
                hit.session == session
                    && hit.fact == fact
                    && hit.restores == restores
                    && !self.chrome_owns_terminal_row(wid, hit.term_row)
            })
        });
        if !shown {
            return false;
        }
        if self.inline_rename_edit(wid).is_some() {
            self.clear_strip_press(wid);
            self.settle_rename_edit(wid);
        }
        self.press_claude_fact(wid, session, fact, restores);
        true
    }

    /// A left press on `session`'s harness words on the glass ([`Self::claude_glass_hit_at`]: a
    /// banner's rows, the card line's link).
    ///
    /// On a link a plain press (no modifier held) in the focused pane opens it, as a
    /// link-modifier press does in any pane, once per double click; in an unfocused pane a plain
    /// press is that pane's focus click and opens nothing, and a press with another modifier
    /// (Shift extending a selection, Option taking one from a tracking app) is the rows' own. A
    /// press the words take settles a tab name being edited first.
    fn press_claude_glass_hit(
        &mut self,
        wid: WindowId,
        session: u64,
        url: Option<&'static str>,
        at: Instant,
    ) -> bool {
        let Some(mods) = self.windows.get(&wid).map(|ws| ws.mods) else {
            return false;
        };
        let modifier = crate::app_mouse::link_modifier_held(mods);
        let front = self.session_is_front(wid, session);
        let opens = url
            .filter(|url| crate::is_safe_url(url))
            .filter(|_| modifier || (front && mods.is_empty()));
        let tracking = front
            && self
                .pool
                .get(session)
                .is_some_and(|owner| owner.ctx.modes.mouse_tracking_enabled());
        let takes = modifier
            || opens.is_some()
            || (tracking
                && !crate::app_mouse::press_starts_selection(
                    tracking,
                    crate::app_mouse::tracking_bypass_held(mods),
                ));
        if !takes {
            return false;
        }
        if self.inline_rename_edit(wid).is_some() {
            self.clear_strip_press(wid);
            self.settle_rename_edit(wid);
        }
        if let Some(url) = opens
            && self
                .windows
                .get_mut(&wid)
                .is_some_and(|ws| !ws.claude_card.link_press_repeats(session, at))
        {
            crate::app_mouse::open_url_external(url);
        }
        true
    }

    /// Pointer and reader clicks share the current row's custody and settle an open rename.
    fn press_visible_claude_chip(
        &mut self,
        wid: WindowId,
        session: u64,
        chip: Option<Chip>,
        at: Instant,
    ) -> bool {
        let shown = self.windows.get(&wid).is_some_and(|ws| {
            ws.claude_lights.hits.iter().any(|hit| {
                hit.session == session
                    && hit.chip == chip
                    && !self.chrome_owns_terminal_row(wid, hit.term_row)
            })
        });
        if !shown {
            return false;
        }
        if self.inline_rename_edit(wid).is_some() {
            self.clear_strip_press(wid);
            self.settle_rename_edit(wid);
        }
        if let Some(chip) = chip {
            self.press_claude_chip(wid, session, chip, at);
        }
        true
    }

    /// What a click at `at` on `chip` of `session` does. A press of the mode light within a double
    /// click's interval of the last one that reached it is that gesture's, and does nothing: a
    /// double click, or a slower triple click each of whose presses comes within the interval of
    /// the one before, turns the light over once, never over and back.
    pub(crate) fn press_claude_chip(
        &mut self,
        wid: WindowId,
        session: u64,
        chip: Chip,
        at: Instant,
    ) {
        match chip {
            Chip::Light(light) => {
                let Some(ws) = self.windows.get_mut(&wid) else {
                    return;
                };
                if ws.claude_lights.light_just_pressed(session, at) {
                    return;
                }
                ws.claude_lights
                    .sessions
                    .entry(session)
                    .or_default()
                    .light_pressed = Some(at);
                self.toggle_claude_light(wid, session, light);
            }
            Chip::Autopilot => self.press_autopilot_chip(wid, session, at),
            Chip::Team => self.press_team_chip(wid, session),
        }
    }

    /// The Autopilot chip's click at `at`.
    fn press_autopilot_chip(&mut self, wid: WindowId, session: u64, at: Instant) {
        if self
            .windows
            .get_mut(&wid)
            .is_some_and(|ws| ws.claude_lights.switch_just_pressed(session, at))
        {
            return;
        }
        let now = Instant::now();
        let in_flight = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.claude_lights.sessions.get(&session))
            .and_then(|s| s.autopilot_pending)
            .is_some_and(|(_, until)| until > now);
        if in_flight {
            return;
        }
        let (_, view, _) = self.session_autopilot(session);
        let Some(want) = view.next() else {
            match crate::autopilot::HarnessSwitch::of(view) {
                Some(face) => {
                    let shown = self
                        .windows
                        .get(&wid)
                        .map_or(face, |ws| ws.claude_lights.switch_shown(session, face).0);
                    self.press_harness_switch(wid, session, shown, at);
                }
                None => {
                    self.open_harness_settings(
                        wid,
                        crate::harness_screen::HarnessFocus::Supervision,
                    );
                }
            }
            return;
        };
        let shown = self.cycle_session_autopilot(session);
        let effect = crate::motion::MotionEffect::ChipWord;
        let fades = self
            .windows
            .get(&wid)
            .is_some_and(|ws| self.effect_policy(effect, ws.focused).animate(effect));
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let lights = ws.claude_lights.sessions.entry(session).or_default();
        // The mode's name comes up beside the glyph, from the click where the word is written
        // already, else from the frame that reads it back ([`WindowLights::note_autopilot_at`]).
        lights.word = Some(ChipWord {
            mode: want,
            from: now,
            fades,
            asked: 0,
        });
        if shown.mode() == Some(want) {
            lights.autopilot = Some(shown);
            lights.autopilot_pending = None;
            redraw(ws);
            return;
        }
        let settle = Duration::from_millis(lights::SETTLE_MS);
        lights.autopilot_pending = Some((want, now + settle));
        redraw(ws);
        wake_after(session, settle);
    }

    /// Flip `light` of `session` with Claude's own input: shift+tab, pressed on until Claude shows
    /// the state the light did not ([`Goal::of_mode`]): `yolo` or auto to manual (`lame`), any
    /// other mode back to yolo or auto.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_mode_toggle",
            action = "Click",
            project = "claude_lights::gesture_tests::project_mode_toggle"
        )
    )]
    pub(crate) fn toggle_claude_light(&mut self, wid: WindowId, session: u64, light: Light) {
        let now = Instant::now();
        if self.windows.values().any(|w| {
            w.claude_lights
                .sessions
                .get(&session)
                .is_some_and(|s| s.pending.is_some())
        }) {
            return;
        }
        let refuse = |ws: &mut crate::WindowState, why: Refusal| {
            ws.claude_lights
                .sessions
                .entry(session)
                .or_default()
                .refused = Some((light, why, now + REFUSAL_SHOWN));
            redraw(ws);
            wake_after(session, REFUSAL_SHOWN);
        };
        if self.slash_in_flight(session) {
            if let Some(ws) = self.windows.get_mut(&wid) {
                refuse(ws, Refusal::Typing);
            }
            return;
        }
        if self.pool.is_rewound(session) {
            if let Some(ws) = self.windows.get_mut(&wid) {
                refuse(ws, Refusal::NotShown);
            }
            return;
        }
        let screen = self.claude_screen_now(session);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let reading = lights::read(screen.as_ref());
        let Some(mode) = reading.mode else {
            refuse(ws, Refusal::NotShown);
            return;
        };
        let outcome = self.input_to_session(
            wid,
            crate::input::InputEvent::KeySequence(lights::SHIFT_TAB.to_vec()),
            crate::input::Source::Human,
            Some(session),
            crate::app_input::PressPhase::Initial,
        );
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if outcome != crate::input::InputOutcome::Ok {
            refuse(ws, Refusal::InputRejected);
            return;
        }
        let settle = Duration::from_millis(lights::SETTLE_MS);
        let s = ws.claude_lights.sessions.entry(session).or_default();
        s.shown.fold(&reading);
        s.refused = None;
        s.pending = Some(Pending {
            light,
            goal: Goal::of_mode(mode),
            started: mode,
            mode_at_press: mode,
            seen: Vec::new(),
            presses: 1,
            deadline: now + settle,
            next_poll: Some(now + MODE_POLL),
            held_mid_turn: false,
        });
        redraw(ws);
        wake_after(session, settle);
        wake_after(session, MODE_POLL);
    }

    /// `session`'s screen read straight from its engine (behind the reader's panic fence).
    fn claude_screen_now(&self, session: u64) -> Option<Screen> {
        let rows = self.claude_rows_now(session)?;
        fenced_screen_now(session, &rows, || lights::read_screen(&rows))
    }

    /// `session`'s live screen copied out of its engine, its rows as text.
    /// `None` while Claude's DEC 2026 frame is unfinished: the event loop never waits for its close.
    fn claude_rows_now(&self, session: u64) -> Option<Vec<String>> {
        let entry = self.pool.get(session)?;
        let t = crate::term_lock(&entry.term);
        if t.grid().display_offset() > 0 || crate::frame_unfinished_now(&t) {
            return None;
        }
        Some(
            (0..usize::from(t.rows()))
                .map(|r| row_text(&t, r))
                .collect(),
        )
    }

    /// The footer wake's lights half.
    pub(crate) fn drain_claude_lights(&mut self) {
        let now = Instant::now();
        self.advance_slash_presses(now);
        self.advance_claude_mode_toggles(now);
        for ws in self.windows.values_mut() {
            let (changed, refused) = ws.claude_lights.expire(now);
            if changed {
                redraw(ws);
            }
            for session in refused {
                wake_after(session, REFUSAL_SHOWN);
            }
        }
    }

    /// Every mode toggle in flight.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_mode_toggle",
            action = "Press",
            project = "claude_lights::gesture_tests::project_mode_toggle"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_mode_toggle",
            action = "Arrive",
            project = "claude_lights::gesture_tests::project_mode_toggle"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_mode_toggle",
            action = "Lap",
            project = "claude_lights::gesture_tests::project_mode_toggle"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_mode_toggle",
            action = "Reject",
            project = "claude_lights::gesture_tests::project_mode_toggle"
        )
    )]
    fn advance_claude_mode_toggles(&mut self, now: Instant) {
        let returns: Vec<(WindowId, u64)> = self
            .windows
            .iter()
            .flat_map(|(wid, ws)| {
                ws.claude_lights
                    .sessions
                    .iter()
                    .filter(|(_, s)| s.pending.is_some())
                    .map(|(session, _)| (*wid, *session))
            })
            .collect();
        for (wid, session) in returns {
            let screen = self.claude_screen_now(session);
            let Some(ws) = self.windows.get_mut(&wid) else {
                continue;
            };
            let step = ws.claude_lights.advance(session, screen.as_ref(), now);
            if step != Advance::Wait {
                redraw(ws);
            }
            match step {
                Advance::Press { expect } => {
                    debug_assert_eq!(screen.as_ref().and_then(|s| s.mode), Some(expect));
                    let outcome = self.input_to_session(
                        wid,
                        crate::input::InputEvent::KeySequence(lights::SHIFT_TAB.to_vec()),
                        crate::input::Source::Controller,
                        Some(session),
                        crate::app_input::PressPhase::Initial,
                    );
                    if outcome != crate::input::InputOutcome::Ok {
                        self.abandon_claude_light(wid, session, Refusal::StoppedIn(expect));
                        continue;
                    }
                    wake_after(session, Duration::from_millis(lights::SETTLE_MS));
                }
                Advance::Arrived | Advance::Lapped => {
                    wake_after(session, REFUSAL_SHOWN);
                    continue;
                }
                Advance::Wait => {}
            }
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.claude_lights.arm_poll(session, now, MODE_POLL);
            }
        }
    }

    /// Stop `session`'s toggle in flight and say why on its light.
    fn abandon_claude_light(&mut self, wid: WindowId, session: u64, why: Refusal) {
        let now = Instant::now();
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let s = ws.claude_lights.sessions.entry(session).or_default();
        let Some(p) = s.pending.take() else {
            return;
        };
        s.refused = Some((p.light, why, now + REFUSAL_SHOWN));
        redraw(ws);
        wake_after(session, REFUSAL_SHOWN);
    }

    /// Keyboard navigation and activation for rule chips and facts.
    pub(crate) fn on_key_claude_lights(
        &mut self,
        wid: WindowId,
        mods: ModifiersState,
        ev: &KeyEvent,
    ) -> bool {
        // A bare modifier is not a keystroke (macOS delivers Ctrl and Shift
        // pressed on their own as presses): it neither selects nor lets a
        // selection go, or every second ctrl+shift+tab would start over.
        if matches!(
            ev.logical_key,
            Key::Named(
                NamedKey::Shift
                    | NamedKey::Control
                    | NamedKey::Alt
                    | NamedKey::AltGraph
                    | NamedKey::Super
                    | NamedKey::Meta
                    | NamedKey::Hyper
                    | NamedKey::Fn
                    | NamedKey::CapsLock
            )
        ) {
            return false;
        }
        if self.autopilot_menu.as_ref().is_some_and(|m| m.wid == wid)
            && self.autopilot_menu_key(wid, ev)
        {
            return true;
        }
        let select_chord = cfg!(target_os = "macos")
            && mods.control_key()
            && mods.shift_key()
            && !mods.alt_key()
            && !mods.super_key()
            && matches!(ev.logical_key, Key::Named(NamedKey::Tab));
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let walkable =
            !ws.claude_lights.footers.is_empty() || !ws.claude_lights.border_toggles.is_empty();
        if ws.claude_lights.selected.is_none() && !(select_chord && walkable) {
            return false;
        }
        let Some(session) = self.focused_session_id(wid) else {
            return false;
        };
        let now = Instant::now();
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        let room = ws
            .claude_lights
            .footers
            .iter()
            .find(|(id, _)| *id == session)
            .map(|(_, room)| *room);
        let stale = ws.claude_lights.selected.is_some_and(|(id, target)| {
            id != session
                || (ws.claude_lights.selection_painted
                    && !ws.claude_lights.shows_uncovered(id, target, |row| {
                        ws.chrome_rows.iter().any(|band| band.contains(&row))
                    }))
        });
        if stale {
            ws.claude_lights.select(None);
            redraw(ws);
        }
        let order = ws.claude_lights.revealable(session);
        let lights_ = &ws.claude_lights;
        let chrome_rows = &ws.chrome_rows;
        let covered = |row: usize| chrome_rows.iter().any(|band| band.contains(&row));
        let fits = |target: Target| match target {
            Target::Chip(chip) => room.is_some_and(|room| {
                lights_
                    .fitted(
                        session,
                        RenderCell::default(),
                        now,
                        ChipAt::bare(RenderCell::default()),
                        Some(chip),
                        None,
                        room,
                    )
                    .is_some_and(|block| block.shows(chip))
            }),
            Target::Fact(_) | Target::Switch => lights_.shows_uncovered(session, target, covered),
        };
        let step = |from: Option<Target>| -> Option<Target> {
            let n = order.len();
            let at = from
                .and_then(|l| order.iter().position(|o| *o == l))
                .unwrap_or(n.checked_sub(1)?);
            (1..=n).map(|k| order[(at + k) % n]).find(|l| fits(*l))
        };
        let selected = ws
            .claude_lights
            .selected
            .filter(|(id, _)| *id == session)
            .map(|(_, l)| l);
        let switched = ws.claude_lights.switch_of(session).is_some();
        if select_chord && (room.is_some() || switched) {
            if let Some(next) = step(selected) {
                ws.claude_lights.select(Some((session, next)));
                redraw(ws);
                if next.fact().is_some() {
                    self.note_fact_readings(wid);
                }
            }
            return true;
        }
        let Some(target) = selected else {
            return false;
        };
        let plain = !mods.control_key() && !mods.alt_key() && !mods.super_key();
        let menu_key = match &ev.logical_key {
            Key::Named(NamedKey::ArrowDown) => plain && !mods.shift_key(),
            Key::Named(NamedKey::F10) => plain && mods.shift_key(),
            Key::Named(NamedKey::ContextMenu) => plain,
            _ => false,
        };
        let chip = match target {
            Target::Chip(chip) => chip,
            Target::Switch => {
                let face = ws
                    .claude_lights
                    .border_toggles
                    .iter()
                    .find(|b| {
                        b.session == session
                            && !ws.chrome_rows.iter().any(|band| band.contains(&b.term_row))
                    })
                    .map(|b| b.face);
                let clicks = matches!(
                    &ev.logical_key,
                    Key::Named(NamedKey::Enter | NamedKey::Space)
                ) && plain;
                let escape = matches!(&ev.logical_key, Key::Named(NamedKey::Escape));
                ws.claude_lights.select(None);
                redraw(ws);
                return match face {
                    Some(face) if clicks => {
                        self.press_switch_as_clicked(wid, session, face, Instant::now());
                        true
                    }
                    _ => escape,
                };
            }
            Target::Fact(fact) => {
                let hit = ws
                    .claude_lights
                    .fact_hits
                    .iter()
                    .filter(|h| {
                        h.session == session
                            && h.fact == fact
                            && !ws.chrome_rows.iter().any(|band| band.contains(&h.term_row))
                    })
                    .min_by_key(|h| (h.frame_row, h.cols.0))
                    .copied();
                let clicks = matches!(
                    &ev.logical_key,
                    Key::Named(NamedKey::Enter | NamedKey::Space)
                ) && plain;
                let escape = matches!(&ev.logical_key, Key::Named(NamedKey::Escape));
                ws.claude_lights.select(None);
                redraw(ws);
                return match hit {
                    Some(hit) if clicks => {
                        self.press_visible_claude_fact(wid, session, fact, hit.restores);
                        true
                    }
                    Some(_) if menu_key => {
                        if let Some(anchor) = self.fact_anchor(wid, session, fact) {
                            self.open_fact_menu(wid, session, fact, anchor, true);
                        }
                        true
                    }
                    _ => escape,
                };
            }
        };
        let opens_menu = chip == Chip::Autopilot && menu_key;
        if opens_menu {
            let anchor = ws
                .claude_lights
                .hits
                .iter()
                .filter(|h| h.session == session && h.chip == Some(Chip::Autopilot))
                .map(|h| (h.frame_row, h.col))
                .min_by_key(|&(_, col)| col);
            ws.claude_lights.select(None);
            redraw(ws);
            if let Some(anchor) = anchor {
                self.open_autopilot_menu(wid, session, anchor, true);
            }
            return true;
        }
        match &ev.logical_key {
            Key::Named(NamedKey::Enter | NamedKey::Space) if plain => {
                ws.claude_lights.select(None);
                redraw(ws);
                self.press_claude_chip(wid, session, chip, Instant::now());
                true
            }
            Key::Named(NamedKey::Escape) => {
                ws.claude_lights.select(None);
                redraw(ws);
                true
            }
            _ => {
                ws.claude_lights.select(None);
                redraw(ws);
                false
            }
        }
    }
}

/// The autopilot chip's accessibility description.
#[must_use]
pub(crate) fn autopilot_description(
    face: ChipFace,
    glass: &ChipGlass,
    words: TitleWords<'_>,
    today: Option<i64>,
    last: Option<String>,
) -> Option<String> {
    let stopped = words.stop_under(face);
    let switch = words.switch_under(face);
    let landed = (face == ChipFace::Landed).then(|| {
        format!(
            "{}.",
            aterm_agent::supervise::mode_words::landed_words(words.receipt)
        )
    });
    let parts: Vec<String> = [
        landed,
        stopped.map(|arm| {
            crate::harness_glass::stop_words(arm, crate::harness_glass::HoldForm::Spoken, switch)
        }),
        words.hold_under(face).and_then(|hold| {
            crate::harness_glass::hold_words(
                hold,
                words.need,
                crate::harness_glass::HoldForm::Spoken,
                today,
                words.said,
            )
        }),
        words
            .need_under(face)
            .map(crate::harness_glass::need_spoken),
        glass
            .pause
            .filter(|_| stopped.is_none())
            .map(|p| p.spoken(today)),
        last.filter(|_| stopped.is_none()),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

impl App {
    /// What a screen reader says of session `session`'s autopilot chip under `view` with `glass`.
    #[must_use]
    pub(crate) fn autopilot_spoken(
        &self,
        session: u64,
        view: AutopilotView,
        glass: &ChipGlass,
    ) -> (String, Option<String>) {
        let face = ChipFace::of(view, glass);
        let own = self.harness_glass.get(&session);
        let hold = own.and_then(crate::harness_glass::SessionGlass::hold);
        let stopped = own.and_then(crate::harness_glass::SessionGlass::stopped);
        let switch = own.is_some_and(crate::harness_glass::SessionGlass::stop_switch);
        let held = TitleWords {
            hold: hold.as_ref(),
            said: None,
            need: None,
            stopped: None,
            switch: false,
            receipt: None,
        }
        .hold_under(face)
        .is_some();
        let today = (glass.task_at.is_some() || glass.pause.is_some() || held)
            .then(|| crate::harness_glass::wall_today().1);
        let read =
            face == ChipFace::Land && hold.is_some_and(crate::harness_glass::HeldGlass::read);
        let last = crate::harness_glass::spoken_last_now(&self.harness_glass, session, read);
        let said = self
            .hold_said
            .get(&session)
            .cloned()
            .filter(|_| hold.is_some());
        let description = with_title_reads(&self.pool, session, |need, receipt| {
            let words = TitleWords {
                hold: hold.as_ref(),
                said: said.as_deref(),
                need,
                stopped,
                switch,
                receipt: receipt.map(std::sync::Arc::as_ref),
            };
            autopilot_description(face, glass, words, today, last)
        });
        (face.spoken(glass, today), description)
    }
}

/// The harness's chips as a screen reader meets them.
#[cfg(a11y_tree)]
impl App {
    /// The harness's chips on window `wid`'s glass, as the accessibility tree publishes them.
    pub(crate) fn claude_chips_a11y(&self, wid: WindowId) -> Vec<crate::a11y_tree::GridChip> {
        use crate::a11y_tree::{GridChip, HarnessChip};
        let Some(ws) = self.windows.get(&wid) else {
            return Vec::new();
        };
        let strip = usize::from(self.chrome_rows(wid));
        let mut chips: Vec<GridChip> = Vec::new();
        for hit in &ws.claude_lights.hits {
            let chip = match hit.chip {
                Some(Chip::Autopilot) => HarnessChip::Autopilot,
                Some(Chip::Team) => HarnessChip::Team,
                Some(Chip::Light(Light::Mode)) => HarnessChip::Mode,
                None => continue,
            };
            let Some(row) = hit.frame_row.checked_sub(strip) else {
                continue;
            };
            if self.chrome_owns_terminal_row(wid, hit.term_row) {
                continue;
            }
            match chips.last_mut() {
                Some(last)
                    if (last.session, last.chip, last.row) == (hit.session, chip, row)
                        && last.col + last.width == hit.col =>
                {
                    last.width += 1;
                }
                _ => chips.push(GridChip {
                    session: hit.session,
                    chip,
                    label: String::new(),
                    description: None,
                    toggled: None,
                    row,
                    col: hit.col,
                    width: 1,
                }),
            }
        }
        let mut switches = Vec::new();
        for b in &ws.claude_lights.border_toggles {
            let Some(row) = b.frame_row.checked_sub(strip) else {
                continue;
            };
            if self.chrome_owns_terminal_row(wid, b.term_row) {
                continue;
            }
            switches.extend(harness_switch_nodes(b, row));
        }
        for node in &mut chips {
            match node.chip {
                HarnessChip::TopEffort
                | HarnessChip::Model
                | HarnessChip::Effort
                | HarnessChip::Path
                | HarnessChip::Branch
                | HarnessChip::Context
                | HarnessChip::Usage
                | HarnessChip::Account
                | HarnessChip::Title
                | HarnessChip::Link(_)
                | HarnessChip::Upgrade => {}
                HarnessChip::Mode => {
                    if let Some(s) = ws.claude_lights.sessions.get(&node.session) {
                        (node.label, node.description) = light_spoken(s, Light::Mode, lights_now());
                    }
                }
                HarnessChip::Autopilot => {
                    let view = ws
                        .claude_lights
                        .sessions
                        .get(&node.session)
                        .and_then(|s| s.autopilot)
                        .unwrap_or(AutopilotView::Unhosted);
                    let glass = self.chip_glass(node.session);
                    let (label, description) = self.autopilot_spoken(node.session, view, &glass);
                    node.label = label;
                    node.description = description;
                }
                HarnessChip::Team => {
                    if let Some(face) = ws
                        .claude_lights
                        .sessions
                        .get(&node.session)
                        .and_then(|s| s.team.as_ref())
                    {
                        (node.label, node.description) = team_spoken(face);
                    }
                }
                HarnessChip::Switch => {}
            }
        }
        for hit in &ws.claude_lights.fact_hits {
            let Some(row) = hit.frame_row.checked_sub(strip) else {
                continue;
            };
            if self.chrome_owns_terminal_row(wid, hit.term_row) {
                continue;
            }
            let (mut label, description) = self.fact_spoken(hit.session, hit.fact, hit.restores);
            let cli = self.session_cli(hit.session);
            // The footer's account is judged against Claude Code's roster alone, so another
            // CLI's chip names no label from it (as its menu takes none): its product alone.
            let judged = (cli == Some(aterm_agent::harness::accounts::Cli::Claude))
                .then(|| {
                    self.shown_facts(hit.session)
                        .and_then(|(facts, _)| facts.account)
                })
                .flatten();
            if hit.fact == Fact::Account
                && hit.restores.is_empty()
                && let Some(cli) = cli
                && let Some(named) = crate::a11y_tree::account_chip_label(cli, judged.as_deref())
            {
                label = named;
            }
            chips.push(GridChip {
                session: hit.session,
                chip: HarnessChip::of_fact(hit.fact),
                label,
                description: (!description.is_empty()).then_some(description),
                toggled: self.fact_toggled(hit.session, hit.fact),
                row,
                col: hit.cols.0,
                width: hit.cols.1.saturating_sub(hit.cols.0),
            });
        }
        for title in &ws.claude_lights.title_hits {
            let Some(row) = title.frame_row.checked_sub(strip) else {
                continue;
            };
            if self.chrome_owns_terminal_row(wid, title.term_row) {
                continue;
            }
            let words: String = ws
                .input_scratch
                .cells
                .get(title.frame_row)
                .and_then(|cells| cells.get(title.cols.clone()))
                .map(|cells| cells.iter().map(|c| c.ch).collect())
                .unwrap_or_default();
            chips.push(GridChip {
                session: title.session,
                chip: HarnessChip::Title,
                label: words.trim().to_owned(),
                description: None,
                toggled: None,
                row,
                col: title.cols.start,
                width: title.cols.len(),
            });
        }
        chips.retain(|node| !node.label.is_empty());
        chips.extend(switches);
        chips.extend(self.claude_links_a11y(wid, strip));
        chips
    }

    /// A screen reader clicked session `session`'s `chip` on window `wid`.
    pub(crate) fn press_claude_chip_for_reader(
        &mut self,
        wid: WindowId,
        session: u64,
        chip: crate::a11y_tree::HarnessChip,
    ) {
        use crate::a11y_tree::HarnessChip;
        let chip = match chip {
            HarnessChip::Autopilot => Chip::Autopilot,
            HarnessChip::Team => Chip::Team,
            HarnessChip::Mode => Chip::Light(Light::Mode),
            HarnessChip::Switch => {
                let shown = self.windows.get(&wid).and_then(|ws| {
                    ws.claude_lights
                        .border_toggles
                        .iter()
                        .find(|b| {
                            b.session == session && !self.chrome_owns_terminal_row(wid, b.term_row)
                        })
                        .map(|b| b.face)
                });
                if let Some(face) = shown {
                    self.press_switch_as_clicked(wid, session, face, Instant::now());
                }
                return;
            }
            HarnessChip::Title => return,
            HarnessChip::Link(link) => {
                self.open_claude_link_for_reader(wid, session, link);
                return;
            }
            HarnessChip::Upgrade => {
                self.press_upgrade_arrow(wid, session);
                return;
            }
            fact => {
                let Some(fact) = fact.fact() else {
                    return;
                };
                let Some(restores) = self.windows.get(&wid).and_then(|ws| {
                    ws.claude_lights
                        .fact_hits
                        .iter()
                        .find(|h| h.session == session && h.fact == fact)
                        .map(|h| h.restores)
                }) else {
                    return;
                };
                self.press_visible_claude_fact(wid, session, fact, restores);
                return;
            }
        };
        self.press_visible_claude_chip(wid, session, Some(chip), Instant::now());
    }

    /// A screen reader asked for session `session`'s Autopilot chip's menu on window `wid`.
    pub(crate) fn open_autopilot_menu_for_reader(&mut self, wid: WindowId, session: u64) -> bool {
        let Some(anchor) = self.windows.get(&wid).and_then(|ws| {
            ws.claude_lights
                .hits
                .iter()
                .filter(|hit| {
                    hit.session == session
                        && hit.chip == Some(Chip::Autopilot)
                        && !self.chrome_owns_terminal_row(wid, hit.term_row)
                })
                .map(|hit| (hit.frame_row, hit.col))
                .min_by_key(|&(_, col)| col)
        }) else {
            return false;
        };
        self.open_autopilot_menu(wid, session, anchor, true);
        true
    }
}

/// The permission-mode light as a screen reader names it: the painted word in its name (WCAG
/// 2.5.3), what the state does and where a click goes as its description.
#[cfg(a11y_tree)]
fn light_spoken(s: &SessionLights, light: Light, now: Instant) -> (String, Option<String>) {
    let word = s
        .shown
        .mode
        .map_or("unknown", |mode| mode.word_on_glass(s.rewritten));
    let refused = s.refused.filter(|(_, _, until)| *until > now);
    let LightTitle { spoken, .. } = light_title(s, light, refused);
    (format!("{}: {word}.", light.title()), Some(spoken))
}

/// The harness switch's accessible name: the words it paints and, under the agent's own switch
/// off, whose harness that is (`product`'s). Lit, or under the master switch off, the switch is
/// the whole harness, every agent's, so no product is named, nor while it turns.
#[cfg(a11y_tree)]
pub(crate) fn switch_label(
    words: &str,
    face: crate::autopilot::HarnessSwitch,
    product: &str,
) -> String {
    use crate::autopilot::{HarnessSwitch, SwitchOff};
    match face {
        HarnessSwitch::Off(SwitchOff::Agent(_)) => format!("{words}: {product} harness"),
        _ => format!("{words}: harness"),
    }
}

/// Harness switch `b`'s accessibility nodes on grid row `row`.
#[cfg(a11y_tree)]
pub(crate) fn harness_switch_nodes(
    b: &BorderSwitch,
    row: usize,
) -> Vec<crate::a11y_tree::GridChip> {
    use crate::a11y_tree::{GridChip, HarnessChip};
    let product = b.program.product();
    let mut nodes = Vec::with_capacity(2);
    if !b.cols.is_empty() {
        let label = switch_label(&crate::claude_footer::switch_words(b), b.face, product);
        nodes.push(GridChip {
            session: b.session,
            chip: HarnessChip::Switch,
            label,
            description: Some(b.face.described(product)),
            toggled: Some(match b.face.toggled() {
                Some(on) => on.into(),
                None => aterm_a11y::model::Toggled::Mixed,
            }),
            row,
            col: b.cols.start,
            width: b.cols.len(),
        });
    }
    let up_words = crate::claude_footer::up_said(&b.paint.up, b.paint.jobs.as_str(), b.paint.press);
    if let (Some(cols), Some(words)) = (b.arrow.as_ref(), up_words) {
        let description = if b.paint.refused.is_empty() {
            words
        } else {
            format!("couldn't update: {}", b.paint.refused.as_str())
        };
        nodes.push(GridChip {
            session: b.session,
            chip: HarnessChip::Upgrade,
            label: upgrade_label(product, &b.paint.up),
            description: Some(description),
            toggled: None,
            row,
            col: cols.start,
            width: cols.len(),
        });
    }
    nodes
}

/// The ⬆️'s button as a screen reader names it: `Update Claude Code to 2.1.310`, `Updating …`
/// once asked.
#[cfg(a11y_tree)]
fn upgrade_label(product: &str, up: &crate::claude_footer::TopUp) -> String {
    let verb = if up.state == crate::claude_footer::UpState::Asked {
        "Updating"
    } else {
        "Update"
    };
    format!("{verb} {product} to {}", up.to.as_str())
}

/// The Team chip as a screen reader names it.
#[cfg(a11y_tree)]
fn team_spoken(face: &TeamFace) -> (String, Option<String>) {
    let label = match face {
        TeamFace::Yours => "You have this tab".to_owned(),
        TeamFace::Worker { label, .. } => format!("Worker of {label}"),
        TeamFace::Manager { workers: 1, .. } => "Manages 1 worker".to_owned(),
        TeamFace::Manager { workers, .. } => format!("Manages {workers} workers"),
    };
    let act = face.act().map(|act| {
        match act {
            TeamAct::GoTo(_) => "Goes to its manager.",
            TeamAct::Overview => "Opens the Overview at it.",
            TeamAct::HandBack => "Hands the tab back.",
        }
        .to_owned()
    });
    (label, act)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mode light's chip.
    const MODE: Chip = Chip::Light(Light::Mode);

    #[test]
    fn inert_session_history_does_not_change_the_lights_repaint_key() {
        let now = Instant::now();
        let mut w = WindowLights {
            hover: Some((7, Target::from(MODE))),
            ..WindowLights::default()
        };
        w.sessions.insert(7, SessionLights::default());
        let hovered = w.fingerprint(|| now);
        assert_ne!(hovered, 0);

        for session in 100..1_100 {
            w.sessions.insert(session, SessionLights::default());
        }
        assert_eq!(w.fingerprint(|| now), hovered);

        w.sessions.get_mut(&1_000).unwrap().refused =
            Some((Light::Mode, Refusal::NotShown, now + Duration::from_secs(1)));
        assert_ne!(w.fingerprint(|| now), hovered);
        assert_eq!(w.fingerprint(|| now + Duration::from_secs(2)), hovered);

        w.hover = None;
        assert_eq!(w.fingerprint(|| now + Duration::from_secs(2)), 0);
    }

    fn blank() -> RenderCell {
        RenderCell {
            ch: ' ',
            fg: [200, 200, 200],
            bg: [0, 0, 0],
            ..RenderCell::default()
        }
    }

    fn screen(rule: &str, mode_row: &str, draft: &str) -> Vec<String> {
        vec![
            rule.into(),
            format!("\u{276F} {draft}"),
            "\u{2500}".repeat(60),
            mode_row.into(),
        ]
    }

    const BYPASS: &str = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)";
    const AUTO: &str = "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)";
    const PLAN: &str = "  \u{23F8} plan mode on (shift+tab to cycle)";
    const ACCEPT: &str = "  \u{23F5}\u{23F5} accept edits on (shift+tab to cycle)";
    const MANUAL: &str = "  \u{23F8} manual mode on \u{00B7} ? for shortcuts";

    fn observe(w: &mut WindowLights, rows: &[String]) -> Step {
        w.observe_rows(7, rows, lights::read_lights)
    }

    #[test]
    fn a_reader_panic_folds_nothing_in_and_types_nothing() {
        let now = Instant::now();
        let rule = "\u{2500}".repeat(60);
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&rule, MANUAL, ""));
        start(&mut w, Light::Mode, Mode::Manual, now);
        let shown = w.sessions[&7].shown;
        let boom = |_: &[String]| -> Option<Reading> { panic!("stand-in reader panic") };
        let plan = screen(&rule, PLAN, "");
        assert_eq!(w.observe_rows(7, &plan, boom), Step::default());
        let s = &w.sessions[&7];
        assert_eq!(s.shown, shown, "the lights keep what they showed");
        let p = s.pending.as_ref().expect("the toggle is still in flight");
        assert_eq!((p.presses, p.mode_at_press), (1, Mode::Manual));
        assert!(
            fenced_screen_now(7, &plan, || -> Option<Screen> {
                panic!("stand-in reader panic")
            })
            .is_none()
        );
        assert!(
            fenced_screen_now(7, &plan, || lights::read_screen(&plan)).is_some(),
            "the real reader sees the composer"
        );
        assert!(observe(&mut w, &plan).wake);
    }

    fn start(w: &mut WindowLights, light: Light, mode: Mode, now: Instant) {
        let s = w.sessions.entry(7).or_default();
        s.pending = Some(Pending {
            light,
            goal: Goal::of_mode(mode),
            started: mode,
            mode_at_press: mode,
            seen: Vec::new(),
            presses: 1,
            deadline: now + Duration::from_millis(lights::SETTLE_MS),
            next_poll: None,
            held_mid_turn: false,
        });
    }

    #[test]
    fn only_deviations_are_drawn_and_the_block_keeps_what_it_saw() {
        let now = Instant::now();
        let bare = "\u{2500}".repeat(60);
        let mut w = WindowLights::default();
        for mode in [BYPASS, AUTO] {
            observe(&mut w, &screen(&bare, mode, ""));
            let rest = w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Revealed);
            assert!(
                rest.lights.is_empty() && rest.cols.is_empty() && rest.title.is_empty(),
                "nothing at rest: {mode:?}"
            );
        }
        observe(&mut w, &screen(&bare, PLAN, ""));
        let block = w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Revealed);
        let text: String = block.lights.iter().map(|c| c.ch).collect();
        assert_eq!(text, "\u{23F8} plan");
        assert_eq!(block.cols, vec![(0..6, MODE)]);
        assert!(block.title.is_empty(), "nothing asked for a title");
        assert_eq!(block.lights[0].fg, blank().fg, "plan, in the row's ink");
        w.observe(7, None);
        assert_eq!(
            w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Revealed)
                .lights,
            block.lights,
            "a covered composer keeps the last reading"
        );
        w.hover = Some((7, Target::from(MODE)));
        let hovered = w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Revealed);
        assert!(!hovered.title.is_empty(), "a hover asks for the title");
        assert_eq!(
            hovered.lights.len(),
            block.lights.len(),
            "the title is apart"
        );
        assert!(
            hovered.lights.iter().all(|c| c.bold),
            "the whole hovered chip is marked"
        );

        let mut w = WindowLights::default();
        observe(&mut w, &screen(&bare, BYPASS, ""));
        w.note_autopilot(7, None, AutopilotView::Unhosted, now);
        assert!(
            w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Revealed)
                .cols
                .is_empty(),
            "no host: no chip"
        );
        w.note_autopilot(7, None, AutopilotView::Cruise, now);
        let rest = w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Revealed);
        let text: String = rest.lights.iter().map(|c| c.ch).collect();
        assert_eq!(text, "\u{221e}", "at rest: Autopilot alone");
        assert_eq!(rest.cols, vec![(0..1, Chip::Autopilot)]);
        assert_eq!(
            rest.lights[0].fg,
            ChipAt::cruise(blank()).letter_at(0),
            "cruise in its column's letter"
        );
        assert!(rest.title.is_empty(), "nothing asked for a title");
        observe(&mut w, &screen(&bare, PLAN, ""));
        let text: String = w
            .block(7, blank(), now, ChipAt::cruise(blank()), Keep::Revealed)
            .lights
            .iter()
            .map(|c| c.ch)
            .collect();
        assert_eq!(text, "\u{221e} \u{23F8} plan", "the light follows");
    }

    /// A WORDED CHIP NEVER RUNS INTO THE NEXT: after `⊘ off` the rule threads between the chips
    /// (` ─ `, the rule's own cell once laid in the rule), so `⊘ off ─ ⏸ plan` never reads as one
    /// phrase, nor does a click's mode name (`⤓ land ─ ⏸ plan`); the block's width counts it (the
    /// name's cells apart, [`Block::word`]) and no chip's hit takes it. CONTROL: a bare glyph
    /// (`∞`, `⤓`, `○`) stays one blank from the next chip.
    #[test]
    fn a_worded_chip_never_runs_into_the_next() {
        let now = Instant::now();
        let bare = "\u{2500}".repeat(60);
        let text = |cells: &[RenderCell]| -> String { cells.iter().map(|c| c.ch).collect() };
        for (view, named, lead) in [
            (AutopilotView::MasterOff, false, "\u{2298} off \u{2500} "),
            (AutopilotView::Off, false, "\u{25cb} "),
            (AutopilotView::Land, false, "\u{2913} "),
            (AutopilotView::Land, true, "\u{2913} land \u{2500} "),
            (AutopilotView::Cruise, false, "\u{221e} "),
        ] {
            let mut w = WindowLights::default();
            observe(&mut w, &screen(&bare, PLAN, ""));
            w.note_autopilot(7, view.mode(), view, now);
            if named {
                w.sessions.get_mut(&7).expect("noted").word = Some(ChipWord {
                    mode: Autopilot::Land,
                    from: now,
                    fades: true,
                    asked: 0,
                });
            }
            let block = w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Revealed);
            assert_eq!(
                text(&block.lights),
                format!("{lead}\u{23F8} plan"),
                "{view:?}"
            );
            let gaps: Vec<usize> = block.rule_gaps().collect();
            let worded = lead.contains('\u{2500}');
            assert_eq!(
                gaps,
                if worded {
                    vec![lead.chars().count() - 2]
                } else {
                    Vec::new()
                },
                "{view:?}"
            );
            assert!(
                gaps.iter()
                    .all(|g| !block.cols.iter().any(|(cols, _)| cols.contains(g))),
                "{view:?}: no chip's hit takes the rule"
            );
            let s = &w.sessions[&7];
            let inks = Inks::of(blank(), ChipAt::cruise(blank()));
            let drawn = drawn_at(s, now, None, None, Keep::Revealed);
            assert_eq!(
                chips_width_at(
                    s,
                    Chip::row().filter(|c| drawn(*c)),
                    Keep::Revealed,
                    inks,
                    None
                ) + block.word,
                block.lights.len(),
                "{view:?}: the fit counts it, the name apart"
            );
        }
    }

    /// THE MODE'S CHIP KEEPS ONE BLANK BEFORE THE TEAM CHIP: the rule sets off the Autopilot
    /// chip's word alone, where `⊘ off yolo` would misread; after the mode's word the Team chip's
    /// own glyph sets it off (`∞ ⏸ plan ⇠ lab-sweep`), as before the rule was drawn, and the fit
    /// counts what is drawn. CONTROL: a harness held off keeps the rule before the mode.
    #[test]
    fn the_mode_chip_keeps_one_blank_before_the_team_chip() {
        let now = Instant::now();
        let bare = "\u{2500}".repeat(60);
        let text = |cells: &[RenderCell]| -> String { cells.iter().map(|c| c.ch).collect() };
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&bare, PLAN, ""));
        w.sessions.get_mut(&7).unwrap().team = Some(TeamFace::Worker {
            manager: Some(3),
            label: std::sync::Arc::from("lab-sweep"),
            task: None,
        });
        for (view, want, gaps) in [
            (
                AutopilotView::Cruise,
                "\u{221e} \u{23F8} plan \u{21e0} lab-sweep",
                0,
            ),
            (
                AutopilotView::MasterOff,
                "\u{2298} off \u{2500} \u{23F8} plan \u{21e0} lab-sweep",
                1,
            ),
        ] {
            w.note_autopilot(7, view.mode(), view, now);
            let block = w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Revealed);
            assert_eq!(text(&block.lights), want, "{view:?}");
            assert_eq!(block.rule_gaps().count(), gaps, "{view:?}: the rule cells");
            let s = &w.sessions[&7];
            let drawn = drawn_at(s, now, None, None, Keep::Revealed);
            assert_eq!(
                chips_width_at(
                    s,
                    Chip::row().filter(|c| drawn(*c)),
                    Keep::Revealed,
                    Inks::of(blank(), ChipAt::cruise(blank())),
                    None,
                ),
                block.lights.len(),
                "{view:?}: the fit counts it"
            );
        }
    }

    /// THE AUTOPILOT CHIP SAYS ITS VIEW IN ITS INK: under Cruise and Land the chip wears the
    /// letter of its own column in its view's palette, Off and the harness held off the quiet
    /// ink; no view reads the system accent or Claude's pill amber. CONTROL: Land's letter at the
    /// chip's column is not Cruise's, so the palette, not a fixed ink, decides.
    #[test]
    fn the_autopilot_chip_says_its_view_in_its_ink() {
        use aterm_effects::harness_card::CardPalette;
        let now = Instant::now();
        let mut w = WindowLights::default();
        let (cruise, land, off, none) = (
            ChipAt::cruise(blank()),
            ChipAt::land(blank()),
            ChipAt::test(Some(CardPalette::Off), blank()),
            ChipAt::test(None, blank()),
        );
        let rows = [
            (
                AutopilotView::Cruise,
                cruise,
                "\u{221e}",
                cruise.letter_at(0),
                format!("{} \u{b7} right-click: more", AutopilotView::Cruise.title()),
                "autopilot: cruise",
            ),
            (
                AutopilotView::Land,
                land,
                "\u{2913}",
                land.letter_at(0),
                format!("{} \u{b7} right-click: more", AutopilotView::Land.title()),
                "autopilot: land",
            ),
            (
                AutopilotView::Off,
                off,
                "\u{25cb}",
                off.quiet(),
                format!("{} \u{b7} right-click: more", AutopilotView::Off.title()),
                "autopilot: off",
            ),
            (
                AutopilotView::MasterOff,
                none,
                "\u{2298} off",
                none.quiet(),
                "harness off in Settings \u{b7} click: turn the harness on \u{b7} right-click: more"
                    .to_owned(),
                "harness off in Settings",
            ),
        ];
        let text = |cells: &[RenderCell]| -> String { cells.iter().map(|c| c.ch).collect() };
        for (view, at, face, ink, title, short) in rows {
            w.note_autopilot(7, view.mode(), view, now);
            w.hover = Some((7, Target::from(Chip::Autopilot)));
            let block = w.block(7, blank(), now, at, Keep::Drawn);
            let n = face.chars().count();
            assert_eq!(text(&block.lights), face, "{view:?}");
            assert_eq!(block.cols[0], (0..n, Chip::Autopilot), "{view:?}");
            assert_eq!(
                text(&block.lights[..n]),
                view.label(),
                "{view:?}: its label"
            );
            assert!(
                block.lights[..n].iter().all(|c| c.fg == ink && c.bold),
                "{view:?}: its ink, marked under the pointer"
            );
            assert_eq!(text(&block.title), format!("{title}  "), "{view:?}");
            assert_eq!(text(&block.short_title), format!("{short}  "), "{view:?}");
            assert!(!block.reason, "a hover's title gives way first");
        }
        assert_ne!(
            land.letter_at(0),
            cruise.letter_at(0),
            "CONTROL: the view's palette decides the letter"
        );
        assert_eq!(
            ModeInk::Value,
            Mode::Auto.ink(),
            "auto reads as a value, not the pill's amber"
        );

        w.hover = None;
        w.note_autopilot(7, None, AutopilotView::Cruise, now);
        w.sessions.get_mut(&7).unwrap().autopilot_pending =
            Some((Autopilot::Land, now + Duration::from_secs(1)));
        let block = w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Drawn);
        assert_eq!(text(&block.lights), "\u{25cc}", "in flight");
        w.note_autopilot(7, None, AutopilotView::Cruise, now);
        assert!(
            w.sessions[&7].autopilot_pending.is_some(),
            "not read back yet"
        );
        w.note_autopilot(7, Some(Autopilot::Land), AutopilotView::Land, now);
        assert!(w.sessions[&7].autopilot_pending.is_none(), "read back");
        assert_eq!(
            text(
                &w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Drawn)
                    .lights
            ),
            "\u{2913}"
        );
        w.sessions.get_mut(&7).unwrap().autopilot_pending =
            Some((Autopilot::Off, now + Duration::from_secs(1)));
        assert_eq!(w.expire(now), (false, Vec::new()), "still in time");
        assert_eq!(w.expire(now + Duration::from_secs(1)), (true, Vec::new()));
        assert!(w.sessions[&7].autopilot_pending.is_none());
        assert_eq!(
            w.fingerprint(|| now),
            0,
            "the lights' own key stays 0 at rest"
        );
    }

    /// A CHIP'S TITLE READS AS A VALUE, THE OFF CHIPS AS QUIET WORDS: on four built-in grounds
    /// every title cell (whole, lean and short, a refusal's too) wears the glass's value ink, at
    /// 7:1 or more, and `○` the quiet ink, at 4.5:1 and quieter than the value. CONTROL: the ink
    /// the titles and the off chips wore before, the pane's halfway to its ground floored at 3:1
    /// alone, misses 4.5:1 on some ground.
    #[test]
    fn a_chips_title_reads_as_a_value_and_the_off_chips_quietly() {
        use crate::chrome_band::contrast;
        let now = Instant::now();
        let rgb = |c: aterm_types::Rgb| [c.r, c.g, c.b];
        let mut old_misses = 0;
        for name in ["Default", "Aterm Night", "Aterm Day", "Linen"] {
            let scheme = aterm_types::scheme::builtin(name).expect("a built-in theme");
            let blank = RenderCell {
                ch: ' ',
                fg: rgb(scheme.foreground),
                bg: rgb(scheme.background),
                ..RenderCell::default()
            };
            let bg = blank.bg;
            let at = ChipAt::cruise(blank);
            let ink = at.value();
            let in_ink = |cells: &[RenderCell]| cells.iter().all(|c| c.fg == ink);
            let mut w = WindowLights::default();
            observe(&mut w, &screen(&"\u{2500}".repeat(60), PLAN, ""));
            w.note_autopilot(7, None, AutopilotView::Cruise, now);
            w.hover = Some((7, Target::from(Chip::Autopilot)));
            let block = w.block(7, blank, now, at, Keep::Drawn);
            assert!(
                !block.title.is_empty() && !block.short_title.is_empty(),
                "{name}: a title"
            );
            assert!(!block.lean[1].is_empty(), "{name}: v1's words, a lean form");
            assert!(
                in_ink(&block.title)
                    && in_ink(&block.short_title)
                    && block.lean.iter().all(|form| in_ink(form)),
                "{name}: every title cell in the value ink"
            );
            w.hover = None;
            w.sessions.get_mut(&7).unwrap().refused = Some((
                Light::Mode,
                Refusal::StoppedIn(Mode::Plan),
                now + REFUSAL_SHOWN,
            ));
            let refused = w.block(7, blank, now, at, Keep::Drawn);
            assert!(refused.reason && !refused.title.is_empty(), "{name}");
            assert!(in_ink(&refused.title), "{name}: the refusal's ink");
            let halfway = std::array::from_fn(|k| {
                ((u16::from(blank.fg[k]) + u16::from(blank.bg[k])) / 2) as u8
            });
            let (title, quiet, old) = (
                contrast(ink, bg),
                contrast(at.quiet(), bg),
                contrast(crate::claude_footer::visible_on(halfway, blank.bg), bg),
            );
            eprintln!("{name}: title {ink:?} {title:.2}:1, quiet {quiet:.2}:1, old {old:.2}:1");
            assert!(
                title >= f64::from(aterm_effects::glass_inks::VALUE_CONTRAST) - 0.01,
                "{name}: a value reads at 7:1 ({title:.2})"
            );
            assert!(quiet >= 4.5, "{name}: the off chips read ({quiet:.2})");
            assert!(
                quiet < title,
                "{name}: and stay quiet ({quiet:.2} < {title:.2})"
            );
            old_misses += usize::from(old < 4.5);
            w.note_autopilot(7, Some(Autopilot::Off), AutopilotView::Off, now);
            let off = w.block(7, blank, now, at, Keep::Drawn);
            assert_eq!(off.lights[0].fg, at.quiet(), "{name}: `○`, the quiet ink");
        }
        assert!(
            old_misses > 0,
            "CONTROL: the old halfway ink reads 4.5:1 everywhere"
        );
    }

    #[test]
    fn the_team_chip_is_drawn_for_a_team_and_given_up_before_autopilot() {
        let now = Instant::now();
        let bare = "\u{2500}".repeat(60);
        let worker = TeamFace::Worker {
            manager: Some(3),
            label: std::sync::Arc::from("lab-sweep"),
            task: None,
        };
        let manager = TeamFace::Manager {
            mark: std::sync::Arc::from("\u{21c9}3"),
            workers: 3,
        };
        let chips = |w: &WindowLights, room: usize| -> Vec<Chip> {
            w.fitted(7, blank(), now, ChipAt::cruise(blank()), None, None, room)
                .map(|b| b.cols.iter().map(|(_, chip)| *chip).collect())
                .unwrap_or_default()
        };
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&bare, BYPASS, ""));
        assert!(chips(&w, 40).is_empty(), "control: no team, no chip");
        w.sessions.get_mut(&7).unwrap().team = Some(manager.clone());
        assert_eq!(chips(&w, 40), [Chip::Team]);
        let block = w
            .fitted(7, blank(), now, ChipAt::cruise(blank()), None, None, 40)
            .unwrap();
        assert_eq!(
            block.lights.iter().map(|c| c.ch).collect::<String>(),
            "\u{21c9}3",
            "the manager's count"
        );
        w.sessions.get_mut(&7).unwrap().team = Some(TeamFace::Yours);
        let block = w
            .fitted(7, blank(), now, ChipAt::cruise(blank()), None, None, 40)
            .unwrap();
        assert!(block.lights[0].ch == '\u{270b}' && block.lights[1].wide);
        assert_eq!(block.lights.len(), cells_of("\u{270b} you"));
        observe(&mut w, &screen(&bare, PLAN, ""));
        w.note_autopilot(7, None, AutopilotView::Cruise, now);
        for face in [worker, manager, TeamFace::Yours] {
            w.sessions.get_mut(&7).unwrap().team = Some(face.clone());
            let mut wider = chips(&w, 40);
            assert_eq!(
                wider,
                [Chip::Autopilot, MODE, Chip::Team],
                "{face:?}: every chip, Team right of the light"
            );
            let mut gave_up: Vec<Chip> = Vec::new();
            for room in (0..40).rev() {
                let narrower = chips(&w, room);
                assert!(narrower.iter().all(|chip| wider.contains(chip)));
                gave_up.extend(wider.iter().filter(|chip| !narrower.contains(chip)));
                wider = narrower;
            }
            assert_eq!(
                gave_up,
                [MODE, Chip::Team, Chip::Autopilot],
                "{face:?}: the light, Team, then Autopilot"
            );
        }
    }

    #[test]
    fn a_team_chip_whose_press_does_nothing_promises_no_act() {
        let now = Instant::now();
        let worker = |manager: Option<u64>| TeamFace::Worker {
            manager,
            label: std::sync::Arc::from("lab-sweep"),
            task: Some(("working", now)),
        };
        let manager = TeamFace::Manager {
            mark: std::sync::Arc::from("\u{21c9}3"),
            workers: 3,
        };
        let faces = [
            (TeamFace::Yours, Some(TeamAct::HandBack)),
            (worker(Some(3)), Some(TeamAct::GoTo(3))),
            (worker(None), None),
            (manager, Some(TeamAct::Overview)),
        ];
        for (face, act) in faces {
            assert_eq!(face.act(), act, "{face:?}");
            let (whole, short) = team_title(Some(&face), now);
            assert!(!short.is_empty(), "{face:?}: it says what it is");
            assert_eq!(whole == short, act.is_none(), "{face:?}: {whole:?}");
            #[cfg(a11y_tree)]
            {
                let (label, description) = team_spoken(&face);
                assert!(
                    label
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == ' ' || c == '-'),
                    "{face:?}: words, no glyph: {label:?}"
                );
                assert_eq!(description.is_some(), act.is_some(), "{face:?}");
            }
        }
        assert_eq!(team_title(None, now), (String::new(), String::new()));
    }

    #[test]
    fn a_narrow_rule_gives_up_the_reveal_the_light_then_autopilot() {
        let now = Instant::now();
        let bare = "\u{2500}".repeat(60);
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&bare, PLAN, ""));
        w.note_autopilot(7, None, AutopilotView::Cruise, now);
        let fitted = |w: &WindowLights, selected: Option<Chip>, room: usize| {
            w.fitted(
                7,
                blank(),
                now,
                ChipAt::cruise(blank()),
                selected,
                None,
                room,
            )
            .map(|b| b.lights.iter().map(|c| c.ch).collect::<String>())
        };
        for room in 8..=10 {
            assert_eq!(
                fitted(&w, None, room).as_deref(),
                Some("\u{221e} \u{23F8} plan"),
                "{room}: every chip"
            );
        }
        for room in 1..=7 {
            assert_eq!(
                fitted(&w, None, room).as_deref(),
                Some("\u{221e}"),
                "{room}: then the light, Autopilot last"
            );
        }
        assert_eq!(fitted(&w, None, 0), None, "then every chip");
        let fits = |w: &WindowLights, chip: Chip, room: usize| {
            w.fitted(
                7,
                blank(),
                now,
                ChipAt::cruise(blank()),
                Some(chip),
                None,
                room,
            )
            .is_some_and(|b| b.shows(chip))
        };
        assert!(fits(&w, MODE, 8) && !fits(&w, MODE, 7));
        assert!((1..=10).all(|room| fits(&w, Chip::Autopilot, room)));
        assert!(!fits(&w, Chip::Autopilot, 0));
        observe(&mut w, &screen(&bare, ACCEPT, ""));
        assert_eq!(
            fitted(&w, None, 17).as_deref(),
            Some("\u{221e} \u{23F5}\u{23F5} accept edits")
        );
        for room in 1..=16 {
            assert_eq!(
                fitted(&w, None, room).as_deref(),
                Some("\u{221e}"),
                "{room}: then the light, and never back"
            );
        }
        observe(&mut w, &screen(&bare, BYPASS, ""));
        assert_eq!(
            fitted(&w, Some(Chip::Autopilot), 11).as_deref(),
            Some("\u{221e} \u{23F5}\u{23F5} bypass"),
            "revealed"
        );
        assert_eq!(
            fitted(&w, Some(Chip::Autopilot), 10).as_deref(),
            Some("\u{221e}"),
            "the reveal first"
        );
        let drawn = |w: &WindowLights, selected: Option<Chip>, room: usize| -> Vec<Chip> {
            w.fitted(
                7,
                blank(),
                now,
                ChipAt::cruise(blank()),
                selected,
                None,
                room,
            )
            .map(|b| b.cols.iter().map(|(_, chip)| *chip).collect())
            .unwrap_or_default()
        };
        for view in [AutopilotView::Cruise, AutopilotView::Land] {
            w.note_autopilot(7, view.mode(), view, now);
            for mode_row in [PLAN, ACCEPT, MANUAL, BYPASS] {
                observe(&mut w, &screen(&bare, mode_row, ""));
                for selected in [None, Some(Chip::Autopilot), Some(MODE)] {
                    let mut wider = drawn(&w, selected, 24);
                    assert!(
                        wider.contains(&Chip::Autopilot),
                        "{view:?} {mode_row:?} {selected:?}: control, every chip in 24 cells"
                    );
                    for room in (0..24).rev() {
                        let narrower = drawn(&w, selected, room);
                        assert!(
                            narrower.iter().all(|chip| wider.contains(chip)),
                            "{view:?} {mode_row:?} {selected:?} {room}: {narrower:?} after \
                             {wider:?}"
                        );
                        wider = narrower;
                    }
                    assert!(wider.is_empty(), "control: no chip in no room");
                    let s = &w.sessions[&7];
                    for keep in Keep::LADDER {
                        let block = w.block_with(
                            7,
                            blank(),
                            now,
                            ChipAt::cruise(blank()),
                            selected,
                            None,
                            keep,
                            None,
                        );
                        let on = drawn_at(s, now, selected, None, keep);
                        assert_eq!(
                            chips_width_at(
                                s,
                                Chip::row().filter(|chip| on(*chip)),
                                keep,
                                Inks::of(blank(), ChipAt::cruise(blank())),
                                None,
                            ),
                            block.lights.len(),
                            "{view:?} {mode_row:?} {selected:?} {keep:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_keyboard_selection_reveals_the_light_at_rest() {
        let now = Instant::now();
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&"\u{2500}".repeat(60), BYPASS, ""));
        assert!(
            w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Revealed)
                .cols
                .is_empty(),
            "at rest"
        );
        w.selected = Some((7, Target::from(MODE)));
        let block = w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Revealed);
        assert!(!block.title.is_empty(), "the selection asks for the title");
        assert_eq!(
            block.lights[0].ch, '\u{23F5}',
            "the expected mode, revealed"
        );
        assert!(block.lights[0].bold);
        assert!(
            w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Drawn)
                .shows(Light::Mode),
            "the selection itself is drawn without the reveal"
        );
    }

    #[test]
    fn a_quiet_row_keys_as_zero() {
        let now = Instant::now();
        let mut w = WindowLights::default();
        assert_eq!(w.fingerprint(|| now), 0);
        w.hover = Some((7, Target::from(MODE)));
        let a = w.fingerprint(|| now);
        w.selected = Some((7, Target::from(MODE)));
        assert_ne!(a, 0);
        assert_ne!(a, w.fingerprint(|| now));
    }
}

/// End-to-end in-process gesture tests for Claude Code footer chips.
#[cfg(all(test, unix))]
mod gesture_tests {
    use std::io::{ErrorKind, Read};
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use aterm_agent::harness::footer::FooterFacts;
    use aterm_agent::harness::lights::{self, Goal, Light, Mode};
    use aterm_agent::supervise::Autopilot;
    use aterm_core::terminal::RenderCell;
    use aterm_session::sink::SinkWriter;
    use aterm_window::event::{ElementState, MouseButton};

    use super::{Chip, Target};
    use crate::autopilot::AutopilotView;
    use crate::{App, WindowId, term_lock};

    /// The mode light's chip.
    pub(super) const MODE: Chip = Chip::Light(Light::Mode);

    /// One synthetic key press and release, as a person taps it.
    #[cfg(target_os = "macos")]
    pub(super) fn tap(
        app: &mut App,
        wid: WindowId,
        named: aterm_window::keyboard::NamedKey,
        code: aterm_window::keyboard::KeyCode,
        text: &str,
    ) {
        use aterm_window::keyboard::{Key, KeyLocation, PhysicalKey, SmolStr};
        for state in [ElementState::Pressed, ElementState::Released] {
            app.on_key(
                wid,
                aterm_window::event::KeyEvent::synthetic_for_test(
                    PhysicalKey::Code(code),
                    Key::Named(named),
                    Some(SmolStr::new(text)),
                    KeyLocation::Standard,
                    state,
                    false,
                ),
            );
        }
    }

    pub(super) const BYPASS: &str = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)";
    pub(super) const AUTO: &str = "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)";
    pub(super) const PLAN: &str = "  \u{23F8} plan mode on (shift+tab to cycle)";
    const ACCEPT: &str = "  \u{23F5}\u{23F5} accept edits on (shift+tab to cycle)";
    const MANUAL: &str = "  \u{23F8} manual mode on \u{00B7} ? for shortcuts";
    /// Claude Code's foreground process group, as the resolver names it.
    pub(super) const PGID: i32 = 4242;

    /// Close the private PTY observer so the next write through the session's sink fails.
    fn hang_up(app: &App, session: u64, reader: UnixStream) {
        let sink = app.pool.get(session).expect("session").ctx.sink.clone();
        drop(reader);
        let mut pfd = libc::pollfd {
            fd: sink.master(),
            events: libc::POLLIN,
            revents: 0,
        };
        let deadline = Instant::now() + Duration::from_secs(120);
        while pfd.revents & libc::POLLHUP == 0 {
            assert!(
                Instant::now() < deadline,
                "the PTY observer's socket never hung up (a copy outlived the drop)"
            );
            pfd.revents = 0;
            // SAFETY: `pfd` is one live, initialized pollfd; poll writes only
            // its `revents`, and the sink keeps `fd` open for this call.
            unsafe { libc::poll(&mut pfd, 1, 100) };
        }
    }

    /// A headless App whose one session writes its PTY input to a private socket.
    pub(super) fn app() -> (App, WindowId, u64, UnixStream) {
        let (reader, writer) = UnixStream::pair().expect("private PTY observer");
        reader.set_nonblocking(true).expect("nonblocking observer");
        let app = App::headless_for_test_with_sink(Arc::new(SinkWriter::new_owned(writer.into())));
        let wid = WindowId(0);
        let session = app.focused_session_id(wid).expect("the front session");
        (app, wid, session, reader)
    }

    /// Inert harness host for test windows, shut down on drop.
    pub(super) struct Hosted(crate::harness_host::HostHandle);

    impl Drop for Hosted {
        fn drop(&mut self) {
            self.0.shutdown_and_join();
        }
    }

    /// Give `app` a harness host ([`Hosted`]).
    pub(super) fn host(app: &mut App) -> Hosted {
        let host = crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        );
        app.harness = Some(host.clone());
        Hosted(host)
    }

    /// `ctrl+shift+tab`, as a person presses it on macOS.
    #[cfg(target_os = "macos")]
    pub(super) fn chord(app: &mut App, wid: WindowId) {
        use aterm_window::keyboard::{KeyCode, ModifiersState, NamedKey};
        app.windows.get_mut(&wid).expect("window").mods =
            ModifiersState::CONTROL | ModifiersState::SHIFT;
        tap(app, wid, NamedKey::Tab, KeyCode::Tab, "\t");
        app.windows.get_mut(&wid).expect("window").mods = ModifiersState::empty();
    }

    /// [`app`] in a 120-column pane.
    pub(super) fn wide_app() -> (App, WindowId, u64, UnixStream) {
        let (mut app, wid, session, reader) = app();
        assert!(app.apply_term_resize(wid, 24, 120));
        (app, wid, session, reader)
    }

    /// [`wide_app`] at 160 columns.
    pub(super) fn wider_app() -> (App, WindowId, u64, UnixStream) {
        let (mut app, wid, session, reader) = app();
        assert!(app.apply_term_resize(wid, 24, 160));
        (app, wid, session, reader)
    }

    /// The pane's columns.
    pub(super) fn cols(app: &App, session: u64) -> usize {
        let term = app.pool.get(session).expect("session").term.clone();
        usize::from(term_lock(&term).cols())
    }

    /// The whole facts, as [`publish_facts`] publishes them.
    pub(super) const WHOLE_FACTS: &str =
        "\u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main \u{2500}";

    /// Publish default Claude Code footer facts for `session`.
    pub(super) fn publish_facts(app: &App, session: u64) {
        publish_facts_at(app, session, "~/aterm");
    }

    /// [`publish_facts`] with working directory `path`.
    pub(super) fn publish_facts_at(app: &App, session: u64, path: &str) {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(PGID);
        assert!(timeline.set_claude_footer(
            PGID,
            None,
            Some(FooterFacts {
                model: Some("Opus 5.5".into()),
                effort: Some("xhigh".into()),
                path: Some(path.into()),
                branch: Some("main".into()),
                ..FooterFacts::default()
            }),
        ));
    }

    pub(super) fn feed(app: &App, session: u64, bytes: &[u8]) {
        let term = app.pool.get(session).expect("session").term.clone();
        term_lock(&term).process(bytes);
    }

    /// Draw Claude Code's bottom block on the 24-row engine with `mode` on the last row.
    pub(super) fn draw_claude(app: &App, session: u64, mode: &str) {
        let rule = "\u{2500}".repeat(cols(app, session));
        let screen = format!(
            "\x1b[?2004h\x1b[2J\x1b[19;1H\u{25CF} done\x1b[21;1H{rule}\x1b[22;1H\u{276F} \
             \x1b[23;1H{rule}\x1b[24;1H{mode}\x1b[22;3H"
        );
        feed(app, session, screen.as_bytes());
    }

    /// The frame row of the pane's footer.
    pub(super) fn footer_row(app: &App, wid: WindowId) -> usize {
        app.windows[&wid].input_scratch.cells.len() - usize::from(app.windows[&wid].rows) + 22
    }

    /// The frame row of Claude's own mode row.
    fn mode_row(app: &App, wid: WindowId) -> usize {
        footer_row(app, wid) + 1
    }

    /// Redraw the mode row with `mode` and return the cursor to the caret.
    pub(super) fn redraw_mode_row(app: &App, session: u64, mode: &str) {
        feed(
            app,
            session,
            format!("\x1b[24;1H\x1b[2K{mode}\x1b[22;3H").as_bytes(),
        );
    }

    /// Run one frame through the capture route's splices.
    pub(super) fn frame(app: &mut App, wid: WindowId) {
        let prepared = app.prepare_terminal_capture_grid_with_cursor_fx_and_plan_outcome(
            wid,
            crate::app_render::ComposedCursorFxClock::Advance(Instant::now()),
        );
        let crate::app_render::CapturePreparation::Ready((grid, plan)) = prepared else {
            panic!("the headless capture must produce a frame");
        };
        app.splice_tab_strip(wid);
        app.splice_claude_footer(
            wid,
            &plan,
            crate::VisibleContentRoute::Terminal {
                composed: grid.composed,
            },
        );
    }

    /// Frame `(row, col)` of `chip`'s first painted cell.
    pub(super) fn hit(app: &App, wid: WindowId, chip: impl Into<Chip>) -> (usize, usize) {
        let chip = chip.into();
        let h = app.windows[&wid]
            .claude_lights
            .hits
            .iter()
            .find(|h| h.chip == Some(chip))
            .unwrap_or_else(|| panic!("{chip:?} is painted"));
        (h.frame_row, h.col)
    }

    /// Whether the painter put `chip` on the glass this frame.
    pub(super) fn painted(app: &App, wid: WindowId, chip: impl Into<Chip>) -> bool {
        let chip = chip.into();
        app.windows[&wid]
            .claude_lights
            .hits
            .iter()
            .any(|h| h.chip == Some(chip))
    }

    pub(super) fn cell_at(app: &App, wid: WindowId, (row, col): (usize, usize)) -> RenderCell {
        app.windows[&wid].input_scratch.cells[row][col]
    }

    /// The glass's tokens on `wid`'s one pane under `palette`, its chips' first cell at frame
    /// column `col`.
    fn chips_at(
        app: &App,
        wid: WindowId,
        palette: Option<aterm_effects::harness_card::CardPalette>,
        col: usize,
    ) -> super::ChipAt {
        let scratch = &app.windows[&wid].input_scratch;
        super::ChipAt {
            col,
            cols: scratch.cols,
            ..super::ChipAt::test(palette, scratch.implicit_blank)
        }
    }

    /// The ink an expected mode (`yolo`, bypass) and a title are drawn in on `wid`'s pane: the
    /// glass's value ink.
    pub(super) fn value_ink(app: &App, wid: WindowId) -> [u8; 3] {
        chips_at(app, wid, None, 0).value()
    }

    /// The quiet ink of `wid`'s pane: the off chips' (`○`, `⊘ off`).
    pub(super) fn quiet_ink(app: &App, wid: WindowId) -> [u8; 3] {
        chips_at(app, wid, None, 0).quiet()
    }

    /// The palette `session`'s glass wears: [`crate::autopilot::harness_theme`] of its reading,
    /// none for a Cruise or Land this window's host holds no live claim on.
    pub(super) fn worn(
        app: &App,
        session: u64,
    ) -> Option<aterm_effects::harness_card::CardPalette> {
        let (word, view, supervised) = app.session_autopilot(session);
        crate::autopilot::harness_theme(view, supervised, word, false)
    }

    /// The letter a chip's cell at frame cell `at` of `wid`'s one pane wears under the palette
    /// `session`'s glass wears ([`worn`]): the lit ink where it wears none.
    pub(super) fn letter_ink(
        app: &App,
        wid: WindowId,
        session: u64,
        (_, col): (usize, usize),
    ) -> [u8; 3] {
        chips_at(app, wid, worn(app, session), col).letter_at(0)
    }

    /// The row as it reads on the glass (the footer's facts read as set).
    pub(super) fn row_text(app: &App, wid: WindowId, row: usize) -> String {
        crate::claude_footer::read_glass(&app.windows[&wid].input_scratch.cells[row])
    }

    /// The window pixel at the centre of frame cell `(row, col)`.
    pub(super) fn px_of(app: &App, wid: WindowId, (row, col): (usize, usize)) -> (f64, f64) {
        let (cw, ch) = app.win_cell_size(wid);
        let (ox, oy) = app.frame_origin(wid);
        let x = ox as f64 + (app.win_pad(wid) + col * cw + cw / 2) as f64;
        let y = oy as f64 + (app.win_pad_top(wid) + app.win_head(wid) + row * ch + ch / 2) as f64;
        assert_eq!(
            app.frame_cell_at(wid, x, y),
            Some((row, col)),
            "the pixel names the cell"
        );
        (x, y)
    }

    /// Read up to `want` bytes (and any trailing available bytes) from the PTY observer.
    pub(super) fn pty(reader: &mut UnixStream, want: usize) -> Vec<u8> {
        let mut got = Vec::new();
        let mut buf = [0u8; 256];
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if got.len() >= want || Instant::now() >= deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => panic!("reading the PTY observer: {e}"),
            }
        }
        got
    }

    /// A double click's interval, whose last instant still continues a run of presses.
    pub(super) fn interval() -> Duration {
        Duration::from_millis(
            u64::try_from(crate::multi_click_ms()).expect("a double click's interval"),
        )
    }

    /// Where a test's stated presses start: a minute ago, far enough from the clock that a press or
    /// a stamp that read the clock instead of its stated time lands on the wrong side of a double
    /// click's interval.
    pub(super) fn a_minute_ago() -> Instant {
        Instant::now()
            .checked_sub(Duration::from_secs(60))
            .expect("a minute of uptime")
    }

    /// A left click at `when` where the pointer is.
    pub(super) fn press_at(app: &mut App, wid: WindowId, when: Instant) {
        app.on_mouse_input_at(wid, ElementState::Pressed, MouseButton::Left, when);
        app.on_mouse_input_at(wid, ElementState::Released, MouseButton::Left, when);
    }

    #[test]
    fn the_effort_tail_stays_on_claudes_row_and_every_chip_stands_clear_in_the_rule() {
        use aterm_phase::prompt::fixtures::{FOOTER_AUTO_EFFORT_HINT_MEASURED, screen};
        let measured = screen(FOOTER_AUTO_EFFORT_HINT_MEASURED)
            .into_iter()
            .find(|r| r.contains("auto mode on") && r.contains("/effort"))
            .expect("the measured mode row");
        let measured = measured.trim_end().to_string();
        for mode in [measured.as_str(), MANUAL] {
            let (mut app, wid, session, _reader) = app();
            let _host = host(&mut app);
            assert!(app.apply_term_resize(wid, 24, 144));
            publish_facts(&app, session);
            app.teams.insert(
                session,
                crate::team::Team {
                    manager: Some(crate::team::Manager {
                        local: None,
                        label: std::sync::Arc::from("lab-sweep"),
                    }),
                    ..crate::team::Team::default()
                },
            );
            draw_claude(&app, session, mode);
            frame(&mut app, wid);
            let claude = row_text(&app, wid, mode_row(&app, wid));
            assert_eq!(claude.trim_end(), mode, "Claude's row is whole");
            if mode == measured {
                app.windows.get_mut(&wid).unwrap().claude_lights.selected =
                    Some((session, Target::from(MODE)));
                frame(&mut app, wid);
            }
            let hits = &app.windows[&wid].claude_lights.hits;
            assert!(!hits.is_empty(), "{mode:?}: a chip is drawn");
            let rule = footer_row(&app, wid);
            let text: Vec<char> = app.windows[&wid].input_scratch.cells[rule]
                .iter()
                .map(|c| c.ch)
                .collect();
            assert!(text.contains(&'\u{2500}'), "control: the rule's glyphs");
            for chip in Chip::row() {
                let cols: Vec<usize> = hits
                    .iter()
                    .filter(|h| h.chip == Some(chip))
                    .map(|h| {
                        assert_eq!(h.frame_row, rule, "the chip is in the rule");
                        h.col
                    })
                    .collect();
                let (Some(&first), Some(&last)) = (cols.iter().min(), cols.iter().max()) else {
                    panic!("{mode:?}: {chip:?} is drawn");
                };
                let row: String = text.iter().collect();
                assert_eq!(text[first - 1], ' ', "{chip:?} before: {row:?}");
                assert_eq!(text[last + 1], ' ', "{chip:?} after: {row:?}");
            }
        }
    }

    #[test]
    fn a_click_on_the_mode_light_presses_forward_to_bypass_and_reads_each_answer_back() {
        for tracking in [false, true] {
            let (mut app, wid, session, mut reader) = app();
            publish_facts(&app, session);
            draw_claude(&app, session, MANUAL);
            if tracking {
                feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
            }
            frame(&mut app, wid);
            let mode = hit(&app, wid, Light::Mode);
            let text = row_text(&app, wid, mode.0);
            assert!(
                text.contains("\u{25C6} Opus 5.5 xhigh"),
                "the footer is painted: {text:?}"
            );
            assert_eq!(cell_at(&app, wid, mode).ch, '\u{23F8}', "{text:?}");
            assert!(text.contains("\u{23F8} manual"), "{text:?}");
            let claude = row_text(&app, wid, mode_row(&app, wid));
            assert!(
                claude.contains("\u{23F8} manual mode on"),
                "the chip never takes the pill's place: Claude's row is whole: {claude:?}"
            );
            assert!(pty(&mut reader, 0).is_empty(), "a frame types nothing");

            let (x, y) = px_of(&app, wid, (mode.0, mode.1 - 1));
            app.on_cursor_moved(wid, x, y);
            assert_eq!(app.windows[&wid].claude_lights.hover, None);
            let (x, y) = px_of(&app, wid, mode);
            app.on_cursor_moved(wid, x, y);
            assert_eq!(
                app.windows[&wid].claude_lights.hover,
                Some((session, Target::from(MODE)))
            );
            assert!(pty(&mut reader, 0).is_empty(), "hover types nothing");

            // Each click a gesture of its own: a double click's interval and more apart.
            let first = a_minute_ago();
            let apart = interval() + Duration::from_millis(1);
            press_at(&mut app, wid, first);
            assert_eq!(
                pty(&mut reader, lights::SHIFT_TAB.len()),
                lights::SHIFT_TAB,
                "tracking={tracking}"
            );
            {
                let p = app.windows[&wid].claude_lights.sessions[&session]
                    .pending
                    .clone()
                    .expect("the toggle is in flight");
                assert_eq!(
                    (p.light, p.started, p.presses),
                    (Light::Mode, Mode::Manual, 1)
                );
            }
            press_at(&mut app, wid, first + apart);
            assert!(pty(&mut reader, 0).is_empty(), "one toggle in flight");

            frame(&mut app, wid);
            let mode = hit(&app, wid, Light::Mode);
            assert_eq!(cell_at(&app, wid, mode).ch, '\u{25CC}');
            let text = row_text(&app, wid, mode.0);
            assert!(
                text.contains("switching to bypass or auto\u{2026}") && text.contains(WHOLE_FACTS),
                "the title, whole in an 80-column pane, beside the whole facts: {text:?}"
            );
            assert_ne!(app.windows[&wid].claude_lights.fingerprint(Instant::now), 0);
            app.on_claude_footer_changed(session);
            assert!(pty(&mut reader, 0).is_empty(), "no answer, no second press");

            for answer in [ACCEPT, PLAN] {
                redraw_mode_row(&app, session, answer);
                frame(&mut app, wid);
                assert!(
                    pty(&mut reader, 0).is_empty(),
                    "no input from the render path"
                );
                app.on_claude_footer_changed(session);
                assert_eq!(
                    pty(&mut reader, lights::SHIFT_TAB.len()),
                    lights::SHIFT_TAB,
                    "the next press"
                );
                frame(&mut app, wid);
                app.on_claude_footer_changed(session);
                assert!(pty(&mut reader, 0).is_empty(), "one press per answer");
            }

            redraw_mode_row(&app, session, BYPASS);
            frame(&mut app, wid);
            app.on_claude_footer_changed(session);
            assert!(pty(&mut reader, 0).is_empty());
            frame(&mut app, wid);
            let s = &app.windows[&wid].claude_lights.sessions[&session];
            assert!(s.pending.is_none() && s.refused.is_none(), "resolved");
            let mode = hit(&app, wid, Light::Mode);
            let cell = cell_at(&app, wid, mode);
            assert_eq!(
                (cell.ch, cell.fg),
                ('\u{23F5}', value_ink(&app, wid)),
                "the arrival is said on the chip for a moment, in the value ink, never red"
            );
            let text = row_text(&app, wid, mode.0);
            assert!(
                text.contains("\u{23F5}\u{23F5} bypass  no permission prompts ")
                    && text.contains(WHOLE_FACTS),
                "the hover's title says what bypass does, beside the whole facts: {text:?}"
            );
            let (x, y) = px_of(&app, wid, mode);
            app.on_cursor_moved(wid, x, y);
            press_at(&mut app, wid, first + apart * 2);
            assert_eq!(
                pty(&mut reader, lights::SHIFT_TAB.len()),
                lights::SHIFT_TAB,
                "a click on bypass turns it off"
            );
            let p = app.windows[&wid].claude_lights.sessions[&session]
                .pending
                .clone()
                .expect("the turn-off is in flight");
            assert_eq!((p.goal, p.started), (Goal::Off, Mode::Bypass));
            redraw_mode_row(&app, session, MANUAL);
            app.on_claude_footer_changed(session);
            assert!(pty(&mut reader, 0).is_empty(), "manual: arrived, no press");
            let s = &app.windows[&wid].claude_lights.sessions[&session];
            assert!(s.pending.is_none() && s.refused.is_none(), "resolved");
            assert!(s.notice.is_some(), "the arrival is said");
        }
    }

    /// Auto is expected, so no chip is drawn for it at rest; a click on its light (a reveal, a
    /// screen reader's press) turns it to manual with one shift+tab, and stops there.
    #[test]
    fn auto_mode_draws_no_mode_chip_and_a_click_turns_it_to_manual() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, AUTO);
        frame(&mut app, wid);
        assert!(
            !app.windows[&wid]
                .claude_lights
                .hits
                .iter()
                .any(|h| h.chip == Some(MODE)),
            "auto is expected: no chip"
        );
        app.toggle_claude_light(wid, session, Light::Mode);
        assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB, "auto: one press");
        let s = &app.windows[&wid].claude_lights.sessions[&session];
        assert_eq!(s.pending.as_ref().map(|p| p.goal), Some(Goal::Off));
        redraw_mode_row(&app, session, MANUAL);
        app.on_claude_footer_changed(session);
        assert!(pty(&mut reader, 0).is_empty(), "manual: no press on");
        let s = &app.windows[&wid].claude_lights.sessions[&session];
        assert!(s.pending.is_none() && s.refused.is_none(), "arrived");
        redraw_mode_row(&app, session, PLAN);
        frame(&mut app, wid);
        click(&mut app, wid, Light::Mode);
        assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB, "plan: one press");
    }

    #[test]
    fn a_toggle_is_never_decided_on_half_a_frame() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, PLAN);
        frame(&mut app, wid);
        feed(
            &app,
            session,
            "\x1b[?2026h\x1b[22;1H\x1b[2K\u{276F} ".as_bytes(),
        );
        let term = app.pool.get(session).expect("session").term.clone();
        assert!(
            term_lock(&term).frame_unfinished_at(Instant::now()),
            "precondition: the frame is unfinished"
        );
        app.toggle_claude_light(wid, session, Light::Mode);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "nothing pressed on half a frame"
        );
        let s = &app.windows[&wid].claude_lights.sessions[&session];
        assert!(s.pending.is_none(), "no toggle began");
        assert!(
            matches!(s.refused, Some((Light::Mode, super::Refusal::NotShown, _))),
            "refused as not on screen"
        );
        feed(
            &app,
            session,
            format!("\x1b[24;1H\x1b[2K{AUTO}\x1b[22;3H\x1b[?2026l").as_bytes(),
        );
        app.toggle_claude_light(wid, session, Light::Mode);
        assert_eq!(
            pty(&mut reader, 3),
            lights::SHIFT_TAB,
            "the whole frame: auto, turned off with one press"
        );
        let s = &app.windows[&wid].claude_lights.sessions[&session];
        assert_eq!(
            s.pending.as_ref().map(|p| (p.goal, p.started)),
            Some((Goal::Off, Mode::Auto)),
            "decided on the whole frame's mode"
        );
    }

    #[test]
    fn at_rest_the_rule_carries_the_facts_and_the_harness_chips_alone() {
        for hosted in [true, false] {
            for mode in [BYPASS, AUTO] {
                let (mut app, wid, session, _reader) = app();
                let _host = hosted.then(|| host(&mut app));
                publish_facts(&app, session);
                draw_claude(&app, session, mode);
                frame(&mut app, wid);
                let lights = &app.windows[&wid].claude_lights;
                let mut chips: Vec<Chip> = lights.hits.iter().filter_map(|h| h.chip).collect();
                chips.dedup();
                let want = if hosted {
                    vec![Chip::Autopilot]
                } else {
                    Vec::new()
                };
                assert_eq!(chips, want, "{mode:?} hosted={hosted}");
                assert_eq!(
                    lights.footers.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
                    vec![session],
                    "{mode:?}"
                );
                let text = row_text(&app, wid, footer_row(&app, wid));
                assert!(text.contains(WHOLE_FACTS), "{text:?}");
                for gone in [
                    "\u{25CB}",
                    "\u{25CF}",
                    "\u{23F5}",
                    "autopilot",
                    Mode::Bypass.gist(),
                    Mode::Auto.gist(),
                    "click: ",
                ] {
                    assert!(
                        !text.contains(gone),
                        "{gone:?} in {text:?}: no light, no title"
                    );
                }
                assert_eq!(
                    text.contains('\u{221e}'),
                    hosted,
                    "the harness's mark: {text:?}"
                );
                if hosted {
                    let at = hit(&app, wid, Chip::Autopilot);
                    assert_eq!(
                        cell_at(&app, wid, at).fg,
                        letter_ink(&app, wid, session, at),
                        "cruise: its column's letter, under the palette its glass wears"
                    );
                }
                let claude = row_text(&app, wid, mode_row(&app, wid));
                assert_eq!(claude.trim_end(), mode, "Claude's row, as drawn");
            }
        }
    }

    /// THE KEYBOARD REACHES EVERY CHIP AT REST (macOS): with no light drawn,
    /// `ctrl+shift+tab` walks the row in its order — Autopilot, always
    /// revealable where a host draws it, then the known light, the expected
    /// mode `⏵⏵ bypass`, revealed — each selection painted on the glass and
    /// typing nothing; Return on the expected mode turns it off with one
    /// shift+tab, and the reveal goes with the selection; Autopilot stays.
    #[cfg(target_os = "macos")]
    #[test]
    fn ctrl_shift_tab_reveals_the_lights_at_rest() {
        use aterm_window::keyboard::{KeyCode, NamedKey};

        for hosted in [true, false] {
            let (mut app, wid, session, mut reader) = app();
            let _host = hosted.then(|| host(&mut app));
            publish_facts(&app, session);
            draw_claude(&app, session, BYPASS);
            frame(&mut app, wid);
            assert!(!painted(&app, wid, MODE), "at rest");
            let walk: &[Chip] = if hosted {
                &[Chip::Autopilot, MODE]
            } else {
                &[MODE]
            };
            for want in walk {
                chord(&mut app, wid);
                assert_eq!(
                    app.windows[&wid].claude_lights.selected,
                    Some((session, Target::from(*want))),
                    "hosted={hosted}"
                );
                frame(&mut app, wid);
                let at = hit(&app, wid, *want);
                assert_eq!(
                    cell_at(&app, wid, at).underline,
                    super::UnderlineStyle::Single,
                    "{want:?}: the selection is on the glass"
                );
            }
            assert!(pty(&mut reader, 0).is_empty(), "the chord is the lights'");
            let text = row_text(&app, wid, footer_row(&app, wid));
            assert!(
                text.contains("\u{23F5}\u{23F5} bypass"),
                "the known light, revealed: {text:?}"
            );
            tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
            assert_eq!(
                pty(&mut reader, lights::SHIFT_TAB.len()),
                lights::SHIFT_TAB,
                "Return on the expected mode turns it off, and is not typed"
            );
            assert_eq!(
                app.windows[&wid].claude_lights.selected, None,
                "the reveal goes with the selection"
            );
            redraw_mode_row(&app, session, MANUAL);
            app.on_claude_footer_changed(session);
            assert!(pty(&mut reader, 0).is_empty(), "manual: arrived");
            frame(&mut app, wid);
            assert_eq!(
                cell_at(&app, wid, hit(&app, wid, MODE)).ch,
                '\u{23F8}',
                "manual deviates: its chip is drawn"
            );
            assert_eq!(
                painted(&app, wid, Chip::Autopilot),
                hosted,
                "Autopilot stays"
            );
        }
    }

    /// The keyboard route (macOS): the chord selects the focused pane's light and types
    /// nothing (again, it goes on to the facts the glass shows, and round to the light);
    /// Return toggles it with one shift+tab, not the Return, and lets go.
    #[cfg(target_os = "macos")]
    #[test]
    fn ctrl_shift_tab_selects_a_light_and_return_toggles_it() {
        use aterm_window::keyboard::{KeyCode, ModifiersState, NamedKey};

        fn set_mods(app: &mut App, wid: WindowId, mods: ModifiersState) {
            app.windows.get_mut(&wid).expect("window").mods = mods;
        }

        let (mut app, wid, session, mut reader) = wide_app();
        publish_facts(&app, session);
        draw_claude(&app, session, PLAN);
        frame(&mut app, wid);

        set_mods(
            &mut app,
            wid,
            ModifiersState::CONTROL | ModifiersState::SHIFT,
        );
        tap(&mut app, wid, NamedKey::Tab, KeyCode::Tab, "\t");
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, Target::from(MODE)))
        );
        let facts = app.windows[&wid].claude_lights.facts_on_glass(session);
        assert!(!facts.is_empty(), "the facts are on the glass");
        for fact in facts {
            tap(&mut app, wid, NamedKey::Tab, KeyCode::Tab, "\t");
            assert_eq!(
                app.windows[&wid].claude_lights.selected,
                Some((session, Target::Fact(fact))),
                "the walk goes on to the facts"
            );
        }
        tap(&mut app, wid, NamedKey::Tab, KeyCode::Tab, "\t");
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, Target::from(MODE))),
            "and round to the one light"
        );
        assert!(pty(&mut reader, 0).is_empty(), "the chord is the lights'");
        frame(&mut app, wid);
        let auto = hit(&app, wid, Light::Mode);
        let text = row_text(&app, wid, auto.0);
        assert!(text.contains("click: bypass or auto"), "{text:?}");
        assert_eq!(
            cell_at(&app, wid, auto).underline,
            super::UnderlineStyle::Single,
            "the selected light is marked"
        );

        set_mods(&mut app, wid, ModifiersState::empty());
        tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
        assert_eq!(
            pty(&mut reader, lights::SHIFT_TAB.len()),
            lights::SHIFT_TAB,
            "the toggle, and not the Return"
        );
        assert_eq!(app.windows[&wid].claude_lights.selected, None);
        frame(&mut app, wid);
        assert_eq!(
            cell_at(&app, wid, hit(&app, wid, Light::Mode)).ch,
            '\u{25CC}'
        );

        redraw_mode_row(&app, session, BYPASS);
        frame(&mut app, wid);
        app.on_claude_footer_changed(session);
        assert!(pty(&mut reader, 0).is_empty());
        assert!(
            app.windows[&wid].claude_lights.sessions[&session]
                .pending
                .is_none()
        );
        frame(&mut app, wid);
        let auto = hit(&app, wid, Light::Mode);
        assert_eq!(cell_at(&app, wid, auto).ch, '\u{23F5}');
        let text = row_text(&app, wid, auto.0);
        assert!(
            text.contains(Mode::Bypass.gist()),
            "the arrival's notice says what bypass does: {text:?}"
        );
        let past = Instant::now() - Duration::from_millis(1);
        if let Some((_, until)) = app
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.claude_lights.sessions.get_mut(&session))
            .and_then(|s| s.notice.as_mut())
        {
            *until = past;
        }
        app.on_claude_footer_changed(session);
        frame(&mut app, wid);
        assert!(
            !app.windows[&wid]
                .claude_lights
                .hits
                .iter()
                .any(|h| h.chip == Some(MODE)),
            "back at rest, no mode chip"
        );

        tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
        assert_eq!(pty(&mut reader, 1), b"\r");
    }

    #[test]
    fn a_pending_mode_toggle_keeps_its_lights_in_an_80_column_pane() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, PLAN);
        frame(&mut app, wid);
        let approve = hit(&app, wid, Light::Mode);
        let (x, y) = px_of(&app, wid, approve);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
        frame(&mut app, wid);
        assert!(
            !app.windows[&wid].claude_lights.hits.is_empty(),
            "the lights stay while the toggle is in flight: {:?}",
            row_text(&app, wid, approve.0)
        );
        let text = row_text(&app, wid, approve.0);
        assert!(
            text.contains(WHOLE_FACTS),
            "the facts stay whole while the toggle is in flight: {text:?}"
        );
        assert!(
            text.contains("switching to bypass or auto\u{2026}"),
            "the title, whole beside the lights and the facts: {text:?}"
        );
        assert_eq!(
            hit(&app, wid, Light::Mode),
            approve,
            "the chip stays under the pointer while its title comes: {text:?}"
        );
    }

    #[test]
    fn a_stop_too_long_to_title_in_full_says_the_reason_alone() {
        for cols in [80_u16, 120] {
            let (mut app, wid, session, mut reader) = app();
            if cols != 80 {
                assert!(app.apply_term_resize(wid, 24, cols));
            }
            publish_facts(&app, session);
            draw_claude(&app, session, MANUAL);
            frame(&mut app, wid);
            click(&mut app, wid, Light::Mode);
            assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
            redraw_mode_row(&app, session, ACCEPT);
            app.on_claude_footer_changed(session);
            assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
            past_deadline(&mut app, wid, session);
            app.on_claude_footer_changed(session);
            frame(&mut app, wid);
            let chip = hit(&app, wid, Light::Mode);
            let text = row_text(&app, wid, chip.0);
            assert!(
                text.contains("stopped in accept edits mode"),
                "{cols}: the reason: {text:?}"
            );
            assert!(
                text.contains("\u{25C6} Opus 5.5 xhigh"),
                "{cols}: never the model's or the effort's room: {text:?}"
            );
            if cols == 80 {
                assert!(
                    !text.contains("click: ") && !text.contains("\u{2387} main"),
                    "no room for the whole title: the reason alone, in the branch's \
                     and the path's room: {text:?}"
                );
            } else {
                assert!(
                    text.contains("stopped in accept edits mode \u{b7} click: bypass or auto")
                        && text.contains(WHOLE_FACTS),
                    "{cols}: the whole title beside the whole facts: {text:?}"
                );
            }
            assert!(
                text.contains("\u{23F5}\u{23F5} accept edits"),
                "{cols}: control: the chip: {text:?}"
            );
        }
    }

    /// A light selected with ctrl+shift+tab is let go by any mouse press, so clicking to
    /// another pane and back leaves no selection armed to take the next Return: Return
    /// submits Claude's prompt.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_mouse_press_lets_a_keyboard_selection_go() {
        use aterm_window::keyboard::{KeyCode, ModifiersState, NamedKey};
        fn click(app: &mut App, wid: WindowId, cell: (usize, usize)) {
            let (x, y) = px_of(app, wid, cell);
            app.on_cursor_moved(wid, x, y);
            app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
            app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        }
        let (mut app, wid, session, mut reader) = app();
        let other = app.split_active_stub_tab(wid);
        let (rows, cols) = {
            let terminal = term_lock(&app.pool.get(session).unwrap().term);
            (terminal.rows(), usize::from(terminal.cols()))
        };
        publish_facts(&app, session);
        let rule = "\u{2500}".repeat(cols);
        let (top_rule, prompt, bottom_rule) = (rows - 3, rows - 2, rows - 1);
        feed(&app, session, format!(
            "\x1b[2J\x1b[{top_rule};1H{rule}\x1b[{prompt};1H\u{276F} \x1b[{bottom_rule};1H{rule}\x1b[{rows};1H  \u{23F5}\u{23F5} bypass permissions on\x1b[{prompt};3H"
        ).as_bytes());
        frame(&mut app, wid);
        let strip =
            app.windows[&wid].input_scratch.cells.len() - usize::from(app.windows[&wid].rows);
        let other_col = app.windows[&wid].cols as usize - 3;
        click(&mut app, wid, (strip + 5, 3));
        assert_eq!(
            app.focused_session_id(wid),
            Some(session),
            "Claude's pane has the keyboard"
        );
        frame(&mut app, wid);
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
        tap(&mut app, wid, NamedKey::Tab, KeyCode::Tab, "\t");
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, Target::from(MODE)))
        );
        click(&mut app, wid, (strip + 5, other_col));
        assert_eq!(app.focused_session_id(wid), Some(other));
        click(&mut app, wid, (strip + 5, 3));
        assert_eq!(app.focused_session_id(wid), Some(session));
        assert!(pty(&mut reader, 0).is_empty());
        tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
        assert_eq!(pty(&mut reader, 1), b"\r", "Return submits Claude's prompt");
    }

    fn click(app: &mut App, wid: WindowId, chip: impl Into<Chip>) {
        let (x, y) = px_of(app, wid, hit(app, wid, chip));
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    }

    fn refusal(app: &App, wid: WindowId, session: u64) -> Option<(Light, super::Refusal)> {
        app.windows[&wid].claude_lights.sessions[&session]
            .refused
            .map(|(light, why, _)| (light, why))
    }

    /// Put `session`'s toggle past its deadline, as the timer would.
    fn past_deadline(app: &mut App, wid: WindowId, session: u64) {
        app.windows
            .get_mut(&wid)
            .and_then(|ws| ws.claude_lights.sessions.get_mut(&session))
            .and_then(|s| s.pending.as_mut())
            .expect("in flight")
            .deadline = Instant::now() - Duration::from_millis(1);
    }

    /// Check the press received its full `SETTLE_MS`, then set the deadline a minute from now.
    pub(super) fn press_not_due(app: &mut App, wid: WindowId, session: u64, since: Instant) {
        let p = app
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.claude_lights.sessions.get_mut(&session))
            .and_then(|s| s.pending.as_mut())
            .expect("in flight");
        let settle = Duration::from_millis(lights::SETTLE_MS);
        assert!(
            since + settle <= p.deadline && p.deadline <= Instant::now() + settle,
            "the drive gives the press its own deadline"
        );
        p.deadline = Instant::now() + Duration::from_secs(60);
    }

    #[test]
    fn a_mode_return_finishes_from_the_engine_with_no_frames() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, MANUAL);
        frame(&mut app, wid);
        let clock = |app: &App| app.pool.get(session).unwrap().ctx.human_input.last();
        let before = clock(&app);
        click(&mut app, wid, Light::Mode);
        assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
        let after_click = clock(&app);
        assert_ne!(after_click, before, "control: the click is the person's");
        app.on_claude_footer_changed(session);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "control: no answer, no press"
        );
        for answer in [ACCEPT, PLAN] {
            redraw_mode_row(&app, session, answer);
            app.on_claude_footer_changed(session);
            assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB, "{answer:?}");
        }
        assert_eq!(
            clock(&app),
            after_click,
            "aterm's own presses do not stamp a person"
        );
        redraw_mode_row(&app, session, BYPASS);
        app.on_claude_footer_changed(session);
        assert!(pty(&mut reader, 0).is_empty(), "bypass: done");
        let s = &app.windows[&wid].claude_lights.sessions[&session];
        assert!(s.pending.is_none() && s.refused.is_none());
        assert!(matches!(s.notice, Some((Light::Mode, _))));
    }

    /// The model's `mode` of the pill `screen` shows.
    fn model_mode(mode: Mode) -> i64 {
        match mode {
            Mode::Bypass => 0,
            Mode::Auto => 5,
            Mode::Manual => 1,
            Mode::AcceptEdits => 2,
            Mode::Plan => 3,
            Mode::DontAsk => 4,
        }
    }

    /// Project the mode-return model state from the genuine [`App`].
    pub(super) fn project_mode_toggle(
        app: &App,
        wid: WindowId,
        session: u64,
        expected: &aterm_spec::interp::State,
    ) -> aterm_spec::interp::State {
        let mut observed = expected.clone();
        let screen = app.claude_screen_now(session);
        observed.insert("covered", i64::from(screen.is_none()));
        let shown = screen.as_ref().and_then(|screen| screen.mode);
        if let Some(mode) = shown {
            observed.insert("mode", model_mode(mode));
        }
        if let Some(screen) = &screen {
            observed.insert("busy", i64::from(screen.busy));
        }
        let s = app.windows[&wid]
            .claude_lights
            .sessions
            .get(&session)
            .cloned()
            .unwrap_or_default();
        let now = Instant::now();
        let phase = if s.pending.is_some() {
            1
        } else if s.refused.is_some_and(|(_, _, until)| until > now)
            || s.notice.is_some_and(|(_, until)| until > now)
        {
            2
        } else {
            0
        };
        observed.insert("phase", phase);
        if let Some(p) = &s.pending {
            observed.insert("presses", i64::from(p.presses));
            observed.insert("start", model_mode(p.started));
            observed.insert(
                "goal",
                match p.goal {
                    Goal::On => 0,
                    Goal::Off => 1,
                },
            );
            observed.insert("late", i64::from(p.deadline <= now));
            if let Some(mode) = shown {
                observed.insert("sent", i64::from(mode == p.mode_at_press));
            }
        } else {
            observed.insert("sent", 0);
            observed.insert("late", 0);
        }
        observed.insert(
            "named",
            i64::from(matches!(
                s.refused,
                Some((Light::Mode, why, until)) if until > now && why.names_mode()
            )),
        );
        observed
    }

    pub(super) fn drive_mode_toggle_action<T>(action: &str, drive: impl FnOnce() -> T) -> T {
        use aterm_spec::xref;

        assert!(xref::reset_entered_anchors());
        let result = drive();
        let entered = xref::entered_anchor_ids();
        xref::disarm_entered_anchors();
        let anchor = xref::refinements()
            .find(|anchor| anchor.machine == "claude_mode_toggle" && anchor.action == action)
            .expect("the mode-toggle action has a shipping anchor");
        assert!(
            entered.contains(anchor.entry_id),
            "{action} missed its shipping function"
        );
        result
    }

    /// Fire `action` on the model and verify the App's projected state and optional negative control.
    pub(super) fn check_mode_toggle_transition(
        app: &App,
        wid: WindowId,
        session: u64,
        action: &str,
        state: &mut aterm_spec::interp::State,
        historical: Option<&[(&'static str, i64)]>,
    ) {
        let model = aterm_models_gui::claude_mode_toggle_model();
        let before = state.clone();
        assert!(model.fire(action, state), "{action} is enabled");
        let observed = project_mode_toggle(app, wid, session, state);
        let (ok, evidence) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &before,
            &observed,
            Some(action),
            "Claude mode toggle real conformance",
        );
        assert!(ok, "{action}: {evidence}");
        if let Some(changes) = historical {
            let mut old = observed.clone();
            for &(var, value) in changes {
                old.insert(var, value);
            }
            let (ok, _) = aterm_spec::verify::validate_transition_tiered(
                &model,
                &[],
                &before,
                &old,
                Some(action),
                "Claude mode toggle negative control",
            );
            assert!(
                !ok,
                "{action} must reject the retired post-state {changes:?}"
            );
        }
    }

    #[test]
    fn the_mode_toggle_conforms_to_its_model() {
        const DONT_ASK: &str = "  \u{23F5}\u{23F5} don't ask on (shift+tab to cycle)";
        for dont_ask in [false, true] {
            let (mut app, wid, session, mut reader) = app();
            publish_facts(&app, session);
            let model = aterm_models_gui::claude_mode_toggle_model();
            let mut state = model.init_state();
            let mut answers = vec![ACCEPT, PLAN, BYPASS];
            if dont_ask {
                state.insert("mode", 0);
                draw_claude(&app, session, DONT_ASK);
                check_mode_toggle_transition(&app, wid, session, "DontAsk", &mut state, None);
                answers.insert(0, MANUAL);
            } else {
                draw_claude(&app, session, MANUAL);
            }
            frame(&mut app, wid);
            let since = Instant::now();
            drive_mode_toggle_action("Click", || click(&mut app, wid, Light::Mode));
            press_not_due(&mut app, wid, session, since);
            assert_eq!(
                pty(&mut reader, 3),
                lights::SHIFT_TAB,
                "the click presses at once"
            );
            check_mode_toggle_transition(
                &app,
                wid,
                session,
                "Click",
                &mut state,
                Some(&[("sent", 0)][..]),
            );
            let last = answers.len() - 1;
            for (i, answer) in answers.into_iter().enumerate() {
                redraw_mode_row(&app, session, answer);
                check_mode_toggle_transition(&app, wid, session, "Answer", &mut state, None);
                if i < last {
                    let since = Instant::now();
                    drive_mode_toggle_action("Press", || app.on_claude_footer_changed(session));
                    press_not_due(&mut app, wid, session, since);
                    assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB, "{answer:?}");
                    check_mode_toggle_transition(&app, wid, session, "Press", &mut state, None);
                }
            }
            drive_mode_toggle_action("Arrive", || app.on_claude_footer_changed(session));
            assert!(
                pty(&mut reader, 0).is_empty(),
                "no press on from bypass, the goal"
            );
            check_mode_toggle_transition(
                &app,
                wid,
                session,
                "Arrive",
                &mut state,
                Some(&[("phase", 1), ("sent", 1), ("overshoot", 1)][..]),
            );
        }
    }

    /// A CLICK ON YOLO TURNS IT OFF (the owner, 2026-11-02): from bypass the toggle's goal is
    /// manual. On Claude Code 2.1.316's cycle (bypass, auto, manual, measured by `lab footers`)
    /// the first shift+tab answers auto, an expected mode short of the goal, so aterm presses
    /// once more, Claude answers manual and nothing more is pressed; a cycle without auto answers
    /// manual at once. A click back while the arrival's notice shows turns it over again. The
    /// model rejects the retired goal (back to an expected mode), and its `Buggy = 2` mutant, a
    /// click on an On light aimed back at an expected mode, breaks `AClickTurnsTheLightOver`.
    #[test]
    fn a_click_on_yolo_turns_it_off_and_conforms_to_its_model() {
        for auto_in in [true, false] {
            let (mut app, wid, session, mut reader) = app();
            publish_facts(&app, session);
            draw_claude(&app, session, BYPASS);
            frame(&mut app, wid);
            let model = aterm_models_gui::claude_mode_toggle_model();
            let mut state = model.init_state();
            state.insert("mode", 0);
            if !auto_in {
                assert!(model.fire("NoAuto", &mut state));
            }
            let since = Instant::now();
            drive_mode_toggle_action("Click", || {
                app.toggle_claude_light(wid, session, Light::Mode);
            });
            press_not_due(&mut app, wid, session, since);
            assert_eq!(
                pty(&mut reader, 3),
                lights::SHIFT_TAB,
                "the click presses at once"
            );
            check_mode_toggle_transition(
                &app,
                wid,
                session,
                "Click",
                &mut state,
                Some(&[("goal", 0)][..]),
            );
            if auto_in {
                redraw_mode_row(&app, session, AUTO);
                check_mode_toggle_transition(&app, wid, session, "Answer", &mut state, None);
                let since = Instant::now();
                drive_mode_toggle_action("Press", || app.on_claude_footer_changed(session));
                press_not_due(&mut app, wid, session, since);
                assert_eq!(
                    pty(&mut reader, 3),
                    lights::SHIFT_TAB,
                    "auto is short of manual: a press on"
                );
                check_mode_toggle_transition(&app, wid, session, "Press", &mut state, None);
            }
            redraw_mode_row(&app, session, MANUAL);
            check_mode_toggle_transition(&app, wid, session, "Answer", &mut state, None);
            drive_mode_toggle_action("Arrive", || app.on_claude_footer_changed(session));
            assert!(pty(&mut reader, 0).is_empty(), "no press on from manual");
            check_mode_toggle_transition(
                &app,
                wid,
                session,
                "Arrive",
                &mut state,
                Some(&[("phase", 1), ("sent", 1), ("overshoot", 1)][..]),
            );

            // The click back, while the arrival's notice is on show (`phase` 2), turns it over.
            let since = Instant::now();
            drive_mode_toggle_action("Click", || {
                app.toggle_claude_light(wid, session, Light::Mode);
            });
            press_not_due(&mut app, wid, session, since);
            assert_eq!(
                pty(&mut reader, 3),
                lights::SHIFT_TAB,
                "the click back presses at once"
            );
            check_mode_toggle_transition(
                &app,
                wid,
                session,
                "Click",
                &mut state,
                Some(&[("phase", 2)][..]),
            );

            let buggy = aterm_spec::interp::with_buggy(&model, 2);
            let mut retired = buggy.init_state();
            retired.insert("mode", 0);
            assert!(buggy.fire("Click", &mut retired), "a click on an On light");
            assert!(
                !buggy.check_invariant("AClickTurnsTheLightOver", &retired),
                "the retired goal breaks the law"
            );
        }
    }

    #[test]
    fn an_overdue_mode_answer_sends_no_late_follow_up_key() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, MANUAL);
        frame(&mut app, wid);
        let model = aterm_models_gui::claude_mode_toggle_model();
        let mut state = model.init_state();
        drive_mode_toggle_action("Click", || click(&mut app, wid, Light::Mode));
        assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
        check_mode_toggle_transition(&app, wid, session, "Click", &mut state, None);

        past_deadline(&mut app, wid, session);
        check_mode_toggle_transition(&app, wid, session, "Timeout", &mut state, None);
        redraw_mode_row(&app, session, ACCEPT);
        check_mode_toggle_transition(&app, wid, session, "Answer", &mut state, None);
        assert!(
            model.successors("Press", &state).is_empty(),
            "a timed-out answer cannot enable another press"
        );
        drive_mode_toggle_action("Expire", || app.on_claude_footer_changed(session));
        assert!(pty(&mut reader, 0).is_empty(), "no late shift+tab");
        check_mode_toggle_transition(
            &app,
            wid,
            session,
            "Expire",
            &mut state,
            Some(&[("phase", 1), ("presses", 2)][..]),
        );
        assert_eq!(
            refusal(&app, wid, session),
            Some((Light::Mode, super::Refusal::StoppedIn(Mode::AcceptEdits)))
        );

        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut late = buggy.init_state();
        for action in ["Click", "Timeout", "Answer", "Press"] {
            assert!(buggy.fire(action, &mut late), "{action}");
        }
        assert!(!buggy.check_invariant("NoLatePress", &late));
    }

    /// Claude answers mid-turn (a plan row with `esc to interrupt`).
    const BUSY_PLAN: &str = "  \u{23F8} plan mode on \u{00B7} esc to interrupt";
    /// A turn in flight before Claude answered: accept edits, busy.
    const BUSY_ACCEPT_ROW: &str = "  \u{23F5}\u{23F5} accept edits on \u{00B7} esc to interrupt";

    #[test]
    fn the_mode_toggles_stops_conform_to_its_model_and_are_named() {
        let model = aterm_models_gui::claude_mode_toggle_model();
        {
            let (mut app, wid, session, mut reader) = app();
            publish_facts(&app, session);
            draw_claude(&app, session, MANUAL);
            frame(&mut app, wid);
            let mut state = model.init_state();
            let since = Instant::now();
            drive_mode_toggle_action("Click", || click(&mut app, wid, Light::Mode));
            press_not_due(&mut app, wid, session, since);
            assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
            check_mode_toggle_transition(&app, wid, session, "Click", &mut state, None);
            redraw_mode_row(&app, session, ACCEPT);
            check_mode_toggle_transition(&app, wid, session, "Answer", &mut state, None);
            let since = Instant::now();
            drive_mode_toggle_action("Press", || app.on_claude_footer_changed(session));
            press_not_due(&mut app, wid, session, since);
            assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
            check_mode_toggle_transition(&app, wid, session, "Press", &mut state, None);
            redraw_mode_row(&app, session, BUSY_ACCEPT_ROW);
            check_mode_toggle_transition(&app, wid, session, "TurnStarts", &mut state, None);
            redraw_mode_row(&app, session, BUSY_PLAN);
            check_mode_toggle_transition(&app, wid, session, "Answer", &mut state, None);
            app.on_claude_footer_changed(session);
            assert!(
                pty(&mut reader, 0).is_empty(),
                "answered mid-turn: aterm's own press is held"
            );
            assert!(
                model.successors("Press", &state).is_empty(),
                "and the model has no press there either"
            );
            feed(&app, session, b"\x1b[2J\x1b[10;1H Do you want to proceed?");
            check_mode_toggle_transition(&app, wid, session, "Cover", &mut state, None);
            assert_eq!(state["boxed"], 0);
            past_deadline(&mut app, wid, session);
            drive_mode_toggle_action("Expire", || app.on_claude_footer_changed(session));
            assert!(pty(&mut reader, 0).is_empty(), "nothing into the box");
            check_mode_toggle_transition(
                &app,
                wid,
                session,
                "Expire",
                &mut state,
                Some(&[("named", 0)][..]),
            );
            assert_eq!(
                refusal(&app, wid, session),
                Some((Light::Mode, super::Refusal::StoppedMidTurn(Mode::Plan)))
            );
            let buggy = aterm_spec::interp::with_buggy(&model, 1);
            let mut raced = buggy.init_state();
            for action in ["TurnStarts", "Click", "Answer", "Press", "Cover"] {
                assert!(buggy.fire(action, &mut raced), "{action}");
            }
            assert!(!buggy.check_invariant("NoPressIntoABox", &raced));
        }
        {
            let (mut app, wid, session, mut reader) = app();
            publish_facts(&app, session);
            draw_claude(&app, session, ACCEPT);
            frame(&mut app, wid);
            let mut state = model.init_state();
            state.insert("mode", 2);
            state.insert("start", 2);
            assert!(model.fire("NoBypass", &mut state));
            assert!(model.fire("NoAuto", &mut state));
            let since = Instant::now();
            drive_mode_toggle_action("Click", || click(&mut app, wid, Light::Mode));
            press_not_due(&mut app, wid, session, since);
            assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
            check_mode_toggle_transition(&app, wid, session, "Click", &mut state, None);
            for answer in [PLAN, MANUAL] {
                redraw_mode_row(&app, session, answer);
                check_mode_toggle_transition(&app, wid, session, "Answer", &mut state, None);
                let since = Instant::now();
                drive_mode_toggle_action("Press", || app.on_claude_footer_changed(session));
                press_not_due(&mut app, wid, session, since);
                assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB, "{answer:?}");
                check_mode_toggle_transition(&app, wid, session, "Press", &mut state, None);
            }
            redraw_mode_row(&app, session, ACCEPT);
            check_mode_toggle_transition(&app, wid, session, "Answer", &mut state, None);
            drive_mode_toggle_action("Lap", || app.on_claude_footer_changed(session));
            assert!(pty(&mut reader, 0).is_empty(), "no second lap");
            check_mode_toggle_transition(
                &app,
                wid,
                session,
                "Lap",
                &mut state,
                Some(&[("named", 0)][..]),
            );
            assert_eq!(
                refusal(&app, wid, session),
                Some((
                    Light::Mode,
                    super::Refusal::NotInCycle(Goal::On, Mode::AcceptEdits)
                ))
            );
        }
        {
            let (mut app, wid, session, mut reader) = app();
            publish_facts(&app, session);
            draw_claude(&app, session, MANUAL);
            frame(&mut app, wid);
            let mut state = model.init_state();
            let since = Instant::now();
            drive_mode_toggle_action("Click", || click(&mut app, wid, Light::Mode));
            press_not_due(&mut app, wid, session, since);
            assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
            check_mode_toggle_transition(&app, wid, session, "Click", &mut state, None);
            redraw_mode_row(&app, session, ACCEPT);
            check_mode_toggle_transition(&app, wid, session, "Answer", &mut state, None);
            hang_up(&app, session, reader);
            drive_mode_toggle_action("Reject", || app.on_claude_footer_changed(session));
            check_mode_toggle_transition(
                &app,
                wid,
                session,
                "Reject",
                &mut state,
                Some(&[("named", 0)][..]),
            );
            assert_eq!(
                refusal(&app, wid, session),
                Some((Light::Mode, super::Refusal::StoppedIn(Mode::AcceptEdits))),
                "the mode it stopped in, not \"input was not accepted\""
            );
        }
    }

    /// Draw Claude Code's bottom block `cols` wide.
    #[cfg(target_os = "macos")]
    fn draw_claude_cols(app: &App, session: u64, mode: &str, cols: usize) {
        let rule = "\u{2500}".repeat(cols);
        let screen = format!(
            "\x1b[?2004h\x1b[2J\x1b[19;1H\u{25CF} done\x1b[21;1H{rule}\x1b[22;1H\u{276F} \
             \x1b[23;1H{rule}\x1b[24;1H{mode}\x1b[22;3H"
        );
        feed(app, session, screen.as_bytes());
    }

    /// NO SELECTION NOBODY CAN SEE: the keyboard selects only a chip the pane
    /// has ROOM for, the reveal giving way first.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_keyboard_never_selects_a_chip_the_pane_has_no_room_for() {
        use aterm_window::keyboard::{KeyCode, NamedKey};
        const ACCEPT_SHORT: &str = "  \u{23F5}\u{23F5} accept edits on";
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        assert!(app.apply_term_resize(wid, 24, 40));
        draw_claude_cols(&app, session, ACCEPT_SHORT, 40);
        frame(&mut app, wid);
        let at_rest = hit(&app, wid, Light::Mode);
        chord(&mut app, wid);
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, Target::from(MODE)))
        );
        frame(&mut app, wid);
        let text = row_text(&app, wid, at_rest.0);
        assert!(
            text.contains("\u{23F5}\u{23F5} accept edits"),
            "the chip stays: {text:?}"
        );
        assert_eq!(hit(&app, wid, Light::Mode).0, at_rest.0);
        assert!(pty(&mut reader, 0).is_empty());
        assert!(app.apply_term_resize(wid, 24, 30));
        draw_claude_cols(&app, session, ACCEPT_SHORT, 30);
        frame(&mut app, wid);
        assert!(app.windows[&wid].claude_lights.hits.is_empty(), "no room");
        tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
        assert_eq!(
            pty(&mut reader, 1),
            b"\r",
            "the Return is Claude's, not an unseen light's"
        );
        assert_eq!(app.windows[&wid].claude_lights.selected, None);
        chord(&mut app, wid);
        let facts = app.windows[&wid].claude_lights.facts_on_glass(session);
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            facts.first().map(|fact| (session, Target::Fact(*fact))),
            "control: no chip to select where none fits, only a fact on the glass"
        );
        assert!(
            pty(&mut reader, 0).is_empty(),
            "and the chord is still the lights': never Claude's shift+tab"
        );
        tap(&mut app, wid, NamedKey::Escape, KeyCode::Escape, "\u{1b}");
        assert_eq!(app.windows[&wid].claude_lights.selected, None);
        tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
        assert_eq!(pty(&mut reader, 1), b"\r");

        let (mut app, wid, session, mut reader) = self::app();
        let _host = host(&mut app);
        publish_facts(&app, session);
        for (cols, chips) in [
            (40_u16, &[Chip::Autopilot, MODE][..]),
            (36, &[Chip::Autopilot, MODE][..]),
            (30, &[Chip::Autopilot][..]),
        ] {
            assert!(app.apply_term_resize(wid, 24, cols));
            draw_claude_cols(&app, session, ACCEPT_SHORT, usize::from(cols));
            frame(&mut app, wid);
            let facts = app.windows[&wid].claude_lights.facts_on_glass(session);
            let switch = app.windows[&wid]
                .claude_lights
                .shows(session, Target::Switch);
            let walk: Vec<Target> = chips
                .iter()
                .map(|chip| Target::Chip(*chip))
                .chain(facts.into_iter().map(Target::Fact))
                .chain(switch.then_some(Target::Switch))
                .chain([Target::Chip(Chip::Autopilot)])
                .collect();
            for want in walk {
                chord(&mut app, wid);
                assert_eq!(
                    app.windows[&wid].claude_lights.selected,
                    Some((session, want)),
                    "{cols} columns"
                );
                frame(&mut app, wid);
                assert!(
                    app.windows[&wid].claude_lights.shows(session, want),
                    "{cols}: {want:?} on the glass"
                );
            }
            assert_eq!(painted(&app, wid, MODE), cols != 30, "{cols}");
            tap(&mut app, wid, NamedKey::Escape, KeyCode::Escape, "\u{1b}");
            assert_eq!(app.windows[&wid].claude_lights.selected, None);
        }
        assert!(pty(&mut reader, 0).is_empty(), "the walk types nothing");
        tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
        assert_eq!(pty(&mut reader, 1), b"\r", "the Return is Claude's");
    }

    /// The session's own Autopilot word and the posture it is read under.
    pub(super) fn autopilot(app: &App, session: u64) -> (Option<Autopilot>, AutopilotView) {
        let (word, view, _) = app.session_autopilot(session);
        (word, view)
    }

    /// How many person gestures reached `session` (`status human_seq=`).
    pub(super) fn human_seq(app: &App, session: u64) -> u64 {
        app.pool
            .get(session)
            .expect("session")
            .ctx
            .human_input
            .seq()
    }

    /// The footer's repaint key for the window's visible plan.
    pub(super) fn footer_fp(app: &App, wid: WindowId) -> u64 {
        let plan = app.active_visible_leaf_plan(wid).expect("a visible plan");
        app.claude_footer_fp(wid, &plan)
    }

    #[test]
    fn a_click_on_autopilot_cycles_its_word_and_types_nothing() {
        let (mut app, wid, session, mut reader) = wider_app();
        let _host = host(&mut app);
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
        frame(&mut app, wid);
        let quiet = quiet_ink(&app, wid);
        let human = human_seq(&app, session);
        assert_eq!(
            autopilot(&app, session),
            (None, AutopilotView::Cruise),
            "the table's default"
        );
        let chip = hit(&app, wid, Chip::Autopilot);
        assert_eq!(
            (cell_at(&app, wid, chip).ch, cell_at(&app, wid, chip).fg),
            ('\u{221e}', letter_ink(&app, wid, session, chip))
        );
        let (x, y) = px_of(&app, wid, chip);
        app.on_cursor_moved(wid, x, y);
        frame(&mut app, wid);
        let text = row_text(&app, wid, chip.0);
        assert!(
            text.contains(&format!(
                "{} \u{b7} right-click: more",
                AutopilotView::Cruise.title()
            )),
            "the hover's title: {text:?}"
        );
        for (want, face, lettered) in [
            (Autopilot::Land, "\u{2913} land", true),
            (Autopilot::Off, "\u{25cb} off", false),
            (Autopilot::Cruise, "\u{221e} cruise", true),
        ] {
            let before = footer_fp(&app, wid);
            click(&mut app, wid, Chip::Autopilot);
            assert_eq!(autopilot(&app, session).0, Some(want), "the word");
            assert_ne!(footer_fp(&app, wid), before, "{want:?}: the click repaints");
            frame(&mut app, wid);
            let at = hit(&app, wid, Chip::Autopilot);
            let text = row_text(&app, wid, at.0);
            assert!(text.contains(face), "{want:?}: {text:?}");
            let ink = if lettered {
                letter_ink(&app, wid, session, at)
            } else {
                quiet
            };
            assert_eq!(cell_at(&app, wid, at).fg, ink, "{want:?}");
            assert!(
                app.windows[&wid].claude_lights.sessions[&session]
                    .autopilot_pending
                    .is_none(),
                "{want:?}: read back"
            );
            let settled = footer_fp(&app, wid);
            frame(&mut app, wid);
            assert_eq!(footer_fp(&app, wid), settled, "{want:?}: still at rest");
        }
        assert!(
            pty(&mut reader, 0).is_empty(),
            "nothing to the PTY: no key, no mouse report"
        );
        assert_eq!(
            human_seq(&app, session),
            human,
            "never a person's keystroke"
        );
        redraw_mode_row(&app, session, PLAN);
        frame(&mut app, wid);
        click(&mut app, wid, Light::Mode);
        assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB, "the light presses");
    }

    #[test]
    fn two_quick_clicks_on_the_autopilot_chip_each_step() {
        let (mut app, wid, session, mut reader) = app();
        let _host = host(&mut app);
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
        frame(&mut app, wid);
        let human = human_seq(&app, session);
        click(&mut app, wid, Chip::Autopilot);
        click(&mut app, wid, Chip::Autopilot);
        assert_eq!(
            autopilot(&app, session),
            (Some(Autopilot::Off), AutopilotView::Off),
            "both clicks stepped"
        );
        assert!(
            app.windows[&wid].claude_lights.sessions[&session]
                .autopilot_pending
                .is_none(),
            "nothing in flight"
        );
        frame(&mut app, wid);
        let at = hit(&app, wid, Chip::Autopilot);
        assert!(
            row_text(&app, wid, at.0).contains("\u{25cb} off"),
            "the chip names off"
        );
        assert!(pty(&mut reader, 0).is_empty(), "nothing to the PTY");
        assert_eq!(
            human_seq(&app, session),
            human,
            "never a person's keystroke"
        );
    }

    #[test]
    fn a_click_between_two_chips_does_nothing_and_reaches_no_program() {
        use crate::native_settings::SettingsRoute;
        let (mut app, wid, session, mut reader) = app();
        let _host = host(&mut app);
        publish_facts(&app, session);
        draw_claude(&app, session, PLAN);
        feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
        frame(&mut app, wid);
        let first = hit(&app, wid, Chip::Autopilot);
        let light = hit(&app, wid, MODE);
        assert_eq!(light, (first.0, first.1 + 2), "`∞`, a blank, the light");
        let press = |app: &mut App, cell: (usize, usize)| {
            let (x, y) = px_of(app, wid, cell);
            app.on_cursor_moved(wid, x, y);
            app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
            app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        };
        press(&mut app, (light.0, light.1 - 1));
        assert_eq!(
            app.windows[&wid].claude_lights.hover, None,
            "the gap: no chip hovered"
        );
        assert!(pty(&mut reader, 0).is_empty(), "the gap: no mouse report");
        assert_eq!(autopilot(&app, session).0, None, "no word written");
        assert!(
            app.native_settings_route_view_target(SettingsRoute::Harness)
                .is_none(),
            "no Settings opened"
        );
        assert!(
            app.windows[&wid].claude_lights.sessions[&session]
                .pending
                .is_none(),
            "no light toggled"
        );
        press(&mut app, (first.0, first.1 - 1));
        assert!(
            pty(&mut reader, 1).starts_with(b"\x1b[<0;"),
            "the pane's cell is the program's"
        );
    }

    #[test]
    fn the_pointer_is_a_hand_over_a_chip_and_the_arrow_between_two() {
        let (mut app, wid, session, mut reader) = app();
        let _host = host(&mut app);
        publish_facts(&app, session);
        draw_claude(&app, session, PLAN);
        frame(&mut app, wid);
        let first = hit(&app, wid, Chip::Autopilot);
        let light = hit(&app, wid, MODE);
        let gap = (light.0, light.1 - 1);
        let pane = (first.0, first.1 - 1);
        let cursor = |app: &App| {
            let ws = &app.windows[&wid];
            (ws.hover_pointer, ws.native_text_cursor)
        };
        let (hand, arrow, beam) = ((true, false), (false, false), (false, true));
        for (cell, want, what) in [
            (pane, beam, "control: the pane's cell"),
            (light, hand, "the light"),
            (gap, arrow, "the blank"),
            (first, hand, "Autopilot"),
            (gap, arrow, "the blank again"),
            (pane, beam, "control: the pane's cell again"),
        ] {
            let (x, y) = px_of(&app, wid, cell);
            app.on_cursor_moved(wid, x, y);
            assert_eq!(cursor(&app), want, "{what}: on motion");
            app.update_hover_cursor(wid);
            assert_eq!(cursor(&app), want, "{what}: re-resolved");
        }
        assert!(pty(&mut reader, 0).is_empty(), "the motion types nothing");
    }

    #[test]
    fn a_modifier_under_an_open_menu_lights_no_chip() {
        let (mut app, wid, session, _reader) = app();
        let _host = host(&mut app);
        publish_facts(&app, session);
        draw_claude(&app, session, PLAN);
        frame(&mut app, wid);
        let first = hit(&app, wid, Chip::Autopilot);
        let light = hit(&app, wid, MODE);
        let pane = (first.0, first.1 - 1);
        let cursor = |app: &App| {
            let ws = &app.windows[&wid];
            (ws.hover_pointer, ws.native_text_cursor)
        };
        let (x, y) = px_of(&app, wid, pane);
        app.on_cursor_moved(wid, x, y);
        assert_eq!(cursor(&app), (false, true));
        let (cx, cy) = px_of(&app, wid, first);
        assert!(app.open_autopilot_menu_at(wid, cx, cy), "the chip opens it");
        let (lx, ly) = px_of(&app, wid, light);
        app.on_cursor_moved(wid, lx, ly);
        assert!(app.autopilot_menu.is_some(), "the motion leaves it open");
        let before = cursor(&app);
        app.update_hover_cursor(wid);
        assert_eq!(cursor(&app), before, "no chip's hand under the menu");
        assert!(app.windows[&wid].link_hover.is_none());
        assert!(app.close_autopilot_menu());
        app.update_hover_cursor(wid);
        assert_eq!(cursor(&app), (true, false), "control: the chip's hand");
    }

    #[test]
    fn a_middle_or_right_press_on_the_chips_reaches_no_program() {
        use crate::native_settings::SettingsRoute;
        let (mut app, wid, session, mut reader) = app();
        let _host = host(&mut app);
        publish_facts(&app, session);
        draw_claude(&app, session, PLAN);
        feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
        frame(&mut app, wid);
        let first = hit(&app, wid, Chip::Autopilot);
        let light = hit(&app, wid, MODE);
        let chips = [first, (light.0, light.1 - 1), light];
        let pane = (first.0, first.1 - 1);
        let press = |app: &mut App, cell: (usize, usize), button: MouseButton| {
            let (x, y) = px_of(app, wid, cell);
            app.on_cursor_moved(wid, x, y);
            app.on_mouse_input(wid, ElementState::Pressed, button);
            app.on_mouse_input(wid, ElementState::Released, button);
        };
        for (button, code) in [(MouseButton::Middle, "1"), (MouseButton::Right, "2")] {
            for cell in chips {
                press(&mut app, cell, button);
                assert!(
                    pty(&mut reader, 0).is_empty(),
                    "{button:?} {cell:?}: no report"
                );
            }
            press(&mut app, pane, button);
            let got = String::from_utf8_lossy(&pty(&mut reader, 1)).into_owned();
            assert!(
                got.starts_with(&format!("\x1b[<{code};")),
                "control: {button:?} on the pane's cell is the program's: {got:?}"
            );
        }
        feed(&app, session, b"\x1b[?1000l\x1b[?1006l");
        app.config.right_click = Some("copy_paste".to_owned());
        crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = Some("PROBE-CLIP".into()));
        for cell in chips {
            press(&mut app, cell, MouseButton::Right);
            assert!(pty(&mut reader, 0).is_empty(), "{cell:?}: no paste");
        }
        press(&mut app, pane, MouseButton::Right);
        let got = String::from_utf8_lossy(&pty(&mut reader, "PROBE-CLIP".len())).into_owned();
        crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = None);
        assert!(
            got.contains("PROBE-CLIP"),
            "control: the pane's cell pastes: {got:?}"
        );
        assert_eq!(autopilot(&app, session).0, None, "no word written");
        assert!(
            app.native_settings_route_view_target(SettingsRoute::Harness)
                .is_none(),
            "no Settings opened"
        );
        assert!(
            app.windows[&wid].claude_lights.sessions[&session]
                .pending
                .is_none(),
            "no light toggled"
        );
    }

    #[test]
    fn a_click_on_the_chips_commits_an_open_in_grid_rename() {
        use crate::app_rename::RenameSurface;
        use crate::app_search::SearchEdit;
        use crate::session_timeline::MetaField;
        let (mut app, wid, session, mut reader) = app();
        let _host = host(&mut app);
        app.tab_strip_rows = 1;
        publish_facts(&app, session);
        draw_claude(&app, session, PLAN);
        frame(&mut app, wid);
        let tab = app.windows[&wid].tab_set.active_id().expect("Claude's tab");
        let pinned = |app: &App| {
            app.pool
                .get(session)
                .expect("session")
                .ctx
                .meta
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .presentation_value(MetaField::Title.wire_name())
        };
        let rename = |app: &mut App, name: &str| {
            assert!(app.begin_session_rename(wid, tab), "the field opens");
            assert_eq!(
                app.inline_rename_edit(wid).map(|edit| edit.surface),
                Some(RenameSurface::InGrid)
            );
            assert!(app.rename_field_edit(wid, SearchEdit::KillToStart));
            assert!(app.rename_field_edit(wid, SearchEdit::Insert(name.to_owned())));
        };
        rename(&mut app, "left open");
        assert!(app.open_harness_settings(wid, crate::harness_screen::HarnessFocus::Top));
        assert!(
            app.inline_rename_edit(wid).is_some(),
            "control: a tab switch settles nothing"
        );
        app.end_session_rename_editor(wid);
        assert_eq!(pinned(&app), None, "control: nothing committed");
        app.switch_tab_in(wid, 0);
        frame(&mut app, wid);
        let policy = |text: &str| {
            crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read(text),
            )
        };
        app.harness_policy = policy("[harness]\nenabled = false\n");
        frame(&mut app, wid);
        rename(&mut app, "off chip");
        click(&mut app, wid, Chip::Autopilot);
        assert!(
            app.inline_rename_edit(wid).is_none(),
            "the off chip: closed"
        );
        assert_eq!(pinned(&app).as_deref(), Some("off chip"), "and committed");
        assert_eq!(
            app.focused_session_id(wid),
            Some(session),
            "`⊘ off`: nothing came to the front"
        );
        assert_eq!(autopilot(&app, session).0, None, "`⊘ off`: no word written");
        app.harness_policy = policy("[harness]\n");
        app.switch_tab_in(wid, 0);
        frame(&mut app, wid);
        super::switch_tests::later(&mut app, wid, session);
        rename(&mut app, "autopilot");
        click(&mut app, wid, Chip::Autopilot);
        assert!(app.inline_rename_edit(wid).is_none(), "Autopilot: closed");
        assert_eq!(pinned(&app).as_deref(), Some("autopilot"));
        assert_eq!(
            autopilot(&app, session).0,
            Some(Autopilot::Land),
            "the click still steps the word"
        );
        frame(&mut app, wid);
        rename(&mut app, "gap");
        let light = hit(&app, wid, MODE);
        let (x, y) = px_of(&app, wid, (light.0, light.1 - 1));
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        assert!(app.inline_rename_edit(wid).is_none(), "the blank: closed");
        assert_eq!(pinned(&app).as_deref(), Some("gap"));
        assert!(
            pty(&mut reader, 0).is_empty(),
            "nothing reached the program"
        );
    }

    #[test]
    fn the_hovered_title_offers_the_menu_where_it_fits_and_is_short_where_not() {
        let v1 = &AutopilotView::Cruise.title();
        let whole = format!("{v1} \u{b7} right-click: more");
        let click = "autopilot: cruise \u{b7} click: land";
        let explained = u16::try_from(
            format!(
                " \u{2014} {}",
                crate::autopilot::mode_words(Autopilot::Cruise).chip
            )
            .chars()
            .count(),
        )
        .unwrap();
        let clicks = format!("{click} \u{b7} right-click: more");
        let (mut app, wid, session, _reader) = app();
        let _host = host(&mut app);
        publish_facts(&app, session);
        let mut seen = Vec::new();
        for width in (60..=170u16).rev() {
            assert!(app.apply_term_resize(wid, 24, width));
            draw_claude(&app, session, BYPASS);
            app.hover_claude_chip(wid, None);
            frame(&mut app, wid);
            app.hover_claude_chip(wid, Some((session, Chip::Autopilot)));
            frame(&mut app, wid);
            let rule = row_text(&app, wid, footer_row(&app, wid));
            let form: u8 = if rule.contains(&whole) {
                5
            } else if rule.contains(v1) {
                assert!(!rule.contains("right-click"), "{width}: {rule:?}");
                4
            } else if rule.contains(&clicks) {
                assert!(!rule.contains("\u{2014}"), "{width}: {rule:?}");
                3
            } else if rule.contains(click) {
                assert!(
                    !rule.contains("right-click") && !rule.contains("\u{2014}"),
                    "{width}: {rule:?}"
                );
                2
            } else if rule.contains("\u{221e}  autopilot: cruise ") {
                assert!(
                    !rule.contains("click") && !rule.contains("\u{2014}"),
                    "{width}: the short form is the mode phrase alone: {rule:?}"
                );
                1
            } else {
                assert!(
                    !rule.contains("autopilot"),
                    "{width}: no cut title: {rule:?}"
                );
                0
            };
            if form > 0 {
                assert!(
                    rule.contains(WHOLE_FACTS),
                    "{width}: the facts stay whole beside a title: {rule:?}"
                );
                let chip = hit(&app, wid, Chip::Autopilot);
                let (x, y) = px_of(&app, wid, chip);
                assert!(
                    app.open_autopilot_menu_at(wid, x, y),
                    "{width}: the chip opens the menu"
                );
                assert!(app.close_autopilot_menu());
            }
            if let Some(&(wider, last)) = seen.last() {
                assert!(
                    form <= last,
                    "{width} after {wider}: a title came back: {rule:?}"
                );
            }
            seen.push((width, form));
        }
        let narrowest = |form: u8| {
            seen.iter()
                .filter(|(_, f)| *f == form)
                .map(|(w, _)| *w)
                .min()
        };
        assert!(
            (0..=5).all(|form| narrowest(form).is_some()),
            "CONTROL: every form is met: {seen:?}"
        );
        let apart = |wider: u8, narrower: u8| {
            narrowest(narrower)
                .zip(narrowest(wider))
                .map(|(n, w)| w - n)
        };
        assert_eq!(
            apart(5, 4),
            Some(20),
            "v1's words stand wherever v1's whole title fit: {seen:?}"
        );
        assert_eq!(
            apart(4, 3),
            Some(explained - 20),
            "both clicks where the explanation's {explained} cells give way to the menu's 20: \
             {seen:?}"
        );
        assert_eq!(
            apart(3, 2),
            Some(20),
            "the click where the menu's line gives way: {seen:?}"
        );
        eprintln!(
            "the whole title from {:?} columns, v1's words from {:?}, both clicks from {:?}, the \
             click from {:?}, the short form from {:?}",
            narrowest(5),
            narrowest(4),
            narrowest(3),
            narrowest(2),
            narrowest(1)
        );
    }

    /// The record the harness switch's refused write posted for `key`.
    fn switch_write_said(app: &App, key: &str) -> Option<String> {
        let want = format!("harness.switch-save.{key}");
        app.messages
            .log()
            .records()
            .rfind(|r| r.key.as_deref() == Some(want.as_str()))
            .map(|r| r.detail.join(" "))
    }

    #[test]
    fn under_the_master_switch_off_the_autopilot_chip_turns_it_on() {
        use crate::native_settings::SettingsRoute;
        let (mut app, wid, session, mut reader) = wider_app();
        let _host = host(&mut app);
        let policy = |text: &str| {
            crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read(text),
            )
        };
        app.harness_policy = policy("[harness]\nenabled = false\n");
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        let chip = hit(&app, wid, Chip::Autopilot);
        assert_eq!(
            (cell_at(&app, wid, chip).ch, cell_at(&app, wid, chip).fg),
            ('\u{2298}', quiet_ink(&app, wid))
        );
        let (x, y) = px_of(&app, wid, chip);
        app.on_cursor_moved(wid, x, y);
        frame(&mut app, wid);
        let text = row_text(&app, wid, chip.0);
        assert!(
            text.contains("\u{2298} off")
                && text.contains(
                    "harness off in Settings \u{b7} click: turn the harness on \u{b7} \
                     right-click: more"
                ),
            "{text:?}"
        );
        assert_eq!(
            switch_write_said(&app, "harness.enabled"),
            None,
            "control: no write yet"
        );
        let tabs = app.windows[&wid].tab_set.len();
        super::SWITCHES_LET_GO.with(|n| n.set(0));
        click(&mut app, wid, Chip::Autopilot);
        let said =
            switch_write_said(&app, "harness.enabled").expect("the master switch's write was made");
        assert!(said.contains("harness.enabled = true"), "{said:?}");
        assert_eq!(
            super::SWITCHES_LET_GO.with(std::cell::Cell::get),
            1,
            "`◌ turning on` was armed, and the refusal let it go"
        );
        assert!(
            app.native_settings_route_view_target(SettingsRoute::Harness)
                .is_none(),
            "Settings ▸ Harness stays shut"
        );
        assert_eq!(app.windows[&wid].tab_set.len(), tabs, "no tab opened");
        assert_eq!(
            autopilot(&app, session),
            (None, AutopilotView::MasterOff),
            "no word written"
        );
        assert!(pty(&mut reader, 0).is_empty());
        app.harness_policy = policy("[harness]\n");
        frame(&mut app, wid);
        super::switch_tests::later(&mut app, wid, session);
        click(&mut app, wid, Chip::Autopilot);
        assert_eq!(
            autopilot(&app, session),
            (Some(Autopilot::Land), AutopilotView::Land),
            "the click wrote the word"
        );
        assert_eq!(app.windows[&wid].tab_set.len(), tabs, "no tab opened");
        assert_eq!(
            app.focused_session_id(wid),
            Some(session),
            "Claude's tab stays in front"
        );
        assert!(pty(&mut reader, 0).is_empty());
    }

    #[test]
    fn under_the_agents_switch_off_the_autopilot_chip_turns_it_on() {
        use crate::harness_screen::HarnessFocus;
        use crate::native_settings::SettingsRoute;
        let (mut app, wid, session, _reader) = wider_app();
        let _host = host(&mut app);
        publish_facts(&app, session);
        app.pool
            .get(session)
            .expect("session")
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .set_program(PGID, Some("claude".into()));
        let landed = |app: &App| {
            app.native_settings_route_view_target(SettingsRoute::Harness)
                .map(|(_, _, _, state)| state.harness_focus)
        };
        let front = |app: &mut App| {
            app.switch_tab_in(wid, 0);
            draw_claude(app, session, BYPASS);
            frame(app, wid);
        };
        assert!(app.open_harness_settings(wid, HarnessFocus::Top));
        assert_eq!(landed(&app), Some(HarnessFocus::Top));
        app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
            crate::app_config::HarnessPolicy::read("[harness.claude]\nenabled = false\n"),
        );
        front(&mut app);
        assert_eq!(
            autopilot(&app, session),
            (None, AutopilotView::AgentOff(aterm_phase::Program::Claude))
        );
        super::SWITCHES_LET_GO.with(|n| n.set(0));
        click(&mut app, wid, Chip::Autopilot);
        assert!(
            switch_write_said(&app, "harness.claude.enabled").is_some(),
            "Claude Code's own switch was written"
        );
        assert_eq!(
            switch_write_said(&app, "harness.enabled"),
            None,
            "not the master's"
        );
        assert_eq!(autopilot(&app, session).0, None, "no word written");
        assert_eq!(landed(&app), None, "⊘ off: no page raised");
        assert_eq!(
            app.focused_session_id(wid),
            Some(session),
            "⊘ off: Claude's tab stays in front"
        );
        frame(&mut app, wid);
        click(&mut app, wid, Chip::Autopilot);
        assert_eq!(
            super::SWITCHES_LET_GO.with(std::cell::Cell::get),
            1,
            "one write for a double click"
        );
        let last = app.windows[&wid].tab_set.len() - 1;
        app.switch_tab_in(wid, last);
        assert_eq!(
            landed(&app),
            Some(HarnessFocus::Top),
            "⊘ off: no page landed"
        );
    }

    #[test]
    fn an_off_session_still_draws_its_chip() {
        for own in [true, false] {
            let (mut app, wid, session, _reader) = app();
            let _host = host(&mut app);
            if own {
                assert!(app.set_session_autopilot(
                    session,
                    Some(Autopilot::Off),
                    crate::autopilot::Surface::Chip
                ));
            } else {
                app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                    crate::app_config::HarnessPolicy::read("[harness]\nautopilot = \"off\"\n"),
                );
            }
            assert_eq!(autopilot(&app, session).1, AutopilotView::Off);
            publish_facts(&app, session);
            draw_claude(&app, session, BYPASS);
            frame(&mut app, wid);
            let chip = hit(&app, wid, Chip::Autopilot);
            let text = row_text(&app, wid, chip.0);
            assert_eq!(
                cell_at(&app, wid, chip).ch,
                '\u{25cb}',
                "own={own}: {text:?}"
            );
            assert!(
                !text.contains("\u{25cb} off"),
                "own={own}: no click, no name: {text:?}"
            );
            assert_eq!(
                cell_at(&app, wid, chip).fg,
                quiet_ink(&app, wid),
                "own={own}: the quiet ink"
            );
        }
    }

    #[test]
    fn a_narrow_rule_gives_the_chips_up_before_the_model() {
        /// What the rule showed at one width.
        #[derive(Debug)]
        struct Seen {
            cols: u16,
            /// The model's fact.
            model: bool,
            /// The path's or the branch's.
            rest: bool,
            /// The effort's, the path's and the branch's.
            facts: [bool; 3],
            /// Autopilot's chip and the light's.
            chips: (bool, bool),
            text: String,
        }
        const LADDER: [(bool, bool); 3] = [(true, true), (true, false), (false, false)];
        const PLAN_SHORT: &str = "  \u{23F8} plan mode on";
        const ACCEPT_SHORT: &str = "  \u{23F5}\u{23F5} accept edits on";
        for pill in [None, Some(PLAN_SHORT), Some(ACCEPT_SHORT)] {
            let (mut app, wid, session, _reader) = app();
            let _host = host(&mut app);
            publish_facts(&app, session);
            let narrowest = pill.map_or(18, |p| p.chars().count().max(18));
            let mut seen = Vec::new();
            for cols in (u16::try_from(narrowest).unwrap()..=80).rev() {
                if cols != 80 {
                    assert!(app.apply_term_resize(wid, 24, cols));
                }
                let rule = "\u{2500}".repeat(usize::from(cols));
                let pill_row = pill.map_or(String::new(), |p| format!("\x1b[24;1H{p}"));
                feed(
                    &app,
                    session,
                    format!(
                        "\x1b[2J\x1b[19;1H\u{25CF} done\x1b[21;1H{rule}\x1b[22;1H\u{276F} \
                         \x1b[23;1H{rule}{pill_row}\x1b[22;3H"
                    )
                    .as_bytes(),
                );
                frame(&mut app, wid);
                let text = row_text(&app, wid, footer_row(&app, wid));
                let model = text.contains("\u{25C6} Opus 5.5");
                let facts = ["xhigh", "\u{2302}", "\u{2387}"].map(|fact| text.contains(fact));
                let rest = facts[1] || facts[2];
                let chips = (
                    painted(&app, wid, Chip::Autopilot),
                    painted(&app, wid, MODE),
                );
                if rest {
                    assert_eq!(
                        chips,
                        (true, pill.is_some()),
                        "{pill:?} {cols}: a fact outlived a chip: {text:?}"
                    );
                }
                seen.push(Seen {
                    cols,
                    model,
                    rest,
                    facts,
                    chips,
                    text,
                });
            }
            let widest = &seen[0];
            assert!(
                widest.cols == 80
                    && widest.model
                    && widest.facts == [true; 3]
                    && widest.chips == (true, pill.is_some()),
                "{pill:?} 80: everything: {:?}",
                widest.text
            );
            let chipped: Vec<&Seen> = seen.iter().filter(|s| s.chips != (false, false)).collect();
            for (fact, name) in ["the effort", "the path", "the branch"].iter().enumerate() {
                assert!(
                    chipped
                        .windows(2)
                        .all(|pair| pair[0].facts[fact] || !pair[1].facts[fact]),
                    "{pill:?}: {name} came back beside the chips: {seen:#?}"
                );
                assert!(
                    chipped.iter().any(|s| s.model && !s.facts[fact]),
                    "{pill:?}: control, {name} given up beside the chips: {seen:#?}"
                );
            }
            let rungs: Vec<usize> = seen
                .iter()
                .filter(|s| s.model)
                .map(|s| {
                    LADDER
                        .iter()
                        .position(|&(autopilot, light)| {
                            (autopilot, light && pill.is_some()) == s.chips
                        })
                        .unwrap_or_else(|| panic!("{pill:?}: off the ladder: {s:?}"))
                })
                .collect();
            assert!(
                rungs.windows(2).all(|pair| pair[0] <= pair[1]),
                "{pill:?}: never back up the ladder: {seen:#?}"
            );
            let shows = |chips: (bool, bool)| seen.iter().any(|s| s.model && s.chips == chips);
            assert!(
                seen.iter()
                    .any(|s| s.model && !s.rest && s.chips == widest.chips),
                "{pill:?}: every chip beside the model alone: {seen:#?}"
            );
            assert!(
                shows((true, false)),
                "{pill:?}: the light given up, Autopilot kept: {seen:#?}"
            );
            if pill.is_none() {
                assert!(
                    shows((false, false)),
                    "the chips given up, the model kept: {seen:#?}"
                );
            }
        }
    }

    /// RETURN ACTS ON A SELECTED HARNESS CHIP (macOS): `ctrl+shift+tab`
    /// selects Autopilot, and Return steps its word — cruise to land — with
    /// NOTHING sent to the PTY (the Return is the chip's, not Claude's) and
    /// no person stamped; the selection lets go.
    #[cfg(target_os = "macos")]
    #[test]
    fn return_on_a_selected_autopilot_chip_cycles_it() {
        use aterm_window::keyboard::{KeyCode, NamedKey};
        let (mut app, wid, session, mut reader) = app();
        let _host = host(&mut app);
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        let human = human_seq(&app, session);
        chord(&mut app, wid);
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, Target::from(Chip::Autopilot)))
        );
        frame(&mut app, wid);
        tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
        assert_eq!(autopilot(&app, session).0, Some(Autopilot::Land));
        assert!(pty(&mut reader, 0).is_empty(), "the Return is the chip's");
        assert_eq!(app.windows[&wid].claude_lights.selected, None);
        frame(&mut app, wid);
        let text = row_text(&app, wid, footer_row(&app, wid));
        assert!(
            text.contains("\u{2913} land"),
            "the press names it: {text:?}"
        );
        assert_eq!(human_seq(&app, session), human, "no person stamped");
        tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
        assert_eq!(pty(&mut reader, 1), b"\r");
        let human = human_seq(&app, session);
        chord(&mut app, wid);
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, Target::from(Chip::Autopilot)))
        );
        frame(&mut app, wid);
        tap(&mut app, wid, NamedKey::Space, KeyCode::Space, " ");
        assert_eq!(
            autopilot(&app, session).0,
            Some(Autopilot::Off),
            "land to off"
        );
        assert!(pty(&mut reader, 0).is_empty(), "the Space is the chip's");
        assert_eq!(human_seq(&app, session), human, "no person stamped");
    }
}

#[cfg(test)]
#[path = "claude_lights_face_tests.rs"]
mod face_tests;

#[cfg(all(test, unix))]
#[path = "claude_lights_yolo_tests.rs"]
mod yolo_tests;

#[cfg(all(test, unix))]
#[path = "claude_facts_gesture_tests.rs"]
mod fact_gesture_tests;

#[cfg(all(test, unix))]
#[path = "fact_menu_glass_tests.rs"]
mod fact_menu_glass_tests;

#[cfg(all(test, unix))]
#[path = "claude_slash_tests.rs"]
mod slash_tests;

#[cfg(all(test, unix))]
#[path = "claude_lights_ticker_tests.rs"]
mod ticker_tests;

#[cfg(all(test, unix))]
#[path = "claude_lights_word_tests.rs"]
mod word_tests;

/// `claude_mode_toggle` environment actions.
#[cfg(all(any(test, debug_assertions), unix))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_mode_toggle",
    action = "Answer",
    reason = "Claude Code's own step: its pill moves on after a shift+tab. The Tier-1 test \
              redraws the real engine's mode row and projects the App state."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_mode_toggle",
    action = "Cover",
    reason = "Claude Code's own step: a box over the composer. The Tier-1 test draws one on \
              the real engine and checks nothing is pressed into it."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_mode_toggle",
    action = "Uncover",
    reason = "Claude Code's own step: the box answered and the composer back."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_mode_toggle",
    action = "Leave",
    reason = "A person's own shift+tab in Claude, away from an expected mode, with no toggle \
              in flight: aterm's own way out of bypass or auto, a click on yolo or auto, is the \
              modeled Click and Press."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_mode_toggle",
    action = "DontAsk",
    reason = "A person's own choice of don't ask in Claude: aterm has no gesture that sets \
              it. The Tier-1 test draws the don't-ask pill and returns from it."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_mode_toggle",
    action = "NoBypass",
    reason = "The session's own cycle, without bypass (a session started without it): the \
              Tier-1 tests answer each press from such a ring."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_mode_toggle",
    action = "NoAuto",
    reason = "The session's own cycle, without auto (a build or an account without it, bypass \
              to manual): the Tier-1 tests answer each press from such a ring."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_mode_toggle",
    action = "TurnStarts",
    reason = "Claude Code's own step: a turn starts. The Tier-1 test draws the busy mode row \
              on the real engine."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_mode_toggle",
    action = "TurnEnds",
    reason = "Claude Code's own step: the turn ends, the busy hint gone from the mode row."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_mode_toggle",
    action = "Timeout",
    reason = "The clock crosses a press's deadline outside an aterm transition. The Tier-1 \
              late-answer test advances the real deadline, then checks the App stops."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn mode_toggle_environment_waivers() {}
#[cfg(all(test, unix))]
#[path = "claude_lights_fit_tests.rs"]
mod fit_tests;

#[cfg(all(test, unix))]
#[path = "claude_lights_switch_tests.rs"]
mod switch_tests;

#[cfg(all(test, unix))]
#[path = "claude_lights_arrow_tests.rs"]
mod arrow_tests;
