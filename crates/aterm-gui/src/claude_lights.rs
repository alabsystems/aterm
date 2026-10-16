// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CLAUDE CODE LIGHTS, host side: aterm's chips in the rule under Claude
//! Code's input box, left of aterm's footer facts (`crate::claude_footer`) —
//! the permission mode's light (owner directions, 2026-09-24 and 2026-09-28)
//! and the harness's own two marks, Autopilot and `⚙` (owner, 2026-10-15;
//! `docs/DESIGN-harness-autopilot-2026-10-15.md` §2.3). What the light means,
//! where it is read from and which of Claude's own inputs flips it are
//! decided in `aterm_agent::harness::lights`; this module puts the chips on
//! the glass and carries the gestures. Fast mode has no chip of aterm's
//! (owner, 2026-09-30): Claude Code draws its own `↯` in its top rule.
//!
//! * DEVIATIONS ONLY, for the light: it is drawn — as a labelled chip, `⏸
//!   plan` — only while it shows what the owner does not expect
//!   (`lights::deviates`), while a toggle of it is in flight, a refusal or a
//!   just-switched notice for it is on show, or the pointer or the keyboard
//!   selection is on it. At rest, everything as expected, no light is drawn,
//!   and with no harness host `claude_lights_block` is `None`: the rule
//!   carries the facts alone. The chip never replaces or covers Claude's own
//!   mode pill, which stays on Claude's row under the rule, whole: the mode
//!   chip is aterm's own click and keyboard target, in aterm's span of the
//!   rule. A rule too narrow for the chip beside the model draws none (the
//!   model gives way last, `footer::fit_rule`).
//! * THE HARNESS'S TWO, ALWAYS — the owner's override of "deviations only"
//!   for these two marks alone (2026-10-15: "i want a small icon that allows
//!   me to toggle it on and off and another icon that takes me to the
//!   settings page for the claude code harness", "I need some visual
//!   indicator that the claude code session is being wrapped and a way to
//!   toggle on and off to show the improvement"). Left of the light, in that
//!   order, [`Chip::Autopilot`] says the session's Autopilot
//!   ([`crate::autopilot::AutopilotView`]: `∞` cruise alone, `⤓ land`, `○
//!   off`, `⊘ off` with the master switch off) and [`Chip::Settings`] is a
//!   `⚙`: both drawn wherever the footer is and the window has a harness
//!   host, at rest included; with no host, neither. They are not lights —
//!   nothing on Claude's screen says them — and the LIGHT STAYS DEVIATIONS
//!   ONLY. A click on Autopilot steps the session's own word (Cruise → Land
//!   → Off → Cruise, [`App::cycle_session_autopilot`]): written in-process,
//!   nothing sent to the PTY, never a person's keystroke, the chip `◐` until
//!   a frame reads the word back. With the master switch off a click on it,
//!   and any click on `⚙`, opens Settings ▸ Harness in that window. Their
//!   repaint key is the footer's (`App::claude_footer_fp`), never
//!   [`WindowLights::fingerprint`], which stays 0 at rest. A narrow rule
//!   gives up the keyboard's reveal first, then `⚙`, then the light, and
//!   Autopilot last — the mark that says the session is wrapped, where the
//!   light only repeats Claude's own pill under the rule ([`Keep`]): one
//!   order, so a chip a rule gave up never comes back in a narrower one,
//!   and nor does a fact. Every chip goes before the model
//!   (`footer::fit_rule`).
//! * THE CHIPS' OWN GLASS: the blank between two chips is theirs too — a
//!   press there does nothing, and neither it, nor a press of any button on
//!   a chip, nor the pointer's motion over either reaches the pane (no
//!   selection, no paste, no mouse report); the pointer is a hand only where
//!   a press acts. A left press on either is a click away from an in-grid
//!   rename field, and commits it as a click on the grid does.
//! * HOVER a chip: its title and state replace nothing — they appear just
//!   right of the chips, where the rule has room: a hover's, a selection's
//!   or a toggle's title is the first thing a narrow rule gives up, short
//!   then gone, before any of the facts, while a REASON — where a mode return
//!   stopped — outranks the branch and the path, never the model or the
//!   effort (`footer::fit_rule`). CLICK it: the light toggles, Autopilot
//!   steps, `⚙` opens Settings ▸ Harness.
//! * `ctrl+shift+tab` (macOS; elsewhere it is aterm's `prev_tab`) REVEALS
//!   the known light of the focused pane and selects the next chip — the
//!   harness's two included, always revealable where drawn (Claude keeps
//!   its own shift+tab); while one is selected, Return or Space acts as its
//!   click (the light in bypass or auto does nothing), and Escape — or any
//!   other key, which then goes on to Claude as usual — lets go, and the
//!   light at rest goes with the selection. Only a chip the pane has ROOM
//!   for is ever selected (the reveal gives way first), and a selection
//!   whose chip is not on the glass is let go by the next key: a chip nobody
//!   can see never takes a Return meant for Claude.
//! * A toggle is Claude's own input, and it is READ BACK: the light shows
//!   `◐` until Claude's screen says it switched, and says in its title where
//!   it stopped if Claude did not within `harness::lights::SETTLE_MS` of a
//!   press.
//! * THE MODE RETURN. A click on the mode chip (manual, accept edits, plan or
//!   don't ask) presses shift+tab AT ONCE — the click is the person's input,
//!   mid-turn included, like their own shift+tab in Claude — and each next
//!   press is decided by the loop from the ENGINE's screen
//!   ([`App::advance_claude_mode_returns`], polled on its own timer), never
//!   from a painted frame: a pane that leaves the screen mid-return still
//!   finishes. aterm's OWN next press goes only while Claude is idle: a box
//!   opens only mid-turn, a read cannot see one opening between the read and
//!   Claude reading the key, and a shift+tab that lands in one is its answer
//!   (the 2.1.283 edit prompt binds it to "yes, allow edits for this
//!   session"). Each press has its OWN deadline; a return that stops early —
//!   a box over the composer, an unanswered press, a turn still running, a
//!   lap, a press the input seam refused — names the mode it stopped in (a
//!   click again goes on). No press ever goes from bypass or auto. The
//!   derived model is `aterm_spec::derive::claude_mode_return_model`.
//! * WHO PRESSED. The click's own first input goes as the person's
//!   (`input::Source::Human`: it stamps the person clock the supervisor keeps
//!   its hands off by — the person just clicked). Every input aterm sends
//!   after it on its own — the mode return's next presses — goes as
//!   `input::Source::Controller`: no person made it
//!   at that instant, and stamping it would hold the supervisor off a session
//!   for its whole `[harness] human_grace_s` (120 s) after every automatic
//!   step, a box that opens meanwhile unanswered.
//! * Input never leaves from the render path: a frame that shows a step's
//!   answer only wakes the loop (`Wake::ClaudeFooter`), whose handler reads
//!   the ENGINE and sends the next step ([`App::drain_claude_lights`]).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::{Act, TaskFace};
use aterm_agent::harness::lights::{self, Light, LightState, Mode, Reading, Screen};
use aterm_agent::supervise::Autopilot;
use aterm_core::terminal::{RenderCell, UnderlineStyle};
use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::autopilot::{AutopilotView, ChipTone, PENDING_GLYPH, SETTINGS_GLYPH, SETTINGS_TITLE};
use crate::{App, WindowId};

/// How long a toggle that did not take keeps saying so.
const REFUSAL_SHOWN: Duration = Duration::from_secs(4);

/// One chip of aterm's span in the rule (design §2.3): a LIGHT, read from
/// what Claude Code draws ([`Light`]), or one of the harness's own two marks
/// — aterm's state, not Claude's, so they are chips and never lights, and
/// `harness::lights::Light` keeps its meaning. The painter's hits, the hover,
/// the keyboard selection and a block's columns are kept per chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Chip {
    /// A light of Claude Code's (today the permission mode): drawn as a
    /// deviation only; a click toggles it with Claude's own input.
    Light(Light),
    /// The session's Autopilot ([`AutopilotView`]): drawn always while a
    /// harness host exists; a click steps its word, or opens Settings ▸
    /// Harness with the master switch off.
    Autopilot,
    /// `⚙`: drawn beside Autopilot; a click opens Settings ▸ Harness.
    Settings,
}

impl Chip {
    /// Every chip in the row's order, left to right: Autopilot, Settings,
    /// then the lights.
    fn row() -> impl Iterator<Item = Chip> {
        [Chip::Autopilot, Chip::Settings]
            .into_iter()
            .chain(Light::ALL.into_iter().map(Chip::Light))
    }
}

impl From<Light> for Chip {
    fn from(light: Light) -> Self {
        Chip::Light(light)
    }
}

/// Land's ink (§2.3): the amber of Claude Code's own auto-mode pill
/// (`lights::Mode::Auto`'s hue) — the work is being wrapped up.
const AMBER: [u8; 3] = [0xE8, 0xB0, 0x3C];

/// A LOCAL CLOCK TIME, `HH:MM` in 24-hour time, as the tab's wait already
/// writes it (`→ 14:05`): what the harness's glass shows of an instant —
/// never an age, which would freeze at rest
/// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.0). Formatted ONCE,
/// where its instant is learned (an act's arrival, a face's move, a timer's
/// arming), then copied: a frame formats nothing.
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

    /// The local clock at unix second `at_unix`, in the zone's offset at that
    /// instant (`presence::local_offset_at`, as the tab's retry plan reads
    /// it); `None` where the platform cannot say.
    #[must_use]
    pub(crate) fn at(at_unix: i64) -> Option<Self> {
        let offset = crate::presence::local_offset_at(at_unix)?;
        let minute = (at_unix + offset).rem_euclid(86_400) / 60;
        Some(Self::of_minute(u16::try_from(minute).unwrap_or(0)))
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

/// A DAY OF THE WEEK, named where a timer lands on another local date than
/// the one it was set on (§9.3.2): a weekday, never "tomorrow", which would
/// go stale at midnight with nothing to repaint it.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Autopilot's timer (design §9.6.2) arms the weekday; until then only the face \
                  table's tests do"
    )
)]
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
    not(test),
    allow(
        dead_code,
        reason = "Autopilot's timer (design §9.6.2) arms the weekday; until then only the face \
                  table's tests do"
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

    /// The weekday of the local date `days` days after 1970-01-01, a
    /// Thursday.
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

/// AN ARMED TIMER as the chip shows it (§9.3.2): the local time a timed
/// Cruise lands, formatted when the timer was indexed, and its weekday where
/// that instant falls on another local date than the one it was set on.
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
    /// The day it lands on, where that is not the day it was set on.
    pub(crate) other_day: Option<Weekday>,
}

/// THE HARNESS'S GLASS FOR ONE AUTOPILOT CHIP (§9.2.3, item 5): what the
/// frame reads beside the session's word, once a frame
/// ([`App::chip_glass`]) — a `Copy` value, so a frame allocates nothing,
/// takes no lock and reads no clock for it: every time in it was formatted
/// where its fact arrived. [`ChipGlass::default`] is v1's chip: no act lit,
/// no face told, no timer armed, no goal held — and the face table
/// ([`ChipFace`]) gives v1's face, ink and title for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct ChipGlass {
    /// The act lit for its dwell (§9.2.1), drawn right of the chips in the
    /// chip's ink ([`act_label`]); `None` at rest.
    pub(crate) lit: Option<Act>,
    /// The task's face (§9.4.2); `None` while unknown, drawn as open.
    pub(crate) task: Option<TaskFace>,
    /// The local clock time the task's face moved there, for the spoken
    /// label and the page (`landed 17:42`); `None` where the zone could not
    /// be read.
    pub(crate) task_at: Option<Clock>,
    /// The armed timer (§9.3.2): a timed Cruise reads `∞ ⤓18:00`.
    pub(crate) land_at: Option<LandAtGlass>,
    /// Codex is pursuing its `/goal` (§9.5.4): under Land the chip reads
    /// `⤓ held`.
    pub(crate) held: bool,
    /// The trail's newest act and the clock time it arrived: the chip's
    /// spoken description and the page's `last:` line.
    pub(crate) last: Option<(Act, Clock)>,
    /// The trail's generation: moves with each act the trail takes, so a
    /// title someone is reading is built again — folded into the repaint key
    /// only while the chip is hovered or selected
    /// ([`WindowLights::autopilot_term`]).
    pub(crate) trail: u8,
}

/// WHICH FACE THE AUTOPILOT CHIP WEARS: its view (§2.3) and its glass
/// (§§9.2–9.5) folded into one of eleven, of which THE FACE TABLE gives the
/// glyph, the word, the inks, the titles and the spoken label — pure
/// functions of it and the glass's clocks. Under Cruise an armed timer shows;
/// under Land a running Codex goal, then the task's face; under Off, the
/// master switch and no host, v1's face alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ChipFace {
    /// `∞`: Cruise.
    Cruise,
    /// `∞ ⤓18:00`: Cruise until a time, then Land (§9.3.2).
    CruiseUntil,
    /// `⤓ land`: Land, its task open or unknown.
    Land,
    /// `✓ landed`: the agent finished after the land instruction (§9.4.1).
    Landed,
    /// `✓ done`: the agent said it was finished before a landing was asked.
    Done,
    /// `⤓ quiet`: the landing is spent, no done reply after its nudges.
    Spent,
    /// `⤓ quiet`: a person's own exchange.
    Theirs,
    /// `⤓ held`: Codex is pursuing its `/goal`, and Land types nothing while
    /// it runs (§9.5.4).
    Held,
    /// `○ off`.
    Off,
    /// `⊘ off`: the master switch is off.
    MasterOff,
    /// `⊘ off`: no host.
    Unhosted,
}

/// How a block gives way in a rule short of room: what
/// [`WindowLights::fitted`] gives up, rung by rung, until the chips fit.
/// Each rung keeps a part of the one before it, so a chip leaves only as
/// the rule narrows: never given up at one width and drawn again at a
/// narrower one. Past [`Keep::Drawn`] the block HOLDS all the room it was
/// fitted to ([`Block::holds`]): the room a given-up chip leaves stays the
/// rule's, and no fact the facts gave up before the chips comes back
/// beside fewer of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keep {
    /// Every chip, the keyboard's reveal of the lights at rest included.
    Revealed,
    /// Every chip drawn without the reveal.
    Drawn,
    /// `⚙` given up as well (Settings ▸ Harness is in the View menu too).
    NoSettings,
    /// The light given up as well — it only repeats Claude's own pill on
    /// the row under the rule — and Autopilot alone, the mark that says the
    /// session is wrapped: the last chip a narrow rule keeps.
    AutopilotOnly,
    /// Autopilot's GLYPH alone (§9.3.2): `∞`, `⤓`, `✓` — the timer's time,
    /// a face's word and the act's label all go before the chip does. The
    /// ladder's last rung, taken only by a STANDING FACE of v2's
    /// ([`ChipFace::standing`]: `∞ ⤓18:00`, `✓ landed`, `✓ done`, `⤓ quiet`,
    /// `⤓ held`): v1's faces keep v1's ladder, which ends at
    /// [`Keep::AutopilotOnly`] — a rule too narrow for `⤓ land` or `○ off`
    /// draws no chip, as it did — so nothing a person sees moves until a
    /// slice draws such a face, and no slice need touch the ladder to draw
    /// one ([`WindowLights::fit_into`]).
    GlyphOnly,
}

impl Keep {
    /// Every rung, in the order a rule short of room tries them.
    /// [`Keep::GlyphOnly`] is tried only for a standing face of v2's.
    const LADDER: [Keep; 5] = [
        Keep::Revealed,
        Keep::Drawn,
        Keep::NoSettings,
        Keep::AutopilotOnly,
        Keep::GlyphOnly,
    ];

    /// Whether this rung keeps `chip`, where it is drawn at all (the
    /// reveal is [`Keep::Revealed`]'s alone).
    fn keeps(self, chip: Chip) -> bool {
        match chip {
            Chip::Autopilot => true,
            Chip::Settings => matches!(self, Keep::Revealed | Keep::Drawn),
            Chip::Light(_) => !matches!(self, Keep::AutopilotOnly | Keep::GlyphOnly),
        }
    }
}

/// Why a toggle did not happen, as its light's title says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Refusal {
    /// The light is not on Claude's screen right now.
    NotShown,
    /// A mode return stopped short of bypass or auto — a press went
    /// unanswered past its own deadline, a box covered the composer, or the
    /// input seam took no next press — in this mode, which the light names:
    /// the session DID switch, just not all the way.
    StoppedIn(Mode),
    /// A mode return stopped in this mode because Claude stayed mid-turn past
    /// the last press's deadline: aterm's own next press waits for a turn to
    /// end (a box opens only mid-turn, and a shift+tab that lands in one is
    /// its answer — in the edit prompt, "allow edits for this session"). The
    /// person's own click presses at once, so a click again goes on.
    StoppedMidTurn(Mode),
    /// The mode cycle came back round without an expected mode: this
    /// session's shift+tab cycle holds neither bypass nor auto. Stopped in
    /// this mode, which the light names.
    NotInCycle(Mode),
    /// The input seam did not accept the click's press: no toggle began.
    InputRejected,
}

impl Refusal {
    /// Whether it names the mode a mode return stopped in — the one thing a
    /// return that ends short of bypass or auto must say
    /// (`claude_mode_return_model`'s `AStopIsNamed`, projected by the
    /// Tier-1 bind).
    #[cfg(test)]
    fn names_mode(self) -> bool {
        matches!(
            self,
            Refusal::StoppedIn(_) | Refusal::StoppedMidTurn(_) | Refusal::NotInCycle(_)
        )
    }

    /// What the light's title says of it.
    fn says(self) -> String {
        match self {
            Refusal::NotShown => "not on screen right now",
            Refusal::StoppedIn(mode) => return format!("stopped in {} mode", mode.word()),
            Refusal::StoppedMidTurn(mode) => {
                return format!("stopped in {} mode mid-turn; click again", mode.word());
            }
            Refusal::NotInCycle(mode) => {
                return format!("no bypass or auto in this cycle; in {} mode", mode.word());
            }
            Refusal::InputRejected => "input was not accepted; try again",
        }
        .to_owned()
    }
}

/// A toggle in flight: what was sent, and what the next frames must see.
#[derive(Debug, Clone)]
struct Pending {
    light: Light,
    /// The mode when the toggle began (a cycle that leaves it is done).
    started: Mode,
    /// The mode when the last shift+tab was sent: a frame showing another
    /// mode is Claude's answer to that press.
    mode_at_press: Mode,
    /// Every mode the cycle has shown since it began: one shown again is a
    /// lap, and the search stops there.
    seen: Vec<Mode>,
    presses: u8,
    /// Past this, the toggle is given up. Refreshed on every press: it is the
    /// time for ONE shift+tab's answer.
    deadline: Instant,
    /// The loop's next look at the engine while one is armed: ONE timer per
    /// toggle, however many wakes arrive meanwhile.
    next_poll: Option<Instant>,
    /// A mode return's: Claude answered the last press but was mid-turn, so
    /// aterm's own next press waited for the turn to end. A deadline reached
    /// so says so (`Refusal::StoppedMidTurn`).
    held_mid_turn: bool,
}

/// How often a mode return in flight reads the engine for Claude's answer:
/// a pill redraw lands within a frame or two.
const MODE_POLL: Duration = Duration::from_millis(80);

/// What a mode return in flight does next, decided from the engine's reading
/// ([`WindowLights::advance`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Advance {
    /// Nothing yet: the last press is unanswered, or the composer is not on
    /// screen (a box, a scrolled view) — nothing is pressed into what cannot
    /// be read.
    Wait,
    /// Claude answered with `expect`, a mode the owner does not expect
    /// either: press again, now.
    Press { expect: Mode },
    /// Bypass or auto shows: done, never a press on from there.
    Arrived,
    /// The cycle came back round: it holds neither bypass nor auto.
    Lapped,
}

/// One session's lights in one window.
#[derive(Debug, Clone, Default)]
struct SessionLights {
    /// The last state each light was SEEN in — kept while a box covers the
    /// composer, so the row does not flicker grey under every dialog.
    shown: Reading,
    pending: Option<Pending>,
    refused: Option<(Light, Refusal, Instant)>,
    /// A toggle that took: its light stays drawn, titled with its new state,
    /// until this instant — then, back as expected, it goes.
    notice: Option<(Light, Instant)>,
    /// The session's Autopilot as the last frame that drew its footer read
    /// it ([`App::session_autopilot`], once per frame,
    /// [`App::note_claude_autopilot`]): `None` before one did. Under
    /// [`AutopilotView::Unhosted`] neither harness chip is drawn.
    autopilot: Option<AutopilotView>,
    /// An Autopilot click in flight: the word it wrote, and the instant it is
    /// let go if no frame has read that word back by then. The chip shows
    /// `◐` meanwhile.
    autopilot_pending: Option<(Autopilot, Instant)>,
    /// The harness's glass on the Autopilot chip as the same frame read it
    /// ([`App::chip_glass`]): its act, its task's face, its timer, its goal
    /// hold. The default — v1's chip — until a fact is told.
    glass: ChipGlass,
}

impl SessionLights {
    /// Whether the harness's two chips are drawn for this session: a footer
    /// read its Autopilot, and the window has a harness host.
    fn harness_chips(&self) -> bool {
        self.autopilot
            .is_some_and(|view| view != AutopilotView::Unhosted)
    }

    /// Whether a rule short of room for the Autopilot chip's face may keep
    /// its glyph alone ([`Keep::GlyphOnly`]): only while it wears a standing
    /// face of v2's ([`ChipFace::standing`]) and no click of it is in flight
    /// (whose `◐` is a glyph alone already). v1's faces give the chip up
    /// whole, as v1 did.
    fn glyph_rung(&self) -> bool {
        self.autopilot_pending.is_none()
            && self
                .autopilot
                .is_some_and(|view| ChipFace::of(view, &self.glass).standing())
    }
}

/// One painted cell of the chips, in frame cells — recorded by the painter,
/// read by the pointer, so a click can only land on a chip the glass shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LightHit {
    frame_row: usize,
    /// The same row as a window grid row, for the chrome register: a band
    /// that later claims the row (the link caption) hides the chip, and a
    /// hidden chip is not a chip.
    term_row: usize,
    col: usize,
    session: u64,
    /// The chip painted there — or `None` for the blank between two chips:
    /// the chips' own glass, where a press does nothing and nothing reaches
    /// the pane under it (no selection, no mouse report).
    chip: Option<Chip>,
}

/// What of the chips' glass the pointer is over
/// ([`App::track_claude_light_hover`]): over either part the motion is the
/// chips', never the pane's, and the pointer is a hand only where a press
/// acts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverChips {
    /// Not the chips': the motion is the pane's.
    No,
    /// A chip: a press acts on it — the hand.
    Chip,
    /// The blank between two chips: theirs, and a press there does nothing
    /// — the arrow.
    Gap,
}

