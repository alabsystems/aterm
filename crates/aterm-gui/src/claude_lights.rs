// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code footer rule chips (permission mode light, [`Chip::Autopilot`], and [`Chip::Team`]).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::{Act, TaskFace};
use aterm_agent::harness::footer::{FactKind, FactMask};
use aterm_agent::harness::lights::{self, Light, Mode, Reading, Screen};
use aterm_agent::supervise::Autopilot;
use aterm_core::terminal::{RenderCell, UnderlineStyle};
use aterm_effects::glass_inks::{GlassInks, HoverPart, field};
use aterm_window::event::KeyEvent;
use aterm_window::keyboard::{Key, ModifiersState, NamedKey};

use crate::autopilot::{AutopilotView, ChipTone, PENDING_GLYPH};
use crate::permission_click::{ClickRefusal, ClickShown};
use crate::{App, WindowId};

/// `chrome`'s `autopilot` lines.
#[path = "claude_lights_drawn.rs"]
mod drawn;

/// How long a click that did not take keeps saying so.
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

    /// The lit ink: a hovered or selected element's ([`GlassInks::lit`]).
    pub(crate) fn lit(self) -> [u8; 3] {
        Inks::of(RenderCell::default(), self).ink
    }

    /// The quiet ink: the off chips' and the quiet marks' ([`GlassInks::quiet`]).
    pub(crate) fn quiet(self) -> [u8; 3] {
        Inks::of(RenderCell::default(), self).quiet
    }

    /// The value ink: a title's and the mode light's `yolo` ([`GlassInks::value`]).
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
    /// The quiet words and marks: the off chips, a worker's, a manager's
    /// ([`GlassInks::quiet`]).
    quiet: [u8; 3],
    /// A chip's title and the mode light's `yolo` ([`GlassInks::value`]).
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

/// How a block gives way in a rule short of room. The mode light keeps its place at every rung
/// wherever it is drawn: on a plain line it is the only way to yolo
/// (`docs/DESIGN-yolo-binary-2026-11-04.md` §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keep {
    /// Every chip drawn.
    Drawn,
    /// Team given up (who drives is on the tab's hover and its strip marks too).
    NoTeam,
    /// Autopilot's glyph alone, beside the light.
    GlyphOnly,
}

impl Keep {
    /// Every rung, in the order a rule short of room tries them.
    const LADDER: [Keep; 3] = [Keep::Drawn, Keep::NoTeam, Keep::GlyphOnly];

    /// Whether this rung keeps `chip`, where it is drawn at all.
    const fn keeps(self, chip: Chip) -> bool {
        match chip {
            Chip::Autopilot | Chip::Light(_) => true,
            Chip::Team => matches!(self, Keep::Drawn),
        }
    }
}

/// Why a click did not stand or did not take, as its light's title says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Refusal {
    /// The light is not on Claude's screen right now.
    NotShown,
    /// The tab's conversation has not been read, so there is none to restart.
    Unread,
    /// The agent's settings default is not read yet, so what a restart would change is not
    /// known ([`App::held_permission_default`]).
    SettingsUnread,
    /// The click can never be made here ([`ClickRefusal`]).
    Click(ClickRefusal),
    /// The click's restart failed.
    Failed,
}

impl Refusal {
    /// What the light's title says of it: the reason, which a short title keeps whole.
    const fn says(self) -> &'static str {
        match self {
            Refusal::NotShown => "not on screen right now",
            Refusal::Unread => "its conversation isn't read yet",
            Refusal::SettingsUnread => "its settings aren't read yet",
            Refusal::Click(why) => why.says(),
            Refusal::Failed => "the restart failed",
        }
    }
}

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

/// One session's lights in one window.
#[derive(Debug, Clone, Default)]
struct SessionLights {
    /// The last state each light was seen in, kept while a box covers the composer.
    shown: Reading,
    /// The tab's standing permission click, as this frame copied it from the App's table
    /// ([`App::observe_claude_lights`]): its light drawn with the glass's in-flight `◌` and titled with what it waits for.
    click: Option<ClickShown>,
    refused: Option<(Light, Refusal, Instant)>,
    /// What a click's restart would end of Claude's running jobs (`1 shell`,
    /// [`aterm_agent::harness::footer::ModeRow::job_words`]), as the frame read them off Claude's
    /// mode row under any pill while the light's title shows; empty otherwise, so a frame at rest
    /// reads no job words.
    jobs: String,
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
    continuation: Option<std::borrow::Cow<'static, str>>,
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

    /// Whether the mode light is drawn (`docs/DESIGN-yolo-binary-2026-11-04.md` §2): on a hosted
    /// session, wherever the bit is read, and with no bit read only while a click stands, its
    /// light then the one way to take it back.
    fn light_drawn(&self) -> bool {
        self.harness_chips() && (self.shown.mode.is_some() || self.click.is_some())
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
    /// Glass whose motion is not the pane's and that is no chip: the blank between two chips, a
    /// top line's mark (the hand where a press opens the site, [`App::mark_press_opens`], the
    /// arrow elsewhere; every button on it taken), or a fact with nothing to click
    /// ([`Fact::clicks`]: the folder, the branch, the context, whose right-click still opens
    /// their card). No chip's hover paint, and the motion is not the pane's.
    Gap,
}

impl OverChips {
    /// What [`App::claude_glass_at`] found.
    pub(crate) const fn of_glass(at: Option<&(u64, GlassAt)>) -> Self {
        match at {
            None => Self::No,
            Some((_, GlassAt::Chip(None) | GlassAt::Mark | GlassAt::Gap)) => Self::Gap,
            Some((_, GlassAt::Fact(fact, restores))) => {
                if fact.clicks(!restores.is_empty()) {
                    Self::Chip
                } else {
                    Self::Gap
                }
            }
            Some((_, GlassAt::Chip(Some(_)) | GlassAt::Switch(_) | GlassAt::Upgrade)) => Self::Chip,
        }
    }
}

/// `cell` of a footer element under the pointer or the keyboard's selection: the glass's one
/// hover ([`GlassInks::hovered`]), which every element a click acts on is painted through (a
/// chip, a fact of line D, line E's words, the harness switch, the top-effort key). The cell
/// keeps its glyph, weight and slant.
#[must_use]
pub(crate) fn hovered_cell(glass: &GlassInks, cell: RenderCell, part: HoverPart) -> RenderCell {
    let rgb = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
    let rest = u32::from_be_bytes([0, cell.fg[0], cell.fg[1], cell.fg[2]]);
    let paint = glass.hovered(rest, part);
    RenderCell {
        fg: rgb(paint.fg),
        underline: if paint.underline.is_some() {
            UnderlineStyle::Single
        } else {
            cell.underline
        },
        underline_color: paint.underline.map(rgb).or(cell.underline_color),
        ..cell
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
    /// The account: line E's quota row, in three parts ([`RowPart`]), or where line E is not the
    /// glass's, line D's fallback unit (`⎆ main 37% left · ↺ Sat 10:00 · ╳ ~Thu 12 Nov 04:00`), one
    /// unit.
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
            Fact::Account => "Account",
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

    /// Whether a click on it does anything: the folder, the branch and the context have nothing
    /// to click (their card is a right-click's, or the menu key's), so they take no hover paint
    /// and the arrow cursor; hidden behind a stub (`stub`), a click shows any fact again.
    #[must_use]
    pub(crate) const fn clicks(self, stub: bool) -> bool {
        stub || !matches!(self, Fact::Path | Fact::Branch | Fact::Context)
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
    /// Which part of line E's quota row it is (`crate::claude_footer::quota`); `None` for a fact
    /// of a rule.
    pub(crate) part: Option<RowPart>,
}

/// A part of line E's quota row (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §2.6): the
/// row is one [`Fact::Account`] in three hits, whose chart names the column under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RowPart {
    /// The named accounts and their figures.
    Lead,
    /// The chart, laid at this many cells a day; its first cell is the hit's first column.
    Chart(u8),
    /// The tail's phrases.
    Tail,
}

/// Where a rule's title slot landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TitleHit {
    pub(crate) frame_row: usize,
    pub(crate) term_row: usize,
    pub(crate) cols: std::ops::Range<usize>,
    pub(crate) session: u64,
}

/// What of the harness's glass is under a pixel ([`App::claude_glass_at`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GlassAt {
    /// A chip, or `None` for the blank between two.
    Chip(Option<Chip>),
    /// The top rule's harness switch (`border_toggles`), as the paint drew it.
    Switch(crate::autopilot::HarnessSwitch),
    /// A top line's mark, `ALab` with its fine print (`border_toggles`): a hover shows the
    /// CLI's whole build in the label's slot, only where the label shortens it (`label_whole`);
    /// a primary press opens the site as a card line's mark does
    /// ([`App::press_claude_glass_hit`]), and a secondary press does nothing.
    Mark,
    /// A top line's ⬆️ of a newer CLI build (`border_toggles`): a press asks for the move now.
    Upgrade,
    /// A blank that is an element's own and no part of what a click acts on (the side blanks of
    /// the harness switch's face): it lights nothing under the pointer, and every press on it is
    /// swallowed, so none reaches the program under the glass.
    Gap,
    /// A fact of the bottom rule and what a click on it shows again.
    Fact(Fact, FactMask),
}