impl OverChips {
    /// What [`App::claude_chips_at`] found.
    pub(crate) fn of(at: Option<(u64, Option<Chip>)>) -> Self {
        match at {
            None => Self::No,
            Some((_, Some(_))) => Self::Chip,
            Some((_, None)) => Self::Gap,
        }
    }
}

/// The lights' state for one window (`WindowState::claude_lights`).
#[derive(Debug, Default)]
pub(crate) struct WindowLights {
    /// One per painted CELL of every drawn chip: the whole chip is the target.
    hits: Vec<LightHit>,
    /// The sessions whose composer rule this frame found on the glass —
    /// chip or none — with the cells its chips may take there. The
    /// keyboard's way to a light at rest needs the rule on the glass AND room
    /// in it for the chip it selects.
    footers: Vec<(u64, usize)>,
    hover: Option<(u64, Chip)>,
    selected: Option<(u64, Chip)>,
    /// A frame has been painted since the selection last moved: from then
    /// on, a selection whose chip is not among the hits is not on the glass,
    /// and is let go rather than left to swallow a Return.
    selection_painted: bool,
    sessions: HashMap<u64, SessionLights>,
    /// Painted blocks handed back, their buffers the next frame's
    /// (`App::claude_lights_block`).
    spare_blocks: Vec<Block>,
}

/// How many painted blocks a window keeps for reuse
/// (`App::recycle_claude_lights_block`).
const SPARE_BLOCKS: usize = 8;

/// The lights' clock on the footer's per-frame paths — the repaint key's
/// term ([`WindowLights::fingerprint`], read only while something timed is
/// held) and the block the rule draws: the wall clock, each read counted
/// (`work_counts::footer_clock_read`), the floor the harness's per-frame
/// cost is measured against.
pub(crate) fn lights_now() -> Instant {
    #[cfg(test)]
    crate::work_counts::footer_clock_read();
    Instant::now()
}

impl WindowLights {
    /// Everything about the lights that is GUI state rather than grid
    /// content, for the repaint key: a hover, a selection, a toggle in flight
    /// or a refusal on show changes the row with no grid damage at all. The
    /// harness chips' own state — the session's Autopilot, a click of it in
    /// flight — is keyed per pane by the footer
    /// ([`Self::autopilot_term`]), so this stays 0 at rest. `now` is read
    /// once, and only where a refusal or a notice is held — whose showing it
    /// bounds — so a window with neither, every window at rest, reads no
    /// clock for it.
    pub(crate) fn fingerprint(&self, now: impl Fn() -> Instant) -> u64 {
        let read = std::cell::Cell::new(None);
        let shows = |until: Instant| {
            let now = read.get().unwrap_or_else(|| {
                let at = now();
                read.set(Some(at));
                at
            });
            until > now
        };
        // Among session entries, only a pending toggle or a refusal still on
        // screen can change the row. `sessions` also remembers light states for
        // hidden panes, so a long-lived window can hold many inert entries.
        let mut active: Vec<_> = self
            .sessions
            .iter()
            .filter(|(_, s)| {
                s.pending.is_some()
                    || s.refused.is_some_and(|(_, _, until)| shows(until))
                    || s.notice.is_some_and(|(_, until)| shows(until))
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
                (p.light, p.presses).hash(&mut h);
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
        }
        h.finish() | 1
    }

    /// Keep ONE poll armed for `session`'s toggle in flight: the loop reads
    /// the engine again after `every`, however many wakes arrive meanwhile.
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

    /// Forget every painted light and footer rule; the painter records this
    /// frame's — before any key is read again, so a selection standing from
    /// here on has been through a painted frame.
    pub(crate) fn clear_hits(&mut self) {
        self.hits.clear();
        self.footers.clear();
        self.selection_painted = true;
    }

    /// The chips the keyboard walks in `session` while a selection lasts, in
    /// the row's order: the harness's two wherever they are drawn, and every
    /// light whose state is known.
    fn revealable(&self, session: u64) -> Vec<Chip> {
        let s = self.sessions.get(&session);
        let shown = s.map(|s| s.shown).unwrap_or_default();
        let harness = s.is_some_and(SessionLights::harness_chips);
        Chip::row()
            .filter(|chip| match chip {
                Chip::Light(light) => revealed(*light, &shown),
                Chip::Autopilot | Chip::Settings => harness,
            })
            .collect()
    }

    /// Move the keyboard selection (a frame must paint it before it counts
    /// as off the glass).
    fn select(&mut self, selected: Option<(u64, Chip)>) {
        self.selected = selected;
        self.selection_painted = false;
    }

    /// Fold in `session`'s Autopilot as this frame's footer read it — its
    /// own `word`, its `view` ([`App::session_autopilot`]) and its harness
    /// glass ([`App::chip_glass`]) — and let an Autopilot click in flight go
    /// once the word reads back as the one it wrote, or once its time is up.
    /// The clock (`now`) is read only for a click in flight that has not read
    /// back: a frame at rest reads none.
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
        if s.autopilot_pending
            .is_some_and(|(want, until)| word == Some(want) || until <= now())
        {
            s.autopilot_pending = None;
        }
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

    /// The harness chips' repaint term for `session`'s footer in this
    /// window: its own Autopilot `word` and its `view`, as the footer's key
    /// read them (once a frame, [`App::session_autopilot`]), an Autopilot
    /// click of it in flight here, and its harness `glass`
    /// ([`App::chip_glass`]) — glass that moves with no grid damage. Folded
    /// into the footer's key (`App::claude_footer_fp`) for every pane whose
    /// footer shows, never into [`Self::fingerprint`], which stays 0 at rest.
    /// A packing of small fields, distinct for every combination the chip
    /// draws differently (§9.2.3's bit table): no hasher, no lock, no clock.
    ///
    /// | Bits | Term |
    /// |---|---|
    /// | 0–23 | v1's word, view and click in flight |
    /// | 24–31 | the lit act (0: none) |
    /// | 32–34 | the task's face (0: open or unknown) |
    /// | 35 | the Codex goal hold |
    /// | 36–46 | the timer's minute of the day + 1 (0: none) |
    /// | 47 | the timer lands on another local date than it was set on |
    /// | 48–55 | the trail's generation, ONLY while this chip is hovered or selected: an act that lights nothing repaints nothing unless someone is reading the title |
    ///
    /// With the default glass it is v1's term, bit for bit.
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
        let reading = [self.hover, self.selected].contains(&Some((session, Chip::Autopilot)));
        let word_bits = |w: Option<Autopilot>| w.map_or(0, |w| 1 + w as u64);
        word_bits(word) | (view as u64) << 8 | word_bits(pending) << 16 | glass_bits(glass, reading)
    }

    /// Read `rows` (`read`, inside the reader's panic fence) and fold the
    /// reading in ([`Self::observe_read`]).
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

    /// Fold a frame's reading of `session`'s rows in ([`Self::observe`]),
    /// as its reader's panic fence left it (`crate::claude_footer`'s
    /// readings, kept with the rows). A reader panic (`None`) folds NOTHING
    /// in: the lights keep what they last showed, a toggle in flight takes no
    /// step (its deadline still expires it), and nothing to send or show.
    fn observe_read(&mut self, session: u64, read: crate::claude_footer::LightsRead) -> Step {
        match read {
            Some(reading) => self.observe(session, reading),
            None => Step::default(),
        }
    }

    /// Fold one frame's reading of `session`'s screen in (`lights::read_lights`:
    /// `None` without a composer): remember what each light shows. A toggle
    /// is NEVER stepped here: a frame that shows Claude's answer — the pill
    /// moved on — only wakes the loop, which reads the ENGINE and decides
    /// ([`App::advance_claude_mode_returns`]) — the same decision whether or
    /// not a frame is painted, and the turn is read there, never here. What
    /// the caller must do: wake the loop.
    fn observe(&mut self, session: u64, reading: Option<Reading>) -> Step {
        let s = self.sessions.entry(session).or_default();
        s.shown.fold(&reading.unwrap_or(Reading::UNKNOWN));
        let Some(p) = s.pending.as_ref() else {
            return Step::default();
        };
        // Claude answered the last press (or the pill moved on its own): the
        // loop reads the engine and decides.
        Step {
            wake: reading.is_some_and(|r| r.mode.is_some_and(|m| m != p.mode_at_press)),
        }
    }

    /// Move `session`'s mode return along from the ENGINE's `screen` (`None`
    /// without a composer on it): done at bypass or auto; the next press
    /// once Claude has answered the last with another mode AND is not
    /// mid-turn — its own deadline set now; or a lap refused, naming the mode
    /// it stopped in. Anything else waits: an unanswered press, a box, a
    /// scrolled view — and a turn in flight, because a box opens only
    /// mid-turn and a shift+tab that lands in one is its ANSWER (the 2.1.283
    /// edit prompt binds it, `confirm:cycleMode`, to "yes, allow edits for
    /// this session"): a read shows no box, but one may open between that
    /// read and Claude reading the key. An idle Claude opens none. Deadlines
    /// are [`Self::expire`]'s.
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
        if mode.is_expected() {
            s.pending = None;
            s.refused = None;
            s.notice = Some((Light::Mode, now + REFUSAL_SHOWN));
            return Advance::Arrived;
        }
        // The footer wake may be delayed past this press's deadline. Read the
        // mode for the stop notice, but do not send another shift+tab (or
        // restart the deadline) from an answer that arrived too late.
        if p.deadline <= now || mode == p.mode_at_press {
            return Advance::Wait;
        }
        // A mode shown before is a LAP — this session's cycle holds neither
        // bypass nor auto (it has three to five modes,
        // `lights::MAX_MODE_PRESSES`): stop, and say where.
        if mode == p.started || p.seen.contains(&mode) || p.presses >= lights::MAX_MODE_PRESSES {
            s.pending = None;
            s.refused = Some((Light::Mode, Refusal::NotInCycle(mode), now + REFUSAL_SHOWN));
            return Advance::Lapped;
        }
        if busy {
            // Answered, mid-turn: the next press waits for the turn to end,
            // within this press's deadline.
            p.held_mid_turn = true;
            return Advance::Wait;
        }
        p.presses += 1;
        p.seen.push(mode);
        p.mode_at_press = mode;
        p.held_mid_turn = false;
        // Each press gets its own deadline: the time for ONE answer.
        p.deadline = now + Duration::from_millis(lights::SETTLE_MS);
        Advance::Press { expect: mode }
    }

    /// Give up on every toggle past its deadline — a mode return past its
    /// LAST press's, naming the mode it stopped in (the session did switch,
    /// just not all the way) — and end every refusal and notice past its
    /// time, and let go of every Autopilot click no frame read back in its
    /// time (the chip then says the word as it reads): whether anything
    /// changed, and the sessions whose refusal starts now (the caller times
    /// its end — a refusal no frame ends would stay on an idle screen).
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "claude_mode_return",
            action = "Expire",
            project = "claude_lights::gesture_tests::project_mode_return"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "claude_mode_return",
            action = "Rest",
            project = "claude_lights::gesture_tests::project_mode_return"
        )
    )]
    fn expire(&mut self, now: Instant) -> (bool, Vec<u64>) {
        let mut any = false;
        let mut refused = Vec::new();
        for (session, s) in &mut self.sessions {
            if let Some(p) = &s.pending
                && p.deadline <= now
            {
                // The last mode seen — by a frame, if a box has kept the
                // engine's reading from it since.
                let why = match s.shown.mode.unwrap_or(p.mode_at_press) {
                    at if at.is_expected() => None,
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
            if s.autopilot_pending.is_some_and(|(_, until)| until <= now) {
                s.autopilot_pending = None;
                any = true;
            }
        }
        (any, refused)
    }

    /// [`Self::block_into`] with the window's own selection and hover: what
    /// the painter draws at `keep`, as the tests read it.
    #[cfg(test)]
    fn block(
        &self,
        session: u64,
        blank: RenderCell,
        now: Instant,
        accent: [u8; 3],
        keep: Keep,
    ) -> Block {
        let mine = |at: Option<(u64, Chip)>| at.filter(|(id, _)| *id == session).map(|(_, c)| c);
        self.block_with(
            session,
            blank,
            now,
            accent,
            mine(self.selected),
            mine(self.hover),
            keep,
        )
    }

    /// The block ([`Self::block_into`]) `room` cells hold, down
    /// [`Keep::LADDER`] — the reveal given up first, then `⚙`, then the
    /// light, then a standing face's word down to its glyph, Autopilot
    /// last — until its chips fit; the selection and the
    /// hover given, so the keyboard can ask whether a selection would FIT
    /// before making it. `None` where no chip fits or none is drawn. Each
    /// rung is MEASURED ([`chips_width_at`], no cell built) and the block is
    /// built once, at the rung that fits: the painter asks this for every
    /// footer it draws, every frame. Without a selection the reveal draws
    /// nothing [`Keep::Drawn`] does not, and is not measured. A block that
    /// gave up a drawn chip holds all of `room` ([`Block::holds`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "the block's inputs: the session, its ground, the clock, the accent, the \
                  pointer's and the keyboard's chips and the room"
    )]
    fn fitted(
        &self,
        session: u64,
        blank: RenderCell,
        now: Instant,
        accent: [u8; 3],
        selected: Option<Chip>,
        hover: Option<Chip>,
        room: usize,
    ) -> Option<Block> {
        let mut block = Block::default();
        self.fit_into(
            session, blank, now, accent, selected, hover, room, &mut block,
        )
        .then_some(block)
    }

    /// [`Self::fitted`] laid into `block` (its buffers reused, cleared
    /// first): whether a block is drawn.
    #[allow(
        clippy::too_many_arguments,
        reason = "fitted's own inputs and the block it lays into"
    )]
    fn fit_into(
        &self,
        session: u64,
        blank: RenderCell,
        now: Instant,
        accent: [u8; 3],
        selected: Option<Chip>,
        hover: Option<Chip>,
        room: usize,
        block: &mut Block,
    ) -> bool {
        let at_rest = SessionLights::default();
        let s = self.sessions.get(&session).unwrap_or(&at_rest);
        let (ink, dim) = (blank.fg, dim_of(blank));
        let glyph_rung = s.glyph_rung();
        let Some(keep) = Keep::LADDER
            .into_iter()
            .filter(|keep| *keep != Keep::Revealed || selected.is_some())
            .filter(|keep| *keep != Keep::GlyphOnly || glyph_rung)
            .find(|keep| {
                let drawn = drawn_at(s, now, selected, hover, *keep);
                let width = chips_width_at(
                    s,
                    Chip::row().filter(|c| drawn(*c)),
                    *keep,
                    accent,
                    ink,
                    dim,
                );
                (1..=room).contains(&width)
            })
        else {
            return false;
        };
        self.block_into(session, blank, now, accent, selected, hover, keep, block);
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
        accent: [u8; 3],
        selected: Option<Chip>,
        hover: Option<Chip>,
        keep: Keep,
    ) -> Block {
        let mut block = Block::default();
        self.block_into(
            session, blank, now, accent, selected, hover, keep, &mut block,
        );
        block
    }

    /// `session`'s chip block: its TITLE (when a chip is selected or hovered,
    /// or a light is switching, refused or just switched — empty otherwise)
    /// and its CHIPS, then one cell of margin, with the columns each chip
    /// spans. Apart, because only the chips must fit: a pane too narrow for
    /// the title keeps its chips and drops the title. `accent` is the
    /// footer's mark ink, which Cruise's `∞` wears; `selected` and `hover`
    /// the keyboard's and the pointer's chips in this session.
    ///
    /// THE LIGHT, DEVIATION ONLY (owner, 2026-09-27: "I don't want to show
    /// the state of parameters that I always expect to be on"). Its chip is
    /// drawn only while it shows what the owner does not expect
    /// (`lights::deviates`), while a toggle of it is in flight, while a
    /// refusal or a just-switched notice for it is on show, while the pointer
    /// or the keyboard selection is on it, and — the keyboard's way to a
    /// light at rest — for every known light while a selection lasts
    /// (`ctrl+shift+tab`, [`App::on_key_claude_lights`]): the reveal, which a
    /// rule short of room gives up first ([`Keep`]).
    ///
    /// THE HARNESS'S TWO, ALWAYS (owner, 2026-10-15, for these two marks
    /// alone: the module header): Autopilot, then `⚙`, left of the light,
    /// wherever the session's footer read a harness host — until `keep` gives
    /// them up ([`Keep::keeps`]). With no host and everything as expected the
    /// block is EMPTY: no chip, no title, nothing to hit.
    ///
    /// THE TITLE is the first DRAWN of: the chip in focus, the light's toggle
    /// in flight, its refusal, its notice — so a hover or a selection left on
    /// a chip this rung gives up never hides why the light says what it says.
    ///
    /// It is laid into `out`, its buffers reused (each cleared first): the
    /// painter's spare ([`App::claude_lights_block`]), so a frame at rest
    /// allocates nothing for its chips.
    #[allow(
        clippy::too_many_arguments,
        reason = "the block's inputs: the session, its ground, the clock, the accent, the \
                  pointer's and the keyboard's chips, how much of the row is kept, and the \
                  block it lays into"
    )]
    fn block_into(
        &self,
        session: u64,
        blank: RenderCell,
        now: Instant,
        accent: [u8; 3],
        selected: Option<Chip>,
        hover: Option<Chip>,
        keep: Keep,
        out: &mut Block,
    ) {
        let at_rest = SessionLights::default();
        let s = self.sessions.get(&session).unwrap_or(&at_rest);
        // The keyboard selection outranks a resting pointer: Return acts on
        // the SELECTED chip, so the row must mark that one.
        let focus = selected.or(hover);
        let refused = s.refused.filter(|(_, _, until)| *until > now);
        let noticed = s
            .notice
            .filter(|(_, until)| *until > now)
            .map(|(light, _)| light);
        let pending = s.pending.as_ref().map(|p| p.light);
        let drawn = drawn_at(s, now, selected, hover, keep);
        let titled = [
            focus,
            pending.map(Chip::Light),
            refused.map(|(l, _, _)| Chip::Light(l)),
            noticed.map(Chip::Light),
        ]
        .into_iter()
        .flatten()
        .find(|chip| drawn(*chip));
        let dim = dim_of(blank);
        let Block {
            title,
            short_title,
            reason,
            lights: cells,
            cols,
            holds,
        } = out;
        title.clear();
        short_title.clear();
        cells.clear();
        cols.clear();
        *reason = false;
        if let Some(chip) = titled {
            let (whole, short, why) = title_of(s, chip, refused, focus == Some(chip));
            *reason = why;
            let lay = |text: &str, out: &mut Vec<RenderCell>| {
                out.extend(text.chars().chain("  ".chars()).map(|ch| {
                    let mut c = blank;
                    c.ch = ch;
                    c.fg = dim;
                    c
                }));
            };
            lay(&whole, title);
            lay(&short, short_title);
        }
        for chip in Chip::row().filter(|c| drawn(*c)) {
            if !cells.is_empty() {
                cells.push(blank);
            }
            let start = cells.len();
            for (ch, ink) in face_of(chip, s, keep, accent, blank.fg, dim).cells() {
                let mut c = blank;
                c.ch = ch;
                c.fg = ink;
                if focus == Some(chip) {
                    c.bold = true;
                    c.underline = UnderlineStyle::Single;
                }
                cells.push(c);
            }
            cols.push((start..cells.len(), chip));
        }
        *holds = cells.len();
    }
}

/// Whether the keyboard's reveal draws and walks `light` in a session that
/// shows `shown`: every light whose state is known.
fn revealed(light: Light, shown: &Reading) -> bool {
    shown.state(light) != LightState::Unknown
}

/// Whether `s`'s block draws a chip at `keep`, the keyboard on `selected`
/// and the pointer on `hover` — the one rule [`WindowLights::block_into`]
/// lays out and [`WindowLights::fitted`] measures (its terms are
/// `block_into`'s: the light a deviation, in flight, refused, just
/// switched, in focus or revealed; the harness's two wherever a host is).
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
    move |chip: Chip| {
        keep.keeps(chip)
            && match chip {
                Chip::Autopilot | Chip::Settings => harness,
                Chip::Light(light) => {
                    lights::deviates(light, &s.shown)
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

/// The cells `chips` take in the row at `keep`, one blank apart, each its
/// face ([`face_of`]: the glyph, and a space and the word where it has one)
/// — what [`WindowLights::block_into`] lays out, counted without laying it.
fn chips_width_at(
    s: &SessionLights,
    chips: impl Iterator<Item = Chip>,
    keep: Keep,
    accent: [u8; 3],
    ink: [u8; 3],
    dim: [u8; 3],
) -> usize {
    chips
        .map(|chip| face_of(chip, s, keep, accent, ink, dim).width())
        .fold(None, |row: Option<usize>, face| {
            Some(row.map_or(face, |row| row + 1 + face))
        })
        .unwrap_or(0)
}

/// What `light`'s title says after its name: the refusal on show, the toggle
/// in flight, else its state — and, under the pointer or the selection, what
/// a click does. And whether it says WHY (the refusal — where a mode return
/// stopped is one): a reason outranks the branch and the path in the rule,
/// and the rest gives way before any fact (`footer::fit_rule`).
fn what_of(
    s: &SessionLights,
    light: Light,
    refused: Option<(Light, Refusal, Instant)>,
    focused: bool,
) -> (String, bool) {
    if let Some((l, why, _)) = refused
        && l == light
    {
        return (why.says(), true);
    }
    if s.pending.as_ref().is_some_and(|p| p.light == light) {
        return ("switching\u{2026}".to_owned(), false);
    }
    // The chip beside it names the mode; a mode not known is not on screen.
    let what = match s.shown.mode {
        Some(_) if focused && s.shown.state(light) == LightState::Off => "click for bypass or auto",
        Some(mode) => mode.word(),
        None => "not on screen right now",
    };
    (what.to_owned(), false)
}

/// What `chip`'s title says, whole and short (each without its trailing
/// gap), and whether it says WHY ([`what_of`]: only a light's refusal does).
/// A light's is its name and [`what_of`]; Autopilot's is the face table's
/// ([`autopilot_title`]: its mode phrase, what the mode does, then what a
/// click does), short to its mode phrase; `⚙`'s is [`SETTINGS_TITLE`],
/// short to `harness settings` alike.
fn title_of(
    s: &SessionLights,
    chip: Chip,
    refused: Option<(Light, Refusal, Instant)>,
    focused: bool,
) -> (String, String, bool) {
    match chip {
        Chip::Light(light) => {
            let (what, why) = what_of(s, light, refused, focused);
            (format!("{}: {what}", light.title()), what, why)
        }
        Chip::Autopilot if s.autopilot_pending.is_some() => (
            "autopilot: switching\u{2026}".to_owned(),
            "switching\u{2026}".to_owned(),
            false,
        ),
        Chip::Autopilot => {
            let view = s.autopilot.unwrap_or(AutopilotView::Unhosted);
            let (whole, short) = autopilot_title(view, &s.glass, "", crate::autopilot_menu::OPENS);
            (whole, short, false)
        }
        Chip::Settings => (
            SETTINGS_TITLE.to_owned(),
            short_title(SETTINGS_TITLE).to_owned(),
            false,
        ),
    }
}

/// A harness chip's title cut to what it names, for a rule with no room for
/// what a click does: up to its first ` — ` or ` · ` (`autopilot: cruise`,
/// `harness off in Settings`), else whole.
fn short_title(title: &str) -> &str {
    [" \u{2014} ", " \u{b7} "]
        .iter()
        .filter_map(|cut| title.find(cut))
        .min()
        .map_or(title, |at| &title[..at])
}

/// A chip's FACE as the painter lays it: its glyph in one ink, then — a
/// space apart, where it has one — its word and the clock it carries in
/// another (one ink for every face but the armed timer's `∞ ⤓18:00`, whose
/// `∞` is Cruise's and whose `⤓18:00` is Land's). A word-less chip has no
/// trailing space. Static text and a copied clock: a frame allocates no face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Face {
    glyph: &'static str,
    glyph_ink: [u8; 3],
    word: Option<&'static str>,
    clock: Option<Clock>,
    word_ink: [u8; 3],
}

impl Face {
    /// A face of one ink.
    const fn plain(glyph: &'static str, word: Option<&'static str>, ink: [u8; 3]) -> Self {
        Self {
            glyph,
            glyph_ink: ink,
            word,
            clock: None,
            word_ink: ink,
        }
    }

    /// Whether anything follows the glyph.
    fn worded(&self) -> bool {
        self.word.is_some() || self.clock.is_some()
    }

    /// The cells it takes.
    fn width(&self) -> usize {
        self.glyph.chars().count()
            + if self.worded() {
                1 + self.word.map_or(0, |w| w.chars().count()) + self.clock.map_or(0, |_| 5)
            } else {
                0
            }
    }

    /// Its characters, each with its ink, in the order they are laid.
    fn cells(self) -> impl Iterator<Item = (char, [u8; 3])> {
        let worded = self.worded();
        let word_ink = self.word_ink;
        let clock = self.clock.map(|c| c.0);
        self.glyph
            .chars()
            .map(move |ch| (ch, self.glyph_ink))
            .chain(
                worded
                    .then_some(' ')
                    .into_iter()
                    .chain(self.word.into_iter().flat_map(str::chars))
                    .chain(clock.into_iter().flatten().map(char::from))
                    .map(move |ch| (ch, word_ink)),
            )
    }
}

/// A chip's FACE at `keep` ([`Face`]). The light by its mode's pill
/// ([`light_face`]); Autopilot by THE FACE TABLE ([`ChipFace`] of its view
/// and its glass: `∞` alone, `⤓ land`, `○ off`, `⊘ off` at rest — as the
/// tests' `AutopilotView::label` spells them — and `∞ ⤓18:00`, `✓ landed`,
/// `✓ done`, `⤓ quiet`, `⤓ held` as its glass says) in its tones — Cruise
/// the footer's mark `accent`, Land [`AMBER`], the rest `dim` — its glyph
/// alone at [`Keep::GlyphOnly`], or [`PENDING_GLYPH`] alone in its view's
/// tone while a click of it is in flight; `⚙` alone in the row's `ink`.
fn face_of(
    chip: Chip,
    s: &SessionLights,
    keep: Keep,
    accent: [u8; 3],
    ink: [u8; 3],
    dim: [u8; 3],
) -> Face {
    let ink_of = |tone: ChipTone| match tone {
        ChipTone::Accent => accent,
        ChipTone::Amber => AMBER,
        ChipTone::Dim => dim,
    };
    match chip {
        Chip::Light(light) => {
            let switching = s.pending.as_ref().is_some_and(|p| p.light == light);
            let (glyph, word, ink) = light_face(light, &s.shown, switching, ink, dim);
            Face::plain(glyph, Some(word), ink)
        }
        Chip::Autopilot => {
            let view = s.autopilot.unwrap_or(AutopilotView::Unhosted);
            if s.autopilot_pending.is_some() {
                return Face::plain(PENDING_GLYPH, None, ink_of(view.tone()));
            }
            let face = ChipFace::of(view, &s.glass);
            let (glyph_tone, word_tone) = face.tones();
            let glyph = Face::plain(face.glyph(), None, ink_of(glyph_tone));
            if keep == Keep::GlyphOnly {
                return glyph;
            }
            Face {
                word: face.word(),
                clock: face.clock(&s.glass),
                word_ink: ink_of(word_tone),
                ..glyph
            }
        }
        Chip::Settings => Face::plain(SETTINGS_GLYPH, None, ink),
    }
}

/// A light's glyph, word and ink. The mode by its own pill glyph and word,
/// in its hue where Claude gives it one (bypass, auto) and the row's ink
/// otherwise; a light whose mode is not known as a dim `◌` and its word. A
/// toggle in flight shows `◐`.
fn light_face(
    light: Light,
    shown: &Reading,
    switching: bool,
    ink: [u8; 3],
    dim: [u8; 3],
) -> (&'static str, &'static str, [u8; 3]) {
    let glyph = |own: &'static str| if switching { PENDING_GLYPH } else { own };
    match shown.mode {
        Some(mode) => (glyph(mode.glyph()), mode.word(), mode.hue().unwrap_or(ink)),
        None => (glyph("\u{25CC}"), light.word(), dim),
    }
}

// ---------------------------------------------------------------------------
// THE FACE TABLE (`docs/DESIGN-harness-autopilot-2026-10-15.md` §§9.2–9.5)
// ---------------------------------------------------------------------------

impl ChipFace {
    /// Every face, in §9.6.0's order.
    #[cfg(test)]
    pub(crate) const ALL: [ChipFace; 11] = [
        ChipFace::Cruise,
        ChipFace::CruiseUntil,
        ChipFace::Land,
        ChipFace::Landed,
        ChipFace::Done,
        ChipFace::Spent,
        ChipFace::Theirs,
        ChipFace::Held,
        ChipFace::Off,
        ChipFace::MasterOff,
        ChipFace::Unhosted,
    ];

    /// The face a chip under `view` wears with `glass`: an armed timer under
    /// Cruise; under Land a running Codex goal first, then the task's face
    /// (open and unknown read as Land's own); v1's face everywhere else. A
    /// timer shows only under Cruise (it exists only beside that word, and
    /// the master switch outranks it), a face only under Land (§9.4.1:
    /// Cruise and Off keep v1's face).
    #[must_use]
    pub(crate) const fn of(view: AutopilotView, glass: &ChipGlass) -> Self {
        match view {
            AutopilotView::Cruise if glass.land_at.is_some() => ChipFace::CruiseUntil,
            AutopilotView::Cruise => ChipFace::Cruise,
            AutopilotView::Land if glass.held => ChipFace::Held,
            AutopilotView::Land => match glass.task {
                Some(TaskFace::Landed) => ChipFace::Landed,
                Some(TaskFace::Done) => ChipFace::Done,
                Some(TaskFace::Spent) => ChipFace::Spent,
                Some(TaskFace::Theirs) => ChipFace::Theirs,
                Some(TaskFace::Open) | None => ChipFace::Land,
            },
            AutopilotView::Off => ChipFace::Off,
            AutopilotView::MasterOff => ChipFace::MasterOff,
            AutopilotView::Unhosted => ChipFace::Unhosted,
        }
    }

    /// Whether it is a STANDING FACE of v2's — wider than v1's at rest for
    /// as long as its fact stands (an armed timer, a task's end, a held
    /// goal) — which a rule too narrow for it gives down to its glyph
    /// ([`Keep::GlyphOnly`], §9.3.2) rather than giving the chip up.
    #[must_use]
    pub(crate) const fn standing(self) -> bool {
        matches!(
            self,
            ChipFace::CruiseUntil
                | ChipFace::Landed
                | ChipFace::Done
                | ChipFace::Spent
                | ChipFace::Theirs
                | ChipFace::Held
        )
    }

    /// The chip's glyph, and its [`Keep::GlyphOnly`] rung: `∞`, `⤓`, `✓`,
    /// `○` or `⊘`.
    #[must_use]
    pub(crate) const fn glyph(self) -> &'static str {
        match self {
            ChipFace::Cruise | ChipFace::CruiseUntil => AutopilotView::Cruise.glyph(),
            ChipFace::Land | ChipFace::Spent | ChipFace::Theirs | ChipFace::Held => {
                AutopilotView::Land.glyph()
            }
            ChipFace::Landed | ChipFace::Done => "\u{2713}",
            ChipFace::Off => AutopilotView::Off.glyph(),
            ChipFace::MasterOff => AutopilotView::MasterOff.glyph(),
            ChipFace::Unhosted => AutopilotView::Unhosted.glyph(),
        }
    }

    /// The word drawn a space after the glyph: none for Cruise (its `∞`
    /// alone), the timer's `⤓` (its clock follows it, [`Self::clock`]), and
    /// each face's word.
    #[must_use]
    pub(crate) const fn word(self) -> Option<&'static str> {
        match self {
            ChipFace::Cruise => AutopilotView::Cruise.word(),
            ChipFace::CruiseUntil => Some(AutopilotView::Land.glyph()),
            ChipFace::Land => AutopilotView::Land.word(),
            ChipFace::Landed => Some("landed"),
            ChipFace::Done => Some("done"),
            ChipFace::Spent | ChipFace::Theirs => Some("quiet"),
            ChipFace::Held => Some("held"),
            ChipFace::Off => AutopilotView::Off.word(),
            ChipFace::MasterOff => AutopilotView::MasterOff.word(),
            ChipFace::Unhosted => AutopilotView::Unhosted.word(),
        }
    }

    /// The clock drawn right after the word, with no space (`⤓18:00`): the
    /// armed timer's alone. Never a countdown, so nothing ticks.
    #[must_use]
    pub(crate) fn clock(self, glass: &ChipGlass) -> Option<Clock> {
        match self {
            ChipFace::CruiseUntil => glass.land_at.map(|at| at.clock),
            _ => None,
        }
    }

    /// The glyph's tone and the word's: Cruise's accent, Land's amber — the
    /// timer's `∞` Cruise's and its `⤓18:00` Land's — and dim where the
    /// harness types nothing (a quiet landing, a held goal) or runs nothing.
    #[must_use]
    pub(crate) const fn tones(self) -> (ChipTone, ChipTone) {
        match self {
            ChipFace::Cruise => (ChipTone::Accent, ChipTone::Accent),
            ChipFace::CruiseUntil => (ChipTone::Accent, ChipTone::Amber),
            ChipFace::Land | ChipFace::Landed | ChipFace::Done => {
                (ChipTone::Amber, ChipTone::Amber)
            }
            ChipFace::Spent
            | ChipFace::Theirs
            | ChipFace::Held
            | ChipFace::Off
            | ChipFace::MasterOff
            | ChipFace::Unhosted => (ChipTone::Dim, ChipTone::Dim),
        }
    }

    /// The MODE PHRASE: the title's head and the whole short title, written
    /// with neither of [`short_title`]'s cut marks (` — `, ` · `), so a
    /// narrow rule shows it whole. The timer's names its time, and its
    /// weekday where it lands on another local date.
    #[must_use]
    pub(crate) fn phrase(self, glass: &ChipGlass) -> std::borrow::Cow<'static, str> {
        let fixed = match self {
            ChipFace::Cruise => "autopilot: cruise",
            ChipFace::CruiseUntil => {
                let Some(at) = glass.land_at else {
                    return "autopilot: cruise".into();
                };
                let day = at
                    .other_day
                    .map_or(String::new(), |d| format!("{} ", d.short()));
                return format!(
                    "autopilot: cruise until {day}{}, then land",
                    at.clock.as_str()
                )
                .into();
            }
            ChipFace::Land => "autopilot: land",
            ChipFace::Landed => "autopilot: land, landed",
            ChipFace::Done => "autopilot: land, done",
            ChipFace::Spent | ChipFace::Theirs => "autopilot: land, quiet",
            ChipFace::Held => "autopilot: land, held",
            ChipFace::Off => "autopilot: off",
            ChipFace::MasterOff => "harness off in Settings",
            ChipFace::Unhosted => "no harness: this aterm has no control socket",
        };
        fixed.into()
    }

    /// What the mode does, after the phrase and ` — ` in the whole title:
    /// none where the phrase says it all (no harness runs).
    #[must_use]
    pub(crate) fn explanation(self) -> Option<std::borrow::Cow<'static, str>> {
        use aterm_agent::supervise::policy::turn_end::LAND_NUDGES;
        let fixed = match self {
            ChipFace::Cruise | ChipFace::CruiseUntil => "keeps the work moving",
            ChipFace::Land => "wrapping up",
            ChipFace::Landed => {
                "the agent finished after the land instruction; the harness types nothing more \
                 until someone else's turn"
            }
            ChipFace::Done => {
                "the agent said it was finished before a landing was asked; the harness types \
                 nothing more until someone else's turn"
            }
            ChipFace::Spent => {
                return Some(
                    format!(
                        "no done reply after the instruction and {LAND_NUDGES} nudges; the \
                         harness types nothing more but a land answer to a question"
                    )
                    .into(),
                );
            }
            ChipFace::Theirs => {
                "a person's own exchange; the harness types nothing until someone else's turn ends"
            }
            ChipFace::Held => "Codex is pursuing its /goal, and Land types nothing while it runs",
            ChipFace::Off => "hands off",
            ChipFace::MasterOff | ChipFace::Unhosted => return None,
        };
        Some(fixed.into())
    }

    /// What a click does (`click: …`): the next mode, `land now` from a
    /// timed Cruise (which also drops the timer), and Settings ▸ Harness where
    /// no harness runs.
    #[must_use]
    pub(crate) const fn click(self) -> &'static str {
        match self {
            ChipFace::Cruise => "land",
            ChipFace::CruiseUntil => "land now",
            ChipFace::Land
            | ChipFace::Landed
            | ChipFace::Done
            | ChipFace::Spent
            | ChipFace::Theirs
            | ChipFace::Held => "off",
            ChipFace::Off => "cruise",
            ChipFace::MasterOff | ChipFace::Unhosted => "open Settings \u{25b8} Harness",
        }
    }

    /// The chip's ACCESSIBILITY LABEL (§9.2.1): the mode and its state in
    /// words, never a glyph — letters, digits, spaces and `.:,` alone, so a
    /// screen reader says nothing it cannot pronounce (`Autopilot: cruise.
    /// Lands at 18:00.`, `Autopilot: land. Landed at 17:42.`).
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "the chips' accessibility nodes speak it (design §9.6.1); until then the \
                      face table's tests do"
        )
    )]
    #[must_use]
    pub(crate) fn spoken(self, glass: &ChipGlass) -> String {
        use aterm_agent::supervise::policy::turn_end::LAND_NUDGES;
        let at = |what: &str| match glass.task_at {
            Some(clock) => format!("Autopilot: land. {what} at {}.", clock.as_str()),
            None => format!("Autopilot: land. {what}."),
        };
        match self {
            ChipFace::Cruise => "Autopilot: cruise.".to_owned(),
            ChipFace::CruiseUntil => match glass.land_at {
                Some(LandAtGlass {
                    clock,
                    other_day: Some(day),
                }) => format!(
                    "Autopilot: cruise. Lands {} at {}.",
                    day.long(),
                    clock.as_str()
                ),
                Some(LandAtGlass { clock, .. }) => {
                    format!("Autopilot: cruise. Lands at {}.", clock.as_str())
                }
                None => "Autopilot: cruise.".to_owned(),
            },
            ChipFace::Land => "Autopilot: land.".to_owned(),
            ChipFace::Landed => at("Landed"),
            ChipFace::Done => at("Done"),
            ChipFace::Spent => {
                format!("Autopilot: land. Quiet: no done reply after {LAND_NUDGES} nudges.")
            }
            ChipFace::Theirs => {
                "Autopilot: land. Quiet: a person is driving the session.".to_owned()
            }
            ChipFace::Held => "Autopilot: land. Held while Codex pursues its goal.".to_owned(),
            ChipFace::Off => "Autopilot: off.".to_owned(),
            ChipFace::MasterOff => "Autopilot: off. The harness is off in Settings.".to_owned(),
            ChipFace::Unhosted => "Autopilot: off. This aterm has no control socket.".to_owned(),
        }
    }
}

/// THE AUTOPILOT CHIP'S TITLE under `view` with `glass`, whole and short
/// (§9.2.1, §9.3.2, §9.4.1, §9.5.4), as hover, the keyboard's selection
/// and the page read it. WHOLE: the mode phrase; ` — ` and what the mode
/// does; ` · last: ` and the `trail` where acts were taken (`""`: none — the
/// trail is the act ticker's to write, newest first with each act's clock
/// time); ` · click: ` and what a click does; and ` · right-click: more`
/// where the chip's menu opens (`menu`). SHORT: the mode phrase alone, which
/// holds neither cut mark: [`short_title`] cuts the whole to it. With
/// the default glass, no trail and no menu it is v1's title
/// (`AutopilotView::title`). Built when a hover or a selection asks, never
/// at rest — but on EVERY frame one is up (`WindowLights::block_into`), so
/// the whole title is sized once from its parts: two allocations for a
/// face of v1's, the whole and the short, as v1's copies of its static
/// titles made — never one regrowth per part.
#[must_use]
pub(crate) fn autopilot_title(
    view: AutopilotView,
    glass: &ChipGlass,
    trail: &str,
    menu: bool,
) -> (String, String) {
    const EXPLAINS: &str = " \u{2014} ";
    const LAST: &str = " \u{b7} last: ";
    const CLICK: &str = " \u{b7} click: ";
    const MORE: &str = " \u{b7} right-click: more";
    let face = ChipFace::of(view, glass);
    let phrase = face.phrase(glass);
    let explanation = face.explanation();
    let click = face.click();
    let mut whole = String::with_capacity(
        phrase.len()
            + explanation.as_ref().map_or(0, |e| EXPLAINS.len() + e.len())
            + if trail.is_empty() {
                0
            } else {
                LAST.len() + trail.len()
            }
            + CLICK.len()
            + click.len()
            + if menu { MORE.len() } else { 0 },
    );
    whole.push_str(&phrase);
    if let Some(explanation) = explanation {
        whole.push_str(EXPLAINS);
        whole.push_str(&explanation);
    }
    if !trail.is_empty() {
        whole.push_str(LAST);
        whole.push_str(trail);
    }
    whole.push_str(CLICK);
    whole.push_str(click);
    if menu {
        whole.push_str(MORE);
    }
    let short = short_title(&whole).to_owned();
    (whole, short)
}

/// THE ACT'S LABEL (§9.2.1): its glyph and its word a space apart
/// (`✓ approved`), drawn right of the chips for its dwell in the CHIP'S ink —
/// Cruise's accent, Land's amber, dim where no harness carries the work.
/// Static text: no short form (a lone glyph in the rule reads as a stray
/// mark), and nothing to format.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the act ticker (design §9.6.1) draws it; until then the face table's tests do"
    )
)]
#[must_use]
pub(crate) const fn act_label(
    view: AutopilotView,
    act: Act,
) -> (&'static str, &'static str, ChipTone) {
    (act.glyph(), act.word(), view.tone())
}

/// The chip's ACCESSIBILITY DESCRIPTION (§9.2.1): the trail's newest act in
/// words and the clock time it arrived (`Last: approved at 14:05.`) —
/// a description, never a live region, so a reader is not interrupted by
/// every act. `None` before any act.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the chips' accessibility nodes describe it (design §9.6.1); until then the \
                  face table's tests do"
    )
)]
#[must_use]
pub(crate) fn spoken_last(glass: &ChipGlass) -> Option<String> {
    glass
        .last
        .map(|(act, clock)| format!("Last: {} at {}.", act.spoken(), clock.as_str()))
}

/// The glass's part of [`WindowLights::autopilot_term`]: bits 24–55 of
/// §9.2.3's table, `reading` whether someone is reading the chip's title
/// (hover or selection), the only time the trail's generation counts. The
/// default glass packs to 0, so the term is v1's.
fn glass_bits(glass: ChipGlass, reading: bool) -> u64 {
    let act = glass.lit.map_or(0, |act| 1 + act as u64);
    let face = match glass.task {
        None | Some(TaskFace::Open) => 0,
        Some(TaskFace::Landed) => 1,
        Some(TaskFace::Done) => 2,
        Some(TaskFace::Spent) => 3,
        Some(TaskFace::Theirs) => 4,
    };
    let timer = glass
        .land_at
        .map_or(0, |at| 1 + u64::from(at.clock.minute()));
    let other_day = glass.land_at.is_some_and(|at| at.other_day.is_some());
    let trail = if reading { u64::from(glass.trail) } else { 0 };
    act << 24
        | face << 32
        | u64::from(glass.held) << 35
        | timer << 36
        | u64::from(other_day) << 47
        | trail << 48
}

/// What one frame's reading of a pane asks of the host.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Step {
    /// Claude answered a toggle's input: wake the loop, which reads the
    /// engine and decides the next step.
    wake: bool,
}

/// Row `r` of `t`'s live screen as text, trailing blanks trimmed — what
/// `aterm ctl text` reads.
fn row_text(t: &aterm_core::terminal::Terminal, r: usize) -> String {
    let mut line = t
        .get_line_text(i32::try_from(r).unwrap_or(i32::MAX), None)
        .unwrap_or_default();
    line.truncate(line.trim_end().len());
    line
}

/// `read` inside the reader's panic fence for `session`'s live screen: a
/// panic is warned once and read as no composer. A toggle is refused
/// (`NotShown`, so nothing is pressed off an unread screen), a mode return
/// takes no step, and the footer lays in no upgrade note
/// (`App::claude_composer_now`).
fn fenced_screen_now<T>(
    session: u64,
    rows: &[String],
    read: impl FnOnce() -> Option<T>,
) -> Option<T> {
    crate::reader_guard::read_or_none(session, "Claude Code live screen", rows, read).flatten()
}

/// `blank`'s ink halfway to its ground: the off light and the title.
fn dim_of(blank: RenderCell) -> [u8; 3] {
    let mix = |a: u8, b: u8| ((u16::from(a) + u16::from(b)) / 2) as u8;
    [
        mix(blank.fg[0], blank.bg[0]),
        mix(blank.fg[1], blank.bg[1]),
        mix(blank.fg[2], blank.bg[2]),
    ]
}

/// Ask `ws`'s window, where it has one, for a frame.
fn redraw(ws: &crate::WindowState) {
    if let Some(w) = ws.os_window.as_ref() {
        w.request_redraw();
    }
}

/// Post the footer wake for `session` after `delay` — a toggle's deadline
/// and a refusal's end are times the grid has no damage to announce.
fn wake_after(session: u64, delay: Duration) {
    let _ = std::thread::Builder::new()
        .name("aterm-claude-light-timer".into())
        .spawn(move || {
            // A sleep and one posted wake: nothing is blocked on it.
            crate::qos::set_self(crate::qos::Role::Background);
            std::thread::sleep(delay);
            crate::claude_footer::post_changed(session);
        });
}