/// The part of a top line's harness `glass` is, as its paint lights it under the pointer
/// ([`WindowLights::part_hover`]); the switch has its own ([`WindowLights::switch_hover`]).
const fn part_of(glass: GlassAt) -> Option<crate::claude_footer::HoverAt> {
    match glass {
        GlassAt::Mark => Some(crate::claude_footer::HoverAt::Label),
        GlassAt::Upgrade => Some(crate::claude_footer::HoverAt::Arrow),
        GlassAt::Chip(_) | GlassAt::Switch(_) | GlassAt::Gap | GlassAt::Fact(..) => None,
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
    /// The sessions whose composer rule this frame found on the glass (`(session, term_row, room)`).
    footers: Vec<(u64, usize, usize)>,
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
    /// The card line's mark under the pointer: `(session, is_front)`.
    pub(crate) glass_hit_hover: Option<(u64, bool)>,
    /// The window pixel the last motion put the pointer on while it hovered the glass.
    glass_hover_px: Option<(f64, f64)>,
    /// The grid the lists were laid.
    drawn_at: Option<crate::app_mouse::GlassGrid>,
    /// Where the last paint laid what a pointer hit-tests ([`WindowLights::places`]) and the
    /// hover it was laid under ([`App::settle_glass_hover`]).
    laid: Option<(u64, GlassHover)>,
    /// The digest of pre-footer `ws.chrome_rows` at the last [`App::settle_glass_hover`].
    chrome_rows_laid: u64,
    /// A paste or close confirmation band covered the top rows since the last footer settle.
    band_covered: bool,
    /// A paint after a re-grid moved the hover under a still pointer ([`App::settle_glass_hover`]).
    cursor_owed: bool,
    /// The facts on the glass.
    pub(crate) fact_hits: Vec<FactHit>,
    /// The quota row's chart column the pointer rests on: its session, the chart's cells a day
    /// and the cell ([`RowPart::Chart`]), whose span the account's title names (§2.6).
    pub(crate) quota_column: Option<(u64, u8, usize)>,
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
    /// The switch's face (` ● on ` when lit, ` ○ off ` when off), its side blanks included.
    pub(crate) cols: std::ops::Range<usize>,
    /// Its inked cells (`● on`, `○ off`: the blank between its mark and its word included and
    /// neither side blank): a press anywhere on them turns it, and a hover underlines exactly
    /// these.
    pub(crate) ink: std::ops::Range<usize>,
    /// The mark's cells, its fine print included (`ALab v0.5 (v2.1.305)`): a hover shows the
    /// CLI's whole build, a press opens the site.
    pub(crate) mark: Option<std::ops::Range<usize>>,
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
    click: Option<ClickShown>,
    refused: Option<(Light, Refusal)>,
    /// The job words' hash ([`SessionLights::jobs`]), `0` with none.
    jobs: u64,
    /// Continuation words fitted into the unselected title.
    continuation: Option<u64>,
    autopilot: Option<AutopilotView>,
    autopilot_pending: bool,
    glass: ChipGlass,
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
        let now = (self.refused.is_some() || self.fact_notice.is_some() || active_word.is_some())
            .then(now);
        let showing = |until: Instant| now.is_some_and(|now| until > now);
        LightsKey {
            word: active_word
                .zip(now)
                .and_then(|((named, word), now)| Some((named, word.step(now)?))),
            shown: self.shown,
            click: self.click,
            refused: self
                .refused
                .filter(|(_, _, until)| showing(*until))
                .map(|(light, why, _)| (light, why)),
            jobs: if self.jobs.is_empty() {
                0
            } else {
                aterm_hash::fnv1a_64(self.jobs.as_bytes()) | 1
            },
            continuation: self
                .continuation
                .as_deref()
                .map(|words| aterm_hash::fnv1a_64(words.as_bytes())),
            autopilot: self.autopilot,
            autopilot_pending: self.autopilot_pending.is_some(),
            glass: self.glass,
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

/// The pointer's hover on the glass: a chip or fact, a quota chart column, a switch, a top
/// line's part, a card line's mark.
type GlassHover = (
    Option<(u64, Target)>,
    Option<(u64, u8, usize)>,
    Option<u64>,
    Option<(u64, crate::claude_footer::HoverAt)>,
    Option<(u64, bool)>,
);

impl WindowLights {
    /// The pointer's hover on the glass now ([`GlassHover`]).
    const fn glass_hover(&self) -> GlassHover {
        (
            self.hover,
            self.quota_column,
            self.switch_hover,
            self.part_hover,
            self.glass_hit_hover,
        )
    }

    /// A digest of the pre-footer chrome bands (`ws.chrome_rows` and `ws.chrome_spans`), `0` when none are open.
    fn chrome_rows_digest(
        chrome_rows: &[std::ops::Range<usize>],
        chrome_spans: &[(std::ops::Range<usize>, std::ops::Range<usize>)],
    ) -> u64 {
        use std::hash::{Hash, Hasher};
        if chrome_rows.is_empty() && chrome_spans.is_empty() {
            return 0;
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        chrome_rows.len().hash(&mut h);
        for band in chrome_rows {
            band.hash(&mut h);
        }
        chrome_spans.len().hash(&mut h);
        for (rows, cols) in chrome_spans {
            (rows, cols).hash(&mut h);
        }
        h.finish() | 1
    }

    /// A digest of where the last paint laid what a pointer hit-tests ([`App::claude_glass_at`] and
    /// [`App::claude_glass_hit_at`]): the chips' cells, the switches with their labels and arrows,
    /// the facts and the card lines' marks.
    fn places(&self, card_hits: u64, focused: Option<u64>) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (card_hits, focused).hash(&mut h);
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
                &b.mark,
                &b.arrow,
                b.session,
            )
                .hash(&mut h);
        }
        self.fact_hits.len().hash(&mut h);
        for f in &self.fact_hits {
            (f.frame_row, f.term_row, f.cols, f.session, f.fact, f.part).hash(&mut h);
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
                s.click.is_some()
                    || s.refused.is_some_and(|(_, _, until)| shows(until))
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
        self.quota_column.hash(&mut h);
        self.selected.hash(&mut h);
        active.sort_by_key(|(id, _)| **id);
        for (id, s) in active {
            id.hash(&mut h);
            if let Some(click) = s.click {
                click.hash(&mut h);
            }
            if let Some((light, why, until)) = s.refused
                && shows(until)
            {
                (light, why).hash(&mut h);
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

    /// Forget every painted light and footer rule.
    pub(crate) fn clear_hits(&mut self) {
        self.hits.clear();
        self.border_toggles.clear();
        self.title_hits.clear();
        self.fact_hits.clear();
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

    /// The whole title someone reads of `session`'s `fact` here, as
    /// [`App::note_fact_readings`] last built it.
    #[cfg(test)]
    pub(crate) fn fact_reading(&self, session: u64, fact: Fact) -> Option<&str> {
        self.fact_readings
            .iter()
            .find(|r| r.session == session && r.fact == fact)
            .map(|r| r.whole.as_str())
    }

    /// The fact drawn lit in `session`'s rule, which hides the facts of `off` behind stubs: the
    /// one the keyboard selected, else the one under the pointer where a click on it does
    /// something ([`Fact::clicks`]). The pointer on the folder, the branch or the context reads
    /// its title and lights nothing; the keyboard's selection lights any, the only way to say
    /// where it is.
    pub(crate) fn lit_fact(&self, session: u64, off: FactMask) -> Option<FactKind> {
        let mine = |at: Option<(u64, Target)>| at.filter(|(id, _)| *id == session);
        if let Some((_, target)) = mine(self.selected) {
            return target.fact().and_then(Fact::kind);
        }
        match mine(self.hover) {
            Some((_, Target::Fact(fact))) => fact.kind().filter(|kind| fact.clicks(off.has(*kind))),
            _ => None,
        }
    }

    /// Whether the pointer or the keyboard's selection is on the top rule's key of `session`.
    #[must_use]
    pub(crate) fn top_effort_lit(&self, session: u64) -> bool {
        let at = Some((session, Target::Fact(Fact::TopEffort)));
        self.hover == at || self.selected == at
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
        let light = s.is_some_and(SessionLights::light_drawn);
        let harness = s.is_some_and(SessionLights::harness_chips);
        let team = s.is_some_and(|s| s.team.is_some());
        Chip::row()
            .filter(|chip| match chip {
                Chip::Light(_) => light,
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
        self.shows_uncovered(session, target, |_, _| false)
    }

    /// Whether `target` of `session` is on this frame's glass and not on a span `covered` owns.
    fn shows_uncovered(
        &self,
        session: u64,
        target: Target,
        covered: impl Fn(usize, std::ops::Range<usize>) -> bool,
    ) -> bool {
        match target {
            Target::Chip(chip) => self.hits.iter().any(|h| {
                h.session == session
                    && h.chip == Some(chip)
                    && !covered(h.term_row, h.col..h.col + 1)
            }),
            Target::Fact(fact) => self.fact_hits.iter().any(|h| {
                h.session == session && h.fact == fact && !covered(h.term_row, h.cols.0..h.cols.1)
            }),
            Target::Switch => self
                .border_toggles
                .iter()
                .any(|b| b.session == session && !covered(b.term_row, b.cols.clone())),
        }
    }

    /// Move the keyboard selection (a frame must paint it before it counts as off the glass).
    const fn select(&mut self, selected: Option<(u64, Target)>) {
        self.selected = selected;
        self.selection_painted = false;
    }

    /// Whether the keyboard selection's target is off the glass or on a span `covered_span` / `covered_row` owns.
    fn selection_stale(
        &self,
        covered_row: impl Fn(usize) -> bool,
        covered_span: impl Fn(usize, std::ops::Range<usize>) -> bool,
    ) -> bool {
        let Some((session, target)) = self.selected else {
            return false;
        };
        match target {
            Target::Chip(_) if !self.selection_painted => !self
                .footers
                .iter()
                .any(|(s, term_row, room)| *s == session && *room > 0 && !covered_row(*term_row)),
            _ => !self.shows_uncovered(session, target, covered_span),
        }
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
                    s.word = None;
                }
                Some(crate::autopilot::HarnessSwitch::Off(crate::autopilot::SwitchOff::Agent(
                    agent,
                ))) => {
                    s.autopilot = Some(AutopilotView::AgentOff(agent));
                    s.autopilot_pending = None;
                    s.word = None;
                }
                Some(crate::autopilot::HarnessSwitch::Turning) => {
                    s.autopilot_pending = None;
                    s.word = None;
                }
                None => {
                    s.autopilot = Some(AutopilotView::Unhosted);
                    s.autopilot_pending = None;
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
    pub(crate) fn light_just_pressed(&mut self, session: u64, now: Instant) -> bool {
        self.sessions
            .get_mut(&session)
            .is_some_and(|s| continues_gesture(&mut s.light_pressed, now))
    }

    /// Each cell of `session`'s mode light as the last frame laid it: its frame row, the same
    /// row as a window grid row, and its column.
    pub(crate) fn light_cells(
        &self,
        session: u64,
    ) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
        self.hits
            .iter()
            .filter(move |hit| {
                hit.session == session && matches!(hit.chip, Some(Chip::Light(Light::Mode)))
            })
            .map(|hit| (hit.frame_row, hit.term_row, hit.col))
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
            | (self.top_hover(session) as u64) << 5
            | u64::from(turning == Some(false)) << 4)
            ^ refused.rotate_left(8)
    }

    /// The grid the lists were laid on, for a test to read or to forge.
    #[cfg(test)]
    pub(crate) fn drawn_at_mut(&mut self) -> &mut Option<crate::app_mouse::GlassGrid> {
        &mut self.drawn_at
    }

    /// The window pixel the pointer last hovered the glass at, while it does.
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
    ) {
        let read =
            crate::reader_guard::read_or_none(session, "Claude Code lights", rows, || read(rows));
        self.observe_read(session, read);
    }

    /// Fold a frame's reading of `session`'s rows in ([`Self::observe`]).
    fn observe_read(&mut self, session: u64, read: crate::claude_footer::LightsRead) {
        if let Some(reading) = read {
            self.observe(session, reading);
        }
    }

    /// Fold one frame's reading of `session`'s screen.
    fn observe(&mut self, session: u64, reading: Option<Reading>) {
        let s = self.sessions.entry(session).or_default();
        s.shown.fold(&reading.unwrap_or(Reading::UNKNOWN));
    }

    /// `session`'s standing permission click as this frame reads the App's table, and the job
    /// words its light's title names ([`SessionLights::jobs`]).
    fn note_click(&mut self, session: u64, click: Option<ClickShown>, jobs: String) {
        let s = self.sessions.entry(session).or_default();
        if s.click != click {
            s.click = click;
        }
        if s.jobs != jobs {
            s.jobs = jobs;
        }
    }

    /// Whether `session`'s mode light is under the pointer or the keyboard, its title showing:
    /// the frame then reads the job words the title names ([`SessionLights::jobs`]).
    pub(crate) fn light_titled(&self, session: u64) -> bool {
        let light = Some((session, Target::Chip(Chip::Light(Light::Mode))));
        self.hover == light || self.selected == light
    }

    /// Let every word on show past its time go: a refusal, a fact notice, an Autopilot click's
    /// word.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_permission_click",
            action = "Rest",
            project = "claude_lights::click_tests::project_permission_click"
        )
    )]
    fn expire(&mut self, now: Instant) -> bool {
        let mut any = false;
        for s in self.sessions.values_mut() {
            if s.refused.is_some_and(|(_, _, until)| until <= now) {
                s.refused = None;
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
        any
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
            let drawn = drawn_at(s, keep);
            chips_width_at(s, Chip::row().filter(|c| drawn(*c)), keep, inks, named)
        };
        let Some(keep) = Keep::LADDER
            .into_iter()
            .filter(|keep| *keep != Keep::GlyphOnly || glyph_rung)
            .find(|keep| (1..=room).contains(&width_at(*keep, None)))
        else {
            return false;
        };
        // The rung is chosen at rest: the mode's name a click brought up takes only room the rest
        // leaves, and never a chip's place.
        let named = s
            .word_at(now)
            .filter(|_| named && keep == Keep::Drawn)
            .filter(|_| drawn_at(s, keep)(Chip::Autopilot))
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
        if keep != Keep::Drawn {
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
                    .filter(|_| keep == Keep::Drawn),
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
        let clicked = s.click.map(|_| Light::Mode);
        let fact_noticed = s
            .fact_notice
            .as_ref()
            .filter(|(_, until)| *until > now)
            .map(|(notice, _)| (notice.text(), matches!(notice, FactNotice::Said(_))));
        let drawn = drawn_at(s, keep);
        let titled = [
            focus,
            clicked.map(Chip::Light),
            refused.map(|(l, _, _)| Chip::Light(l)),
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
                if chip == Chip::Autopilot {
                    lay_lean(&words.whole, lean);
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
            None => {
                // A press names the mode before the quiet default status: the name may take
                // this room until it fades, without competing with the continuation caption.
                if named.is_none()
                    && let Some(text) = s.continuation.as_deref()
                {
                    lay(text, title_ink, title);
                    lay(text, title_ink, short_title);
                }
            }
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
            let italic = face.italic;
            // The step of the mode's name a click brought up, fading after the glyph.
            let fading = named
                .filter(|_| chip == Chip::Autopilot)
                .map(|(_, step)| (face.glyph.chars().count(), step));
            for (k, (ch, ink)) in face.cells().enumerate() {
                let mut c = blank;
                c.ch = ch;
                c.fg = ink;
                c.italic = italic;
                // Under the pointer or the keyboard the chip wears the glass's one hover, its
                // blank between glyph and word included; a name a click brought up fades from
                // the hover's ink as it fades from its own at rest, under the same line.
                if focus == Some(chip) {
                    c = hovered_cell(&inks.at.glass, c, HoverPart::Text);
                    if let Some((_, step)) = fading.filter(|(glyphs, _)| k >= *glyphs) {
                        c.fg = ChipWord::ink(step, c.fg, inks.ground);
                    }
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

/// Whether `s`'s block draws a chip at `keep`: the mode light wherever
/// [`SessionLights::light_drawn`], at every rung ([`Keep`]).
fn drawn_at(s: &SessionLights, keep: Keep) -> impl Fn(Chip) -> bool + '_ {
    let harness = s.harness_chips();
    let team = s.team.is_some();
    let light = s.light_drawn();
    move |chip: Chip| {
        keep.keeps(chip)
            && match chip {
                Chip::Autopilot => harness,
                Chip::Team => team,
                Chip::Light(_) => light,
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

/// A chip's title: whole, short, and whether it is a reason (a light's refusal, or what a click's
/// restart waits for or ends), which outranks the facts and keeps its short form in the rule
/// (`footer::fit_rule`). The Autopilot chip's leaner forms are cut from its whole
/// ([`lean_titles`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct TitleText {
    whole: String,
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

/// What `light`'s title says (`docs/DESIGN-yolo-binary-2026-11-04.md` §2): the permission mode in
/// Claude Code's own words, as its pill says it ([`Mode::pill_words`]), then what a click does and
/// what the click's restart would end of Claude's running jobs. On the off chip that is
/// `plan mode on · click: yolo · ends 1 shell`; on the on chip
/// `bypass permissions on · click: settings default · ends 1 shell`, the default named only by
/// the click's box, which reads the settings ([`crate::permission_box`]). A standing click says
/// what its restart waits for, what it ends and that a click takes it back, a refusal its reason.
/// A title that says what a restart waits for or ends is a reason, its short form keeping that
/// clause ([`TitleText::reason`]). Where the host `restarts` nothing
/// ([`crate::permission_click::RESTARTS`]) the title names no click and nothing a restart would
/// end: the mode alone off, the mode and what it means (`bypass permissions on · no prompts`)
/// on.
fn light_title(
    s: &SessionLights,
    light: Light,
    refused: Option<(Light, Refusal, Instant)>,
    restarts: bool,
) -> LightTitle {
    if let Some((l, why, _)) = refused
        && l == light
    {
        let short = why.says().to_owned();
        return LightTitle {
            spoken: sentence(&short),
            text: TitleText {
                whole: short.clone(),
                short,
                reason: true,
            },
        };
    }
    let ends = crate::claude_footer::ends_words(
        &s.jobs,
        aterm_agent::harness::upgrade::RESTART_ENDS_BACKGROUND,
    );
    let ends_said = if ends.is_empty() {
        String::new()
    } else {
        format!(
            " Ends {}.",
            s.jobs
                .replace(aterm_agent::harness::footer::MODE_ROW_SEP, ", ")
        )
    };
    if let Some(click) = s.click {
        return match click.when {
            Some(when) => {
                let short = format!("restarts {when}{ends}");
                LightTitle {
                    spoken: format!(
                        "{}{ends_said} Click to take it back.",
                        sentence(&format!("restarts {when}"))
                    ),
                    text: TitleText {
                        whole: format!("{short}{TITLE_CLICK}take back"),
                        short,
                        reason: true,
                    },
                }
            }
            None => {
                let words = format!("restarting\u{2026}{ends}");
                LightTitle {
                    spoken: format!("Restarting.{ends_said}"),
                    text: TitleText {
                        whole: words.clone(),
                        short: words,
                        reason: !ends.is_empty(),
                    },
                }
            }
        };
    }
    let Some(mode) = s.shown.mode else {
        let words = "not on screen right now";
        return LightTitle {
            spoken: sentence(words),
            text: TitleText {
                whole: words.to_owned(),
                short: words.to_owned(),
                reason: false,
            },
        };
    };
    let head = mode.pill_words();
    if mode == Mode::Bypass && !restarts {
        return LightTitle {
            spoken: format!("{} {}", sentence(head), sentence(YOLO_ON_MEANS)),
            text: TitleText {
                whole: format!(
                    "{head}{}{YOLO_ON_MEANS}",
                    aterm_agent::harness::footer::MODE_ROW_SEP
                ),
                short: head.to_owned(),
                reason: false,
            },
        };
    }
    if !restarts {
        return LightTitle {
            spoken: sentence(head),
            text: TitleText {
                whole: head.to_owned(),
                short: head.to_owned(),
                reason: false,
            },
        };
    }
    // A click on the on chip restarts onto the line with no permission flag, which lands in the
    // settings' default; on the off chip onto the bypass line
    // (`docs/DESIGN-yolo-binary-2026-11-04.md` §3).
    let yolo = aterm_agent::harness::footer::YOLO;
    let (to, click_said) = if mode == Mode::Bypass {
        (
            YOLO_OFF_TO,
            format!("Click to leave {yolo} for the settings' default mode."),
        )
    } else {
        (yolo, format!("Click to turn {yolo} on."))
    };
    LightTitle {
        spoken: format!("{} {click_said}{ends_said}", sentence(head)),
        text: TitleText {
            whole: format!("{head}{TITLE_CLICK}{to}{ends}"),
            short: format!("{head}{ends}"),
            reason: !ends.is_empty(),
        },
    }
}

/// What the on chip's title says bypass means, after the mode's own words, where no click is
/// named.
const YOLO_ON_MEANS: &str = "no prompts";

/// What the on chip's title says a click restarts into, after `click: `.
const YOLO_OFF_TO: &str = "settings default";

/// What `chip`'s title says ([`TitleText`], each form without its trailing gap).
fn title_of(
    s: &SessionLights,
    chip: Chip,
    refused: Option<(Light, Refusal, Instant)>,
) -> TitleText {
    let plain = |(whole, short): (String, String)| TitleText {
        whole,
        short,
        reason: false,
    };
    match chip {
        Chip::Light(light) => {
            light_title(s, light, refused, crate::permission_click::RESTARTS).text
        }
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
    /// Its cells slanted: the mode light's word while its click stands.
    italic: bool,
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
            italic: false,
        }
    }

    /// Whether anything follows the glyph.
    const fn worded(&self) -> bool {
        self.word.is_some() || self.clock.is_some()
    }

    /// The cells it takes: two for a wide character (`✋`, a cjk name). A face with no glyph is
    /// its words alone, no blank before them.
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
        Chip::Light(_) => light_face(s, inks),
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

/// The mode light's face (`docs/DESIGN-yolo-binary-2026-11-04.md` §2): one word, `yolo`, with no
/// mark, its ink saying whether it is on. In bypass the glass's value ink, as any value on the
/// rule (never the letter's hue, the accent or the warning ink: it is a fact, not an alarm); in
/// any other mode the glass's quiet ink, as the other off chips. While a click stands the word
/// keeps its cells and is slanted in the row's own ink, whichever way the click goes and
/// whether or not a bit is read (with none read the light is drawn only while a click stands,
/// [`SessionLights::light_drawn`]), so the chip is one width in every state.
fn light_face(s: &SessionLights, inks: Inks) -> Face<'static> {
    use aterm_agent::harness::footer::YOLO;
    if s.click.is_some() {
        return Face {
            italic: true,
            ..Face::plain("", Some(YOLO), inks.ink)
        };
    }
    let ink = if s.shown.yolo() == Some(true) {
        inks.title
    } else {
        inks.quiet
    };
    Face::plain("", Some(YOLO), ink)
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
            + click.len(),
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
    let short = short_title(&whole).to_owned();
    (whole, short)
}

/// The Autopilot title's leaner forms, between its whole and its short form, widest first:
/// without its trail, then without its lapsed pause too, then its mode phrase and what a click
/// does. A form that gives up nothing its wider neighbour had is empty.
#[must_use]
pub(crate) fn lean_titles(whole: &str) -> [(&str, &str); LEAN_FORMS] {
    let Some(click) = whole.rfind(TITLE_CLICK) else {
        return [("", ""); LEAN_FORMS];
    };
    let tail = &whole[click..];
    let trail_at = whole[..click].find(TITLE_LAST);
    let head_end = trail_at.unwrap_or(click);
    let without_trail = match trail_at {
        Some(at) => (&whole[..at], tail),
        None => ("", ""),
    };
    let pause_at = whole[..head_end].find(TITLE_PAUSE);
    let without_pause = match pause_at {
        Some(at) => (&whole[..at], tail),
        None => ("", ""),
    };
    let click_only = match whole[..pause_at.unwrap_or(head_end)].find(TITLE_EXPLAINS) {
        Some(phrase) => (&whole[..phrase], tail),
        None => ("", ""),
    };
    [without_trail, without_pause, click_only]
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
    /// The mode light's title ([`light_title`]: what the bit means, then `· click: <goal>` and
    /// what a click's restart ends), or a harness chip's title.
    pub(crate) title: Vec<RenderCell>,
    /// The whole title with parts given up, widest first.
    pub(crate) lean: [Vec<RenderCell>; LEAN_FORMS],
    /// The title's head alone (what the light's bit means and what a click's restart ends, a
    /// refusal's reason or `switching…`; a harness chip's short form), for a pane with no room for
    /// the whole title.
    pub(crate) short_title: Vec<RenderCell>,
    /// The title is a reason (a refusal's, or what a click's restart waits for or ends,
    /// [`light_title`]), which outranks the branch and the path in the rule.
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
    /// Synthetic private-dev input through the production lights reading and chip composition:
    /// Autopilot `view`'s chip over Claude Code's mode row `mode` (`  ⏸ plan mode on (shift+tab
    /// to cycle)`), Autopilot's name worded on its chip where `worded`, as a click that just
    /// reached it shows it (`⤓ land ─ yolo`), else its glyph alone. The light beside it says
    /// the bit the row reads, as on a live pane: `yolo` in the value ink on a bypass row, in
    /// the quiet ink on any other. A sample laid under a palette wears the chip of that
    /// palette's mode, so the two agree.
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    pub(crate) fn layout_scene(
        blank: RenderCell,
        at: ChipAt,
        (view, mode, worded): (AutopilotView, &str, bool),
    ) -> Block {
        let mut lights = WindowLights::default();
        let rows = [
            "─".repeat(80),
            "❯ ".to_owned(),
            "─".repeat(80),
            mode.to_owned(),
        ];
        lights.observe(0, lights::read_lights(&rows));
        let now = Instant::now();
        lights.note_autopilot_at(0, view.mode(), view, ChipGlass::default(), || now);
        let s = lights.sessions.entry(0).or_default();
        s.word = view.mode().filter(|_| worded).map(|mode| ChipWord {
            mode,
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
            (Keep::Drawn, named),
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
    /// Fold one Claude Code pane's reading of its screen into its lights, with its tab's
    /// standing permission click as the frame read the App's table ([`Self::claude_click_shown`])
    /// and the job words its light's title names ([`SessionLights::jobs`]).
    pub(crate) fn observe_claude_lights(
        &mut self,
        wid: WindowId,
        session: u64,
        read: crate::claude_footer::LightsRead,
        click: Option<ClickShown>,
        jobs: String,
    ) {
        // While the chip's title shows (under the pointer or the keyboard) a click may follow:
        // each frame drawn then asks the resolver to read the settings' default again, at most
        // once a recheck, and the window thread reads no file. The click uses the last read held.
        if crate::permission_click::RESTARTS
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.claude_lights.light_titled(session))
        {
            self.ask_default_read(session);
        }
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        ws.claude_lights.observe_read(session, read);
        ws.claude_lights.note_click(session, click, jobs);
    }

    /// Session `session`'s standing permission click as a frame draws it.
    pub(crate) fn claude_click_shown(&self, session: u64) -> Option<ClickShown> {
        let entry = self.pool.get(session)?;
        self.permission_clicks
            .clicks
            .shown(entry.ctx.self_id.as_str())
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
    ) {
        let glass = self.chip_glass(session);
        let continuation = view
            .mode()
            .filter(|mode| *mode != Autopilot::Off)
            .map(|_| self.continuation_words(session));
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.claude_lights
                .sessions
                .entry(session)
                .or_default()
                .continuation = continuation;
            ws.claude_lights
                .note_autopilot_at(session, word, view, glass, || {
                    #[cfg(test)]
                    crate::work_counts::harness_clock_read();
                    Instant::now()
                });
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
            .filter_map(|(wid, ws)| {
                let counting = ws.claude_lights.sessions.values().any(|s| {
                    s.continuation
                        .as_deref()
                        .is_some_and(|text| text.starts_with("resumes in "))
                });
                (counting || ws.claude_lights.words_due(now)).then_some(*wid)
            })
            .collect()
    }

    /// The loop's next wake for a mode name a click brought up
    /// (`metrics::DeadlineOwner::ChipWord`).
    pub(crate) fn claude_chip_word_wake(&self, now: Instant) -> Option<Instant> {
        self.windows
            .values()
            .filter_map(|ws| {
                let countdown = ws
                    .claude_lights
                    .sessions
                    .values()
                    .any(|s| {
                        s.continuation
                            .as_deref()
                            .is_some_and(|text| text.starts_with("resumes in "))
                    })
                    .then_some(now + Duration::from_secs(1));
                ws.claude_lights
                    .word_wake(now)
                    .into_iter()
                    .chain(countdown)
                    .min()
            })
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

    /// `session`'s composer rule is on the glass at `term_row` this frame (chip or none, facts or none).
    pub(crate) fn note_claude_footer_row(
        &mut self,
        wid: WindowId,
        session: u64,
        term_row: usize,
        room: usize,
    ) {
        if let Some(ws) = self.windows.get_mut(&wid)
            && !ws
                .claude_lights
                .footers
                .iter()
                .any(|(s, _, _)| *s == session)
        {
            ws.claude_lights.footers.push((session, term_row, room));
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

    /// Whether a floating or pre-frame chrome surface (stats lens, paste/close banner, or find
    /// bar) owns window pixel `(x, y)`.
    pub(crate) fn chrome_overlay_at(&self, wid: WindowId, x: f64, y: f64) -> bool {
        self.stats_lens_at(wid, x, y)
            || self.band_banner_at(wid, x, y).is_some()
            || self.find_bar_cell_at(wid, x, y).is_some()
    }

    /// What of the chips' glass is at frame cell `(row, col)` of the grid the
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
        (!self.chrome_owns_terminal_cell(wid, hit.term_row, hit.col))
            .then_some((hit.session, hit.chip))
    }

    /// The frame cell under window pixel `(x, y)` on the grid the glass's
    /// lists were laid on.
    pub(crate) fn glass_cell_at(&self, wid: WindowId, x: f64, y: f64) -> Option<(usize, usize)> {
        let drawn = self.windows.get(&wid)?.claude_lights.drawn_at;
        self.laid_cell_at(wid, drawn, x, y)
    }

    /// The pointer left the window.
    pub(crate) fn clear_claude_light_hover(&mut self, wid: WindowId) {
        let mut had_fact = false;
        if let Some(ws) = self.windows.get_mut(&wid) {
            let prev_hover = ws.claude_lights.hover.take();
            had_fact = prev_hover.is_some_and(|(_, t)| t.fact().is_some());
            ws.claude_lights.glass_hit_hover = None;
            if prev_hover.is_some()
                | ws.claude_lights.switch_hover.take().is_some()
                | ws.claude_lights.part_hover.take().is_some()
                | ws.claude_lights.quota_column.take().is_some()
                | ws.claude_lights.glass_hover_px.take().is_some()
            {
                redraw(ws);
            }
        }
        if had_fact {
            self.note_fact_readings(wid);
        }
    }

    /// The glass's hover follows the glass, with no motion.
    pub(crate) fn settle_glass_hover(
        &mut self,
        wid: WindowId,
        grid: Option<crate::app_mouse::GlassGrid>,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) {
        let band_covered = self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.claude_lights.band_covered);
        let band_active = band_covered
            && (self.paste_banner.as_ref().is_some_and(|p| p.wid == wid)
                || self.close_band(wid).is_some()
                || self.windows.get(&wid).is_some_and(|ws| {
                    ws.pointer_position_known
                        && self.chrome_overlay_at(wid, ws.last_cursor_px.0, ws.last_cursor_px.1)
                }));
        let focused = self.focused_session_id(wid);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let regridded = std::mem::replace(&mut ws.claude_lights.drawn_at, grid) != grid;
        let chrome = WindowLights::chrome_rows_digest(&ws.chrome_rows, &ws.chrome_spans);
        let chrome_changed =
            std::mem::replace(&mut ws.claude_lights.chrome_rows_laid, chrome) != chrome;
        let uncovered =
            !band_active && std::mem::replace(&mut ws.claude_lights.band_covered, false);
        let card_hits = ws.claude_card.glass_hits_digest();
        let lights = &mut ws.claude_lights;
        let was = lights.glass_hover();
        let laid = (lights.places(card_hits, focused), was);
        // Glass laid elsewhere under the same hover moved under a still pointer (a re-laid rule,
        // a newly drawn ⬆️, a changed chrome band): track it again. A paint under a hover that
        // changed since the last paint (a motion's, or a track's here) only records its places,
        // so glass a hover itself re-lays never moves the hover again.
        let prev = lights.laid.replace(laid);
        let places_changed = prev.is_none_or(|(places, _)| places != laid.0);
        let moved = prev.is_some_and(|(places, hover)| hover == was && places != laid.0);
        if (regridded || moved || chrome_changed || uncovered)
            && ws.pointer_position_known
            && !ws.selecting
        {
            let (x, y) = ws.last_cursor_px;
            let _ = self.track_claude_light_hover(wid, x, y);
            if let Some(ws) = self.windows.get_mut(&wid) {
                let lights = &mut ws.claude_lights;
                lights.cursor_owed |= lights.glass_hover() != was;
            }
            self.evict_stale_glass_selection(wid, Some(plan));
        } else if regridded || places_changed || chrome_changed {
            self.evict_stale_glass_hover_in_plan(wid, Some(plan));
        }
    }

    /// Record that a paste or close confirmation band covered the top rows in `wid`.
    pub(crate) fn note_band_covered(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.claude_lights.band_covered = true;
        }
    }

    /// Clear any keyboard selection in `wid` whose target moved away or was covered by a chrome band.
    fn evict_stale_glass_selection(
        &mut self,
        wid: WindowId,
        plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) {
        let Some(needs_pane_span) = self.windows.get(&wid).and_then(|ws| {
            ws.claude_lights.selected.map(|(_, target)| {
                matches!(target, Target::Chip(_))
                    && !ws.claude_lights.selection_painted
                    && !ws.chrome_spans.is_empty()
            })
        }) else {
            return;
        };
        let pane_span = if needs_pane_span {
            let (_, pane_col, _, pane_cols) = plan.map_or_else(
                || self.focused_pane_rect(wid),
                |plan| self.focused_pane_rect_from_plan(wid, plan),
            );
            usize::from(pane_col)..usize::from(pane_col.saturating_add(pane_cols)).max(1)
        } else {
            0..1
        };
        let mut had_fact = false;
        if let Some(ws) = self.windows.get_mut(&wid) {
            let stale = {
                let ws_ref = &*ws;
                ws_ref.claude_lights.selection_stale(
                    |row| {
                        crate::app_render::chrome_owns_terminal_span_of(
                            ws_ref,
                            row,
                            pane_span.clone(),
                        )
                    },
                    |row, cols| crate::app_render::chrome_owns_terminal_span_of(ws_ref, row, cols),
                )
            };
            if stale {
                had_fact = ws
                    .claude_lights
                    .selected
                    .is_some_and(|(_, t)| t.fact().is_some());
                ws.claude_lights.select(None);
                redraw(ws);
            }
        }
        if had_fact {
            self.note_fact_readings(wid);
        }
    }

    /// Clear any glass hover in `wid` whose target moved away or was covered by a chrome band.
    pub(crate) fn evict_stale_glass_hover(&mut self, wid: WindowId) {
        self.evict_stale_glass_hover_in_plan(wid, None);
    }

    fn evict_stale_glass_hover_in_plan(
        &mut self,
        wid: WindowId,
        plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) {
        self.evict_stale_glass_selection(wid, plan);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if WindowLights::chrome_rows_digest(&ws.chrome_rows, &ws.chrome_spans)
            != ws.claude_lights.chrome_rows_laid
        {
            ws.claude_lights.band_covered = true;
        }
        let Some((x, y)) = ws.claude_lights.glass_hover_px else {
            return;
        };
        let (at, column, glass_hit) = if self.chrome_overlay_at(wid, x, y) {
            self.note_band_covered(wid);
            (None, None, None)
        } else {
            let at = self.claude_glass_at(wid, x, y);
            let column = at
                .filter(|(_, g)| matches!(g, GlassAt::Fact(Fact::Account, _)))
                .and_then(|_| self.quota_column_at(wid, x, y));
            let glass_hit = self.glass_hit_hover_at(wid, x, y);
            (at, column, glass_hit)
        };
        let over = at.and_then(|(session, glass)| match glass {
            GlassAt::Chip(chip) => Some((session, Target::Chip(chip?))),
            GlassAt::Fact(fact, _) => Some((session, Target::Fact(fact))),
            GlassAt::Switch(_) | GlassAt::Mark | GlassAt::Upgrade | GlassAt::Gap => None,
        });
        let switch =
            at.and_then(|(session, glass)| matches!(glass, GlassAt::Switch(_)).then_some(session));
        let part = at.and_then(|(session, glass)| Some((session, part_of(glass)?)));
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let lights = &mut ws.claude_lights;
        let mut gone = false;
        let mut cursor_gone = false;
        let mut fact_gone = false;
        if lights.hover.is_some() && lights.hover != over {
            fact_gone = lights.hover.is_some_and(|(_, t)| t.fact().is_some());
            lights.hover = None;
            lights.quota_column = None;
            gone = true;
        } else if lights.quota_column.is_some() && lights.quota_column != column {
            fact_gone = true;
            lights.quota_column = column;
            gone = true;
        }
        if lights.switch_hover.is_some() && lights.switch_hover != switch {
            lights.switch_hover = None;
            gone = true;
        }
        if lights.part_hover.is_some() && lights.part_hover != part {
            lights.part_hover = None;
            gone = true;
        }
        if lights.glass_hit_hover.is_some() && lights.glass_hit_hover != glass_hit {
            lights.glass_hit_hover = glass_hit;
            cursor_gone = true;
        }
        if lights.hover.is_none()
            && lights.switch_hover.is_none()
            && lights.part_hover.is_none()
            && lights.glass_hit_hover.is_none()
        {
            lights.glass_hover_px = None;
        }
        if gone || cursor_gone {
            lights.cursor_owed = true;
        }
        if gone {
            redraw(ws);
            if fact_gone {
                self.note_fact_readings(wid);
            }
        }
    }

    fn glass_hit_hover_at(&self, wid: WindowId, x: f64, y: f64) -> Option<(u64, bool)> {
        let (session, _) = self.claude_glass_hit_at(wid, x, y)?;
        Some((session, self.session_is_front(wid, session)))
    }

    /// Whether a press on `session`'s mark in `wid` would open the site under the modifiers
    /// held now: with the link modifier, or with none in the pane in front
    /// ([`Self::press_claude_glass_hit`]).
    pub(crate) fn mark_press_opens(&self, wid: WindowId, session: u64) -> bool {
        let mods = self.windows.get(&wid).map(|ws| ws.mods).unwrap_or_default();
        crate::app_mouse::link_modifier_held(mods)
            || (mods.is_empty() && self.session_is_front(wid, session))
    }

    /// Whether the pointer rests on a top line's mark in `wid` ([`GlassAt::Mark`]).
    pub(crate) fn pointer_on_top_mark(&self, wid: WindowId) -> bool {
        let Some((x, y)) = self.windows.get(&wid).map(|ws| ws.last_cursor_px) else {
            return false;
        };
        matches!(self.claude_glass_at(wid, x, y), Some((_, GlassAt::Mark)))
    }

    /// The cursor follows the glass's hover.
    #[cfg(test)]
    pub(crate) fn refresh_glass_cursor_for_frame(&mut self, wid: WindowId) {
        self.refresh_glass_cursor_for_frame_with_plan(wid, None);
    }

    /// The cursor follows the glass's hover, reusing an already-built visible plan when available.
    pub(crate) fn refresh_glass_cursor_for_frame_with_plan(
        &mut self,
        wid: WindowId,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) {
        let owed = self.windows.get_mut(&wid).is_some_and(|ws| {
            std::mem::take(&mut ws.claude_lights.cursor_owed) && ws.pointer_position_known
        });
        if owed {
            self.update_hover_cursor_from_plan(wid, frame_plan);
        }
    }

    /// The pointer on `at`.
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn hover_claude_chip(&mut self, wid: WindowId, at: Option<(u64, Chip)>) {
        let mut had_fact = false;
        if let Some(ws) = self.windows.get_mut(&wid) {
            had_fact = ws
                .claude_lights
                .hover
                .is_some_and(|(_, t)| t.fact().is_some());
            ws.claude_lights.hover = at.map(|(session, chip)| (session, Target::Chip(chip)));
            ws.claude_lights.quota_column = None;
            ws.claude_lights.glass_hover_px = None;
        }
        if had_fact {
            self.note_fact_readings(wid);
        }
    }

    /// The pointer on `at`.
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn hover_claude_fact(&mut self, wid: WindowId, at: Option<(u64, Fact)>) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.claude_lights.hover = at.map(|(session, fact)| (session, Target::Fact(fact)));
            ws.claude_lights.quota_column = None;
            ws.claude_lights.glass_hover_px = None;
        }
        self.note_fact_readings(wid);
    }

    /// A mouse press anywhere lets a keyboard selection go.
    pub(crate) fn clear_claude_light_selection(&mut self, wid: WindowId) {
        let mut had_fact = false;
        if let Some(ws) = self.windows.get_mut(&wid)
            && let Some((_, target)) = ws.claude_lights.selected.take()
        {
            had_fact = target.fact().is_some();
            redraw(ws);
        }
        if had_fact {
            self.note_fact_readings(wid);
        }
    }

    /// Pointer motion: update hover state and return [`OverChips`].
    pub(crate) fn track_claude_light_hover(&mut self, wid: WindowId, x: f64, y: f64) -> OverChips {
        let at = self.claude_glass_at(wid, x, y);
        let over = at.and_then(|(session, glass)| match glass {
            GlassAt::Chip(chip) => Some((session, Target::Chip(chip?))),
            GlassAt::Fact(fact, _) => Some((session, Target::Fact(fact))),
            GlassAt::Switch(_) | GlassAt::Mark | GlassAt::Upgrade | GlassAt::Gap => None,
        });
        let switch =
            at.and_then(|(session, glass)| matches!(glass, GlassAt::Switch(_)).then_some(session));
        let part = at.and_then(|(session, glass)| Some((session, part_of(glass)?)));
        let column = over
            .filter(|(_, t)| *t == Target::Fact(Fact::Account))
            .and_then(|_| self.quota_column_at(wid, x, y));
        let glass_hit = self.glass_hit_hover_at(wid, x, y);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return OverChips::No;
        };
        if ws.selecting {
            return OverChips::No;
        }
        ws.claude_lights.glass_hover_px =
            (over.is_some() || switch.is_some() || part.is_some() || glass_hit.is_some())
                .then_some((x, y));
        ws.claude_lights.glass_hit_hover = glass_hit;
        if ws.claude_lights.hover != over
            || ws.claude_lights.switch_hover != switch
            || ws.claude_lights.part_hover != part
            || ws.claude_lights.quota_column != column
        {
            let had_fact = ws
                .claude_lights
                .hover
                .is_some_and(|(_, t)| t.fact().is_some());
            ws.claude_lights.hover = over;
            ws.claude_lights.quota_column = column;
            ws.claude_lights.switch_hover = switch;
            ws.claude_lights.part_hover = part;
            redraw(ws);
            if had_fact || over.is_some_and(|(_, t)| t.fact().is_some()) {
                self.note_fact_readings(wid);
            }
        }
        OverChips::of_glass(at.as_ref())
    }

    /// What of the harness's glass is under window pixel `(x, y)`.
    pub(crate) fn claude_glass_at(&self, wid: WindowId, x: f64, y: f64) -> Option<(u64, GlassAt)> {
        let ws = self.windows.get(&wid)?;
        let lights = &ws.claude_lights;
        if lights.hits.is_empty() && lights.border_toggles.is_empty() && lights.fact_hits.is_empty()
        {
            return None;
        }
        let (row, col) = self.glass_cell_at(wid, x, y)?;
        let hit = if let Some((session, chip)) = self.chips_at_cell(wid, (row, col)) {
            Some((session, GlassAt::Chip(chip)))
        } else if let Some(b) = lights.border_toggles.iter().find(|b| {
            b.frame_row == row
                && b.arrow.as_ref().is_some_and(|cols| cols.contains(&col))
                && !self.chrome_owns_terminal_cell(wid, b.term_row, col)
        }) {
            Some((b.session, GlassAt::Upgrade))
        } else if let Some(b) = lights.border_toggles.iter().find(|b| {
            b.frame_row == row
                && b.ink.contains(&col)
                && !self.chrome_owns_terminal_cell(wid, b.term_row, col)
        }) {
            Some((b.session, GlassAt::Switch(b.face)))
        } else if let Some(b) = lights.border_toggles.iter().find(|b| {
            b.frame_row == row
                && b.cols.contains(&col)
                && !self.chrome_owns_terminal_cell(wid, b.term_row, col)
        }) {
            // The switch's side blanks: its own cells, outside what a click turns.
            Some((b.session, GlassAt::Gap))
        } else if let Some(b) = lights.border_toggles.iter().find(|b| {
            b.frame_row == row
                && b.mark.as_ref().is_some_and(|cols| cols.contains(&col))
                && !self.chrome_owns_terminal_cell(wid, b.term_row, col)
        }) {
            Some((b.session, GlassAt::Mark))
        } else {
            lights
                .fact_hits
                .iter()
                .find(|h| {
                    h.frame_row == row
                        && (h.cols.0..h.cols.1).contains(&col)
                        && !self.chrome_owns_terminal_cell(wid, h.term_row, col)
                })
                .map(|h| (h.session, GlassAt::Fact(h.fact, h.restores)))
        }?;
        (!self.chrome_overlay_at(wid, x, y)).then_some(hit)
    }

    /// The quota row's chart column under window pixel `(x, y)` ([`RowPart::Chart`]): its
    /// session, the chart's cells a day and the cell.
    fn quota_column_at(&self, wid: WindowId, x: f64, y: f64) -> Option<(u64, u8, usize)> {
        let ws = self.windows.get(&wid)?;
        if ws.claude_lights.fact_hits.is_empty() {
            return None;
        }
        let (row, col) = self.glass_cell_at(wid, x, y)?;
        let hit = ws
            .claude_lights
            .fact_hits
            .iter()
            .find(|h| {
                h.frame_row == row
                    && (h.cols.0..h.cols.1).contains(&col)
                    && !self.chrome_owns_terminal_cell(wid, h.term_row, col)
            })
            .and_then(|h| match h.part {
                Some(RowPart::Chart(per_day)) => Some((h.session, per_day, col - h.cols.0)),
                _ => None,
            })?;
        (!self.chrome_overlay_at(wid, x, y)).then_some(hit)
    }

    /// Build again every fact title someone reads in window `wid` (hovered or selected). The
    /// pointer on a column of the quota row's chart reads that column's title
    /// ([`App::quota_column_title`]), the row's own where the column names no time.
    pub(crate) fn note_fact_readings(&mut self, wid: WindowId) {
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        let focus: Vec<(u64, Fact)> = [ws.claude_lights.selected, ws.claude_lights.hover]
            .into_iter()
            .flatten()
            .filter_map(|(session, t)| Some((session, t.fact()?)))
            .collect();
        let (hover, column) = (ws.claude_lights.hover, ws.claude_lights.quota_column);
        let built: Vec<(u64, Fact, (String, String))> = focus
            .into_iter()
            .map(|(session, fact)| {
                let (whole, short) = self.fact_title(session, fact);
                let whole = column
                    .filter(|&(s, ..)| {
                        s == session && hover == Some((session, Target::Fact(Fact::Account)))
                    })
                    .filter(|_| fact == Fact::Account)
                    .and_then(|(_, per_day, cell)| self.quota_column_title(session, per_day, cell))
                    .unwrap_or(whole);
                (session, fact, (whole, short))
            })
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
            let swallowed = self.glass_swallows_at(wid, x, y);
            if swallowed && self.inline_rename_edit(wid).is_some() {
                self.clear_strip_press(wid);
                self.settle_rename_edit(wid);
            }
            return swallowed;
        };
        let secondary = cfg!(target_os = "macos")
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.mods.control_key());
        match glass {
            // A secondary click (control-click on macOS) is a right-click: on the permission
            // chip it opens the chip's box and makes no click.
            GlassAt::Chip(Some(Chip::Light(_))) if secondary => {
                if self.inline_rename_edit(wid).is_some() {
                    self.clear_strip_press(wid);
                    self.settle_rename_edit(wid);
                }
                self.explain_permission(wid, session);
                true
            }
            // On another chip, the switch or the ⬆️ it turns nothing, opens nothing and asks
            // for no move, as a right-click there does.
            GlassAt::Chip(_) | GlassAt::Switch(_) | GlassAt::Upgrade if secondary => {
                if self.inline_rename_edit(wid).is_some() {
                    self.clear_strip_press(wid);
                    self.settle_rename_edit(wid);
                }
                true
            }
            GlassAt::Switch(face) => {
                self.press_switch_as_clicked(wid, session, face, at);
                true
            }
            GlassAt::Chip(chip) => self.press_visible_claude_chip(wid, session, chip, at),
            GlassAt::Upgrade => self.press_upgrade_arrow(wid, session),
            GlassAt::Fact(..) if secondary => {
                if self.inline_rename_edit(wid).is_some() {
                    self.clear_strip_press(wid);
                    self.settle_rename_edit(wid);
                }
                self.open_fact_menu_at(wid, x, y);
                true
            }
            // The folder, the branch and the context have nothing to click: the press is
            // swallowed, as a gap's is.
            GlassAt::Fact(fact, restores) if !fact.clicks(!restores.is_empty()) => {
                if self.inline_rename_edit(wid).is_some() {
                    self.clear_strip_press(wid);
                    self.settle_rename_edit(wid);
                }
                true
            }
            GlassAt::Fact(fact, restores) => {
                self.press_visible_claude_fact(wid, session, fact, restores)
            }
            // A gap is nothing: its press, and its release, go nowhere.
            GlassAt::Gap => {
                if self.inline_rename_edit(wid).is_some() {
                    self.clear_strip_press(wid);
                    self.settle_rename_edit(wid);
                }
                true
            }
            // The mark takes every press on it: a primary press opens the site where a card
            // line's mark would, and any other settles a tab name being edited and goes nowhere.
            GlassAt::Mark => {
                let url = aterm_types::identity::SITE_URL;
                if (secondary || !self.press_claude_glass_hit(wid, session, url, at))
                    && self.inline_rename_edit(wid).is_some()
                {
                    self.clear_strip_press(wid);
                    self.settle_rename_edit(wid);
                }
                true
            }
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
                        && b.arrow.as_ref().is_some_and(|cols| {
                            !self.chrome_owns_terminal_span(wid, b.term_row, cols.clone())
                        })
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
                    && !self.chrome_owns_terminal_span(wid, hit.term_row, hit.cols.0..hit.cols.1)
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

    /// A left press on one of `session`'s marks on the glass: the card line's
    /// ([`Self::claude_glass_hit_at`]) or the top line's ([`GlassAt::Mark`]), `url` its link.
    ///
    /// A plain press (no modifier held) in the focused pane opens the link, as a link-modifier
    /// press does in any pane, once per double click; in an unfocused pane a plain press is that
    /// pane's focus click and opens nothing, and a press with another modifier (Shift extending
    /// a selection, Option taking one from a tracking app) is the rows' own. A press the mark
    /// takes settles a tab name being edited first.
    fn press_claude_glass_hit(
        &mut self,
        wid: WindowId,
        session: u64,
        url: &'static str,
        at: Instant,
    ) -> bool {
        let Some(mods) = self.windows.get(&wid).map(|ws| ws.mods) else {
            return false;
        };
        let modifier = crate::app_mouse::link_modifier_held(mods);
        let front = self.session_is_front(wid, session);
        let opens = Some(url)
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
                    && !self.chrome_owns_terminal_cell(wid, hit.term_row, hit.col)
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
    /// the one before, stands one click, never a click and its take-back.
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
                self.click_permission(wid, session, light);
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
        // Under a switch holding the harness off no chip is drawn where the top rule's switch
        // stands (it is the one way back, `App::pane_bare`); where it cannot stand (a program
        // the timeline does not name) the chip is the way back and presses the switch; with no
        // host the chip opens the page.
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

    /// A click on `session`'s permission light `light` (`docs/DESIGN-yolo-binary-2026-11-04.md`
    /// §2, §3): it types nothing. Where the tab's click stands it takes it back, until its
    /// restart is signalled (after that it does nothing). Else, on the off chip, it stands a
    /// click asking bypass, whose lane restarts the tab's conversation on the line with
    /// `--dangerously-skip-permissions` when it may; on the on chip it stands a click asking the
    /// line with no permission flag, which lands in the settings' default mode, unless that
    /// default is itself yolo, when it stands nothing; nor does it stand one on the off chip
    /// where the settings turn bypass off ([`Self::explain_unchanged`]).
    /// Either way the chip's box opens saying the mode and what the click did
    /// ([`crate::permission_box`]). A pill it cannot read, or a conversation not read, refuses
    /// it. On a host with no restart ([`crate::permission_click::RESTARTS`]) it only opens the
    /// box.
    pub(crate) fn click_permission(&mut self, wid: WindowId, session: u64, light: Light) {
        if !crate::permission_click::RESTARTS {
            self.explain_permission(wid, session);
            return;
        }
        let Some(tab) = self
            .pool
            .get(session)
            .map(|entry| entry.ctx.self_id.as_str().to_owned())
        else {
            return;
        };
        if self.permission_clicks.clicks.get(&tab).is_some() {
            self.take_back_click(wid, session, &tab);
            return;
        }
        let refuse = |ws: &mut crate::WindowState, why: Refusal| {
            let now = Instant::now();
            ws.claude_lights
                .sessions
                .entry(session)
                .or_default()
                .refused = Some((light, why, now + REFUSAL_SHOWN));
            redraw(ws);
            wake_after(session, REFUSAL_SHOWN);
        };
        let Some(pill) = self.permission_pill(session) else {
            if let Some(ws) = self.windows.get_mut(&wid) {
                refuse(ws, Refusal::NotShown);
            }
            return;
        };
        use crate::permission_box::Told;
        let Some(default) = self.held_permission_default(&tab) else {
            // Before the resolver's first read of this agent nothing says what the restart
            // would change: ask for the read and let the next click decide on it.
            self.ask_default_read(session);
            if let Some(ws) = self.windows.get_mut(&wid) {
                refuse(ws, Refusal::SettingsUnread);
            }
            return;
        };
        let (told, stands) = crate::permission_box::click_told(pill, default);
        if !stands {
            self.explain_unchanged(wid, session, &told);
            return;
        }
        let Some(conversation) = crate::harness_host::published_snapshot(&tab)
            .and_then(|snap| snap.session)
            .or_else(|| self.footer_conversation(session))
        else {
            if let Some(ws) = self.windows.get_mut(&wid) {
                refuse(ws, Refusal::Unread);
            }
            return;
        };
        self.stand_click(wid, session, &tab, pill, conversation);
        debug_assert!(matches!(told, Told::TurningOn | Told::Leaving(_)));
        self.open_permission_box(wid, session, &told);
    }

    /// The permission mode `session`'s pill reads now, straight off its engine; none on a rewound
    /// pane or an unfinished frame.
    pub(crate) fn permission_pill(&self, session: u64) -> Option<Mode> {
        if self.pool.is_rewound(session) {
            return None;
        }
        self.claude_screen_now(session)
            .and_then(|screen| screen.mode)
    }

    /// A click that stands nothing because a restart would change nothing (the settings'
    /// default is yolo, or they turn bypass off): the chip's box says so, `told`, naming the
    /// settings document, and no refusal is said.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_permission_click",
            action = "Explain",
            project = "claude_lights::click_tests::project_permission_click"
        )
    )]
    fn explain_unchanged(
        &mut self,
        wid: WindowId,
        session: u64,
        told: &crate::permission_box::Told,
    ) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            let s = ws.claude_lights.sessions.entry(session).or_default();
            s.refused = None;
        }
        self.open_permission_box(wid, session, told);
        self.redraw_click(session);
    }

    /// Stand tab `tab`'s click from pill `pill` on `conversation`, asking the other line (bypass
    /// from any other mode, the settings' from bypass), and start its lane.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_permission_click",
            action = "Click",
            project = "claude_lights::click_tests::project_permission_click"
        )
    )]
    fn stand_click(
        &mut self,
        wid: WindowId,
        session: u64,
        tab: &str,
        pill: Mode,
        conversation: String,
    ) {
        use aterm_agent::supervise::policy::turn_end::PermissionTo;
        let from_yolo = pill == Mode::Bypass;
        let click = aterm_agent::harness::permission_click::Click {
            tab: tab.to_owned(),
            conversation,
            to: if from_yolo {
                PermissionTo::Settings
            } else {
                PermissionTo::Bypass
            },
            at: aterm_time::unix_secs()
                .and_then(|now| i64::try_from(now).ok())
                .unwrap_or(0),
        };
        aterm_log::info!(
            "harness @{tab}: a permission click stands: {} asks {}",
            said_bit(from_yolo),
            click.to.word()
        );
        self.permission_clicks.clicks.stand(click, from_yolo);
        if let Some(ws) = self.windows.get_mut(&wid) {
            let s = ws.claude_lights.sessions.entry(session).or_default();
            s.refused = None;
        }
        self.start_click_lane(tab);
        self.redraw_click(session);
    }

    /// A click on tab `tab`'s standing click: taken back until its restart is signalled
    /// ([`crate::permission_click::Clicks::take_back`]).
    fn take_back_click(&mut self, wid: WindowId, session: u64, tab: &str) {
        if let Some(taken) = self.permission_clicks.clicks.take_back(tab) {
            aterm_log::info!(
                "harness @{tab}: the permission click on {} is taken back",
                said_bit(taken.from_yolo)
            );
        }
        self.refresh_permission_box(session);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.claude_lights
                .sessions
                .entry(session)
                .or_default()
                .refused = None;
        }
        self.redraw_click(session);
    }

    /// Redraw every window that draws `session`'s lights.
    fn redraw_click(&mut self, session: u64) {
        for ws in self.windows.values_mut() {
            if ws.claude_lights.sessions.contains_key(&session) {
                redraw(ws);
            }
        }
        crate::claude_footer::post_changed(session);
    }

    /// What a look of tab `tab`'s click lane found, once it moved the ask
    /// (`Wake::PermissionClick`, or [`App::step_click`]): a refusal or a failure said on its light,
    /// the chip's open box brought to what now holds ([`App::refresh_permission_box`]), and
    /// every look's change drawn.
    pub(crate) fn apply_click_look(
        &mut self,
        tab: &str,
        look: aterm_agent::harness::permission_click::Look,
    ) {
        use aterm_agent::harness::permission_click::Look;
        let session = self
            .store
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .by_sid(&aterm_session::SessionId::new(tab))
            .map(|h| h.local_id);
        let Some(session) = session else {
            return;
        };
        match look {
            Look::Refused(why) => {
                self.say_click_refusal(session, Refusal::Click(ClickRefusal::of(&why)))
            }
            Look::Failed(_) => self.say_click_refusal(session, Refusal::Failed),
            Look::Waits(_) | Look::InFlight(_) | Look::Done | Look::Ended(_) | Look::Moved(_) => {}
        }
        // The chip's box says what its click is doing: an ask that ended or was signalled
        // changes that.
        self.refresh_permission_box(session);
        self.redraw_click(session);
    }

    /// Say `why` on `session`'s permission light, in every window that draws it.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_permission_click",
            action = "Refuse",
            project = "claude_lights::click_tests::project_permission_click"
        )
    )]
    fn say_click_refusal(&mut self, session: u64, why: Refusal) {
        let until = Instant::now() + REFUSAL_SHOWN;
        for ws in self.windows.values_mut() {
            if let Some(s) = ws.claude_lights.sessions.get_mut(&session) {
                s.refused = Some((Light::Mode, why, until));
            }
        }
        wake_after(session, REFUSAL_SHOWN);
    }

    /// The conversation Claude Code's own registry names for `session`'s program in front, as its
    /// footer facts were read ([`aterm_agent::harness::footer::FactsOwner`]). A click keys on it
    /// where the host published no snapshot: a tab whose agent started under Off, which no
    /// harness supervises. The lane's own look reads the agent again before anything is taken.
    fn footer_conversation(&self, session: u64) -> Option<String> {
        let entry = self.pool.get(session)?;
        let timeline = entry
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        timeline
            .claude_footer()?
            .owner
            .as_ref()
            .map(|owner| owner.session_id.clone())
    }

    /// `session`'s screen read straight from its engine (behind the reader's panic fence), with
    /// the window title Claude Code set over it: under a status line its `◐`/`◑` is the only sign
    /// that a reply still being drawn is a turn running.
    fn claude_screen_now(&self, session: u64) -> Option<Screen> {
        let (rows, title) = self.claude_rows_now(session)?;
        fenced_screen_now(session, &rows, || lights::read_screen(&rows, Some(&title)))
    }

    /// `session`'s live screen copied out of its engine, its rows as text, and the program's
    /// window title read in the same hold.
    /// `None` while Claude's DEC 2026 frame is unfinished: the event loop never waits for its close.
    fn claude_rows_now(&self, session: u64) -> Option<(Vec<String>, std::sync::Arc<str>)> {
        let entry = self.pool.get(session)?;
        let t = crate::term_lock(&entry.term);
        if t.grid().display_offset() > 0 || crate::frame_unfinished_now(&t) {
            return None;
        }
        Some((
            (0..usize::from(t.rows()))
                .map(|r| row_text(&t, r))
                .collect(),
            t.title_arc(),
        ))
    }

    /// The footer wake's lights half.
    pub(crate) fn drain_claude_lights(&mut self) {
        let now = Instant::now();
        self.advance_slash_presses(now);
        for ws in self.windows.values_mut() {
            if ws.claude_lights.expire(now) {
                redraw(ws);
            }
        }
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
        let has_chrome_spans = !ws.chrome_spans.is_empty();
        let Some(session) = self.focused_session_id(wid) else {
            return false;
        };
        let pane_span = if has_chrome_spans {
            let (_, pane_col, _, pane_cols) = self.focused_pane_rect(wid);
            usize::from(pane_col)..usize::from(pane_col.saturating_add(pane_cols)).max(1)
        } else {
            0..1
        };
        let now = Instant::now();
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        let room = ws
            .claude_lights
            .footers
            .iter()
            .find(|(id, term_row, _)| {
                *id == session
                    && !crate::app_render::chrome_owns_terminal_span_of(
                        ws,
                        *term_row,
                        pane_span.clone(),
                    )
            })
            .map(|(_, _, room)| *room);
        let stale = {
            let ws_ref = &*ws;
            ws_ref.claude_lights.selected.is_some_and(|(id, target)| {
                id != session
                    || (matches!(target, Target::Chip(_)) && room.is_none())
                    || ((ws_ref.claude_lights.selection_painted
                        || !matches!(target, Target::Chip(_)))
                        && !ws_ref
                            .claude_lights
                            .shows_uncovered(id, target, |row, cols| {
                                crate::app_render::chrome_owns_terminal_span_of(ws_ref, row, cols)
                            }))
            })
        };
        let stale_had_fact = stale
            && ws
                .claude_lights
                .selected
                .is_some_and(|(_, target)| target.fact().is_some());
        if stale {
            ws.claude_lights.select(None);
            redraw(ws);
        }
        let order = ws.claude_lights.revealable(session);
        let ws_ref = &*ws;
        let lights_ = &ws_ref.claude_lights;
        let covered = |row: usize, cols: std::ops::Range<usize>| {
            crate::app_render::chrome_owns_terminal_span_of(ws_ref, row, cols)
        };
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
            let had_fact = stale_had_fact || selected.is_some_and(|t| t.fact().is_some());
            if let Some(next) = step(selected) {
                ws.claude_lights.select(Some((session, next)));
                redraw(ws);
                if had_fact || next.fact().is_some() {
                    self.note_fact_readings(wid);
                }
            } else if had_fact {
                self.note_fact_readings(wid);
            }
            return true;
        }
        let Some(target) = selected else {
            if stale_had_fact {
                self.note_fact_readings(wid);
            }
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
                let ws_ref = &*ws;
                let face = ws_ref
                    .claude_lights
                    .border_toggles
                    .iter()
                    .find(|b| {
                        b.session == session
                            && !crate::app_render::chrome_owns_terminal_span_of(
                                ws_ref,
                                b.term_row,
                                b.cols.clone(),
                            )
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
                let ws_ref = &*ws;
                let hit = ws_ref
                    .claude_lights
                    .fact_hits
                    .iter()
                    .filter(|h| {
                        h.session == session
                            && h.fact == fact
                            && !crate::app_render::chrome_owns_terminal_span_of(
                                ws_ref,
                                h.term_row,
                                h.cols.0..h.cols.1,
                            )
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
                self.note_fact_readings(wid);
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
        match &ev.logical_key {
            Key::Named(NamedKey::Enter | NamedKey::Space) if plain => {
                ws.claude_lights.select(None);
                redraw(ws);
                if self.inline_rename_edit(wid).is_some() {
                    self.clear_strip_press(wid);
                    self.settle_rename_edit(wid);
                }
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
            if self.chrome_owns_terminal_cell(wid, hit.term_row, hit.col) {
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
            if self.chrome_owns_terminal_span(wid, b.term_row, b.cols.clone()) {
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
                | HarnessChip::Mark
                | HarnessChip::CardMark
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
            if self.chrome_owns_terminal_span(wid, hit.term_row, hit.cols.0..hit.cols.1) {
                continue;
            }
            // Line E is one chip over its parts, the row's words its name (glass design §2.6).
            if hit.part.is_some() {
                let parts = ws.claude_lights.fact_hits.iter().filter(|h| {
                    h.part.is_some() && h.session == hit.session && h.frame_row == hit.frame_row
                });
                if parts.clone().any(|h| h.cols.0 < hit.cols.0) {
                    continue;
                }
                let end = parts.map(|h| h.cols.1).max().unwrap_or(hit.cols.1);
                let (_, description) = self.fact_spoken(hit.session, hit.fact, hit.restores);
                let label = self
                    .shown_facts(hit.session)
                    .and_then(|(facts, _)| Some(facts.quota_view()?.spoken.clone()))
                    .unwrap_or_default();
                chips.push(GridChip {
                    session: hit.session,
                    chip: HarnessChip::of_fact(hit.fact),
                    label,
                    description: (!description.is_empty()).then_some(description),
                    toggled: None,
                    row,
                    col: hit.cols.0,
                    width: end.saturating_sub(hit.cols.0),
                });
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
            if self.chrome_owns_terminal_span(wid, title.term_row, title.cols.clone()) {
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
                            b.session == session
                                && !self.chrome_owns_terminal_span(wid, b.term_row, b.cols.clone())
                        })
                        .map(|b| b.face)
                });
                if let Some(face) = shown {
                    self.press_switch_as_clicked(wid, session, face, Instant::now());
                }
                return;
            }
            HarnessChip::Title => return,
            HarnessChip::CardMark => {
                self.open_claude_link_for_reader(wid, session);
                return;
            }
            HarnessChip::Mark => {
                let shown = self.windows.get(&wid).is_some_and(|ws| {
                    ws.claude_lights.border_toggles.iter().any(|b| {
                        b.session == session
                            && b.mark.as_ref().is_some_and(|cols| {
                                !self.chrome_owns_terminal_span(wid, b.term_row, cols.clone())
                            })
                    })
                });
                let url = aterm_types::identity::SITE_URL;
                if shown && crate::is_safe_url(url) {
                    if self.inline_rename_edit(wid).is_some() {
                        self.clear_strip_press(wid);
                        self.settle_rename_edit(wid);
                    }
                    crate::app_mouse::open_url_external(url);
                }
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
                        .find(|h| {
                            h.session == session
                                && h.fact == fact
                                && !self.chrome_owns_terminal_span(
                                    wid,
                                    h.term_row,
                                    h.cols.0..h.cols.1,
                                )
                        })
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
}

/// The bit in words, as a log line and a screen reader say it (`yolo on`, `yolo off`): the chip
/// shows it by its ink alone.
pub(crate) const fn said_bit(yolo: bool) -> &'static str {
    if yolo {
        aterm_agent::harness::footer::YOLO_ON_SAID
    } else {
        aterm_agent::harness::footer::YOLO_OFF_SAID
    }
}

/// The permission-mode light as a screen reader names it: the painted word in its name (WCAG
/// 2.5.3) with its ink said as `on` or `off`, the mode and what a click does as its description.
#[cfg(a11y_tree)]
fn light_spoken(s: &SessionLights, light: Light, now: Instant) -> (String, Option<String>) {
    let word = s.shown.yolo().map_or("unknown", said_bit);
    let refused = s.refused.filter(|(_, _, until)| *until > now);
    let LightTitle { spoken, .. } =
        light_title(s, light, refused, crate::permission_click::RESTARTS);
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
    let mut nodes = Vec::with_capacity(3);
    // The node is the cells a press turns the switch on ([`BorderSwitch::ink`]), so a reader's
    // hit on any of them acts.
    if !b.ink.is_empty() {
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
            col: b.ink.start,
            width: b.ink.len(),
        });
    }
    if let Some(cols) = b.mark.as_ref() {
        let (company, site) = (aterm_types::identity::COMPANY, aterm_types::identity::SITE);
        nodes.push(GridChip {
            session: b.session,
            chip: HarnessChip::Mark,
            label: format!("{company}, {site}"),
            description: Some(format!(
                "Opens {} in the browser",
                aterm_types::identity::SITE_URL
            )),
            toggled: None,
            row,
            col: cols.start,
            width: cols.len(),
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
#[path = "claude_lights_tests.rs"]
mod tests;

/// End-to-end in-process gesture tests for Claude Code footer chips.
#[cfg(all(test, unix))]
#[path = "claude_lights_gesture_tests.rs"]
mod gesture_tests;

#[cfg(test)]
#[path = "claude_lights_face_tests.rs"]
mod face_tests;

#[cfg(all(test, unix))]
#[path = "claude_lights_click_tests.rs"]
mod click_tests;

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

/// `claude_permission_click` environment actions.
#[cfg(all(any(test, debug_assertions), unix))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_permission_click",
    action = "ToYolo",
    reason = "Claude Code's own step: its pill moves to bypass (a person's shift+tab, or a \
              restart that lands there). The Tier-1 test redraws the real engine's mode row."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_permission_click",
    action = "ToOff",
    reason = "Claude Code's own step: its pill leaves bypass (a person's shift+tab, the plan \
              tool, a restart that lands in the settings' mode)."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_permission_click",
    action = "DefaultYolo",
    reason = "A person's or an administrator's edit: a Claude Code settings document comes to \
              name bypass as `permissions.defaultMode`, which the click reads \
              (`claude_settings::permission_default`); the Tier-1 test scripts that read."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_permission_click",
    action = "DefaultElse",
    reason = "The same edit undone: the settings name another default mode, or none."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_permission_click",
    action = "BypassBarred",
    reason = "A person's or an administrator's edit: a Claude Code settings document sets \
              `permissions.disableBypassPermissionsMode`, which the click reads \
              (`claude_settings::permission_default`); the Tier-1 test scripts that read."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_permission_click",
    action = "BypassAllowed",
    reason = "The same edit undone: no settings document turns bypass off."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_permission_click",
    action = "Hold",
    reason = "The tab's own state: a box over the composer or a person's hold, which the \
              harness's look reads (`upgrade::press_gate`); the Tier-1 test scripts the look's \
              wait."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_permission_click",
    action = "Free",
    reason = "The tab's own state: the box answered, the hold ended."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_permission_click",
    action = "NoLine",
    reason = "The tab's own facts: a job with no shell, a claim, a coordinated record, which the \
              harness's look reads; the Tier-1 test scripts its refusal."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_permission_click",
    action = "Apart",
    reason = "The clock passes a double click's interval after the run's last press \
              (`crate::multi_click_ms`); the Tier-1 test presses at instants past it."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn permission_click_environment_waivers() {}
#[cfg(all(test, unix))]
#[path = "claude_lights_fit_tests.rs"]
mod fit_tests;

#[cfg(all(test, unix))]
#[path = "claude_lights_switch_tests.rs"]
mod switch_tests;

#[cfg(all(test, unix))]
#[path = "claude_lights_arrow_tests.rs"]
mod arrow_tests;