/// What the painter needs for one pane: the title and the chips (see
/// [`WindowLights::block`]) and the columns each chip spans in them.
#[derive(Debug, Default)]
pub(crate) struct Block {
    /// `Light: what`, or a harness chip's title — or empty.
    pub(crate) title: Vec<RenderCell>,
    /// `what` alone, for a pane with no room for the whole title.
    pub(crate) short_title: Vec<RenderCell>,
    /// The title is a REASON ([`what_of`]), which outranks the branch and
    /// the path in the rule; any other title gives way before every fact.
    pub(crate) reason: bool,
    /// The chips, one blank apart (the rule's layout pads the block).
    pub(crate) lights: Vec<RenderCell>,
    /// The cells the block holds in the rule (`footer::LightsWidth`'s
    /// chips): its chips' — or, where a rule short of room gave up a drawn
    /// chip ([`Keep`] past [`Keep::Drawn`]), all the room it was fitted to.
    /// The facts give way before the chips (`footer::fit_rule`), so the
    /// room a given-up chip leaves stays the rule's: no fact a wider rule
    /// gave up comes back beside fewer chips.
    pub(crate) holds: usize,
    cols: Vec<(std::ops::Range<usize>, Chip)>,
}

impl Block {
    /// Whether `chip` is drawn.
    pub(crate) fn shows(&self, chip: impl Into<Chip>) -> bool {
        let chip = chip.into();
        self.cols.iter().any(|(_, c)| *c == chip)
    }
}

#[cfg(test)]
impl Block {
    /// A block of `lights` (the chips' cells, as one chip) and its titles,
    /// for the rule's layout tests (`claude_footer`).
    pub(crate) fn of_chips(
        lights: Vec<RenderCell>,
        title: Vec<RenderCell>,
        short_title: Vec<RenderCell>,
        reason: bool,
    ) -> Block {
        Block {
            cols: vec![(0..lights.len(), Chip::Light(Light::Mode))],
            title,
            short_title,
            reason,
            holds: lights.len(),
            lights,
        }
    }

    /// The block a session at rest draws under `view` — the harness's two
    /// chips alone, as the painter builds them — for the rule's layout tests
    /// (`claude_footer`).
    pub(crate) fn of_autopilot(view: AutopilotView, blank: RenderCell, accent: [u8; 3]) -> Block {
        let mut w = WindowLights::default();
        let now = Instant::now();
        w.note_autopilot(0, view.mode(), view, now);
        w.block(0, blank, now, accent, Keep::Drawn)
    }
}

impl App {
    /// Fold one Claude Code pane's reading of its screen into its lights
    /// (`read`: `lights::read_lights` behind its panic fence, taken with the
    /// pane's rows and kept with them — `crate::claude_footer`'s readings):
    /// remember what each shows, and wake the loop when a toggle in flight
    /// was answered. Called for EVERY Claude Code pane each frame, painted
    /// footer or not.
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

    /// THE HARNESS'S GLASS on session `local_id`'s Autopilot chip
    /// ([`ChipGlass`]): its act, its task's face, its timer, a Codex goal's
    /// hold. Read beside the session's Autopilot once a frame (the footer's
    /// repaint key and its splice) — so it takes no lock, reads no clock and
    /// allocates nothing: one lookup in the App's glass
    /// (`crate::harness_glass`), none at all while no session has any, none
    /// in the timers' index while no timer is armed and none in the goal
    /// holds while no Codex goal runs. The default glass, v1's chip, for a
    /// session with none.
    #[must_use]
    pub(crate) fn chip_glass(&self, local_id: u64) -> ChipGlass {
        let mut glass = self
            .harness_glass
            .get(&local_id)
            .map_or_else(ChipGlass::default, crate::harness_glass::SessionGlass::chip);
        if !self.autopilot_timers.is_empty() {
            glass.land_at = self.autopilot_timers.glass(local_id);
        }
        if !self.codex_goal_holds.is_empty() {
            glass.held = self.codex_goal_holds.contains(&local_id);
        }
        glass
    }

    /// THE SEAM FOR A CODEX GOAL'S HOLD (design §9.5.4): Codex session
    /// `local_id`'s `/goal` runs (`held`) or no longer does, as the Codex
    /// splice reads it off Codex's status row (`aterm_phase::codex::
    /// goal_state`) — so under Land its chip reads `⤓ held`
    /// ([`Self::chip_glass`]). Whether the hold moved: a move changes the
    /// glass, so the footer's repaint key moves with it, and the caller posts
    /// the footer. Nothing holds one until the Codex chips (§9.6.4); a
    /// retired session's hold goes with it (`retire_session_registration`).
    pub(crate) fn note_codex_goal_hold(&mut self, local_id: u64, held: bool) -> bool {
        if held {
            self.codex_goal_holds.insert(local_id)
        } else {
            self.codex_goal_holds.remove(&local_id)
        }
    }

    /// Fold in `session`'s Autopilot for the footer this frame draws: its
    /// own word and its view, read ONCE per frame by the painter
    /// ([`App::session_autopilot`], before any lock the host must not be
    /// taken under), and its harness glass ([`Self::chip_glass`]) — what the
    /// harness's two chips say, and the read-back that lets an Autopilot click
    /// in flight go.
    pub(crate) fn note_claude_autopilot(
        &mut self,
        wid: WindowId,
        session: u64,
        word: Option<Autopilot>,
        view: AutopilotView,
    ) {
        let glass = self.chip_glass(session);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.claude_lights
                .note_autopilot_at(session, word, view, glass, || {
                    #[cfg(test)]
                    crate::work_counts::harness_clock_read();
                    Instant::now()
                });
        }
    }

    /// The chip block to paint in `session`'s composer rule, its chips in
    /// `room` cells (`footer::chips_room`) — `None` when no chip is drawn:
    /// with no harness host and everything as expected, the rule carries the
    /// facts alone. Short of room, the keyboard's reveal goes first, then
    /// `⚙`, then the light (a deviation, the selection itself), then a
    /// standing face's word ([`Keep::GlyphOnly`]), Autopilot last
    /// ([`Keep`]); a rule too narrow even for `∞` draws none. The block
    /// is laid into a spare of the window's (the harness's two are drawn at
    /// rest, every frame): the painter hands it back once its hits are
    /// noted ([`Self::recycle_claude_lights_block`]), so a frame allocates
    /// nothing for it once the spares are grown.
    pub(crate) fn claude_lights_block(
        &mut self,
        wid: WindowId,
        session: u64,
        blank: RenderCell,
        room: usize,
    ) -> Option<Block> {
        let accent = crate::chrome_band::band_colors(self.theme).accent;
        let lights = &mut self.windows.get_mut(&wid)?.claude_lights;
        let mine = |at: Option<(u64, Chip)>| at.filter(|(id, _)| *id == session).map(|(_, c)| c);
        let (selected, hover) = (mine(lights.selected), mine(lights.hover));
        let mut block = lights.spare_blocks.pop().unwrap_or_default();
        let drawn = lights.fit_into(
            session,
            blank,
            lights_now(),
            accent,
            selected,
            hover,
            room,
            &mut block,
        );
        if drawn {
            Some(block)
        } else {
            lights.spare_blocks.push(block);
            None
        }
    }

    /// Hand a painted block back to window `wid`'s spares
    /// ([`Self::claude_lights_block`]). At most a few are kept — one per
    /// footer a frame paints is all a window uses.
    pub(crate) fn recycle_claude_lights_block(&mut self, wid: WindowId, block: Block) {
        if let Some(ws) = self.windows.get_mut(&wid)
            && ws.claude_lights.spare_blocks.len() < SPARE_BLOCKS
        {
            ws.claude_lights.spare_blocks.push(block);
        }
    }

    /// Every window repaints: a `[claude_footer]` switch changed what their
    /// Claude Code rules say.
    pub(crate) fn redraw_every_window(&self) {
        for ws in self.windows.values() {
            redraw(ws);
        }
    }

    /// `session`'s composer rule is on the glass this frame (chip or none,
    /// facts or none), with `room` cells for its chips — the keyboard's
    /// chord is the lights' there, even with no room for a chip.
    pub(crate) fn note_claude_footer_row(&mut self, wid: WindowId, session: u64, room: usize) {
        if let Some(ws) = self.windows.get_mut(&wid)
            && !ws.claude_lights.footers.iter().any(|(s, _)| *s == session)
        {
            ws.claude_lights.footers.push((session, room));
        }
    }

    /// Record where `block`'s chips landed: frame row `frame_row` (window
    /// grid row `term_row`), the CHIPS starting at frame column `col` — one
    /// hit per cell, so the whole chip is the target — and the blank between
    /// each two, theirs too: a target that does nothing.
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

    /// What of the chips' glass is under window pixel `(x, y)`: a chip, or
    /// `None` for the blank between two — never what a later band painted
    /// over (the link caption yields to no chip: it claims the row in the
    /// chrome register, and the chip under it is gone from the glass).
    pub(crate) fn claude_chips_at(
        &self,
        wid: WindowId,
        x: f64,
        y: f64,
    ) -> Option<(u64, Option<Chip>)> {
        let ws = self.windows.get(&wid)?;
        if ws.claude_lights.hits.is_empty() {
            return None;
        }
        let (row, col) = self.frame_cell_at(wid, x, y)?;
        let hit = ws
            .claude_lights
            .hits
            .iter()
            .find(|h| h.frame_row == row && h.col == col)?;
        (!self.chrome_owns_terminal_row(wid, hit.term_row)).then_some((hit.session, hit.chip))
    }

    /// The pointer left the window: no chip is hovered any more.
    pub(crate) fn clear_claude_light_hover(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid)
            && ws.claude_lights.hover.take().is_some()
        {
            redraw(ws);
        }
    }

    /// The pointer on `at` — a session's chip, or none — in window `wid`,
    /// as a motion over it leaves it ([`Self::track_claude_light_hover`]),
    /// for a test that reads what a hovered frame costs.
    #[cfg(test)]
    pub(crate) fn hover_claude_chip(&mut self, wid: WindowId, at: Option<(u64, Chip)>) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.claude_lights.hover = at;
        }
    }

    /// A mouse press anywhere lets a keyboard selection go: the person has
    /// moved on (a click to another pane and back must not leave a selected
    /// chip waiting to swallow the next Return).
    pub(crate) fn clear_claude_light_selection(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid)
            && ws.claude_lights.selected.take().is_some()
        {
            redraw(ws);
        }
    }

    /// Pointer motion: move the hover (repainting only on a change) and say
    /// what of the chips the pointer is over — a chip, or the blank between
    /// two, which hovers none ([`OverChips`]). Over either the caller keeps
    /// the motion from the grid, so neither a selection nor a PTY motion
    /// report starts.
    pub(crate) fn track_claude_light_hover(&mut self, wid: WindowId, x: f64, y: f64) -> OverChips {
        let at = self.claude_chips_at(wid, x, y);
        let over = at.and_then(|(session, chip)| Some((session, chip?)));
        let Some(ws) = self.windows.get_mut(&wid) else {
            return OverChips::No;
        };
        if ws.selecting {
            return OverChips::No;
        }
        if ws.claude_lights.hover != over {
            ws.claude_lights.hover = over;
            redraw(ws);
        }
        OverChips::of(at)
    }

    /// A left press on a chip acts on it ([`Self::press_claude_chip`]), and
    /// one on the blank between two is taken and does nothing; whether the
    /// press was the chips'. Either is a click away from an open in-grid
    /// rename field ([`App::inline_rename_edit`]) and commits it first, the
    /// rule a click on the grid keeps (`App::on_mouse_input`): `⚙` — and
    /// Autopilot with the master switch off — bring Settings ▸ Harness to the
    /// front, and a field left open there would take the keys meant for it
    /// into the tab's name.
    pub(crate) fn press_claude_light(&mut self, wid: WindowId) -> bool {
        let (x, y) = self
            .windows
            .get(&wid)
            .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
        let Some((session, chip)) = self.claude_chips_at(wid, x, y) else {
            return false;
        };
        if self.inline_rename_edit(wid).is_some() {
            self.clear_strip_press(wid);
            self.settle_rename_edit(wid);
        }
        if let Some(chip) = chip {
            self.press_claude_chip(wid, session, chip);
        }
        true
    }

    /// What a click on `chip` of `session` does — and Return or Space on it
    /// selected: the light toggles with Claude's own input
    /// ([`Self::toggle_claude_light`], unchanged); Autopilot steps the
    /// session's word ([`Self::press_autopilot_chip`]); `⚙` opens Settings ▸
    /// Harness in `wid` ([`App::open_harness_settings`]). Neither harness
    /// chip sends anything to the PTY.
    fn press_claude_chip(&mut self, wid: WindowId, session: u64, chip: Chip) {
        match chip {
            Chip::Light(light) => self.toggle_claude_light(wid, session, light),
            Chip::Autopilot => self.press_autopilot_chip(wid, session),
            Chip::Settings => {
                self.open_harness_settings(wid);
            }
        }
    }

    /// The Autopilot chip's click (design §2.3): the session's EFFECTIVE
    /// mode steps to the next — Cruise → Land → Off → Cruise — written as its
    /// own word ([`App::cycle_session_autopilot`]): in-process, the same
    /// store `meta set autopilot` writes, ringing the host, nothing sent to
    /// the PTY and never a person's keystroke. The write is the window's own
    /// and reads back at once, so the chip takes its new face on the next
    /// frame and a second click right after it steps again (Cruise → Land →
    /// Off as fast as a person clicks). Only a word that did NOT read back
    /// shows `◐` until a frame reads it ([`WindowLights::note_autopilot`]),
    /// let go at its deadline (one like a mode press's, `lights::SETTLE_MS`)
    /// if none does; a click meanwhile does nothing. Where the harness runs
    /// no mode — the master switch off — nothing is written and Settings ▸
    /// Harness opens in `wid` instead.
    fn press_autopilot_chip(&mut self, wid: WindowId, session: u64) {
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
            self.open_harness_settings(wid);
            return;
        };
        let shown = self.cycle_session_autopilot(session);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let lights = ws.claude_lights.sessions.entry(session).or_default();
        if shown.mode() == Some(want) {
            // Read back already: nothing is in flight.
            lights.autopilot = Some(shown);
            lights.autopilot_pending = None;
            redraw(ws);
            return;
        }
        let settle = Duration::from_millis(lights::SETTLE_MS);
        lights.autopilot_pending = Some((want, now + settle));
        redraw(ws);
        // A word that never reads back is let go on this wake.
        wake_after(session, settle);
    }

    /// Flip `light` of `session` with Claude's own input (see the module
    /// header): refused, with the reason on the light, when its state is not
    /// on screen or the input seam takes no press. A light already as
    /// expected (the mode light in bypass or auto) does nothing at all.
    /// Decided from the ENGINE's screen as it is now, not from the last
    /// painted frame: a mode that changed since is the one the toggle starts
    /// from.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "claude_mode_return",
            action = "Click",
            project = "claude_lights::gesture_tests::project_mode_return"
        )
    )]
    pub(crate) fn toggle_claude_light(&mut self, wid: WindowId, session: u64, light: Light) {
        let now = Instant::now();
        // One toggle per SESSION, whichever window started it: the same
        // session shown in two windows must not run two mode cycles at once,
        // each pressing on the other's answers.
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
        let screen = self.claude_screen_now(session);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let reading = lights::read(screen.as_ref());
        // The mode light in a mode the owner expects: nothing to do — its one
        // action is back to bypass or auto, and the session is there. Never a
        // press away from it (owner, 2026-09-27).
        if reading.state(light) == LightState::On {
            return;
        }
        // Don't ask too: it leads on to manual, and forward from there.
        let Some(mode) = reading.mode.filter(|_| lights::deviates(light, &reading)) else {
            refuse(ws, Refusal::NotShown);
            return;
        };
        // The click's own input: the person's (see the module header).
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
        // The toggle reads the engine for its answer on its own timer,
        // painted frames or none.
        wake_after(session, MODE_POLL);
    }

    /// `session`'s screen read straight from its engine (behind the reader's
    /// panic fence) — `None` without a composer on screen or on a reader
    /// panic. The engine's lock is held only to copy the rows out; the reader
    /// runs after it is dropped.
    fn claude_screen_now(&self, session: u64) -> Option<Screen> {
        let rows = self.claude_rows_now(session)?;
        fenced_screen_now(session, &rows, || lights::read_screen(&rows))
    }

    /// `session`'s screen and whether its composer is EMPTY, read straight
    /// from its engine behind the reader's panic fence — by the rule the
    /// upgrade's gate reads it with ([`upgrade::composer_is_empty`]: a dim
    /// suggestion on the caret row is no draft). `None` without a composer
    /// on screen or on a reader panic. The engine's lock is held only to copy
    /// the rows, the cursor and each row's column-2 DIM flag out. Read by the
    /// footer for a live upgrade's note (`crate::claude_footer`: which words,
    /// and none while a turn runs), every frame while one stands.
    ///
    /// [`upgrade::composer_is_empty`]: aterm_agent::harness::upgrade::composer_is_empty
    pub(crate) fn claude_composer_now(&self, session: u64) -> Option<(Screen, bool)> {
        let (rows, cursor, dim_col2) = {
            let entry = self.pool.get(session)?;
            let t = crate::term_lock(&entry.term);
            if t.grid().display_offset() > 0 {
                return None;
            }
            let rows: Vec<String> = (0..usize::from(t.rows()))
                .map(|r| row_text(&t, r))
                .collect();
            let cursor = t.cursor();
            let cursor = (usize::from(cursor.row), usize::from(cursor.col));
            let dim_col2: Vec<bool> = (0..t.rows())
                .map(|row| {
                    t.grid()
                        .row_at_screen(row)
                        .and_then(|cells| cells.get(2))
                        .is_some_and(|cell| cell.flags().contains(aterm_core::grid::CellFlags::DIM))
                })
                .collect();
            (rows, cursor, dim_col2)
        };
        fenced_screen_now(session, &rows, || {
            let caret = aterm_phase::phase::composer_draft(&rows).map(|(caret, _)| caret);
            let dim = caret.is_some_and(|row| dim_col2.get(row).copied().unwrap_or(false));
            let screen = lights::read_screen(&rows)?;
            let empty = aterm_agent::harness::upgrade::composer_is_empty(&rows, Some(cursor), dim);
            Some((screen, empty))
        })
    }

    /// `session`'s live screen copied out of its engine, its rows as text.
    /// `None` for a gone session or a view scrolled into history. The lock is
    /// held only to copy.
    fn claude_rows_now(&self, session: u64) -> Option<Vec<String>> {
        let entry = self.pool.get(session)?;
        let t = crate::term_lock(&entry.term);
        if t.grid().display_offset() > 0 {
            return None;
        }
        Some(
            (0..usize::from(t.rows()))
                .map(|r| row_text(&t, r))
                .collect(),
        )
    }

    /// The footer wake's lights half: move every mode return along from the
    /// ENGINE ([`Self::advance_claude_mode_returns`]), then give up on toggles
    /// past their deadline (and time the end of the refusal that says so).
    pub(crate) fn drain_claude_lights(&mut self) {
        let now = Instant::now();
        self.advance_claude_mode_returns(now);
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

    /// Every mode return in flight, one step from the ENGINE's screen as it
    /// is now (`claude_screen_now`, not a painted frame — a pane in a
    /// background tab or an occluded window has no frames, and its return
    /// must still finish): the next shift+tab the moment Claude has answered
    /// the last with a mode the owner does not expect either and is not
    /// mid-turn ([`WindowLights::advance`]), sent at once against the reading
    /// that decided it; done at bypass or auto; a lap refused, and a press
    /// the input seam did not take stopped — each naming the mode. A press
    /// still unanswered, or held for a turn to end, keeps ONE poll armed
    /// until its deadline, which [`WindowLights::expire`] keeps. The next
    /// presses are aterm's own, not the person's: `Source::Controller`
    /// (module header).
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "claude_mode_return",
            action = "Press",
            project = "claude_lights::gesture_tests::project_mode_return"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "claude_mode_return",
            action = "Arrive",
            project = "claude_lights::gesture_tests::project_mode_return"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "claude_mode_return",
            action = "Lap",
            project = "claude_lights::gesture_tests::project_mode_return"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "claude_mode_return",
            action = "Reject",
            project = "claude_lights::gesture_tests::project_mode_return"
        )
    )]
    fn advance_claude_mode_returns(&mut self, now: Instant) {
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
                    // Sent against the very reading that decided it.
                    debug_assert_eq!(screen.as_ref().and_then(|s| s.mode), Some(expect));
                    let outcome = self.input_to_session(
                        wid,
                        crate::input::InputEvent::KeySequence(lights::SHIFT_TAB.to_vec()),
                        crate::input::Source::Controller,
                        Some(session),
                        crate::app_input::PressPhase::Initial,
                    );
                    if outcome != crate::input::InputOutcome::Ok {
                        // The press did not go: the session is where the read
                        // that decided it found it — said by name.
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
            // Still in flight: keep one poll armed.
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

    /// The keyboard half (see the module header): `ctrl+shift+tab` selects
    /// the focused pane's next chip; with one selected, Return/Space act as
    /// its click ([`Self::press_claude_chip`]) and Escape lets go. Any other
    /// key lets go and is NOT consumed. Whether the key was the lights'.
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
        // Ordinary typing cannot change the lights when nothing is selected.
        // Avoid resolving the focused pane and scanning the painted hits on
        // every key in that common case. A selected chip still needs the full
        // path so a key can dismiss it, even if its pane has since moved.
        let select_chord = cfg!(target_os = "macos")
            && mods.control_key()
            && mods.shift_key()
            && !mods.alt_key()
            && !mods.super_key()
            && matches!(ev.logical_key, Key::Named(NamedKey::Tab));
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        if ws.claude_lights.selected.is_none()
            && (!select_chord || ws.claude_lights.footers.is_empty())
        {
            return false;
        }
        let Some(session) = self.focused_session_id(wid) else {
            return false;
        };
        let now = Instant::now();
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        // The pane's footer rule is on the glass, with this much room for its
        // chips: they can be selected there, drawn at rest or not.
        let room = ws
            .claude_lights
            .footers
            .iter()
            .find(|(id, _)| *id == session)
            .map(|(_, room)| *room);
        // A selection that is not the focused pane's, or — once a frame has
        // painted it — whose chip is not on the glass (a box covers the
        // composer, the pane narrowed, the chip did not fit), is let go
        // rather than left armed to swallow a later Return.
        let stale = ws.claude_lights.selected.is_some_and(|(id, chip)| {
            id != session
                || (ws.claude_lights.selection_painted
                    && !ws
                        .claude_lights
                        .hits
                        .iter()
                        .any(|h| h.session == id && h.chip == Some(chip)))
        });
        if stale {
            ws.claude_lights.select(None);
            redraw(ws);
        }
        // macOS only: everywhere else `ctrl+shift+tab` is aterm's default
        // `prev_tab` (`keybinding::PLATFORM_DEFAULT_PAIRS`), and this gate runs
        // before the keybindings. The lights stay clickable on every platform.
        // The selection walks every revealable chip — the harness's two, and
        // the lights the reveal draws while it lasts, the ones at rest
        // included — but only onto one the pane's row has ROOM for (the
        // reveal, `⚙` and the light given up first, `Keep`): a selection
        // nobody can see must never take the next Return.
        let order = ws.claude_lights.revealable(session);
        let lights_ = &ws.claude_lights;
        let fits = |chip: Chip| {
            room.is_some_and(|room| {
                lights_
                    .fitted(
                        session,
                        RenderCell::default(),
                        now,
                        [0; 3],
                        Some(chip),
                        None,
                        room,
                    )
                    .is_some_and(|block| block.shows(chip))
            })
        };
        let step = |from: Option<Chip>| -> Option<Chip> {
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
        // The chord is the lights' wherever the pane's footer rule is on the
        // glass — even with no chip to select for want of room: passed on,
        // it would reach Claude as shift+tab, and cycle its mode.
        if select_chord && room.is_some() {
            if let Some(next) = step(selected) {
                ws.claude_lights.select(Some((session, next)));
                redraw(ws);
            }
            return true;
        }
        let Some(chip) = selected else {
            return false;
        };
        let plain = !mods.control_key() && !mods.alt_key() && !mods.super_key();
        match &ev.logical_key {
            Key::Named(NamedKey::Enter | NamedKey::Space) if plain => {
                ws.claude_lights.select(None);
                redraw(ws);
                self.press_claude_chip(wid, session, chip);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The mode light's chip.
    const MODE: Chip = Chip::Light(Light::Mode);

    /// The footer's mark accent, as a theme gives it: Cruise's ink.
    const ACCENT: [u8; 3] = [0, 128, 255];

    #[test]
    fn inert_session_history_does_not_change_the_lights_repaint_key() {
        let now = Instant::now();
        let mut w = WindowLights {
            hover: Some((7, MODE)),
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

    const MANUAL: &str = "  \u{23F8} manual mode on \u{00B7} ? for shortcuts";
    const ACCEPT: &str = "  \u{23F5}\u{23F5} accept edits on (shift+tab to cycle)";

    fn observe(w: &mut WindowLights, rows: &[String]) -> Step {
        w.observe_rows(7, rows, lights::read_lights)
    }

    /// The lights read behind the reader's panic fence: a panicking reader
    /// folds NOTHING in — the lights keep what they showed, a mode cycle in
    /// flight takes no step (no shift+tab queued, its press count and mode
    /// untouched) — and a toggle's own reading is refused (`None`: nothing is
    /// pressed). Controls: the same screen through the real readers DOES step
    /// the cycle, and a toggle's reading of it is a composer.
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
        // A toggle's reading.
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
        // Control: the real reader wakes the loop on the same screen.
        assert!(observe(&mut w, &plan).wake);
    }

    fn start(w: &mut WindowLights, light: Light, mode: Mode, now: Instant) {
        let s = w.sessions.entry(7).or_default();
        s.pending = Some(Pending {
            light,
            started: mode,
            mode_at_press: mode,
            seen: Vec::new(),
            presses: 1,
            deadline: now + Duration::from_millis(lights::SETTLE_MS),
            next_poll: None,
            held_mid_turn: false,
        });
    }

    /// The engine's reading of one pane: `rule`, `mode_row`, empty prompt.
    fn engine(rule: &str, mode_row: &str) -> Option<Screen> {
        lights::read_screen(&screen(rule, mode_row, ""))
    }

    /// A mode return presses again only when Claude ANSWERED the last press
    /// — each press with its OWN deadline — waits under a box, and stops at
    /// the first mode the owner expects: auto, in a session whose cycle has
    /// no bypass. A frame only wakes the loop; it never decides.
    #[test]
    fn a_mode_return_presses_until_an_expected_mode_shows() {
        let now = Instant::now();
        let rule = "\u{2500}".repeat(60);
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&rule, ACCEPT, ""));
        start(&mut w, Light::Mode, Mode::AcceptEdits, now);
        assert_eq!(
            w.advance(7, engine(&rule, ACCEPT).as_ref(), now),
            Advance::Wait,
            "no answer yet: no second press"
        );
        assert!(
            observe(&mut w, &screen(&rule, PLAN, "")).wake,
            "a frame that shows the answer wakes the loop"
        );
        assert!(
            w.sessions[&7].pending.as_ref().unwrap().presses == 1,
            "and decides nothing"
        );
        let later = now + Duration::from_millis(2000);
        assert_eq!(
            w.advance(7, engine(&rule, PLAN).as_ref(), later),
            Advance::Press { expect: Mode::Plan },
            "answered with plan: press again"
        );
        let p = w.sessions[&7].pending.clone().unwrap();
        assert_eq!(
            (p.presses, p.deadline),
            (2, later + Duration::from_millis(lights::SETTLE_MS)),
            "the press's own deadline"
        );
        assert_eq!(
            w.advance(7, engine(&rule, PLAN).as_ref(), later),
            Advance::Wait,
            "one press per answer"
        );
        assert_eq!(
            w.advance(7, None, later),
            Advance::Wait,
            "a box: nothing pressed"
        );
        assert_eq!(
            w.advance(7, engine(&rule, AUTO).as_ref(), later),
            Advance::Arrived
        );
        assert!(w.sessions[&7].pending.is_none(), "auto reached");
        assert!(
            matches!(w.sessions[&7].notice, Some((Light::Mode, _))),
            "the arrival is said on the chip for a moment"
        );
        assert_eq!(w.sessions[&7].shown.state(Light::Mode), LightState::On);
        assert_eq!(
            w.advance(7, engine(&rule, BYPASS).as_ref(), later),
            Advance::Wait,
            "never a press on from an expected mode"
        );
    }

    /// Claude mid-turn, on the mode row.
    const BUSY_ACCEPT: &str = "  \u{23F5}\u{23F5} accept edits on \u{00B7} esc to interrupt";

    /// A BOX OPENS ONLY MID-TURN, and a shift+tab that lands in one is its
    /// answer (Claude Code 2.1.283's edit prompt binds it to "yes, allow
    /// edits for this session"). So aterm's own next press, answered while
    /// Claude is mid-turn, WAITS for the turn to end — within the last
    /// press's deadline — and a return still held there at the deadline
    /// says so, naming the mode and that a click goes on. Control: the same
    /// answer at a turn's end is pressed through at once.
    #[test]
    fn a_follow_up_press_waits_out_a_turn_and_a_held_return_says_so() {
        let now = Instant::now();
        let rule = "\u{2500}".repeat(60);
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&rule, MANUAL, ""));
        start(&mut w, Light::Mode, Mode::Manual, now);
        assert_eq!(
            w.advance(7, engine(&rule, BUSY_ACCEPT).as_ref(), now),
            Advance::Wait,
            "answered mid-turn: held"
        );
        assert!(w.sessions[&7].pending.as_ref().unwrap().held_mid_turn);
        assert_eq!(
            w.advance(7, engine(&rule, ACCEPT).as_ref(), now),
            Advance::Press {
                expect: Mode::AcceptEdits
            },
            "control: the turn ended, the press goes"
        );
        assert!(!w.sessions[&7].pending.as_ref().unwrap().held_mid_turn);
        // Held to the deadline: stopped, and said so.
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&rule, MANUAL, ""));
        start(&mut w, Light::Mode, Mode::Manual, now);
        assert_eq!(
            w.advance(7, engine(&rule, BUSY_ACCEPT).as_ref(), now),
            Advance::Wait
        );
        let due = now + Duration::from_millis(lights::SETTLE_MS);
        assert_eq!(w.expire(due), (true, vec![7]));
        assert!(matches!(
            w.sessions[&7].refused,
            Some((Light::Mode, Refusal::StoppedMidTurn(Mode::AcceptEdits), _))
        ));
        let title: String = w
            .block(7, blank(), due, ACCENT, Keep::Revealed)
            .title
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(
            title
                .starts_with("Permission mode: stopped in accept edits mode mid-turn; click again"),
            "{title:?}"
        );
    }

    /// EACH PRESS ITS OWN DEADLINE, and a stop names its mode. The retired
    /// single deadline set at the click (2.5 s for up to four presses) gave up
    /// on a return Claude was still answering and said "Claude did not
    /// switch" of a session it had switched; here the second press, sent 2 s
    /// in, is still waited for at 3 s — and only past ITS deadline is the
    /// return stopped, saying where.
    #[test]
    fn a_mode_return_stops_only_past_its_last_presss_deadline_and_names_the_mode() {
        let now = Instant::now();
        let rule = "\u{2500}".repeat(60);
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&rule, MANUAL, ""));
        start(&mut w, Light::Mode, Mode::Manual, now);
        let two = now + Duration::from_millis(2000);
        assert_eq!(
            w.advance(7, engine(&rule, ACCEPT).as_ref(), two),
            Advance::Press {
                expect: Mode::AcceptEdits
            }
        );
        assert_eq!(
            w.expire(now + Duration::from_millis(3000)),
            (false, Vec::new()),
            "the click's deadline is past; the press's is not"
        );
        assert!(w.sessions[&7].pending.is_some());
        assert_eq!(
            w.expire(two + Duration::from_millis(lights::SETTLE_MS)),
            (true, vec![7])
        );
        let s = &w.sessions[&7];
        assert!(s.pending.is_none());
        assert!(matches!(
            s.refused,
            Some((Light::Mode, Refusal::StoppedIn(Mode::AcceptEdits), _))
        ));
        let title: String = w
            .block(7, blank(), two, ACCENT, Keep::Revealed)
            .title
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(
            title.starts_with("Permission mode: stopped in accept edits mode"),
            "{title:?}"
        );
    }

    /// The loop may be busy when Claude answers a press. A late wake must
    /// not turn the answer into another press with a fresh deadline; the
    /// expired return stops in the mode Claude actually reached.
    #[test]
    fn a_late_mode_answer_cannot_restart_an_expired_return() {
        let now = Instant::now();
        let rule = "\u{2500}".repeat(60);
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&rule, MANUAL, ""));
        start(&mut w, Light::Mode, Mode::Manual, now);
        let due = now + Duration::from_millis(lights::SETTLE_MS);

        assert_eq!(
            w.advance(7, engine(&rule, ACCEPT).as_ref(), due),
            Advance::Wait,
            "the elapsed deadline forbids a second shift+tab"
        );
        let p = w.sessions[&7].pending.as_ref().unwrap();
        assert_eq!((p.presses, p.deadline), (1, due));
        assert_eq!(w.expire(due), (true, vec![7]));
        assert!(matches!(
            w.sessions[&7].refused,
            Some((Light::Mode, Refusal::StoppedIn(Mode::AcceptEdits), _))
        ));

        // The same answer before its deadline still makes the next press.
        let mut timely = WindowLights::default();
        start(&mut timely, Light::Mode, Mode::Manual, now);
        assert_eq!(
            timely.advance(
                7,
                engine(&rule, ACCEPT).as_ref(),
                due - Duration::from_millis(1)
            ),
            Advance::Press {
                expect: Mode::AcceptEdits
            }
        );

        // Already at the requested mode: accepting the late observation
        // sends no key and keeps the actual success visible.
        let mut arrived = WindowLights::default();
        start(&mut arrived, Light::Mode, Mode::Manual, now);
        assert_eq!(
            arrived.advance(7, engine(&rule, BYPASS).as_ref(), due),
            Advance::Arrived
        );
        assert!(arrived.sessions[&7].pending.is_none());
    }

    /// THE LIGHT, DEVIATIONS ONLY: at rest — bypass or auto — no light is
    /// drawn, and with no harness host the block is EMPTY. Plan mode draws a
    /// labelled chip, a cell of margin after; the title comes APART from the
    /// chip (only the chip must fit); a box over the composer keeps the last
    /// reading rather than dropping the row. THE HARNESS'S TWO, ALWAYS
    /// (owner, 2026-10-15): with a host, at rest the block is Autopilot and
    /// `⚙` alone — `∞` in the footer's accent, `⚙` in the row's ink, no
    /// title — and the light, a deviation, follows them. NEGATIVE CONTROL:
    /// with no host (`Unhosted`), neither.
    #[test]
    fn only_deviations_are_drawn_and_the_block_keeps_what_it_saw() {
        let now = Instant::now();
        let bare = "\u{2500}".repeat(60);
        let mut w = WindowLights::default();
        for mode in [BYPASS, AUTO] {
            observe(&mut w, &screen(&bare, mode, ""));
            let rest = w.block(7, blank(), now, ACCENT, Keep::Revealed);
            assert!(
                rest.lights.is_empty() && rest.cols.is_empty() && rest.title.is_empty(),
                "nothing at rest: {mode:?}"
            );
        }
        // Control: plan mode.
        observe(&mut w, &screen(&bare, PLAN, ""));
        let block = w.block(7, blank(), now, ACCENT, Keep::Revealed);
        let text: String = block.lights.iter().map(|c| c.ch).collect();
        assert_eq!(text, "\u{23F8} plan");
        assert_eq!(block.cols, vec![(0..6, MODE)]);
        assert!(block.title.is_empty(), "nothing asked for a title");
        assert_eq!(block.lights[0].fg, blank().fg, "plan, in the row's ink");
        w.observe(7, None);
        assert_eq!(
            w.block(7, blank(), now, ACCENT, Keep::Revealed).lights,
            block.lights,
            "a covered composer keeps the last reading"
        );
        w.hover = Some((7, MODE));
        let hovered = w.block(7, blank(), now, ACCENT, Keep::Revealed);
        let title: String = hovered.title.iter().map(|c| c.ch).collect();
        assert_eq!(title, "Permission mode: click for bypass or auto  ");
        assert_eq!(
            hovered.lights.len(),
            block.lights.len(),
            "the title is apart"
        );
        assert!(
            hovered.lights.iter().all(|c| c.bold),
            "the whole hovered chip is marked"
        );

        // The harness's two, at rest, before the light.
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&bare, BYPASS, ""));
        w.note_autopilot(7, None, AutopilotView::Unhosted, now);
        assert!(
            w.block(7, blank(), now, ACCENT, Keep::Revealed)
                .cols
                .is_empty(),
            "no host: no chip"
        );
        w.note_autopilot(7, None, AutopilotView::Cruise, now);
        let rest = w.block(7, blank(), now, ACCENT, Keep::Revealed);
        let text: String = rest.lights.iter().map(|c| c.ch).collect();
        assert_eq!(text, "\u{221e} \u{2699}", "at rest: Autopilot and the gear");
        assert_eq!(
            rest.cols,
            vec![(0..1, Chip::Autopilot), (2..3, Chip::Settings)]
        );
        assert_eq!(
            (rest.lights[0].fg, rest.lights[2].fg),
            (ACCENT, blank().fg),
            "cruise in the accent, the gear in the row's ink"
        );
        assert!(rest.title.is_empty(), "nothing asked for a title");
        observe(&mut w, &screen(&bare, PLAN, ""));
        let text: String = w
            .block(7, blank(), now, ACCENT, Keep::Revealed)
            .lights
            .iter()
            .map(|c| c.ch)
            .collect();
        assert_eq!(text, "\u{221e} \u{2699} \u{23F8} plan", "the light follows");
    }

    /// THE AUTOPILOT CHIP SAYS ITS VIEW (design §2.3): `∞` alone in the
    /// footer's accent, `⤓ land` amber (the auto pill's hue), `○ off` and,
    /// with the master switch off, `⊘ off` dim — a word-less chip with no
    /// trailing space — each titled as the design's table says, short to
    /// what it names; `⚙` titled as the settings. A click in flight shows
    /// `◐` alone until the word it wrote reads back, or its deadline lets it
    /// go.
    #[test]
    fn the_autopilot_chip_says_its_view_in_its_ink() {
        let now = Instant::now();
        let dim = dim_of(blank());
        let mut w = WindowLights::default();
        let rows = [
            (
                AutopilotView::Cruise,
                "\u{221e}",
                ACCENT,
                "autopilot: cruise \u{2014} keeps the work moving \u{b7} click: land",
                "autopilot: cruise",
            ),
            (
                AutopilotView::Land,
                "\u{2913} land",
                AMBER,
                "autopilot: land \u{2014} wrapping up \u{b7} click: off",
                "autopilot: land",
            ),
            (
                AutopilotView::Off,
                "\u{25cb} off",
                dim,
                "autopilot: off \u{2014} hands off \u{b7} click: cruise",
                "autopilot: off",
            ),
            (
                AutopilotView::MasterOff,
                "\u{2298} off",
                dim,
                "harness off in Settings \u{b7} click: open Settings \u{25b8} Harness",
                "harness off in Settings",
            ),
        ];
        let text = |cells: &[RenderCell]| -> String { cells.iter().map(|c| c.ch).collect() };
        for (view, face, ink, title, short) in rows {
            w.note_autopilot(7, view.mode(), view, now);
            w.hover = Some((7, Chip::Autopilot));
            let block = w.block(7, blank(), now, ACCENT, Keep::Drawn);
            let n = face.chars().count();
            assert_eq!(text(&block.lights), format!("{face} \u{2699}"), "{view:?}");
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
            assert_eq!(block.lights[n + 1].fg, blank().fg, "{view:?}: the gear");
            assert_eq!(text(&block.title), format!("{title}  "), "{view:?}");
            assert_eq!(text(&block.short_title), format!("{short}  "), "{view:?}");
            assert!(!block.reason, "a hover's title gives way first");
        }
        w.hover = Some((7, Chip::Settings));
        let block = w.block(7, blank(), now, ACCENT, Keep::Drawn);
        assert_eq!(text(&block.title), format!("{SETTINGS_TITLE}  "));
        assert_eq!(
            text(&block.short_title),
            format!("{}  ", short_title(SETTINGS_TITLE)),
            "a narrow rule keeps what the gear is"
        );
        assert_eq!(
            Some(AMBER),
            Mode::Auto.hue(),
            "land wears the auto pill's amber"
        );

        // A click in flight: `◐` alone, held until the word it wrote reads
        // back — not by a frame that still reads the old one.
        w.hover = None;
        w.note_autopilot(7, None, AutopilotView::Cruise, now);
        w.sessions.get_mut(&7).unwrap().autopilot_pending =
            Some((Autopilot::Land, now + Duration::from_secs(1)));
        let block = w.block(7, blank(), now, ACCENT, Keep::Drawn);
        assert_eq!(text(&block.lights), "\u{25d0} \u{2699}");
        w.note_autopilot(7, None, AutopilotView::Cruise, now);
        assert!(
            w.sessions[&7].autopilot_pending.is_some(),
            "not read back yet"
        );
        w.note_autopilot(7, Some(Autopilot::Land), AutopilotView::Land, now);
        assert!(w.sessions[&7].autopilot_pending.is_none(), "read back");
        assert_eq!(
            text(&w.block(7, blank(), now, ACCENT, Keep::Drawn).lights),
            "\u{2913} land \u{2699}"
        );
        // Never read back: its deadline lets it go, and says so to the loop.
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

    /// A RULE SHORT OF ROOM gives up the keyboard's reveal first, then `⚙`,
    /// then the light, and Autopilot — the mark that says the session is
    /// wrapped — last, and nothing where not even `∞` fits. ONE ORDER: for
    /// every pill, selection and room from 24 cells down, the chips drawn
    /// are a part of those one cell more drew — a chip a rule gave up never
    /// comes back in a narrower one (the review of 2026-10-15: under accept
    /// edits `⚙` was drawn in 19 cells, gone in 17 and back from 14 down to
    /// 3, and under plan Autopilot was gone in 6 and 7 cells and back in 5).
    /// The keyboard's fit asks the same ladder: a selected chip the room
    /// cannot hold is no fit. And what `fitted` MEASURES of a rung is what
    /// the painter lays out of it, so the one block it builds fits. CONTROL:
    /// the walk is no constant — every chip is drawn in the widest room and
    /// none in the narrowest.
    #[test]
    fn a_narrow_rule_gives_up_the_reveal_the_gear_the_light_then_autopilot() {
        let now = Instant::now();
        let bare = "\u{2500}".repeat(60);
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&bare, PLAN, ""));
        w.note_autopilot(7, None, AutopilotView::Cruise, now);
        let fitted = |w: &WindowLights, selected: Option<Chip>, room: usize| {
            w.fitted(7, blank(), now, ACCENT, selected, None, room)
                .map(|b| b.lights.iter().map(|c| c.ch).collect::<String>())
        };
        // `∞ ⚙ ⏸ plan`: 10 cells.
        assert_eq!(
            fitted(&w, None, 10).as_deref(),
            Some("\u{221e} \u{2699} \u{23F8} plan")
        );
        for room in 8..=9 {
            assert_eq!(
                fitted(&w, None, room).as_deref(),
                Some("\u{221e} \u{23F8} plan"),
                "{room}: the gear first"
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
            w.fitted(7, blank(), now, ACCENT, Some(chip), None, room)
                .is_some_and(|b| b.shows(chip))
        };
        assert!(fits(&w, Chip::Settings, 10) && !fits(&w, Chip::Settings, 9));
        assert!((1..=9).all(|room| !fits(&w, Chip::Settings, room)));
        assert!(fits(&w, MODE, 8) && !fits(&w, MODE, 7));
        assert!((1..=10).all(|room| fits(&w, Chip::Autopilot, room)));
        assert!(!fits(&w, Chip::Autopilot, 0));
        // Accept edits: the light alone is 15 cells.
        observe(&mut w, &screen(&bare, ACCEPT, ""));
        assert_eq!(
            fitted(&w, None, 19).as_deref(),
            Some("\u{221e} \u{2699} \u{23F5}\u{23F5} accept edits")
        );
        assert_eq!(
            fitted(&w, None, 17).as_deref(),
            Some("\u{221e} \u{23F5}\u{23F5} accept edits"),
            "the gear first"
        );
        for room in 1..=16 {
            assert_eq!(
                fitted(&w, None, room).as_deref(),
                Some("\u{221e}"),
                "{room}: then the light, and never the gear back"
            );
        }
        // In bypass the light is the reveal's: given up before the gear.
        observe(&mut w, &screen(&bare, BYPASS, ""));
        assert_eq!(
            fitted(&w, Some(Chip::Autopilot), 13).as_deref(),
            Some("\u{221e} \u{2699} \u{23F5}\u{23F5} bypass"),
            "revealed"
        );
        assert_eq!(
            fitted(&w, Some(Chip::Autopilot), 12).as_deref(),
            Some("\u{221e} \u{2699}"),
            "the reveal first"
        );
        // One order, whatever the pill, the selection and Autopilot's face.
        let drawn = |w: &WindowLights, selected: Option<Chip>, room: usize| -> Vec<Chip> {
            w.fitted(7, blank(), now, ACCENT, selected, None, room)
                .map(|b| b.cols.iter().map(|(_, chip)| *chip).collect())
                .unwrap_or_default()
        };
        for view in [AutopilotView::Cruise, AutopilotView::Land] {
            w.note_autopilot(7, view.mode(), view, now);
            for mode_row in [PLAN, ACCEPT, MANUAL, BYPASS] {
                observe(&mut w, &screen(&bare, mode_row, ""));
                for selected in [
                    None,
                    Some(Chip::Autopilot),
                    Some(Chip::Settings),
                    Some(MODE),
                ] {
                    let mut wider = drawn(&w, selected, 24);
                    assert!(
                        wider.contains(&Chip::Autopilot) && wider.contains(&Chip::Settings),
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
                    // Measured as laid out, rung by rung.
                    let s = &w.sessions[&7];
                    for keep in Keep::LADDER {
                        let block = w.block_with(7, blank(), now, ACCENT, selected, None, keep);
                        let on = drawn_at(s, now, selected, None, keep);
                        assert_eq!(
                            chips_width_at(
                                s,
                                Chip::row().filter(|chip| on(*chip)),
                                keep,
                                ACCENT,
                                blank().fg,
                                dim_of(blank())
                            ),
                            block.lights.len(),
                            "{view:?} {mode_row:?} {selected:?} {keep:?}"
                        );
                    }
                }
            }
        }
    }

    /// A FOCUS LEFT ON A CHIP THE RULE GAVE UP never hides the light's
    /// title: a hover — or a selection — still on `⚙` once a narrower rule
    /// dropped it, while the light says where its return stopped, leaves
    /// the light's REASON the title (a reason, which outranks the branch and
    /// the path). CONTROL: with `⚙` on the glass, the focus is the title.
    #[test]
    fn a_focus_on_a_chip_the_rule_gave_up_leaves_the_lights_title() {
        let now = Instant::now();
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&"\u{2500}".repeat(60), PLAN, ""));
        w.note_autopilot(7, None, AutopilotView::Cruise, now);
        w.sessions.get_mut(&7).unwrap().refused = Some((
            Light::Mode,
            Refusal::StoppedIn(Mode::Plan),
            now + REFUSAL_SHOWN,
        ));
        let text = |cells: &[RenderCell]| -> String { cells.iter().map(|c| c.ch).collect() };
        for (selected, hover) in [(None, Some(Chip::Settings)), (Some(Chip::Settings), None)] {
            // `∞ ⏸ plan`: the gear given up.
            let block = w
                .fitted(7, blank(), now, ACCENT, selected, hover, 9)
                .expect("a block");
            assert_eq!(text(&block.lights), "\u{221e} \u{23F8} plan");
            assert_eq!(
                text(&block.title),
                "Permission mode: stopped in plan mode  ",
                "{selected:?} {hover:?}"
            );
            assert!(block.reason, "{selected:?} {hover:?}: a reason");
            // Control: the gear drawn, its title.
            let block = w
                .fitted(7, blank(), now, ACCENT, selected, hover, 10)
                .expect("a block");
            assert!(block.shows(Chip::Settings));
            assert_eq!(text(&block.title), format!("{SETTINGS_TITLE}  "));
            assert!(!block.reason);
        }
    }

    /// The keyboard selection reveals the light at rest and marks it: in
    /// bypass, the chip names the mode.
    #[test]
    fn the_keyboard_selection_reveals_the_light_at_rest() {
        let now = Instant::now();
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&"\u{2500}".repeat(60), BYPASS, ""));
        assert!(
            w.block(7, blank(), now, ACCENT, Keep::Revealed)
                .cols
                .is_empty(),
            "at rest"
        );
        w.selected = Some((7, MODE));
        let block = w.block(7, blank(), now, ACCENT, Keep::Revealed);
        let title: String = block.title.iter().map(|c| c.ch).collect();
        assert_eq!(title, "Permission mode: bypass  ");
        assert_eq!(
            block.lights[0].ch, '\u{23F5}',
            "the expected mode, revealed"
        );
        assert!(block.lights[0].bold);
        assert!(
            w.block(7, blank(), now, ACCENT, Keep::Drawn)
                .shows(Light::Mode),
            "the selection itself is drawn without the reveal"
        );
    }

    #[test]
    fn a_quiet_row_keys_as_zero() {
        let now = Instant::now();
        let mut w = WindowLights::default();
        assert_eq!(w.fingerprint(|| now), 0);
        w.hover = Some((7, MODE));
        let a = w.fingerprint(|| now);
        w.selected = Some((7, MODE));
        assert_ne!(a, 0);
        assert_ne!(a, w.fingerprint(|| now));
    }
}

/// THE LIGHTS' GESTURES, END TO END, IN PROCESS. The control socket cannot
/// drive them: `pointer move` reaches `App::on_cursor_moved` only, and `key` /
/// `mouse` enter the input seam BELOW `App::on_key` and `App::on_mouse_input`.
/// So these drive the winit-level handlers of a headless App: a real engine
/// drawing Claude Code's bottom block, one frame through the capture route's
/// splices (the same `splice_claude_footer` the window runs, which observes
/// the pane, paints the row and records where each light landed), the
/// pointer moved to the pixel of the light the painter RECORDED, a real
/// press — and the bytes the session's PTY sink received, read back off a
/// private socket.
#[cfg(all(test, unix))]
mod gesture_tests {
    use std::io::{ErrorKind, Read};
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use aterm_agent::harness::footer::FooterFacts;
    use aterm_agent::harness::lights::{self, Light, Mode};
    use aterm_agent::supervise::Autopilot;
    use aterm_core::terminal::RenderCell;
    use aterm_session::sink::SinkWriter;
    use winit::event::{ElementState, MouseButton};

    use super::Chip;
    use crate::autopilot::AutopilotView;
    use crate::{App, WindowId, term_lock};

    /// The mode light's chip.
    const MODE: Chip = Chip::Light(Light::Mode);

    /// One synthetic key press and release, as a person taps it.
    #[cfg(target_os = "macos")]
    fn tap(
        app: &mut App,
        wid: WindowId,
        named: winit::keyboard::NamedKey,
        code: winit::keyboard::KeyCode,
        text: &str,
    ) {
        use winit::keyboard::{Key, KeyLocation, PhysicalKey, SmolStr};
        for state in [ElementState::Pressed, ElementState::Released] {
            app.on_key(
                wid,
                winit::event::KeyEvent::synthetic_for_test(
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

    const BYPASS: &str = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)";
    const AUTO: &str = "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)";
    const PLAN: &str = "  \u{23F8} plan mode on (shift+tab to cycle)";
    const ACCEPT: &str = "  \u{23F5}\u{23F5} accept edits on (shift+tab to cycle)";
    const MANUAL: &str = "  \u{23F8} manual mode on \u{00B7} ? for shortcuts";
    /// Claude Code's foreground process group, as the resolver names it.
    const PGID: i32 = 4242;

    /// Close the private PTY observer FOR REAL, so the next write through the
    /// session's sink genuinely fails. Dropping `reader` closes only this
    /// process's descriptor: while another test is forking, the child holds a
    /// copy until it execs — longer on a loaded machine — and meanwhile a
    /// write to the socket still succeeds (on a loaded machine with sibling
    /// tests running, `FollowRejected` read a failed-write fixture's Enter as
    /// accepted). So wait for the sink's own end
    /// to see the hang-up, which it reports only once every copy is gone
    /// (`POLLHUP`; a `shutdown` does not help — a send to a shut-down Unix
    /// socket peer still succeeds on macOS). Bounded, so a copy that never
    /// closes fails here by name instead of as a model mismatch — by a hang
    /// detector, not a timing claim: a sibling's child reaches its exec when
    /// the machine schedules it, and at a landing run's load (100-240) that
    /// is not within any few seconds.
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

    /// A headless App whose one session writes its PTY input to a private
    /// socket (the `pointer_license_tests` fixture's observer).
    fn app() -> (App, WindowId, u64, UnixStream) {
        let (reader, writer) = UnixStream::pair().expect("private PTY observer");
        reader.set_nonblocking(true).expect("nonblocking observer");
        let app = App::headless_for_test_with_sink(Arc::new(SinkWriter::new_owned(writer.into())));
        let wid = WindowId(0);
        let session = app.focused_session_id(wid).expect("the front session");
        (app, wid, session, reader)
    }

    /// The window's harness host in a test: one that supervises nothing
    /// (`Acts` inert, no sessions) — what the harness's chips are drawn for —
    /// shut down when the guard drops, a failing test's included.
    struct Hosted(crate::harness_host::HostHandle);

    impl Drop for Hosted {
        fn drop(&mut self) {
            self.0.shutdown_and_join();
        }
    }

    /// Give `app` a harness host ([`Hosted`]): its posture reads `On`, the
    /// table's default Autopilot (cruise).
    fn host(app: &mut App) -> Hosted {
        let host = crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        );
        app.harness = Some(host.clone());
        Hosted(host)
    }

    /// `ctrl+shift+tab`, as a person presses it (macOS: elsewhere the chord
    /// is aterm's `prev_tab`).
    #[cfg(target_os = "macos")]
    fn chord(app: &mut App, wid: WindowId) {
        use winit::keyboard::{KeyCode, ModifiersState, NamedKey};
        app.windows.get_mut(&wid).expect("window").mods =
            ModifiersState::CONTROL | ModifiersState::SHIFT;
        tap(app, wid, NamedKey::Tab, KeyCode::Tab, "\t");
        app.windows.get_mut(&wid).expect("window").mods = ModifiersState::empty();
    }

    /// [`app`] in a 120-column pane: room for a light's WHOLE title beside
    /// the owner's whole facts. A hover's or a selection's title gives way
    /// first in a narrower rule (`footer::fit_rule`), so a test about what
    /// such a title SAYS runs here; a reason shows at 80 columns too.
    fn wide_app() -> (App, WindowId, u64, UnixStream) {
        let (mut app, wid, session, reader) = app();
        assert!(app.apply_term_resize(wid, 24, 120));
        (app, wid, session, reader)
    }

    /// The pane's columns.
    fn cols(app: &App, session: u64) -> usize {
        let term = app.pool.get(session).expect("session").term.clone();
        usize::from(term_lock(&term).cols())
    }

    /// The owner's whole facts, as [`publish_facts`] publishes them.
    const WHOLE_FACTS: &str = "\u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main \u{2500}";

    /// The facts the resolver publishes for the session's Claude Code
    /// process; without them no footer, and no light, is painted.
    fn publish_facts(app: &App, session: u64) {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(PGID);
        assert!(timeline.set_claude_footer(
            PGID,
            None,
            Some(FooterFacts {
                model: Some("Opus 5.5".into()),
                effort: Some("xhigh".into()),
                path: Some("~/aterm".into()),
                branch: Some("main".into()),
                ..FooterFacts::default()
            }),
        ));
    }

    fn feed(app: &App, session: u64, bytes: &[u8]) {
        let term = app.pool.get(session).expect("session").term.clone();
        term_lock(&term).process(bytes);
    }

    /// Claude Code's bottom block on the 24-row engine (80 columns unless a
    /// test widened it, [`wide_app`]): a transcript row, the composer between
    /// its two pane-wide rules with an EMPTY draft and the cursor at its
    /// caret (row 21, column 2), and `mode` on the last row — with bracketed
    /// paste ON, as Claude Code always has it.
    fn draw_claude(app: &App, session: u64, mode: &str) {
        let rule = "\u{2500}".repeat(cols(app, session));
        let screen = format!(
            "\x1b[?2004h\x1b[2J\x1b[19;1H\u{25CF} done\x1b[21;1H{rule}\x1b[22;1H\u{276F} \
             \x1b[23;1H{rule}\x1b[24;1H{mode}\x1b[22;3H"
        );
        feed(app, session, screen.as_bytes());
    }

    /// The frame row of the pane's footer: the composer's bottom rule, which
    /// the facts and the lights are written into.
    fn footer_row(app: &App, wid: WindowId) -> usize {
        app.windows[&wid].input_scratch.cells.len() - usize::from(app.windows[&wid].rows) + 22
    }

    /// The frame row of Claude's own mode row (the pane's last row), which
    /// the footer never writes.
    fn mode_row(app: &App, wid: WindowId) -> usize {
        footer_row(app, wid) + 1
    }

    /// Claude answers a press: the mode row reads `mode`, the cursor back at
    /// the caret.
    fn redraw_mode_row(app: &App, session: u64, mode: &str) {
        feed(
            app,
            session,
            format!("\x1b[24;1H\x1b[2K{mode}\x1b[22;3H").as_bytes(),
        );
    }

    /// One frame through the capture route's splices (the terminal arm of
    /// `App::render_image`): extract, the strip, then the footer.
    fn frame(app: &mut App, wid: WindowId) {
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

    /// Where the painter put `chip` this frame: (frame row, frame column) of
    /// its first cell.
    fn hit(app: &App, wid: WindowId, chip: impl Into<Chip>) -> (usize, usize) {
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
    fn painted(app: &App, wid: WindowId, chip: impl Into<Chip>) -> bool {
        let chip = chip.into();
        app.windows[&wid]
            .claude_lights
            .hits
            .iter()
            .any(|h| h.chip == Some(chip))
    }

    fn cell_at(app: &App, wid: WindowId, (row, col): (usize, usize)) -> RenderCell {
        app.windows[&wid].input_scratch.cells[row][col]
    }

    /// The row as it reads on the glass (the footer's facts read as set).
    fn row_text(app: &App, wid: WindowId, row: usize) -> String {
        crate::claude_footer::read_glass(&app.windows[&wid].input_scratch.cells[row])
    }

    /// The window pixel at the centre of frame cell `(row, col)` — the
    /// inverse of `App::frame_cell_at`, which the hit test reads, and checked
    /// against it so a geometry slip fails here rather than as a missed click.
    fn px_of(app: &App, wid: WindowId, (row, col): (usize, usize)) -> (f64, f64) {
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

    /// What the session's PTY received: up to a minute for `want` bytes, then
    /// 30 ms more for any that should NOT have come.
    fn pty(reader: &mut UnixStream, want: usize) -> Vec<u8> {
        let mut got = Vec::new();
        let mut buf = [0u8; 256];
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut linger: Option<Instant> = None;
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    let now = Instant::now();
                    if got.len() >= want {
                        if now >= *linger.get_or_insert(now + Duration::from_millis(30)) {
                            break;
                        }
                    } else if now >= deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => panic!("reading the PTY observer: {e}"),
            }
        }
        got
    }

    /// THE CHIP NEVER TOUCHES OTHER INK (main's 35d8c4ca7, a real render on
    /// 2.1.284, 2026-09-28): in auto mode Claude right-aligns `◐ medium ·
    /// /effort` on its mode row, and chips then drawn on that row beside it
    /// glued onto it. The chip is in the composer's bottom rule now, and
    /// Claude's row stays Claude's — so the MEASURED screen, drawn at its own
    /// 144 columns, keeps the tail exactly where Claude put it, and the chip
    /// in the rule has a blank cell on each side: never glued to a rule
    /// glyph, a title or a fact — every chip of the row, the harness's two
    /// beside the light included (a host draws them). CONTROL: the rule's
    /// own glyphs are ink a chip must not touch, and they are there.
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
            draw_claude(&app, session, mode);
            frame(&mut app, wid);
            let claude = row_text(&app, wid, mode_row(&app, wid));
            assert_eq!(claude.trim_end(), mode, "Claude's row is whole");
            if mode == measured {
                // Auto mode is as expected: no chip at rest. Reveal it.
                app.windows.get_mut(&wid).unwrap().claude_lights.selected = Some((session, MODE));
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

    /// A CLICK ON THE MODE LIGHT in manual mode: exactly one shift+tab
    /// reaches the PTY; the light shows `◐` until Claude's screen answers; an
    /// answer that is not yet an expected mode is pressed through from the
    /// WAKE (never from the frame); and the bypass pill resolves it — and no
    /// press ever goes on from there. Run with the mouse untracked and
    /// tracked (SGR 1000/1006): a press the light takes is not a mouse
    /// report, and neither is its release. Controls: the blank before the
    /// light is not a light, and a second click while one toggle is in
    /// flight types nothing.
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

            // Hover: the gap before the light is not one; the light is.
            let (x, y) = px_of(&app, wid, (mode.0, mode.1 - 1));
            app.on_cursor_moved(wid, x, y);
            assert_eq!(app.windows[&wid].claude_lights.hover, None);
            let (x, y) = px_of(&app, wid, mode);
            app.on_cursor_moved(wid, x, y);
            assert_eq!(app.windows[&wid].claude_lights.hover, Some((session, MODE)));
            assert!(pty(&mut reader, 0).is_empty(), "hover types nothing");

            // The click: one shift+tab, and no press or release report.
            app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
            app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
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
            app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
            app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
            assert!(pty(&mut reader, 0).is_empty(), "one toggle in flight");

            // The glass says it is switching; no answer yet, no second press.
            frame(&mut app, wid);
            let mode = hit(&app, wid, Light::Mode);
            assert_eq!(cell_at(&app, wid, mode).ch, '\u{25D0}');
            let text = row_text(&app, wid, mode.0);
            assert!(
                text.contains("switching\u{2026}") && text.contains(WHOLE_FACTS),
                "the title, short in an 80-column pane, beside the whole facts: {text:?}"
            );
            assert_ne!(app.windows[&wid].claude_lights.fingerprint(Instant::now), 0);
            app.on_claude_footer_changed(session);
            assert!(pty(&mut reader, 0).is_empty(), "no answer, no second press");

            // Claude answers with accept edits, then plan: each frame only
            // wakes the loop, whose drain reads the engine and presses.
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

            // Claude answers with bypass: resolved, lit, nothing more sent —
            // bypass is expected, and so is auto after it: no press on.
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
                ('\u{23F5}', Mode::Bypass.hue().unwrap()),
                "the arrival is said on the chip for a moment"
            );
            let text = row_text(&app, wid, mode.0);
            assert!(
                text.contains("\u{23F5}\u{23F5} bypass  Permission mode: bypass ")
                    && text.contains(WHOLE_FACTS),
                "the hover's title, whole in an 80-column pane beside the whole facts: {text:?}"
            );
            // A click on the chip while it says so presses nothing.
            click(&mut app, wid, Light::Mode);
            assert!(
                pty(&mut reader, 0).is_empty(),
                "never pressed away from bypass"
            );
        }
    }

    /// AUTO IS AS EXPECTED AS BYPASS (owner, 2026-09-27: "when I toggle
    /// auto-approve, it turns off auto mode (what?!)"): in auto mode no mode
    /// chip is drawn at all, and a toggle of the mode light (the keyboard's
    /// Return on a revealed chip) presses NOTHING. Control: in plan mode the
    /// chip is drawn and its click presses.
    #[test]
    fn auto_mode_draws_no_mode_chip_and_is_never_left() {
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
        assert!(pty(&mut reader, 0).is_empty(), "auto stays auto");
        let s = &app.windows[&wid].claude_lights.sessions[&session];
        assert!(s.pending.is_none() && s.refused.is_none());
        // Control.
        redraw_mode_row(&app, session, PLAN);
        frame(&mut app, wid);
        click(&mut app, wid, Light::Mode);
        assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB, "plan: one press");
    }

    /// AT REST NO LIGHT IS DRAWN (owner, 2026-09-27: "I don't want to show
    /// the state of parameters that I always expect to be on") — ONLY THE
    /// HARNESS'S TWO (owner, 2026-10-15: "I need some visual indicator that
    /// the claude code session is being wrapped"): bypass or auto, a host —
    /// the rule carries `∞` (cruise, the table's default, in the footer's
    /// accent) and `⚙` (the row's ink) at its left and the facts at its
    /// right, nothing else to hit, the rule known to be on the glass (the
    /// keyboard's way to the lights); and Claude's own row under it, its pill
    /// and hint, is exactly as Claude drew it (owner, 2026-09-28). NEGATIVE
    /// CONTROL: with no host the rule carries the facts alone.
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
                    vec![Chip::Autopilot, Chip::Settings]
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
                    "Permission mode",
                ] {
                    assert!(
                        !text.contains(gone),
                        "{gone:?} in {text:?}: no light, no title"
                    );
                }
                assert_eq!(
                    text.contains("\u{221e} \u{2699}"),
                    hosted,
                    "the harness's two: {text:?}"
                );
                if hosted {
                    let blank = app.windows[&wid].input_scratch.implicit_blank;
                    let accent = crate::chrome_band::band_colors(app.theme).accent;
                    let ink = |chip| cell_at(&app, wid, hit(&app, wid, chip)).fg;
                    assert_eq!(ink(Chip::Autopilot), accent, "cruise: the accent");
                    assert_eq!(ink(Chip::Settings), blank.fg, "the gear: the row's ink");
                }
                // Claude's own row is whole: its pill, its hint.
                let claude = row_text(&app, wid, mode_row(&app, wid));
                assert_eq!(claude.trim_end(), mode, "Claude's row, as drawn");
            }
        }
    }

    /// THE KEYBOARD REACHES EVERY CHIP AT REST (macOS): with no light drawn,
    /// `ctrl+shift+tab` walks the row in its order — Autopilot and `⚙`,
    /// always revealable where a host draws them, then the known light, the
    /// expected mode `⏵⏵ bypass`, revealed — each selection painted on the
    /// glass and typing nothing; Return on the expected mode presses
    /// nothing; the reveal goes with the selection, and the harness's two
    /// stay. NEGATIVE CONTROL: with no host the chord reaches the light
    /// alone, and after the Return nothing is drawn at all.
    #[cfg(target_os = "macos")]
    #[test]
    fn ctrl_shift_tab_reveals_the_lights_at_rest() {
        use winit::keyboard::{KeyCode, NamedKey};

        for hosted in [true, false] {
            let (mut app, wid, session, mut reader) = app();
            let _host = hosted.then(|| host(&mut app));
            publish_facts(&app, session);
            draw_claude(&app, session, BYPASS);
            frame(&mut app, wid);
            assert!(!painted(&app, wid, MODE), "at rest");
            let walk: &[Chip] = if hosted {
                &[Chip::Autopilot, Chip::Settings, MODE]
            } else {
                &[MODE]
            };
            for want in walk {
                chord(&mut app, wid);
                assert_eq!(
                    app.windows[&wid].claude_lights.selected,
                    Some((session, *want)),
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
            assert!(
                pty(&mut reader, 0).is_empty(),
                "Return on the expected mode presses nothing"
            );
            frame(&mut app, wid);
            assert!(
                !painted(&app, wid, MODE),
                "the reveal went with the selection"
            );
            assert_eq!(
                painted(&app, wid, Chip::Autopilot) && painted(&app, wid, Chip::Settings),
                hosted,
                "the harness's two stay"
            );
        }
    }

    /// THE KEYBOARD ROUTE (macOS: elsewhere `ctrl+shift+tab` is `prev_tab`):
    /// the chord selects the focused pane's light and types nothing (again,
    /// it stays on the one light); Return toggles it — one
    /// shift+tab, and NOT the Return; the selection lets go. Control: with
    /// nothing selected, Return is Claude's again.
    #[cfg(target_os = "macos")]
    #[test]
    fn ctrl_shift_tab_selects_a_light_and_return_toggles_it() {
        use winit::keyboard::{KeyCode, ModifiersState, NamedKey};

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
            Some((session, MODE))
        );
        tap(&mut app, wid, NamedKey::Tab, KeyCode::Tab, "\t");
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, MODE)),
            "the one light stays selected"
        );
        assert!(pty(&mut reader, 0).is_empty(), "the chord is the lights'");
        frame(&mut app, wid);
        let auto = hit(&app, wid, Light::Mode);
        let text = row_text(&app, wid, auto.0);
        assert!(text.contains("click for bypass or auto"), "{text:?}");
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
            '\u{25D0}'
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
            text.contains("Permission mode: bypass"),
            "the arrival's notice: {text:?}"
        );
        // Its moment over, back as expected: the chip goes.
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

        // Control: nothing selected, so Return is Claude's.
        tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
        assert_eq!(pty(&mut reader, 1), b"\r");
    }

    /// REGRESSION (review 2026-09-25): in the default 80-column pane a
    /// PENDING mode toggle's title (`Permission mode: switching…  `) never
    /// pushes the lights out of the rule — and never the facts either: the
    /// transient title gives way FIRST (`footer::fit_rule`), so the facts
    /// stay whole; set as a line of type (`footer::set_facts`) they leave
    /// the whole title room beside them in 80 columns, and a pane too narrow
    /// for it gets it short (`footer::fit_rule`'s own tests); the chip it
    /// names does not move.
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
            text.contains("Permission mode: switching\u{2026}"),
            "the title, whole beside the lights and the facts: {text:?}"
        );
        assert_eq!(
            hit(&app, wid, Light::Mode),
            approve,
            "the chip stays under the pointer while its title comes: {text:?}"
        );
    }

    /// A title too long for the pane comes as the reason alone, the chip
    /// beside it: a return stopped in accept edits, in the default 80-column
    /// pane, says where it stopped without its light's name. The REASON
    /// outranks the branch and the path (`footer::fit_rule`): it takes their
    /// room, never the model's or the effort's. In a 120-column pane it
    /// comes whole, beside the whole facts. Control: the chip is there.
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
                    !text.contains("Permission mode") && !text.contains("\u{2387} main"),
                    "no room for the whole title: the reason alone, in the branch's \
                     and the path's room: {text:?}"
                );
            } else {
                assert!(
                    text.contains("Permission mode: stopped in accept edits mode")
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

    /// DON'T ASK GOES FORWARD TOO (the review of 2026-09-27: its chip and
    /// the manual promised a click back, and the click refused): Claude's
    /// shift+tab leads from don't ask on to manual, and from there forward —
    /// four presses to bypass, each on Claude's answer, and none from there.
    /// The chip names the mode in aterm's span of the rule — Claude's own
    /// pill stays on its row — and its title says what a click does.
    #[test]
    fn a_mode_light_in_dont_ask_mode_presses_forward_to_bypass() {
        const DONT_ASK: &str = "  \u{23F5}\u{23F5} don't ask on (shift+tab to cycle)";
        let (mut app, wid, session, mut reader) = wide_app();
        publish_facts(&app, session);
        draw_claude(&app, session, DONT_ASK);
        frame(&mut app, wid);
        let chip = hit(&app, wid, Light::Mode);
        let (x, y) = px_of(&app, wid, chip);
        app.on_cursor_moved(wid, x, y);
        frame(&mut app, wid);
        let text = row_text(&app, wid, chip.0);
        assert!(
            text.contains("\u{23F5}\u{23F5} don't ask") && !text.contains("don't ask on"),
            "the chip, in the rule: {text:?}"
        );
        let claude = row_text(&app, wid, mode_row(&app, wid));
        assert!(
            claude.contains("\u{23F5}\u{23F5} don't ask on"),
            "and Claude's own pill, whole: {claude:?}"
        );
        assert!(
            text.contains("click for bypass or auto"),
            "the title says what a click does: {text:?}"
        );
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB, "the click presses");
        let mut presses = 1;
        for answer in [MANUAL, ACCEPT, PLAN] {
            redraw_mode_row(&app, session, answer);
            app.on_claude_footer_changed(session);
            assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB, "{answer:?}");
            presses += 1;
        }
        redraw_mode_row(&app, session, BYPASS);
        app.on_claude_footer_changed(session);
        assert!(pty(&mut reader, 0).is_empty(), "bypass: done");
        assert_eq!(presses, 4);
        let s = &app.windows[&wid].claude_lights.sessions[&session];
        assert!(s.pending.is_none() && s.refused.is_none());
        assert!(matches!(s.notice, Some((Light::Mode, _))));
    }

    /// REGRESSION (review 2026-09-25): a light selected with ctrl+shift+tab
    /// is let go by any mouse press, so clicking to another pane and back
    /// leaves no selection armed to take the next Return — Return submits
    /// Claude's prompt, as the person expects.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_mouse_press_lets_a_keyboard_selection_go() {
        use winit::keyboard::{KeyCode, ModifiersState, NamedKey};
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
        // The half-width pane's rule has room for the revealed chip beside
        // the model: the mode's light is selected.
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, MODE))
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

    /// A return that ENDED is over: its deadline passed with the press
    /// unanswered (stopped, naming manual), and Claude's late answer then
    /// draws no further press. Control: before the deadline the same answer
    /// is pressed through.
    #[test]
    fn an_ended_mode_return_presses_nothing_more() {
        for ended in [true, false] {
            let (mut app, wid, session, mut reader) = app();
            publish_facts(&app, session);
            draw_claude(&app, session, MANUAL);
            frame(&mut app, wid);
            click(&mut app, wid, Light::Mode);
            assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
            if ended {
                past_deadline(&mut app, wid, session);
                app.on_claude_footer_changed(session);
                assert_eq!(
                    refusal(&app, wid, session),
                    Some((Light::Mode, super::Refusal::StoppedIn(Mode::Manual)))
                );
            }
            redraw_mode_row(&app, session, ACCEPT);
            app.on_claude_footer_changed(session);
            if ended {
                assert!(
                    pty(&mut reader, 0).is_empty(),
                    "the ended return presses nothing"
                );
            } else {
                assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB, "control: in time");
            }
        }
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

    /// Hold `session`'s mode-return press well BEFORE its deadline, however
    /// slowly the test reaches the next drain — once the drive just done
    /// (`since` taken before it) is seen to have given the press its own
    /// full `SETTLE_MS`. The conformance tests fire the tiered checks between
    /// a press and the drain of Claude's answer, and on a loaded machine (a
    /// landing run at load 100-170, the checks' `ty` spawns among it) those
    /// alone can spend the whole 2.5 s: the drain then read the press as
    /// overdue and sent nothing, and the test failed on the empty PTY
    /// (`left: []`, `right: [27, 91, 90]`) — the deadline's case, not the
    /// answer's the test is about. The deadline's own cases stay with
    /// [`past_deadline`] and the tests that time it.
    fn press_not_due(app: &mut App, wid: WindowId, session: u64, since: Instant) {
        let p = app
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.claude_lights.sessions.get_mut(&session))
            .and_then(|s| s.pending.as_mut())
            .expect("in flight");
        // Bounded on both sides: a press that did not re-arm keeps the 60 s
        // hold the previous call left, past `now + SETTLE_MS`.
        let settle = Duration::from_millis(lights::SETTLE_MS);
        assert!(
            since + settle <= p.deadline && p.deadline <= Instant::now() + settle,
            "the drive gives the press its own deadline"
        );
        p.deadline = Instant::now() + Duration::from_secs(60);
    }

    /// THE RETURN RUNS WITHOUT FRAMES (the review of 2026-09-27): a pane that
    /// leaves the screen — a background tab, an occluded window — gets no
    /// frames, and a return decided from painted frames stranded there. Each
    /// press here is decided from the ENGINE by the wake alone, no frame after
    /// the click: manual → accept edits → plan → bypass, then nothing. And
    /// only the click is the person's: the follow-up presses leave the person
    /// clock as the click left it, so the supervisor's grace is the click's
    /// alone. Controls: the click itself stamps it, and a wake before Claude
    /// answers presses nothing.
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
            Mode::Bypass | Mode::Auto => 0,
            Mode::Manual => 1,
            Mode::AcceptEdits => 2,
            Mode::Plan => 3,
            Mode::DontAsk => 4,
        }
    }

    /// The mode return's model state, read off the genuine App: the pill,
    /// the turn in flight and the covering box from the ENGINE; the phase,
    /// the start, the press count, whether the last press is unanswered and
    /// whether a stop is named from the session's lights. `auto`, `fresh`,
    /// `ring`, `overshoot` and `boxed` are the driven action's facts — the
    /// bytes each step sent are asserted against the PTY beside it.
    pub(super) fn project_mode_return(
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

    fn drive_mode_return_action<T>(action: &str, drive: impl FnOnce() -> T) -> T {
        use aterm_spec::xref;

        assert!(xref::reset_entered_anchors());
        let result = drive();
        let entered = xref::entered_anchor_ids();
        xref::disarm_entered_anchors();
        let anchor = xref::refinements()
            .find(|anchor| anchor.machine == "claude_mode_return" && anchor.action == action)
            .expect("the mode-return action has a shipping anchor");
        assert!(
            entered.contains(anchor.entry_id),
            "{action} missed its shipping function"
        );
        result
    }

    /// Fire `action` on the model, then check the App's projected state is a
    /// transition the model allows — and that `historical` (the retired
    /// behaviour's post-state, where given) is one it does NOT: the negative
    /// control, so a pass is never vacuous.
    fn check_mode_return_transition(
        app: &App,
        wid: WindowId,
        session: u64,
        action: &str,
        state: &mut aterm_spec::interp::State,
        historical: Option<&[(&'static str, i64)]>,
    ) {
        let model = aterm_spec::derive::claude_mode_return_model();
        let before = state.clone();
        assert!(model.fire(action, state), "{action} is enabled");
        let observed = project_mode_return(app, wid, session, state);
        let (ok, evidence) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &before,
            &observed,
            Some(action),
            "Claude mode return real conformance",
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
                "Claude mode return negative control",
            );
            assert!(
                !ok,
                "{action} must reject the retired post-state {changes:?}"
            );
        }
    }

    /// TIER-1: the genuine click, drain and expiry against
    /// `claude_mode_return_model`. From manual (and from don't ask): the
    /// click's one press (never armed for later), each next press decided
    /// from the engine, the arrival at bypass. Negative controls: a click
    /// that arms instead of pressing, and the old toggle that pressed on
    /// from an expected mode, are each rejected by the model.
    #[test]
    fn the_mode_return_conforms_to_its_model() {
        const DONT_ASK: &str = "  \u{23F5}\u{23F5} don't ask on (shift+tab to cycle)";
        for dont_ask in [false, true] {
            let (mut app, wid, session, mut reader) = app();
            publish_facts(&app, session);
            let model = aterm_spec::derive::claude_mode_return_model();
            let mut state = model.init_state();
            let mut answers = vec![ACCEPT, PLAN, BYPASS];
            if dont_ask {
                // The person picked don't ask in Claude, from bypass (the
                // model's `DontAsk`, from its expected mode).
                state.insert("mode", 0);
                draw_claude(&app, session, DONT_ASK);
                check_mode_return_transition(&app, wid, session, "DontAsk", &mut state, None);
                answers.insert(0, MANUAL);
            } else {
                draw_claude(&app, session, MANUAL);
            }
            frame(&mut app, wid);
            let since = Instant::now();
            drive_mode_return_action("Click", || click(&mut app, wid, Light::Mode));
            press_not_due(&mut app, wid, session, since);
            assert_eq!(
                pty(&mut reader, 3),
                lights::SHIFT_TAB,
                "the click presses at once"
            );
            check_mode_return_transition(
                &app,
                wid,
                session,
                "Click",
                &mut state,
                Some(&[("sent", 0)][..]),
            );
            let last = answers.len() - 1;
            for (i, answer) in answers.into_iter().enumerate() {
                // Claude answers; the vendor's step, projected all the same.
                redraw_mode_row(&app, session, answer);
                check_mode_return_transition(&app, wid, session, "Answer", &mut state, None);
                if i < last {
                    let since = Instant::now();
                    drive_mode_return_action("Press", || app.on_claude_footer_changed(session));
                    press_not_due(&mut app, wid, session, since);
                    assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB, "{answer:?}");
                    check_mode_return_transition(&app, wid, session, "Press", &mut state, None);
                }
            }
            drive_mode_return_action("Arrive", || app.on_claude_footer_changed(session));
            assert!(pty(&mut reader, 0).is_empty(), "no press on from bypass");
            check_mode_return_transition(
                &app,
                wid,
                session,
                "Arrive",
                &mut state,
                Some(&[("phase", 1), ("sent", 1), ("overshoot", 1)][..]),
            );
        }
    }

    /// TIER-1: Claude answered before a stalled loop next drained the
    /// footer, but that press's deadline had elapsed. The engine reading
    /// can name the mode it reached; it cannot authorize another key.
    #[test]
    fn an_overdue_mode_answer_sends_no_late_follow_up_key() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, MANUAL);
        frame(&mut app, wid);
        let model = aterm_spec::derive::claude_mode_return_model();
        let mut state = model.init_state();
        drive_mode_return_action("Click", || click(&mut app, wid, Light::Mode));
        assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
        check_mode_return_transition(&app, wid, session, "Click", &mut state, None);

        past_deadline(&mut app, wid, session);
        check_mode_return_transition(&app, wid, session, "Timeout", &mut state, None);
        redraw_mode_row(&app, session, ACCEPT);
        check_mode_return_transition(&app, wid, session, "Answer", &mut state, None);
        assert!(
            model.successors("Press", &state).is_empty(),
            "a timed-out answer cannot enable another press"
        );
        drive_mode_return_action("Expire", || app.on_claude_footer_changed(session));
        assert!(pty(&mut reader, 0).is_empty(), "no late shift+tab");
        check_mode_return_transition(
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

        // Historical behavior rearmed the deadline and sent a second key.
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

    /// TIER-1, THE STOPS: every way a started return ends short of bypass or
    /// auto, driven on the genuine App against `claude_mode_return_model` —
    /// and each NAMES the mode it stopped in. (a) A turn in flight: Claude
    /// answers mid-turn, aterm's own next press is HELD (nothing sent), a box
    /// opens over the session and takes nothing, and the deadline stops the
    /// return naming the mode. (b) A cycle without bypass or auto laps back
    /// to its start. (c) The input seam takes no next press. Negative
    /// controls: each stop with its name dropped (the retired "Claude did
    /// not switch" and "input was not accepted"), and a press sent mid-turn,
    /// are rejected by the model.
    #[test]
    fn the_mode_returns_stops_conform_to_its_model_and_are_named() {
        let model = aterm_spec::derive::claude_mode_return_model();
        // (a) Mid-turn: held, boxed, expired by name.
        {
            let (mut app, wid, session, mut reader) = app();
            publish_facts(&app, session);
            draw_claude(&app, session, MANUAL);
            frame(&mut app, wid);
            let mut state = model.init_state();
            let since = Instant::now();
            drive_mode_return_action("Click", || click(&mut app, wid, Light::Mode));
            press_not_due(&mut app, wid, session, since);
            assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
            check_mode_return_transition(&app, wid, session, "Click", &mut state, None);
            redraw_mode_row(&app, session, ACCEPT);
            check_mode_return_transition(&app, wid, session, "Answer", &mut state, None);
            let since = Instant::now();
            drive_mode_return_action("Press", || app.on_claude_footer_changed(session));
            press_not_due(&mut app, wid, session, since);
            assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
            check_mode_return_transition(&app, wid, session, "Press", &mut state, None);
            // A turn starts before Claude answers the press, then it answers.
            redraw_mode_row(&app, session, BUSY_ACCEPT_ROW);
            check_mode_return_transition(&app, wid, session, "TurnStarts", &mut state, None);
            redraw_mode_row(&app, session, BUSY_PLAN);
            check_mode_return_transition(&app, wid, session, "Answer", &mut state, None);
            app.on_claude_footer_changed(session);
            assert!(
                pty(&mut reader, 0).is_empty(),
                "answered mid-turn: aterm's own press is held"
            );
            assert!(
                model.successors("Press", &state).is_empty(),
                "and the model has no press there either"
            );
            // A box opens over the session: it takes nothing.
            feed(&app, session, b"\x1b[2J\x1b[10;1H Do you want to proceed?");
            check_mode_return_transition(&app, wid, session, "Cover", &mut state, None);
            assert_eq!(state["boxed"], 0);
            past_deadline(&mut app, wid, session);
            drive_mode_return_action("Expire", || app.on_claude_footer_changed(session));
            assert!(pty(&mut reader, 0).is_empty(), "nothing into the box");
            check_mode_return_transition(
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
            // Negative control: the retired press mid-turn is a boxed key.
            let buggy = aterm_spec::interp::with_buggy(&model, 1);
            let mut raced = buggy.init_state();
            for action in ["TurnStarts", "Click", "Answer", "Press", "Cover"] {
                assert!(buggy.fire(action, &mut raced), "{action}");
            }
            assert!(!buggy.check_invariant("NoPressIntoABox", &raced));
        }
        // (b) A cycle without bypass or auto: back at the start, named.
        {
            let (mut app, wid, session, mut reader) = app();
            publish_facts(&app, session);
            draw_claude(&app, session, ACCEPT);
            frame(&mut app, wid);
            let mut state = model.init_state();
            state.insert("mode", 2);
            state.insert("start", 2);
            assert!(model.fire("Narrow", &mut state));
            let since = Instant::now();
            drive_mode_return_action("Click", || click(&mut app, wid, Light::Mode));
            press_not_due(&mut app, wid, session, since);
            assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
            check_mode_return_transition(&app, wid, session, "Click", &mut state, None);
            for answer in [PLAN, MANUAL] {
                redraw_mode_row(&app, session, answer);
                check_mode_return_transition(&app, wid, session, "Answer", &mut state, None);
                let since = Instant::now();
                drive_mode_return_action("Press", || app.on_claude_footer_changed(session));
                press_not_due(&mut app, wid, session, since);
                assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB, "{answer:?}");
                check_mode_return_transition(&app, wid, session, "Press", &mut state, None);
            }
            redraw_mode_row(&app, session, ACCEPT);
            check_mode_return_transition(&app, wid, session, "Answer", &mut state, None);
            drive_mode_return_action("Lap", || app.on_claude_footer_changed(session));
            assert!(pty(&mut reader, 0).is_empty(), "no second lap");
            check_mode_return_transition(
                &app,
                wid,
                session,
                "Lap",
                &mut state,
                Some(&[("named", 0)][..]),
            );
            assert_eq!(
                refusal(&app, wid, session),
                Some((Light::Mode, super::Refusal::NotInCycle(Mode::AcceptEdits)))
            );
        }
        // (c) The seam takes no next press: stopped, named.
        {
            let (mut app, wid, session, mut reader) = app();
            publish_facts(&app, session);
            draw_claude(&app, session, MANUAL);
            frame(&mut app, wid);
            let mut state = model.init_state();
            let since = Instant::now();
            drive_mode_return_action("Click", || click(&mut app, wid, Light::Mode));
            press_not_due(&mut app, wid, session, since);
            assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
            check_mode_return_transition(&app, wid, session, "Click", &mut state, None);
            redraw_mode_row(&app, session, ACCEPT);
            check_mode_return_transition(&app, wid, session, "Answer", &mut state, None);
            hang_up(&app, session, reader);
            drive_mode_return_action("Reject", || app.on_claude_footer_changed(session));
            check_mode_return_transition(
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

    /// Claude Code's bottom block drawn `cols` wide — [`draw_claude`] for a
    /// window resized narrower. Only the chip-room test draws it, and that
    /// test is macOS's.
    #[cfg(target_os = "macos")]
    fn draw_claude_cols(app: &App, session: u64, mode: &str, cols: usize) {
        let rule = "\u{2500}".repeat(cols);
        let screen = format!(
            "\x1b[?2004h\x1b[2J\x1b[19;1H\u{25CF} done\x1b[21;1H{rule}\x1b[22;1H\u{276F} \
             \x1b[23;1H{rule}\x1b[24;1H{mode}\x1b[22;3H"
        );
        feed(app, session, screen.as_bytes());
    }

    /// NO SELECTION NOBODY CAN SEE (the review of 2026-09-27): the keyboard
    /// selects only a chip the pane has ROOM for, the reveal giving way
    /// first. At 40 columns, accept edits: the chip drawn at rest stays on
    /// the glass when the chord selects it. The window narrowed to 30
    /// columns, no chip fits: the standing
    /// selection is let go by the next key, and that Return is CLAUDE'S —
    /// never swallowed by a light off the glass. Control: at 30 columns a
    /// fresh chord selects nothing (and reaches Claude as no shift+tab), and
    /// Return is Claude's again. WITH A HOST the walk takes in the harness's
    /// two, but only what the row holds (`Keep`): at 40 columns Autopilot,
    /// `⚙` and the light, round again; at 36 `⚙` is given up, and the walk
    /// is Autopilot and the light; at 30 the light is given up too — and
    /// `⚙` stays given up, never drawn again in a narrower rule — so the
    /// walk is Autopilot alone; and once Escape lets go, the Return is
    /// Claude's.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_keyboard_never_selects_a_chip_the_pane_has_no_room_for() {
        use winit::keyboard::{KeyCode, NamedKey};
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
            Some((session, MODE))
        );
        frame(&mut app, wid);
        let text = row_text(&app, wid, at_rest.0);
        assert!(
            text.contains("\u{23F5}\u{23F5} accept edits"),
            "the chip stays: {text:?}"
        );
        assert_eq!(hit(&app, wid, Light::Mode).0, at_rest.0);
        assert!(pty(&mut reader, 0).is_empty());
        // Narrower: no chip fits at all.
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
        assert_eq!(
            app.windows[&wid].claude_lights.selected, None,
            "control: nothing to select where nothing fits"
        );
        assert!(
            pty(&mut reader, 0).is_empty(),
            "and the chord is still the lights': never Claude's shift+tab"
        );
        tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
        assert_eq!(pty(&mut reader, 1), b"\r");

        // With a host: the walk visits only the chips the row holds, each on
        // the glass once selected.
        let (mut app, wid, session, mut reader) = self::app();
        let _host = host(&mut app);
        publish_facts(&app, session);
        for (cols, walk) in [
            (
                40_u16,
                &[Chip::Autopilot, Chip::Settings, MODE, Chip::Autopilot][..],
            ),
            (36, &[Chip::Autopilot, MODE, Chip::Autopilot][..]),
            (30, &[Chip::Autopilot, Chip::Autopilot][..]),
        ] {
            assert!(app.apply_term_resize(wid, 24, cols));
            draw_claude_cols(&app, session, ACCEPT_SHORT, usize::from(cols));
            frame(&mut app, wid);
            for want in walk {
                chord(&mut app, wid);
                assert_eq!(
                    app.windows[&wid].claude_lights.selected,
                    Some((session, *want)),
                    "{cols} columns"
                );
                frame(&mut app, wid);
                assert!(painted(&app, wid, *want), "{cols}: {want:?} on the glass");
            }
            assert_eq!(painted(&app, wid, Chip::Settings), cols == 40, "{cols}");
            assert_eq!(painted(&app, wid, MODE), cols != 30, "{cols}");
            tap(&mut app, wid, NamedKey::Escape, KeyCode::Escape, "\u{1b}");
            assert_eq!(app.windows[&wid].claude_lights.selected, None);
        }
        assert!(pty(&mut reader, 0).is_empty(), "the walk types nothing");
        tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
        assert_eq!(pty(&mut reader, 1), b"\r", "the Return is Claude's");
    }

    /// The session's own Autopilot word and the posture it is read under.
    fn autopilot(app: &App, session: u64) -> (Option<Autopilot>, AutopilotView) {
        let (word, view, _) = app.session_autopilot(session);
        (word, view)
    }

    /// How many person gestures reached `session` (`status human_seq=`).
    fn human_seq(app: &App, session: u64) -> u64 {
        app.pool
            .get(session)
            .expect("session")
            .ctx
            .human_input
            .seq()
    }

    /// The footer's repaint key for the window's visible plan.
    fn footer_fp(app: &App, wid: WindowId) -> u64 {
        let plan = app.active_visible_leaf_plan(wid).expect("a visible plan");
        app.claude_footer_fp(wid, &plan)
    }

    /// THE AUTOPILOT CHIP, CLICKED (design §2.3): cruise → land → off →
    /// cruise, each click writing the session's own word in-process — the
    /// word `meta set autopilot` writes — and the next frame's chip saying
    /// it, read back (no `◐` left standing): `∞` in the footer's accent,
    /// `⤓ land` amber, `○ off` dim — an Off session's chip drawn like any
    /// other. A click sends NOTHING to the PTY — no key, and no mouse report
    /// though the pane tracks the mouse — and is not a person's keystroke
    /// (`status human_seq=` holds). The hover's title says what a click
    /// does, and the footer's repaint key follows the word. CONTROL: the
    /// light beside it, clicked, does press — the click was a click.
    #[test]
    fn a_click_on_autopilot_cycles_its_word_and_types_nothing() {
        let (mut app, wid, session, mut reader) = wide_app();
        let _host = host(&mut app);
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
        frame(&mut app, wid);
        let accent = crate::chrome_band::band_colors(app.theme).accent;
        let dim = super::dim_of(app.windows[&wid].input_scratch.implicit_blank);
        let human = human_seq(&app, session);
        assert_eq!(
            autopilot(&app, session),
            (None, AutopilotView::Cruise),
            "the table's default"
        );
        let chip = hit(&app, wid, Chip::Autopilot);
        assert_eq!(
            (cell_at(&app, wid, chip).ch, cell_at(&app, wid, chip).fg),
            ('\u{221e}', accent)
        );
        let (x, y) = px_of(&app, wid, chip);
        app.on_cursor_moved(wid, x, y);
        frame(&mut app, wid);
        let text = row_text(&app, wid, chip.0);
        assert!(
            text.contains("autopilot: cruise \u{2014} keeps the work moving \u{b7} click: land"),
            "the hover's title: {text:?}"
        );
        for (want, face, ink) in [
            (Autopilot::Land, "\u{2913} land", super::AMBER),
            (Autopilot::Off, "\u{25cb} off", dim),
            (Autopilot::Cruise, "\u{221e}", accent),
        ] {
            let before = footer_fp(&app, wid);
            click(&mut app, wid, Chip::Autopilot);
            assert_eq!(autopilot(&app, session).0, Some(want), "the word");
            assert_ne!(footer_fp(&app, wid), before, "{want:?}: the click repaints");
            frame(&mut app, wid);
            let at = hit(&app, wid, Chip::Autopilot);
            let text = row_text(&app, wid, at.0);
            assert!(
                text.contains(&format!("{face} \u{2699}")),
                "{want:?}: {text:?}"
            );
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
        // Control: the light is a click away, and it presses.
        redraw_mode_row(&app, session, PLAN);
        frame(&mut app, wid);
        click(&mut app, wid, Light::Mode);
        assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB, "the light presses");
    }

    /// QUICK CLICKS COUNT: the window's own write reads back at once, so a
    /// second click before any frame has drawn the first steps again — a
    /// person clicking Cruise → Land → Off gets Off. FAILS when a click's
    /// read-back waits for a frame (the second click was dropped while `◐`
    /// stood, seen on a real aterm, 2026-10-15).
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
            row_text(&app, wid, at.0).contains("\u{25cb} off \u{2699}"),
            "the chip reads off"
        );
        assert!(pty(&mut reader, 0).is_empty(), "nothing to the PTY");
        assert_eq!(
            human_seq(&app, session),
            human,
            "never a person's keystroke"
        );
    }

    /// THE GEAR (design §2.3): a click on `⚙` opens Settings ▸ Harness — a
    /// Settings tab at `/harness`, in the window clicked — and sends nothing
    /// to the PTY, writes no word and stamps no person.
    #[test]
    fn a_click_on_the_gear_opens_settings_harness() {
        use crate::native_settings::SettingsRoute;
        let (mut app, wid, session, mut reader) = app();
        let _host = host(&mut app);
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        let human = human_seq(&app, session);
        assert!(
            app.native_settings_route_view_target(SettingsRoute::Harness)
                .is_none()
        );
        click(&mut app, wid, Chip::Settings);
        let (at, ..) = app
            .native_settings_route_view_target(SettingsRoute::Harness)
            .expect("Settings ▸ Harness is open");
        assert_eq!(at, wid, "in the window clicked");
        assert!(pty(&mut reader, 0).is_empty(), "nothing to the PTY");
        assert_eq!(autopilot(&app, session).0, None, "no word written");
        assert_eq!(human_seq(&app, session), human);
    }

    /// THE BLANK BETWEEN TWO CHIPS IS THEIRS (the review of 97e5ad4f2): in
    /// `∞ ⚙ ⏸ plan` a click on the blank cell between two chips, in a pane
    /// that tracks the mouse, sends NOTHING to the PTY — no press or release
    /// report — writes no word, opens no Settings and toggles no light, and
    /// the pointer there hovers no chip. CONTROL: the cell left of the first
    /// chip is the pane's, and the same click there is the program's.
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
        let gear = hit(&app, wid, Chip::Settings);
        let light = hit(&app, wid, MODE);
        assert_eq!(gear, (first.0, first.1 + 2), "`∞`, a blank, `⚙`");
        assert_eq!(light, (gear.0, gear.1 + 2), "`⚙`, a blank, the light");
        let press = |app: &mut App, cell: (usize, usize)| {
            let (x, y) = px_of(app, wid, cell);
            app.on_cursor_moved(wid, x, y);
            app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
            app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        };
        for gap in [(gear.0, gear.1 - 1), (light.0, light.1 - 1)] {
            press(&mut app, gap);
            assert_eq!(
                app.windows[&wid].claude_lights.hover, None,
                "{gap:?}: no chip hovered"
            );
            assert!(pty(&mut reader, 0).is_empty(), "{gap:?}: no mouse report");
        }
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
        // Control: left of the chips, the pane's own cell.
        press(&mut app, (first.0, first.1 - 1));
        assert!(
            pty(&mut reader, 1).starts_with(b"\x1b[<0;"),
            "the pane's cell is the program's"
        );
    }

    /// THE HAND ONLY WHERE A PRESS ACTS (the review of 2026-10-15): over a
    /// chip the pointer is a hand; over the blank between two — the chips'
    /// own glass, where a press does nothing — the arrow, never the hand
    /// that promised a click, nor the grid's I-beam; on the motion path and
    /// on the re-resolve a modifier asks for (`App::update_hover_cursor`)
    /// alike. CONTROL: the pane's own cell beside the chips is the grid's,
    /// an I-beam.
    #[test]
    fn the_pointer_is_a_hand_over_a_chip_and_the_arrow_between_two() {
        let (mut app, wid, session, mut reader) = app();
        let _host = host(&mut app);
        publish_facts(&app, session);
        draw_claude(&app, session, PLAN);
        frame(&mut app, wid);
        let first = hit(&app, wid, Chip::Autopilot);
        let gear = hit(&app, wid, Chip::Settings);
        let gap = (gear.0, gear.1 - 1);
        let pane = (first.0, first.1 - 1);
        // (a hand, an I-beam), as the window's cursor state says them.
        let cursor = |app: &App| {
            let ws = &app.windows[&wid];
            (ws.hover_pointer, ws.native_text_cursor)
        };
        let (hand, arrow, beam) = ((true, false), (false, false), (false, true));
        for (cell, want, what) in [
            (pane, beam, "control: the pane's cell"),
            (gear, hand, "the gear"),
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

    /// EVERY BUTTON ON THE CHIPS' GLASS IS THEIRS (the review of 2026-10-15):
    /// a middle or a right press on a chip, or on the blank between two,
    /// reaches no program — in a pane that tracks the mouse no press or
    /// release report, and under `right_click = "copy_paste"` with nothing
    /// tracking no paste — and acts on no chip: a left press is the chips'
    /// click. CONTROL: the same presses on the pane's own cell beside the
    /// chips are the program's — each reported, and the right one pasted.
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
        let gear = hit(&app, wid, Chip::Settings);
        let chips = [first, gear, (gear.0, gear.1 - 1), hit(&app, wid, MODE)];
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
            // Control: the pane's own cell reports the press.
            press(&mut app, pane, button);
            let got = String::from_utf8_lossy(&pty(&mut reader, 1)).into_owned();
            assert!(
                got.starts_with(&format!("\x1b[<{code};")),
                "control: {button:?} on the pane's cell is the program's: {got:?}"
            );
        }
        // Nothing tracking, right-click copy/paste: no paste from the chips.
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

    /// A CLICK ON THE CHIPS IS A CLICK AWAY FROM AN OPEN RENAME (the review
    /// of 2026-10-15): with the in-grid strip's rename field open on
    /// Claude's tab and a name typed into it, a click on `⚙` commits the name
    /// — as View ▸ Coding Agent Harness… and a click on the grid do — before
    /// Settings ▸ Harness comes to the front, so the keys meant for the page
    /// never land in the tab's name. So does a click on Autopilot, which
    /// still steps its word, and one on the blank between two chips.
    /// NEGATIVE CONTROL: Settings ▸ Harness brought to the front by no click
    /// — a tab switch, which settles nothing by design — leaves the field
    /// open, taking keys.
    #[test]
    fn a_click_on_the_chips_commits_an_open_in_grid_rename() {
        use crate::app_rename::RenameSurface;
        use crate::app_search::SearchEdit;
        use crate::native_settings::SettingsRoute;
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
            // The field is seeded with the pin, the caret at its end.
            assert!(app.rename_field_edit(wid, SearchEdit::KillToStart));
            assert!(app.rename_field_edit(wid, SearchEdit::Insert(name.to_owned())));
        };
        // Negative control: the page brought to the front by no click.
        rename(&mut app, "left open");
        assert!(app.open_harness_settings(wid));
        assert!(
            app.inline_rename_edit(wid).is_some(),
            "control: a tab switch settles nothing"
        );
        app.end_session_rename_editor(wid);
        assert_eq!(pinned(&app), None, "control: nothing committed");
        app.switch_tab_in(wid, 0);
        frame(&mut app, wid);
        // The gear.
        rename(&mut app, "gear");
        click(&mut app, wid, Chip::Settings);
        assert!(app.inline_rename_edit(wid).is_none(), "the gear: closed");
        assert_eq!(pinned(&app).as_deref(), Some("gear"), "and committed");
        let (at, ..) = app
            .native_settings_route_view_target(SettingsRoute::Harness)
            .expect("Settings ▸ Harness is open");
        assert_eq!(at, wid);
        app.switch_tab_in(wid, 0);
        frame(&mut app, wid);
        // Autopilot.
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
        // The blank between two.
        rename(&mut app, "gap");
        let gear = hit(&app, wid, Chip::Settings);
        let (x, y) = px_of(&app, wid, (gear.0, gear.1 - 1));
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

    /// THE MASTER SWITCH OFF (`[harness] enabled = false`, design §2.1): the
    /// chip says `⊘ off`, dim, titled with where it is switched, and a click
    /// opens Settings ▸ Harness instead of cycling — no word is written (a
    /// session ON never lifts a global off), nothing reaches the PTY. The
    /// gear stays beside it. NEGATIVE CONTROL: with the switch on again the
    /// same click writes the word.
    #[test]
    fn under_the_master_switch_off_the_autopilot_chip_opens_settings() {
        use crate::native_settings::SettingsRoute;
        let (mut app, wid, session, mut reader) = wide_app();
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
        let dim = super::dim_of(app.windows[&wid].input_scratch.implicit_blank);
        let chip = hit(&app, wid, Chip::Autopilot);
        assert_eq!(
            (cell_at(&app, wid, chip).ch, cell_at(&app, wid, chip).fg),
            ('\u{2298}', dim)
        );
        let (x, y) = px_of(&app, wid, chip);
        app.on_cursor_moved(wid, x, y);
        frame(&mut app, wid);
        let text = row_text(&app, wid, chip.0);
        assert!(
            text.contains("\u{2298} off \u{2699}")
                && text.contains(
                    "harness off in Settings \u{b7} click: open Settings \u{25b8} Harness"
                ),
            "{text:?}"
        );
        click(&mut app, wid, Chip::Autopilot);
        assert!(
            app.native_settings_route_view_target(SettingsRoute::Harness)
                .is_some(),
            "Settings ▸ Harness is open"
        );
        assert_eq!(
            autopilot(&app, session),
            (None, AutopilotView::MasterOff),
            "nothing written"
        );
        assert!(pty(&mut reader, 0).is_empty());
        // Negative control: the switch on, the same click — the press path,
        // back on Claude's tab — cycles, and opens nothing.
        app.harness_policy = policy("[harness]\n");
        app.switch_tab_in(wid, 0);
        frame(&mut app, wid);
        let tabs = app.windows[&wid].tab_set.len();
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

    /// AN OFF SESSION STILL SAYS SO (design §2.3): Off is the harness's
    /// hands off, not its chip gone — `○ off`, dim, drawn at rest whether
    /// the session's own word says it or the table's default does
    /// (`[harness] autopilot = "off"`), the gear beside it.
    #[test]
    fn an_off_session_still_draws_its_chip() {
        for own in [true, false] {
            let (mut app, wid, session, _reader) = app();
            let _host = host(&mut app);
            if own {
                assert!(app.set_session_autopilot(session, Some(Autopilot::Off)));
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
            assert!(
                text.contains("\u{25cb} off \u{2699}"),
                "own={own}: {text:?}"
            );
            assert_eq!(
                cell_at(&app, wid, chip).fg,
                super::dim_of(app.windows[&wid].input_scratch.implicit_blank),
                "own={own}: dim"
            );
        }
    }

    /// A NARROW RULE GIVES THE CHIPS UP BEFORE THE MODEL (design §2.3: "a
    /// narrow rule gives them up after every fact but the model"; no row is
    /// added): narrowing the pane column by column from 80, the path and
    /// the branch go while every chip stays — no width shows either without
    /// them — and the chips then go down the ladder (`Keep`), never back up
    /// it while the model is drawn, the model still drawn where no chip is.
    /// With no pill under the composer (default mode) the harness's two
    /// alone: `⚙` goes, then Autopilot. Under a pill the light, a deviation,
    /// joins them: `⚙` goes, then the light, Autopilot last — a pill too
    /// wide for the rule never takes the mark that says the session is
    /// wrapped. And while a chip stands NOTHING COMES BACK: no fact either —
    /// the effort, the path, the branch — once a wider rule gave it up, for
    /// the room a given-up chip leaves stays the rule's (`Block::holds`; the
    /// review of 2026-10-15: under accept edits the light's 15 cells, given
    /// up at 35 columns, brought the effort and the path back beside `∞`
    /// alone). Claude's composer keeps its rows throughout: the chips take
    /// the rule's cells, never a row.
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
            /// Autopilot's chip, the gear's and the light's.
            chips: (bool, bool, bool),
            text: String,
        }
        /// The chips a narrowing rule draws, rung by rung (`Keep::LADDER`
        /// past the reveal), the light where a pill shows: every chip, the
        /// gear given up, then the light, then Autopilot.
        const LADDER: [(bool, bool, bool); 4] = [
            (true, true, true),
            (true, false, true),
            (true, false, false),
            (false, false, false),
        ];
        // Pills short enough not to wrap, each scanned only where it fits.
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
                    painted(&app, wid, Chip::Settings),
                    painted(&app, wid, MODE),
                );
                if rest {
                    assert_eq!(
                        chips,
                        (true, true, pill.is_some()),
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
                    && widest.chips == (true, true, pill.is_some()),
                "{pill:?} 80: everything: {:?}",
                widest.text
            );
            // With no chip left the rule is the facts' alone, their own
            // ladder's (`footer::fit_rule`): the scan's last widths.
            let chipped: Vec<&Seen> = seen
                .iter()
                .filter(|s| s.chips != (false, false, false))
                .collect();
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
                        .position(|&(autopilot, gear, light)| {
                            (autopilot, gear, light && pill.is_some()) == s.chips
                        })
                        .unwrap_or_else(|| panic!("{pill:?}: off the ladder: {s:?}"))
                })
                .collect();
            assert!(
                rungs.windows(2).all(|pair| pair[0] <= pair[1]),
                "{pill:?}: never back up the ladder: {seen:#?}"
            );
            let shows =
                |chips: (bool, bool, bool)| seen.iter().any(|s| s.model && s.chips == chips);
            assert!(
                seen.iter()
                    .any(|s| s.model && !s.rest && s.chips == widest.chips),
                "{pill:?}: every chip beside the model alone: {seen:#?}"
            );
            if pill.is_some() {
                assert!(
                    shows((true, false, true)),
                    "{pill:?}: the gear first: {seen:#?}"
                );
            }
            assert!(
                shows((true, false, false)),
                "{pill:?}: the gear given up, then the light: {seen:#?}"
            );
            if pill.is_none() {
                assert!(
                    shows((false, false, false)),
                    "the chips given up, the model kept: {seen:#?}"
                );
            }
        }
    }

    /// RETURN ACTS ON A SELECTED HARNESS CHIP (macOS): `ctrl+shift+tab`
    /// selects Autopilot, and Return steps its word — cruise to land — with
    /// NOTHING sent to the PTY (the Return is the chip's, not Claude's) and
    /// no person stamped; the selection lets go. CONTROL: with nothing
    /// selected, Return is Claude's again. Space on a selected `⚙` opens
    /// Settings ▸ Harness, and is not Claude's either.
    #[cfg(target_os = "macos")]
    #[test]
    fn return_on_a_selected_autopilot_chip_cycles_it() {
        use winit::keyboard::{KeyCode, NamedKey};
        let (mut app, wid, session, mut reader) = app();
        let _host = host(&mut app);
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        let human = human_seq(&app, session);
        chord(&mut app, wid);
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, Chip::Autopilot))
        );
        frame(&mut app, wid);
        tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
        assert_eq!(autopilot(&app, session).0, Some(Autopilot::Land));
        assert!(pty(&mut reader, 0).is_empty(), "the Return is the chip's");
        assert_eq!(app.windows[&wid].claude_lights.selected, None);
        frame(&mut app, wid);
        let text = row_text(&app, wid, footer_row(&app, wid));
        assert!(text.contains("\u{2913} land \u{2699}"), "{text:?}");
        assert_eq!(human_seq(&app, session), human, "no person stamped");
        // Control: nothing selected, the Return is Claude's.
        tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
        assert_eq!(pty(&mut reader, 1), b"\r");
        // Space on the gear.
        chord(&mut app, wid);
        frame(&mut app, wid);
        chord(&mut app, wid);
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, Chip::Settings))
        );
        frame(&mut app, wid);
        tap(&mut app, wid, NamedKey::Space, KeyCode::Space, " ");
        assert!(
            app.native_settings_route_view_target(crate::native_settings::SettingsRoute::Harness)
                .is_some(),
            "Settings \u{25b8} Harness is open"
        );
        assert!(pty(&mut reader, 0).is_empty(), "the Space is the chip's");
    }

    // The mode-return machine's actions that are not aterm's: Claude's own
    // answers and boxes, and a person's shift+tab. The Tier-1 test above
    // drives each on the real engine and projects it all the same.
    #[aterm_spec::spec_unmodeled(
        machine = "claude_mode_return",
        action = "Answer",
        reason = "Claude Code's own step: its pill moves on after a shift+tab. The Tier-1 test \
                  redraws the real engine's mode row and projects the App state."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "claude_mode_return",
        action = "Cover",
        reason = "Claude Code's own step: a box over the composer. The Tier-1 test draws one on \
                  the real engine and checks nothing is pressed into it."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "claude_mode_return",
        action = "Uncover",
        reason = "Claude Code's own step: the box answered and the composer back."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "claude_mode_return",
        action = "Leave",
        reason = "A person's own shift+tab in Claude, away from an expected mode: aterm has no \
                  gesture that leaves bypass or auto."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "claude_mode_return",
        action = "DontAsk",
        reason = "A person's own choice of don't ask in Claude: aterm has no gesture that sets \
                  it. The Tier-1 test draws the don't-ask pill and returns from it."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "claude_mode_return",
        action = "Narrow",
        reason = "The session's own cycle, without bypass or auto (a session started without \
                  them): the Tier-1 test answers each press from such a ring."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "claude_mode_return",
        action = "TurnStarts",
        reason = "Claude Code's own step: a turn starts. The Tier-1 test draws the busy mode row \
                  on the real engine."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "claude_mode_return",
        action = "TurnEnds",
        reason = "Claude Code's own step: the turn ends, the busy hint gone from the mode row."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "claude_mode_return",
        action = "Timeout",
        reason = "The clock crosses a press's deadline outside an aterm transition. The Tier-1 \
                  late-answer test advances the real deadline, then checks the App stops."
    )]
    #[expect(
        dead_code,
        reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it"
    )]
    fn mode_return_environment_waivers() {}
}

#[cfg(test)]
#[path = "claude_lights_face_tests.rs"]
mod face_tests;
