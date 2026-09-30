// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CLAUDE CODE LIGHTS, host side: a few lights in the rule under Claude
//! Code's input box, left of aterm's footer facts (`crate::claude_footer`) —
//! the permission mode and fast mode (owner directions, 2026-09-24 and
//! 2026-09-28). What each light means, where it is read from and which of
//! Claude's own inputs flips it are decided in `aterm_agent::harness::lights`;
//! this module puts them on the glass and carries the gestures.
//!
//! * DEVIATIONS ONLY: a light is drawn — as a labelled chip, `⏸ plan`,
//!   `○ fast` — only while it shows what the owner does not
//!   expect (`lights::deviates`), while a toggle of it is in flight, a
//!   refusal or a just-switched notice for it is on show, or the pointer or
//!   the keyboard selection is on it. At rest, everything as expected,
//!   nothing is drawn: `claude_lights_block` is `None` and the rule carries
//!   the facts alone. The chips never replace or cover Claude's own mode
//!   pill, which stays on Claude's row under the rule, whole: the mode chip
//!   is aterm's own click and keyboard target, in aterm's span of the rule.
//!   A rule too narrow for the chips beside the model draws none (the model
//!   gives way last, `footer::fit_rule`).
//! * HOVER a chip: its title and state replace nothing — they appear just
//!   right of the chips, where the rule has room: a hover's, a selection's or
//!   a toggle's title is the first thing a narrow rule gives up, short then
//!   gone, before any of the facts, while a REASON — where a mode return
//!   stopped, a refusal — outranks the branch and the path, never the model
//!   or the effort (`footer::fit_rule`). CLICK it: it toggles.
//! * `ctrl+shift+tab` (macOS; elsewhere it is aterm's `prev_tab`) REVEALS
//!   every known light of the focused pane and selects the next (Claude
//!   keeps its own shift+tab); while one is selected, Return or Space toggles
//!   it (on the mode in bypass or auto it does nothing), ←/→ move, and
//!   Escape — or any other key, which then goes on to Claude as usual — lets
//!   go, and the lights at rest go with the selection. Only a chip the pane has ROOM for is ever
//!   selected (the reveal gives way first), and a selection whose chip is
//!   not on the glass is let go by the next key: a light nobody can see
//!   never takes a Return meant for Claude.
//! * A toggle is Claude's own input, and it is READ BACK: the light shows
//!   `◐` until Claude's screen says it switched, and says so in its title if
//!   Claude did not within `harness::lights::settle_ms`. A slash command is
//!   pasted only into an EMPTY composer, never while a driver may still be
//!   typing its own prompt there, and it is SUBMITTED only while the composer
//!   holds exactly it (`lights::composer_holds`) and no person has touched
//!   the session since aterm typed it (`HumanInputStamp::last`). Not submitted in
//!   time, it is TAKEN BACK — one ctrl+u, under the same two conditions, read
//!   back once — and anything else leaves it where it is, the light saying so:
//!   aterm never types into, or deletes, what may be a person's. `/fast` is
//!   Claude's IMMEDIATE command: it is typed and submitted mid-turn too, and
//!   Claude's own answer is READ (`lights::fast_answers`), for up to
//!   `lights::FAST_SETTLE_MS` from the Enter — a refusal shows in Claude's
//!   words, and one that is the account's or the build's own answer
//!   (`lights::FastRefusal::lasting`) is latched per Claude build and model,
//!   in memory until aterm restarts, so the light stops asking
//!   (`App::claude_fast_latch`). A `/fast on` that took an auto session out
//!   of auto (Claude's server flag, `lights::AUTO_OFF_FOR_FAST`) says so, and
//!   is latched for the build: in auto mode the fast light then stops asking
//!   and refuses a click.
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
//!   after it on its own — the mode return's next presses, a typed command's
//!   Enter or ctrl+u — goes as `input::Source::Controller`: no person made it
//!   at that instant, and stamping it would hold the supervisor off a session
//!   for its whole `[harness] human_grace_s` (120 s) after every automatic
//!   step, a box that opens meanwhile unanswered.
//! * Input never leaves from the render path: a frame that shows a step's
//!   answer only wakes the loop (`Wake::ClaudeFooter`), whose handler reads
//!   the ENGINE and sends the next step ([`App::drain_claude_lights`]).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use aterm_agent::harness::lights::{
    self, Drive, FastAnswer, FastRefusal, Light, LightState, Mode, Reading, Screen,
};
use aterm_core::terminal::{RenderCell, UnderlineStyle};
use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::{App, WindowId};

/// How long a toggle that did not take keeps saying so.
const REFUSAL_SHOWN: Duration = Duration::from_secs(4);

/// Why a toggle did not happen, as its light's title says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Refusal {
    /// The light is not on Claude's screen right now.
    NotShown,
    /// A slash command would have been typed over the person's draft.
    Draft,
    /// The input was sent and Claude did not switch.
    NoChange,
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
    /// Claude is mid-turn; a slash command it does not run mid-turn waits
    /// for it to finish.
    Busy,
    /// Claude answered the command with a refusal, in these words.
    Vendor(FastRefusal),
    /// Turning fast mode on in auto mode would turn auto mode off: this
    /// Claude build has done it before (`lights::AUTO_OFF_FOR_FAST`).
    CostsAuto,
    /// The slash command aterm typed is still in the composer: a person
    /// touched the session before it went, or it could not be taken back.
    LeftTyped,
    /// A driver may still be typing its prompt into the composer (a `lease`,
    /// or a `turn` whose prompt Claude has not taken yet): a slash command
    /// must not land in it.
    Driven,
    /// The input seam accepted nothing: no toggle owns the command.
    InputRejected,
    /// Earlier accepted input has not yet left the session's ordered FIFO.
    InputPending,
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
            Refusal::Draft => "clear the prompt first",
            Refusal::NoChange => "Claude did not switch",
            Refusal::StoppedIn(mode) => return format!("stopped in {} mode", mode.word()),
            Refusal::StoppedMidTurn(mode) => {
                return format!("stopped in {} mode mid-turn; click again", mode.word());
            }
            Refusal::NotInCycle(mode) => {
                return format!("no bypass or auto in this cycle; in {} mode", mode.word());
            }
            Refusal::Busy => "wait for Claude to finish",
            Refusal::Vendor(why) => why.title(),
            Refusal::CostsAuto => "Claude turns auto mode off with it here",
            Refusal::LeftTyped => "command left in the prompt",
            Refusal::Driven => "a driver has the session",
            Refusal::InputRejected => "input was not accepted; try again",
            Refusal::InputPending => "wait for queued input",
        }
        .to_owned()
    }
}

/// A toggle in flight: what was sent, and what the next frames must see.
#[derive(Debug, Clone)]
struct Pending {
    light: Light,
    from: LightState,
    drive: Drive,
    /// The mode when the toggle began (a cycle that leaves it is done).
    started: Mode,
    /// The mode when the last shift+tab was sent: a frame showing another
    /// mode is Claude's answer to that press.
    mode_at_press: Mode,
    /// Every mode the cycle has shown since it began: one shown again is a
    /// lap, and the search stops there.
    seen: Vec<Mode>,
    presses: u8,
    /// Past this, the toggle is given up. A mode return's is refreshed on
    /// every press: it is the time for ONE shift+tab's answer. A command's
    /// is set again at its Enter: the time Claude takes to answer it.
    deadline: Instant,
    /// The loop's next look at the engine while one is armed: ONE timer per
    /// toggle, however many wakes arrive meanwhile.
    next_poll: Option<Instant>,
    /// A mode return's: Claude answered the last press but was mid-turn, so
    /// aterm's own next press waited for the turn to end. A deadline reached
    /// so says so (`Refusal::StoppedMidTurn`).
    held_mid_turn: bool,
    /// A command's: the fewest of Claude's transcript echoes of it that carry
    /// an answer since the click (`lights::FastAnswers::echoed`). Only a
    /// count above it is an answer to THIS toggle — never an earlier try's
    /// still on screen.
    echoes_floor: usize,
    /// A command's: the notification under the composer that was up at the
    /// click (`lights::FastAnswers::notice`) — an earlier try's answer, not
    /// this one's. Cleared the moment the slot shows anything else or
    /// nothing; from then on a notification there is this toggle's.
    stale_notice: Option<FastAnswer>,
    /// A command's: Claude's newest word on fast mode seen since the click,
    /// stale or not — what the light says if the toggle's own answer never
    /// comes to be told apart (Claude's words, rather than "did not switch").
    last_said: Option<FastAnswer>,
}

/// How often a mode return in flight reads the engine for Claude's answer:
/// a pill redraw lands within a frame or two.
const MODE_POLL: Duration = Duration::from_millis(80);

/// How often a submitted command in flight reads the engine for Claude's
/// answer: `/fast` may take its whole `lights::FAST_SETTLE_MS` to answer.
const ANSWER_POLL: Duration = Duration::from_millis(250);

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

/// One engine read of a session whose command is in flight
/// ([`WindowLights::answer`]).
#[derive(Debug, Clone, Copy)]
struct CommandRead {
    /// Fast mode off the composer's rule.
    fast: LightState,
    /// The permission mode its pill names.
    mode: Option<Mode>,
    /// Claude's answers to the command on screen.
    answers: lights::FastAnswers,
    /// Claude says auto mode is off because fast mode is on
    /// (`lights::says_auto_off_for_fast`).
    auto_off: bool,
}

/// How a command in flight ended, for the host's latch
/// (`App::claude_fast_latch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommandEnd {
    /// Claude refused it, for this reason.
    refusal: Option<FastRefusal>,
    /// Turning fast mode on took the session out of auto mode.
    cost_auto: bool,
}

/// How often a typed command's composer is read until its Enter goes: the
/// paste's echo usually lands within a frame or two.
const TYPED_POLL: Duration = Duration::from_millis(80);

/// How long after a ctrl+u the composer is read back.
const CLEAR_READ_BACK: Duration = Duration::from_millis(300);

/// A slash command aterm typed into a session's composer for a light, from the
/// paste until it was submitted, taken back or left (`App::claude_typed`).
/// Kept per SESSION, not per window: the text is in the session's composer
/// whichever window — if any — still shows it, so closing the window that
/// started the toggle does not strand it.
#[derive(Debug, Clone)]
pub(crate) struct Typed {
    light: Light,
    /// The light's state when the toggle began: another one on screen means
    /// the command went, whoever pressed its Enter.
    from: LightState,
    cmd: &'static str,
    /// The session's person clock right after aterm's own last write to it
    /// (`HumanInputStamp::last`): any other value is a person's gesture since
    /// — the composer may hold their text now, and aterm types nothing more.
    stamp: u64,
    /// Claude runs the command mid-turn (`Drive::Command::immediate`): its
    /// Enter does not wait for the turn to end.
    immediate: bool,
    /// The driver holding the session's lease at the paste (`who`'s
    /// `driving=` token), if any: a lease taken or changed SINCE is another
    /// hand, and aterm types nothing more.
    lease_at_paste: Option<String>,
    /// Another hand came to the session since the paste — a person's
    /// gesture, or a driver's lease: aterm types nothing more, and the
    /// deadline says what became of the command.
    hands_off: bool,
    /// Past this, the command is taken back, or left and said.
    deadline: Instant,
    /// The next poll's instant while one is armed: ONE timer per record,
    /// however many footer wakes arrive meanwhile.
    next_poll: Option<Instant>,
    /// A ctrl+u went out; the composer is read back at this instant.
    cleared: Option<Instant>,
    /// The deadline found the prompt empty and was moved once: the paste's
    /// echo may still be on its way from a busy Claude. The late look only
    /// takes the command back — no Enter goes after the light has said
    /// Claude did not switch.
    late: bool,
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
    /// until this instant — then, back as expected, it goes. The words, where
    /// Claude said more than the new state (`Fast mode ON · model set to …`),
    /// belong to THIS notice and go with it: a later notice of another light
    /// can never inherit them.
    notice: Option<(Light, Instant, Option<&'static str>)>,
}

/// One painted light, in frame cells — recorded by the painter, read by the
/// pointer, so a click can only land on a light the glass shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LightHit {
    frame_row: usize,
    /// The same row as a window grid row, for the chrome register: a band
    /// that later claims the row (the link caption) hides the light, and a
    /// hidden light is not a light.
    term_row: usize,
    col: usize,
    session: u64,
    light: Light,
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
    hover: Option<(u64, Light)>,
    selected: Option<(u64, Light)>,
    /// A frame has been painted since the selection last moved: from then
    /// on, a selection whose chip is not among the hits is not on the glass,
    /// and is let go rather than left to swallow a Return.
    selection_painted: bool,
    sessions: HashMap<u64, SessionLights>,
}

/// What Claude said of fast mode that outlives one toggle, for every window
/// (`App::claude_fast_latch`). In memory only: until aterm restarts.
#[derive(Debug, Default)]
pub(crate) struct FastLatch {
    /// Claude's LASTING refusals (`FastRefusal::lasting`), by the Claude
    /// build (`FooterFacts::version`) and model (`FooterFacts::model`) that
    /// said them — a refusal of one model does not stop asking on another,
    /// and a new build asks once more. While one holds, the fast light stops
    /// asking there.
    refused: HashMap<(Option<String>, Option<String>), FastRefusal>,
    /// The Claude builds on which turning fast mode on took a session out of
    /// auto mode (`lights::AUTO_OFF_FOR_FAST`).
    costs_auto: std::collections::HashSet<Option<String>>,
    /// Bumped on every change: a term of every window's repaint key, so a
    /// latch taken in one window repaints the chips of every other.
    generation: u64,
}

impl FastLatch {
    /// Latch `why` for `build` and `model`; whether it is new.
    fn refuse(&mut self, build: Option<String>, model: Option<String>, why: FastRefusal) -> bool {
        let new = self.refused.insert((build, model), why) != Some(why);
        if new {
            self.generation = self.generation.wrapping_add(1);
        }
        new
    }

    /// Latch that fast mode costs auto mode on `build`; whether it is new.
    fn cost_auto(&mut self, build: Option<String>) -> bool {
        let new = self.costs_auto.insert(build);
        if new {
            self.generation = self.generation.wrapping_add(1);
        }
        new
    }

    /// The repaint key's term.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
}

impl WindowLights {
    /// Everything about the lights that is GUI state rather than grid
    /// content, for the repaint key: a hover, a selection, a toggle in flight
    /// or a refusal on show changes the row with no grid damage at all.
    pub(crate) fn fingerprint(&self, now: Instant) -> u64 {
        // Among session entries, only a pending toggle or a refusal still on
        // screen can change the row. `sessions` also remembers light states for
        // hidden panes, so a long-lived window can hold many inert entries.
        let mut active: Vec<_> = self
            .sessions
            .iter()
            .filter(|(_, s)| {
                s.pending.is_some()
                    || s.refused.is_some_and(|(_, _, until)| until > now)
                    || s.notice.is_some_and(|(_, until, _)| until > now)
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
                && until > now
            {
                (light, why).hash(&mut h);
            }
            if let Some((light, until, said)) = s.notice
                && until > now
            {
                (light, "notice", said).hash(&mut h);
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

    /// The lights the keyboard walks in `session` while a selection lasts:
    /// every one whose state is known, in the row's order.
    fn revealable(&self, session: u64) -> Vec<Light> {
        let shown = self
            .sessions
            .get(&session)
            .map(|s| s.shown)
            .unwrap_or_default();
        Light::ALL
            .into_iter()
            .filter(|l| revealed(*l, &shown))
            .collect()
    }

    /// Move the keyboard selection (a frame must paint it before it counts
    /// as off the glass).
    fn select(&mut self, selected: Option<(u64, Light)>) {
        self.selected = selected;
        self.selection_painted = false;
    }

    /// Read `rows` (`read`, inside the reader's panic fence) and fold the
    /// reading in ([`Self::observe`]). A reader panic folds NOTHING in: the
    /// lights keep what they last showed, a toggle in flight takes no step
    /// (its deadline still expires it), and nothing to send or show.
    fn observe_rows(
        &mut self,
        session: u64,
        rows: &[String],
        read: impl FnOnce(&[String]) -> Option<Screen>,
    ) -> Step {
        let Some((screen, auto_off)) = crate::reader_guard::read_or_none(
            &format!("{session}"),
            "Claude Code lights",
            rows,
            || (read(rows), lights::says_auto_off_for_fast(rows)),
        ) else {
            return Step::default();
        };
        Step {
            auto_off_for_fast: auto_off,
            ..self.observe(session, screen.as_ref())
        }
    }

    /// Fold one frame's reading of `session`'s screen in: remember what each
    /// light shows. A toggle is NEVER
    /// stepped here: a frame that shows Claude's answer — the pill moved on,
    /// the fast tag switched — only wakes the loop, which reads the ENGINE
    /// and decides ([`App::advance_claude_mode_returns`],
    /// [`App::answer_claude_commands`]) — the same decision whether or not a
    /// frame is painted. (A command's Enter is judged from the engine by the
    /// loop too, `App::drain_claude_typed`.) What the caller must do: wake the
    /// loop.
    fn observe(&mut self, session: u64, screen: Option<&Screen>) -> Step {
        let s = self.sessions.entry(session).or_default();
        s.shown.fold(&lights::read(screen));
        let Some(p) = s.pending.as_ref() else {
            return Step::default();
        };
        let wake = match (&p.drive, screen) {
            // Claude answered the last press (or the pill moved on its own):
            // the loop reads the engine and decides.
            (Drive::CycleMode, Some(screen)) => screen.mode.is_some_and(|m| m != p.mode_at_press),
            (Drive::CycleMode, None) => false,
            (Drive::Command { .. }, _) => {
                let now_state = s.shown.state(p.light);
                now_state != p.from && now_state != LightState::Unknown
            }
        };
        Step {
            wake,
            ..Step::default()
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
        let Some(p) = s.pending.as_mut().filter(|p| p.drive == Drive::CycleMode) else {
            return Advance::Wait;
        };
        let Some((mode, busy)) = screen.and_then(|screen| Some((screen.mode?, screen.busy))) else {
            return Advance::Wait;
        };
        s.shown.mode = Some(mode);
        if mode.is_expected() {
            s.pending = None;
            s.refused = None;
            s.notice = Some((Light::Mode, now + REFUSAL_SHOWN, None));
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

    /// Resolve `session`'s command in flight from the ENGINE: its light
    /// switched (`fast`, off the rule), or Claude answered it — a transcript
    /// echo beyond the count at the click, else a notification other than
    /// the one up at the click (`answers`, `lights::fast_answers`). A refusal
    /// is said in Claude's words; a switch that took an auto session out of
    /// auto (`auto_off`: Claude's own words on screen, or the pill `mode` left
    /// auto for a mode the owner does not expect) is said too. Both are
    /// returned, for the host's latch. Anything else waits — Claude may be
    /// applying it, its composer hidden — noting Claude's newest word for
    /// the deadline's title.
    fn answer(&mut self, session: u64, read: &CommandRead, now: Instant) -> Option<CommandEnd> {
        let s = self.sessions.get_mut(&session)?;
        let p = s
            .pending
            .as_mut()
            .filter(|p| matches!(p.drive, Drive::Command { .. }))?;
        let answers = read.answers;
        // The notification slot: the one up at the click is an earlier
        // try's until the slot shows anything else, or nothing.
        if answers.notice != p.stale_notice {
            p.stale_notice = None;
        }
        let notice = answers.notice.filter(|_| p.stale_notice.is_none());
        let echo = answers.echo.filter(|_| answers.echoed > p.echoes_floor);
        p.echoes_floor = p.echoes_floor.min(answers.echoed);
        p.last_said = answers.newest().or(p.last_said);
        // The echo is tied to the command; the notification names none.
        let answered = echo.or(notice);
        let light = p.light;
        let switched = read.fast != p.from && read.fast != LightState::Unknown;
        let turned_on = match answered {
            Some(FastAnswer::On | FastAnswer::OnModelMoved) => true,
            Some(_) => false,
            None => switched && read.fast == LightState::On,
        };
        let cost_auto = turned_on
            && p.started == Mode::Auto
            && (read.auto_off || read.mode.is_some_and(|m| !m.is_expected()));
        let (refusal, said) = match answered {
            Some(FastAnswer::Refused(why)) => (Some(why), None),
            _ if cost_auto => (None, Some("on \u{00B7} Claude turned auto mode off")),
            Some(FastAnswer::OnModelMoved) => (None, Some("on \u{00B7} Claude switched the model")),
            Some(FastAnswer::On | FastAnswer::Off) => (None, None),
            None if switched => (None, None),
            None => return None,
        };
        s.pending = None;
        match refusal {
            Some(why) => {
                s.refused = Some((light, Refusal::Vendor(why), now + REFUSAL_SHOWN));
                s.notice = None;
            }
            None => {
                s.refused = None;
                s.notice = Some((light, now + REFUSAL_SHOWN, said));
                if read.fast != LightState::Unknown {
                    s.shown.fast = read.fast;
                }
            }
        }
        Some(CommandEnd { refusal, cost_auto })
    }

    /// Give up on every toggle past its deadline — a mode return past its
    /// LAST press's, naming the mode it stopped in (the session did switch,
    /// just not all the way) — and end every refusal and notice past its
    /// time: whether anything changed, and the sessions whose refusal starts
    /// now (the caller times its end — a refusal no frame ends would stay on
    /// an idle screen).
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
                let why = match p.drive {
                    // The last mode seen — by a frame, if a box has kept the
                    // engine's reading from it since.
                    Drive::CycleMode => match s.shown.mode.unwrap_or(p.mode_at_press) {
                        at if at.is_expected() => None,
                        at if p.held_mid_turn => Some(Refusal::StoppedMidTurn(at)),
                        at => Some(Refusal::StoppedIn(at)),
                    },
                    // Claude's own newest word where it refused — an answer
                    // the count could not tie to this try (a retry answered
                    // in the same notification slot) is still Claude's.
                    Drive::Command { .. } => Some(match p.last_said {
                        Some(FastAnswer::Refused(why)) => Refusal::Vendor(why),
                        _ => Refusal::NoChange,
                    }),
                };
                match why {
                    Some(why) => s.refused = Some((p.light, why, now + REFUSAL_SHOWN)),
                    None => s.notice = Some((p.light, now + REFUSAL_SHOWN, None)),
                }
                s.pending = None;
                refused.push(*session);
                any = true;
            }
            if s.refused.is_some_and(|(_, _, until)| until <= now) {
                s.refused = None;
                any = true;
            }
            if s.notice.is_some_and(|(_, until, _)| until <= now) {
                s.notice = None;
                any = true;
            }
        }
        (any, refused)
    }

    /// `session`'s light block: its TITLE (when a light is selected, hovered,
    /// switching, refused or just switched — empty otherwise) and its CHIPS,
    /// then one cell of margin, with the columns each chip spans. Apart,
    /// because only the chips must fit: a pane too narrow for the title keeps
    /// its chips and drops the title.
    ///
    /// DEVIATION ONLY (owner, 2026-09-27: "I don't want to show the state of
    /// parameters that I always expect to be on"). A chip is drawn only while
    /// its light shows what the owner does not expect
    /// (`lights::deviates`), while a toggle of it is in flight, while a
    /// refusal or a just-switched notice for it is on show, while the pointer
    /// or the keyboard selection is on it, and — the keyboard's way to a
    /// light at rest — for every known light while a selection lasts
    /// (`ctrl+shift+tab`, [`App::on_key_claude_lights`]) — the reveal, which
    /// a caller short of room drops first (`reveal: false`,
    /// [`App::claude_lights_block`]). With everything as expected the block
    /// is EMPTY: no chip, no title, nothing to hit.
    fn block(
        &self,
        session: u64,
        blank: RenderCell,
        expect: &lights::Expect,
        now: Instant,
        reveal: bool,
    ) -> Block {
        self.block_with(
            session,
            blank,
            expect,
            now,
            self.selected
                .filter(|(id, _)| *id == session)
                .map(|(_, l)| l),
            self.hover.filter(|(id, _)| *id == session).map(|(_, l)| l),
            reveal,
        )
    }

    /// [`Self::block`] with the selection and the hover given — so the
    /// keyboard can ask whether a selection would FIT before making it.
    #[expect(
        clippy::too_many_arguments,
        reason = "the block's inputs, each one named; a struct would only rename them"
    )]
    fn block_with(
        &self,
        session: u64,
        blank: RenderCell,
        expect: &lights::Expect,
        now: Instant,
        selected: Option<Light>,
        hover: Option<Light>,
        reveal: bool,
    ) -> Block {
        let at_rest = SessionLights::default();
        let s = self.sessions.get(&session).unwrap_or(&at_rest);
        // The keyboard selection outranks a resting pointer: Return toggles
        // the SELECTED light, so the row must mark that one.
        let focus = selected.or(hover).map(|l| (session, l));
        let refused = s.refused.filter(|(_, _, until)| *until > now);
        let noticed = s
            .notice
            .filter(|(_, until, _)| *until > now)
            .map(|(light, _, _)| light);
        let pending = s.pending.as_ref().map(|p| p.light);
        let drawn = |light: Light| {
            lights::deviates(light, &s.shown, expect)
                || pending == Some(light)
                || refused.is_some_and(|(l, _, _)| l == light)
                || noticed == Some(light)
                || focus.is_some_and(|(_, l)| l == light)
                || (reveal && selected.is_some() && revealed(light, &s.shown))
        };
        let titled = focus
            .map(|(_, l)| l)
            .or(pending)
            .or(refused.map(|(l, _, _)| l))
            .or(noticed)
            .filter(|light| drawn(*light));
        let dim = dim_of(blank);
        let mut title = Vec::new();
        let mut short_title = Vec::new();
        let mut reason = false;
        if let Some(light) = titled {
            let (what, why) = what_of(
                s,
                light,
                refused,
                focus.is_some_and(|(_, l)| l == light),
                expect,
                now,
            );
            reason = why;
            let cells = |text: String| -> Vec<RenderCell> {
                text.chars()
                    .map(|ch| {
                        let mut c = blank;
                        c.ch = ch;
                        c.fg = dim;
                        c
                    })
                    .collect()
            };
            title = cells(format!("{}: {what}  ", light.title()));
            short_title = cells(format!("{what}  "));
        }
        let mut cells = Vec::new();
        let mut cols = Vec::new();
        for light in Light::ALL.into_iter().filter(|l| drawn(*l)) {
            if !cells.is_empty() {
                cells.push(blank);
            }
            let start = cells.len();
            let switching = pending == Some(light);
            let (glyph, word, ink) = chip(light, &s.shown, switching, blank.fg, dim);
            for ch in glyph
                .chars()
                .chain(std::iter::once(' '))
                .chain(word.chars())
            {
                let mut c = blank;
                c.ch = ch;
                c.fg = ink;
                if focus.is_some_and(|(_, l)| l == light) {
                    c.bold = true;
                    c.underline = UnderlineStyle::Single;
                }
                cells.push(c);
            }
            cols.push((start..cells.len(), light));
        }
        Block {
            title,
            short_title,
            reason,
            lights: cells,
            cols,
        }
    }
}

/// Whether the keyboard's reveal draws and walks `light` in a session that
/// shows `shown`: every light whose state is known.
fn revealed(light: Light, shown: &Reading) -> bool {
    shown.state(light) != LightState::Unknown
}

/// What `light`'s title says after its name: the refusal on show, the toggle
/// in flight, what Claude said of a switch, else its state — and, under the
/// pointer or the selection, what a click does. And whether it says WHY (the
/// refusal — where a mode return stopped is one — or Claude's words): a
/// reason outranks the branch and the path in the rule, and the rest gives
/// way before any fact (`footer::fit_rule`).
fn what_of(
    s: &SessionLights,
    light: Light,
    refused: Option<(Light, Refusal, Instant)>,
    focused: bool,
    expect: &lights::Expect,
    now: Instant,
) -> (String, bool) {
    if let Some((l, why, _)) = refused
        && l == light
    {
        return (why.says(), true);
    }
    if let Some(p) = s.pending.as_ref().filter(|p| p.light == light) {
        // A command stays in flight while Claude applies it, its composer
        // hidden for up to `lights::FAST_SETTLE_MS`: not a refusal.
        let what = match (&p.drive, p.from) {
            (Drive::Command { .. }, LightState::Off) => "turning on\u{2026}",
            (Drive::Command { .. }, _) => "turning off\u{2026}",
            (Drive::CycleMode, _) => "switching\u{2026}",
        };
        return (what.to_owned(), false);
    }
    if let Some((l, until, Some(said))) = s.notice
        && l == light
        && until > now
    {
        return (said.to_owned(), true);
    }
    let what = match (light, s.shown.state(light), s.shown.mode) {
        // The chip beside it names the mode.
        (Light::Mode, LightState::Off, Some(_)) if focused => "click for bypass or auto".to_owned(),
        (Light::Mode, LightState::On | LightState::Off, Some(mode)) => mode.word().to_owned(),
        (Light::Fast, LightState::Off, Some(Mode::Auto)) if focused && expect.fast_costs_auto => {
            "off \u{00B7} Claude turns auto mode off with it here".to_owned()
        }
        (Light::Fast, LightState::Off, _) if focused => "off \u{00B7} click to turn on".to_owned(),
        (Light::Fast, LightState::On, _) if focused => "on \u{00B7} click to turn off".to_owned(),
        (_, LightState::On, _) => "on".to_owned(),
        (_, LightState::Off, _) => "off".to_owned(),
        (_, LightState::Unknown, _) => "not on screen right now".to_owned(),
    };
    (what, false)
}

/// A chip's glyph, word and ink. The mode by its own pill glyph and word, in
/// its hue where Claude gives it one (bypass, auto) and the row's ink
/// otherwise; fast mode as a dot and its word — lit in its hue when on, dim
/// when off. A toggle in flight shows `◐`.
fn chip(
    light: Light,
    shown: &Reading,
    switching: bool,
    ink: [u8; 3],
    dim: [u8; 3],
) -> (&'static str, &'static str, [u8; 3]) {
    let dot = |state: LightState| match (switching, state) {
        (true, _) => "\u{25D0}",
        (false, LightState::On) => "\u{25CF}",
        (false, LightState::Off) => "\u{25CB}",
        (false, LightState::Unknown) => "\u{25CC}",
    };
    match (light, shown.mode) {
        (Light::Mode, Some(mode)) => (
            if switching {
                dot(LightState::On)
            } else {
                mode.glyph()
            },
            mode.word(),
            mode.hue().unwrap_or(ink),
        ),
        _ => {
            let state = shown.state(light);
            let lit = switching || state == LightState::On;
            (
                dot(state),
                light.word(),
                if lit { light.hue() } else { dim },
            )
        }
    }
}

/// What one frame's reading of a pane asks of the host.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Step {
    /// Claude answered a toggle's input: wake the loop, which reads the
    /// engine and decides the next step.
    wake: bool,
    /// Claude says auto mode is off because fast mode is on
    /// (`lights::says_auto_off_for_fast`): the host latches it for the
    /// session's build.
    auto_off_for_fast: bool,
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

/// A toggle's reading of the whole screen: its lights [`Screen`] and whether
/// the composer is EMPTY (`harness::upgrade::composer_is_empty`, the rule the
/// live-upgrade driver types by) — `None` without a composer on screen.
/// `cursor` is the terminal cursor; `dim_col2[r]` whether row `r`'s column 2
/// is drawn DIM (a placeholder suggestion on the caret row, not a draft).
fn read_screen_now(
    rows: &[String],
    cursor: (usize, usize),
    dim_col2: &[bool],
) -> Option<(Screen, bool)> {
    let caret = aterm_phase::phase::composer_draft(rows).map(|(caret, _)| caret);
    let dim = caret.is_some_and(|row| dim_col2.get(row).copied().unwrap_or(false));
    let screen = lights::read_screen(rows)?;
    let empty = aterm_agent::harness::upgrade::composer_is_empty(rows, Some(cursor), dim);
    Some((screen, empty))
}

/// `read` inside the reader's panic fence for a toggle on `session`: a
/// panic is warned once and read as no composer (the toggle is refused,
/// `NotShown` — nothing is typed off an unread screen).
fn fenced_screen_now(
    session: u64,
    rows: &[String],
    read: impl FnOnce() -> Option<(Screen, bool)>,
) -> Option<(Screen, bool)> {
    crate::reader_guard::read_or_none(
        &format!("{session}"),
        "Claude Code light toggle",
        rows,
        read,
    )
    .flatten()
}

/// One session's live screen as the toggles read it, copied out of its
/// engine under the lock ([`App::claude_rows_now`]).
struct EngineRows {
    /// Its rows as text, trailing blanks trimmed.
    rows: Vec<String>,
    /// The terminal cursor: (row, column).
    cursor: (usize, usize),
    /// Whether each row's column 2 is drawn DIM (on the caret row, a
    /// placeholder suggestion, not a draft).
    dim_col2: Vec<bool>,
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

/// A typed command's composer, read from the engine (`App::typed_view`).
#[derive(Debug, Clone, Copy)]
struct TypedView {
    /// The session's person clock at the read (`HumanInputStamp::last`).
    stamp: u64,
    /// A composer is on screen and the view is not scrolled back.
    readable: bool,
    /// The composer is empty (a placeholder suggestion is empty).
    empty: bool,
    /// Claude is mid-turn.
    busy: bool,
    /// The composer holds exactly the command (`lights::composer_holds`).
    holds: bool,
    /// The light shows another state than when the toggle began: the
    /// command went.
    switched: bool,
}

impl TypedView {
    fn unreadable(stamp: u64) -> Self {
        Self {
            stamp,
            readable: false,
            empty: false,
            busy: false,
            holds: false,
            switched: false,
        }
    }
}

/// A ctrl+u did not take a typed command back: a Claude Code whose composer
/// no longer kills the line with it. Said in aterm's log once a run, so the
/// drift is found by reading the log rather than by commands left behind.
fn note_uncleared(session: u64, cmd: &str) {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        aterm_log::warn!(
            "claude lights: session {session}: ctrl+u did not take back {cmd:?}; the command is \
             left in the prompt (a Claude Code whose composer changed?)"
        );
    }
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
pub(crate) struct Block {
    /// `Light: what` — or empty.
    pub(crate) title: Vec<RenderCell>,
    /// `what` alone, for a pane with no room for the whole title.
    pub(crate) short_title: Vec<RenderCell>,
    /// The title is a REASON ([`what_of`]), which outranks the branch and
    /// the path in the rule; any other title gives way before every fact.
    pub(crate) reason: bool,
    /// The chips, one blank apart (the rule's layout pads the block).
    pub(crate) lights: Vec<RenderCell>,
    cols: Vec<(std::ops::Range<usize>, Light)>,
}

#[cfg(test)]
impl Block {
    /// Whether `light`'s chip is drawn.
    pub(crate) fn shows(&self, light: Light) -> bool {
        self.cols.iter().any(|(_, l)| *l == light)
    }

    /// A block of `lights` (the chips' cells, as one fast chip) and its
    /// titles, for the rule's layout tests (`claude_footer`).
    pub(crate) fn of_chips(
        lights: Vec<RenderCell>,
        title: Vec<RenderCell>,
        short_title: Vec<RenderCell>,
        reason: bool,
    ) -> Block {
        Block {
            cols: vec![(0..lights.len(), Light::Fast)],
            title,
            short_title,
            reason,
            lights,
        }
    }
}

impl App {
    /// Read one Claude Code pane's screen for its lights: remember what each
    /// shows and move a toggle in flight along, waking the loop when its next
    /// step was queued. Called for EVERY Claude Code pane each frame, painted
    /// footer or not: a pasted slash command opens Claude's completion list
    /// where the mode row was, and the toggle must still see the command land.
    pub(crate) fn observe_claude_lights(&mut self, wid: WindowId, session: u64, rows: &[String]) {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let step = ws
            .claude_lights
            .observe_rows(session, rows, lights::read_screen);
        if step.wake {
            crate::claude_footer::post_changed(session);
        }
        if step.auto_off_for_fast {
            let (build, _) = self.claude_build_and_model(session);
            if self.claude_fast_latch.cost_auto(build) {
                self.redraw_every_window();
            }
        }
    }

    /// The light block to paint in `session`'s composer rule, its chips in
    /// `room` cells (`footer::chips_room`) — `None` when no chip is drawn:
    /// at rest the rule carries the facts alone. Short of room, the
    /// keyboard's reveal goes first, so a chip drawn at rest (a deviation,
    /// the selection itself) stays; a rule too narrow even for those draws
    /// none.
    pub(crate) fn claude_lights_block(
        &self,
        wid: WindowId,
        session: u64,
        blank: RenderCell,
        room: usize,
    ) -> Option<Block> {
        let ws = self.windows.get(&wid)?;
        let expect = self.claude_lights_expect(session);
        let now = Instant::now();
        [true, false]
            .into_iter()
            .map(|reveal| ws.claude_lights.block(session, blank, &expect, now, reveal))
            .find(|block| block.lights.len() <= room)
            .filter(|block| !block.cols.is_empty())
    }

    /// `session`'s Claude build and model, as its footer facts name them —
    /// the fast latch's key.
    fn claude_build_and_model(&self, session: u64) -> (Option<String>, Option<String>) {
        self.pool
            .get(session)
            .and_then(|entry| {
                entry
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .claude_footer_shown()
                    .map(|facts| (facts.version, facts.model))
            })
            .unwrap_or_default()
    }

    /// What the owner expects of `session`'s lights: fast mode ON, unless
    /// Claude refused it lastingly for this session's Claude build and
    /// model — and not in auto mode where this build has taken auto off for
    /// it (the latch, `App::claude_fast_latch`).
    fn claude_lights_expect(&self, session: u64) -> lights::Expect {
        let latch = &self.claude_fast_latch;
        if latch.refused.is_empty() && latch.costs_auto.is_empty() {
            return lights::Expect::default();
        }
        let (build, model) = self.claude_build_and_model(session);
        lights::Expect {
            fast_costs_auto: latch.costs_auto.contains(&build),
            fast: !latch.refused.contains_key(&(build, model)),
        }
    }

    /// Every window repaints: a latch changed what their chips say.
    fn redraw_every_window(&self) {
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
    /// hit per cell, so the whole chip is the target.
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
            ws.claude_lights
                .hits
                .extend(block.cols.iter().flat_map(|(span, light)| {
                    span.clone().map(move |at| LightHit {
                        frame_row,
                        term_row,
                        col: col + at,
                        session,
                        light: *light,
                    })
                }));
        }
    }

    /// The light under window pixel `(x, y)`, if the glass shows one there —
    /// never one a later band painted over (the link caption yields to no
    /// light: it claims the row in the chrome register, and the light under
    /// it is gone from the glass).
    pub(crate) fn claude_light_at(&self, wid: WindowId, x: f64, y: f64) -> Option<(u64, Light)> {
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
        (!self.chrome_owns_terminal_row(wid, hit.term_row)).then_some((hit.session, hit.light))
    }

    /// The pointer left the window: no light is hovered any more.
    pub(crate) fn clear_claude_light_hover(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid)
            && ws.claude_lights.hover.take().is_some()
        {
            redraw(ws);
        }
    }

    /// A mouse press anywhere lets a keyboard selection go: the person has
    /// moved on (a click to another pane and back must not leave a selected
    /// light waiting to swallow the next Return).
    pub(crate) fn clear_claude_light_selection(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid)
            && ws.claude_lights.selected.take().is_some()
        {
            redraw(ws);
        }
    }

    /// Pointer motion: move the hover (repainting only on a change) and say
    /// whether the pointer is over a light — the caller then keeps the motion
    /// from the grid, so neither a selection nor a PTY motion report starts.
    pub(crate) fn track_claude_light_hover(&mut self, wid: WindowId, x: f64, y: f64) -> bool {
        let over = self.claude_light_at(wid, x, y);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        if ws.selecting {
            return false;
        }
        if ws.claude_lights.hover != over {
            ws.claude_lights.hover = over;
            redraw(ws);
        }
        over.is_some()
    }

    /// A left press on a light toggles it; whether the press was one.
    pub(crate) fn press_claude_light(&mut self, wid: WindowId) -> bool {
        let (x, y) = self
            .windows
            .get(&wid)
            .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
        let Some((session, light)) = self.claude_light_at(wid, x, y) else {
            return false;
        };
        self.toggle_claude_light(wid, session, light);
        true
    }

    /// Flip `light` of `session` with Claude's own input (see the module
    /// header): refused, with the reason on the light, when its state is not
    /// on screen, or a slash command would land on a draft, on a turn Claude
    /// does not run it in, or in a driver's prompt. A light already as
    /// expected (the mode light in bypass or auto) does nothing at all.
    /// Decided from the
    /// ENGINE's screen as it is now, not from the last painted frame: a mode
    /// that changed since is the one the toggle starts from.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "claude_light_admission",
            action = "StartAccepted",
            project = "claude_lights::gesture_tests::project_admission"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "claude_light_admission",
            action = "StartRejected",
            project = "claude_lights::gesture_tests::project_admission"
        )
    )]
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
        // each pressing on the other's answers — and a command aterm typed is
        // the session's until it is submitted, taken back or left.
        if self.claude_typed.contains_key(&session)
            || self.windows.values().any(|w| {
                w.claude_lights
                    .sessions
                    .get(&session)
                    .is_some_and(|s| s.pending.is_some())
            })
        {
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
        let reading = self.claude_screen_now(session);
        let lease = self.session_lease(session);
        let expect = self.claude_lights_expect(session);
        let ordering = self
            .pool
            .get(session)
            .is_some_and(|entry| crate::app_input::paste_order::is_ordering(&entry.ctx.sink));
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let Some((screen, empty)) = reading else {
            refuse(ws, Refusal::NotShown);
            return;
        };
        let reading = lights::read(Some(&screen));
        let from = reading.state(light);
        // The mode light in a mode the owner expects: nothing to do — its one
        // action is back to bypass or auto, and the session is there. Never a
        // press away from it (owner, 2026-09-27).
        if light == Light::Mode && from == LightState::On {
            return;
        }
        let Some(drive) = lights::drive(light, &reading) else {
            refuse(ws, Refusal::NotShown);
            return;
        };
        if let Drive::Command { immediate, .. } = drive {
            if !empty {
                refuse(ws, Refusal::Draft);
                return;
            }
            // A command Claude does not run mid-turn would flip a turn later,
            // the light wrong meanwhile: it waits for the turn. `/fast` is
            // IMMEDIATE (`lights::Drive::Command`): Claude runs it mid-turn,
            // the composer still up.
            if screen.busy && !immediate {
                refuse(ws, Refusal::Busy);
                return;
            }
            // A driver may still be typing into this composer: a cooperative
            // `lease`'s holder at any time, a `turn` until Claude has taken
            // its prompt. A busy Claude under a `turn` HAS taken it — a
            // command typed now cannot join that prompt. A mode light stays
            // free — shift+tab never touches the prompt.
            if lease
                .as_ref()
                .is_some_and(|(_, turn)| !*turn || !screen.busy)
            {
                refuse(ws, Refusal::Driven);
                return;
            }
            if ordering {
                // The empty screen can precede an earlier accepted paste's
                // echo. Do not add our command behind that unread draft.
                refuse(ws, Refusal::InputPending);
                return;
            }
            // Fast mode on, in auto mode, on a build that has taken auto off
            // for it before: the trade is not made on a click the person
            // may take for "fast mode" alone.
            if light == Light::Fast
                && from == LightState::Off
                && screen.mode == Some(Mode::Auto)
                && expect.fast_costs_auto
            {
                refuse(ws, Refusal::CostsAuto);
                return;
            }
        }
        let command = match drive {
            Drive::Command { cmd, immediate } => Some((cmd, immediate)),
            Drive::CycleMode => None,
        };
        // A mode light is only ever clicked while its pill is read (`from` is
        // Unknown otherwise), so the mode is there to start the cycle from.
        // Don't ask too: it leads on to manual, and forward from there.
        let Some(mode) = screen.mode.or(command.map(|_| Mode::Manual)) else {
            refuse(ws, Refusal::NotShown);
            return;
        };
        let (first, poll) = match command {
            None => (
                crate::input::InputEvent::KeySequence(lights::SHIFT_TAB.to_vec()),
                MODE_POLL,
            ),
            Some((cmd, _)) => (
                crate::input::InputEvent::Paste(
                    cmd.to_owned(),
                    crate::input::PasteFraming::AtDrain,
                ),
                ANSWER_POLL,
            ),
        };
        // The toggle's answer may take `settle`; the typed command's echo and
        // Enter, one ordinary step.
        let settle = Duration::from_millis(lights::settle_ms(&drive));
        let echo = Duration::from_millis(lights::SETTLE_MS);
        // Claude's answers to the command already on screen: an earlier try's
        // is never this one's.
        let answers = command
            .and_then(|(cmd, _)| {
                self.claude_rows_now(session)
                    .map(|now| lights::fast_answers(&now.rows, cmd))
            })
            .unwrap_or_default();
        // The click's own input: the person's (see the module header).
        let outcome = self.input_to_session(
            wid,
            first,
            crate::input::Source::Human,
            Some(session),
            crate::app_input::PressPhase::Initial,
        );
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if outcome != crate::input::InputOutcome::Ok {
            // A full/disconnected FIFO accepted no command. In particular,
            // an earlier queued paste's identical echo must never become ours.
            refuse(ws, Refusal::InputRejected);
            return;
        }
        let s = ws.claude_lights.sessions.entry(session).or_default();
        s.shown.fold(&reading);
        s.refused = None;
        s.pending = Some(Pending {
            light,
            from,
            drive,
            started: mode,
            mode_at_press: mode,
            seen: Vec::new(),
            presses: 1,
            deadline: now + settle,
            next_poll: Some(now + poll),
            held_mid_turn: false,
            echoes_floor: answers.echoed,
            stale_notice: answers.notice,
            last_said: None,
        });
        redraw(ws);
        wake_after(session, settle);
        // The toggle reads the engine for its answer on its own timer,
        // painted frames or none.
        wake_after(session, poll);
        if let Some((cmd, immediate)) = command
            && let Some(entry) = self.pool.get(session)
        {
            // The paste stamped the person clock itself (it went through the
            // seam as the click's input): read AFTER it, so only a later
            // gesture moves it.
            let stamp = entry.ctx.human_input.last();
            self.claude_typed.insert(
                session,
                Typed {
                    light,
                    from,
                    cmd,
                    stamp,
                    immediate,
                    lease_at_paste: lease.map(|(token, _)| token),
                    hands_off: false,
                    deadline: now + echo,
                    next_poll: Some(now + TYPED_POLL),
                    cleared: None,
                    late: false,
                },
            );
            wake_after(session, TYPED_POLL);
        }
    }

    /// The driver holding `session`'s lease now, as `who`'s `driving=` token
    /// (`<turn-id>` for a `turn`, `lease:<holder>` for a live `lease`,
    /// `Lease::driving_token`), and whether it is a `turn` — `None` when no
    /// driver holds it (a lapsed lease reads as none).
    fn session_lease(&self, session: u64) -> Option<(String, bool)> {
        let entry = self.pool.get(session)?;
        let lease = entry
            .ctx
            .turn_lease
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let lease = lease.as_ref()?;
        lease
            .driving_token(crate::metrics::now_us())
            .map(|token| (token, matches!(lease, crate::Lease::Turn { .. })))
    }

    /// `session`'s screen read straight from its engine ([`read_screen_now`],
    /// behind the reader's panic fence) — `None` without a composer on
    /// screen or on a reader panic. The engine's lock is held only to copy
    /// the rows out; the reader runs after it is dropped.
    fn claude_screen_now(&self, session: u64) -> Option<(Screen, bool)> {
        let EngineRows {
            rows,
            cursor,
            dim_col2,
        } = self.claude_rows_now(session)?;
        fenced_screen_now(session, &rows, || read_screen_now(&rows, cursor, &dim_col2))
    }

    /// `session`'s live screen copied out of its engine: its rows as text,
    /// the cursor, and whether each row's column 2 is drawn DIM (on the caret
    /// row, a placeholder suggestion, not a draft — the `cell` verb's
    /// reading, which `composer_is_empty` wants). `None` for a gone session
    /// or a view scrolled into history. The lock is held only to copy.
    fn claude_rows_now(&self, session: u64) -> Option<EngineRows> {
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
        Some(EngineRows {
            rows,
            cursor,
            dim_col2,
        })
    }

    /// The footer wake's lights half: move every mode return along from the
    /// ENGINE ([`Self::advance_claude_mode_returns`]), give up on toggles past
    /// their deadline (and time the end of the refusal that says so), then
    /// the typed commands' Enter or take-back ([`Self::drain_claude_typed`]).
    pub(crate) fn drain_claude_lights(&mut self) {
        let now = Instant::now();
        self.advance_claude_mode_returns(now);
        self.answer_claude_commands(now);
        for ws in self.windows.values_mut() {
            let (changed, refused) = ws.claude_lights.expire(now);
            if changed {
                redraw(ws);
            }
            for session in refused {
                wake_after(session, REFUSAL_SHOWN);
            }
        }
        self.drain_claude_typed(now);
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
                    .filter(|(_, s)| {
                        s.pending
                            .as_ref()
                            .is_some_and(|p| p.drive == Drive::CycleMode)
                    })
                    .map(|(session, _)| (*wid, *session))
            })
            .collect();
        for (wid, session) in returns {
            let screen = self.claude_screen_now(session).map(|(screen, _)| screen);
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

    /// Every slash command in flight, resolved from the ENGINE: its light
    /// switched, or Claude's own answer to it (`lights::fast_answers`) — a
    /// refusal said in Claude's words, and latched for this Claude build and
    /// model when it is lasting (`lights::FastRefusal::lasting`); a switch
    /// that took auto mode off latched for the build. One poll is kept
    /// armed while the command is in flight — through a view scrolled into
    /// history, which the engine read skips: an idle Claude hides its
    /// composer while it applies `/fast`, and nothing paints meanwhile.
    fn answer_claude_commands(&mut self, now: Instant) {
        let commands: Vec<(WindowId, u64, &'static str)> = self
            .windows
            .iter()
            .flat_map(|(wid, ws)| {
                ws.claude_lights.sessions.iter().filter_map(|(session, s)| {
                    match s.pending.as_ref()?.drive {
                        Drive::Command { cmd, .. } => Some((*wid, *session, cmd)),
                        Drive::CycleMode => None,
                    }
                })
            })
            .collect();
        for (wid, session, cmd) in commands {
            let read = self
                .claude_rows_now(session)
                .and_then(|EngineRows { rows, .. }| {
                    crate::reader_guard::read_or_none(
                        &format!("{session}"),
                        "Claude Code fast answer",
                        &rows,
                        || {
                            let screen = lights::read_screen(&rows);
                            CommandRead {
                                fast: lights::read(screen.as_ref()).fast,
                                mode: screen.and_then(|screen| screen.mode),
                                answers: lights::fast_answers(&rows, cmd),
                                auto_off: lights::says_auto_off_for_fast(&rows),
                            }
                        },
                    )
                });
            let (build, model) = self.claude_build_and_model(session);
            let Some(ws) = self.windows.get_mut(&wid) else {
                continue;
            };
            let end = read.and_then(|read| ws.claude_lights.answer(session, &read, now));
            let Some(end) = end else {
                // Not answered, or not readable (scrolled back, a reader
                // panic): look again.
                ws.claude_lights.arm_poll(session, now, ANSWER_POLL);
                continue;
            };
            redraw(ws);
            wake_after(session, REFUSAL_SHOWN);
            let mut latched = false;
            if let Some(why) = end.refusal.filter(|why| why.lasting()) {
                latched |= self.claude_fast_latch.refuse(build.clone(), model, why);
            }
            if end.cost_auto {
                latched |= self.claude_fast_latch.cost_auto(build);
            }
            if latched {
                self.redraw_every_window();
            }
        }
    }

    /// The typed commands' half of the footer wake: for each slash command a
    /// light typed, read the session's composer from the ENGINE and decide —
    /// submit it (the composer holds exactly it, Claude is idle or runs the
    /// command mid-turn, and no other hand has come to the session since),
    /// wait, or at the deadline take it
    /// back (one ctrl+u under the same conditions, read back once) or leave it
    /// and say so. No key ever goes into a composer another hand has touched:
    /// the text may be theirs.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "claude_light_admission",
            action = "FollowAccepted",
            project = "claude_lights::gesture_tests::project_admission"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "claude_light_admission",
            action = "FollowRejected",
            project = "claude_lights::gesture_tests::project_admission"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "claude_light_admission",
            action = "Wait",
            project = "claude_lights::gesture_tests::project_admission"
        )
    )]
    fn drain_claude_typed(&mut self, now: Instant) {
        let sessions: Vec<u64> = self.claude_typed.keys().copied().collect();
        for session in sessions {
            let Some(t) = self.claude_typed.get(&session).cloned() else {
                continue;
            };
            // A ctrl+u's read-back is not due yet: its own wake is posted.
            if t.cleared.is_some_and(|at| now < at) {
                continue;
            }
            let Some(view) = self.typed_view(session, &t) else {
                self.claude_typed.remove(&session);
                continue;
            };
            if t.cleared.is_some() {
                self.claude_typed.remove(&session);
                if view.holds {
                    note_uncleared(session, t.cmd);
                    self.leave_claude_typed(session, t.light, now);
                } else if !view.switched {
                    // Taken back: the command never went, and nothing will
                    // answer it — said now, not at the end of the answer's
                    // own window (`lights::FAST_SETTLE_MS`).
                    self.end_claude_command(session, t.light, now);
                }
                continue;
            }
            // A lease taken or changed since the paste is another hand; one
            // that was live at the paste (a `turn` whose prompt Claude had
            // already taken) is not.
            let lease = self.session_lease(session).map(|(token, _)| token);
            let leased = lease.is_some() && lease != t.lease_at_paste;
            let hands_off = t.hands_off || view.stamp != t.stamp || leased;
            let ordering = self
                .pool
                .get(session)
                .is_some_and(|entry| crate::app_input::paste_order::is_ordering(&entry.ctx.sink));
            if now >= t.deadline {
                self.claude_typed.remove(&session);
                if ordering {
                    // The command may still arrive after this deadline.
                    // Never queue Return or cleanup behind undelivered input.
                    self.leave_claude_typed(session, t.light, now);
                    continue;
                }
                // It went — the person's own Return, say: nothing to say.
                if view.switched {
                    continue;
                }
                if !hands_off
                    && view.holds
                    && let Some(kill) = crate::control::parse_key("ctrl+u")
                {
                    let Some(stamp) = self.send_typed(session, kill) else {
                        self.leave_claude_typed(session, t.light, now);
                        continue;
                    };
                    self.claude_typed.insert(
                        session,
                        Typed {
                            stamp,
                            cleared: Some(now + CLEAR_READ_BACK),
                            ..t
                        },
                    );
                    wake_after(session, CLEAR_READ_BACK);
                    continue;
                }
                if !hands_off && view.readable && view.empty && !t.late {
                    // Nothing in the prompt yet: one late look, in case the
                    // paste's echo is still on its way.
                    self.claude_typed.insert(
                        session,
                        Typed {
                            deadline: now + REFUSAL_SHOWN,
                            next_poll: None,
                            late: true,
                            ..t
                        },
                    );
                    wake_after(session, REFUSAL_SHOWN);
                    continue;
                }
                if !(view.readable && view.empty) {
                    self.leave_claude_typed(session, t.light, now);
                }
                continue;
            }
            if hands_off {
                // No more keys; the toggle's settle wake brings the deadline.
                if let Some(rec) = self.claude_typed.get_mut(&session) {
                    rec.hands_off = true;
                }
                continue;
            }
            if t.late {
                // The late look waits for its own wake; it never submits.
                continue;
            }
            if !ordering && view.holds && (!view.busy || t.immediate) {
                let accepted = self.send_typed(
                    session,
                    crate::input::InputEvent::KeySequence(b"\r".to_vec()),
                );
                self.claude_typed.remove(&session);
                if accepted.is_none() {
                    // Do not silently forget an unsubmitted command or retry
                    // later under a composer that another gesture may change.
                    self.leave_claude_typed(session, t.light, now);
                } else {
                    self.open_claude_answer_window(session, t.light, now);
                }
                continue;
            }
            if t.next_poll.is_none_or(|at| now >= at)
                && let Some(rec) = self.claude_typed.get_mut(&session)
            {
                rec.next_poll = Some(now + TYPED_POLL);
                wake_after(session, TYPED_POLL);
            }
        }
    }

    /// A typed command's Enter went: Claude's answer may take its whole
    /// window (`lights::settle_ms`, the up-to-8 s org check of `/fast`) FROM
    /// NOW — the paste's echo may have taken most of a step already, and
    /// the window must not be spent before the command is even submitted.
    fn open_claude_answer_window(&mut self, session: u64, light: Light, now: Instant) {
        let mut settle = None;
        for ws in self.windows.values_mut() {
            if let Some(p) = ws
                .claude_lights
                .sessions
                .get_mut(&session)
                .and_then(|s| s.pending.as_mut())
                .filter(|p| p.light == light)
            {
                let window = Duration::from_millis(lights::settle_ms(&p.drive));
                p.deadline = p.deadline.max(now + window);
                settle = Some(window);
            }
        }
        if let Some(window) = settle {
            wake_after(session, window);
        }
    }

    /// Send one of a typed command's keys to `session` through the input
    /// seam — aterm's own write after the click, not a person's gesture, so
    /// as `Source::Controller` (the module header) — and return the person
    /// clock right after acceptance: any other value later is a person.
    /// `None` means no route or rejected input; the caller must say text is left.
    fn send_typed(&mut self, session: u64, ev: crate::input::InputEvent) -> Option<u64> {
        // A window that shows the session (its focused pane, else any of its
        // tabs); a session no window shows is reached through any window.
        let wid = self
            .windows_with_focused_session(session)
            .first()
            .copied()
            .or_else(|| {
                self.tabs_viewing_session(session)
                    .first()
                    .map(|(wid, _)| *wid)
            })
            .or_else(|| self.windows.keys().next().copied())?;
        if self.input_to_session(
            wid,
            ev,
            crate::input::Source::Controller,
            Some(session),
            crate::app_input::PressPhase::Initial,
        ) != crate::input::InputOutcome::Ok
        {
            return None;
        }
        self.pool
            .get(session)
            .map(|entry| entry.ctx.human_input.last())
    }

    /// A typed command is left in `session`'s composer: every window that
    /// knows the session drops the toggle and says so on the light.
    fn leave_claude_typed(&mut self, session: u64, light: Light, now: Instant) {
        for ws in self.windows.values_mut() {
            let Some(s) = ws.claude_lights.sessions.get_mut(&session) else {
                continue;
            };
            if s.pending.as_ref().is_some_and(|p| p.light == light) {
                s.pending = None;
            }
            s.refused = Some((light, Refusal::LeftTyped, now + REFUSAL_SHOWN));
            redraw(ws);
        }
        wake_after(session, REFUSAL_SHOWN);
    }

    /// A typed command was taken back before it went: every window whose
    /// toggle of `light` is still in flight on `session` ends it, saying
    /// Claude did not switch.
    fn end_claude_command(&mut self, session: u64, light: Light, now: Instant) {
        for ws in self.windows.values_mut() {
            let Some(s) = ws.claude_lights.sessions.get_mut(&session) else {
                continue;
            };
            if s.pending.take_if(|p| p.light == light).is_none() {
                continue;
            }
            s.refused = Some((light, Refusal::NoChange, now + REFUSAL_SHOWN));
            redraw(ws);
        }
        wake_after(session, REFUSAL_SHOWN);
    }

    /// `session`'s composer as the typed command `typed` sees it, read from the
    /// engine (the reader behind its panic fence; a panic reads as
    /// unreadable) — `None` when the session is gone.
    fn typed_view(&self, session: u64, typed: &Typed) -> Option<TypedView> {
        let stamp = self.pool.get(session)?.ctx.human_input.last();
        let Some(EngineRows {
            rows,
            cursor,
            dim_col2,
        }) = self.claude_rows_now(session)
        else {
            return Some(TypedView::unreadable(stamp));
        };
        let read = crate::reader_guard::read_or_none(
            &format!("{session}"),
            "Claude Code typed command",
            &rows,
            || {
                read_screen_now(&rows, cursor, &dim_col2).map(|(screen, empty)| {
                    let now = lights::read(Some(&screen)).state(typed.light);
                    TypedView {
                        stamp,
                        readable: true,
                        empty,
                        busy: screen.busy,
                        holds: lights::composer_holds(&rows, cursor, typed.cmd),
                        switched: now != typed.from && now != LightState::Unknown,
                    }
                })
            },
        )
        .flatten();
        Some(read.unwrap_or_else(|| TypedView::unreadable(stamp)))
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
    /// the focused pane's next light; with one selected, Return/Space toggle,
    /// ←/→ move and Escape lets go. Any other key lets go and is NOT consumed.
    /// Whether the key was the lights'.
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
        // every key in that common case. A selected light still needs the full
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
        let expect = self.claude_lights_expect(session);
        let now = Instant::now();
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        // The pane's footer rule is on the glass, with this much room for its
        // chips: its lights can be revealed there, drawn at rest or not.
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
        let stale = ws.claude_lights.selected.is_some_and(|(id, light)| {
            id != session
                || (ws.claude_lights.selection_painted
                    && !ws
                        .claude_lights
                        .hits
                        .iter()
                        .any(|h| h.session == id && h.light == light))
        });
        if stale {
            ws.claude_lights.select(None);
            redraw(ws);
        }
        // macOS only: everywhere else `ctrl+shift+tab` is aterm's default
        // `prev_tab` (`keybinding::PLATFORM_DEFAULT_PAIRS`), and this gate runs
        // before the keybindings. The lights stay clickable on every platform.
        // The selection walks every revealable light — the reveal draws them
        // while it lasts, the ones at rest included — but only onto one whose
        // chip FITS the pane's row (the reveal is dropped first): a selection
        // nobody can see must never take the next Return.
        let order = ws.claude_lights.revealable(session);
        let lights_ = &ws.claude_lights;
        let fits = |light: Light| {
            room.is_some_and(|room| {
                lights_
                    .block_with(
                        session,
                        RenderCell::default(),
                        &expect,
                        now,
                        Some(light),
                        None,
                        false,
                    )
                    .lights
                    .len()
                    <= room
            })
        };
        let step = |from: Option<Light>, forward: bool| -> Option<Light> {
            let n = order.len();
            let at = from
                .and_then(|l| order.iter().position(|o| *o == l))
                .unwrap_or(if forward { n.checked_sub(1)? } else { 0 });
            (1..=n)
                .map(|k| {
                    order[if forward {
                        (at + k) % n
                    } else {
                        (at + n * k - k) % n
                    }]
                })
                .find(|l| fits(*l))
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
            if let Some(next) = step(selected, true) {
                ws.claude_lights.select(Some((session, next)));
                redraw(ws);
            }
            return true;
        }
        let Some(light) = selected else {
            return false;
        };
        let plain = !mods.control_key() && !mods.alt_key() && !mods.super_key();
        match &ev.logical_key {
            Key::Named(NamedKey::Enter | NamedKey::Space) if plain => {
                ws.claude_lights.select(None);
                redraw(ws);
                self.toggle_claude_light(wid, session, light);
                true
            }
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowLeft) if plain => {
                let forward = matches!(ev.logical_key, Key::Named(NamedKey::ArrowRight));
                let next = step(Some(light), forward).map(|l| (session, l));
                ws.claude_lights.select(next);
                redraw(ws);
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

    #[test]
    fn inert_session_history_does_not_change_the_lights_repaint_key() {
        let now = Instant::now();
        let mut w = WindowLights {
            hover: Some((7, Light::Mode)),
            ..WindowLights::default()
        };
        w.sessions.insert(7, SessionLights::default());
        let hovered = w.fingerprint(now);
        assert_ne!(hovered, 0);

        for session in 100..1_100 {
            w.sessions.insert(session, SessionLights::default());
        }
        assert_eq!(w.fingerprint(now), hovered);

        w.sessions.get_mut(&1_000).unwrap().refused =
            Some((Light::Fast, Refusal::Draft, now + Duration::from_secs(1)));
        assert_ne!(w.fingerprint(now), hovered);
        assert_eq!(w.fingerprint(now + Duration::from_secs(2)), hovered);

        w.hover = None;
        assert_eq!(w.fingerprint(now + Duration::from_secs(2)), 0);
    }

    /// Manual cost diagnostic: run with `--ignored --nocapture` to compare a
    /// visible light with and without a long history of inert Claude sessions.
    #[test]
    #[ignore = "manual fingerprint cost diagnostic"]
    fn lights_fingerprint_one_vs_thousand_inert_sessions() {
        const SAMPLES: u128 = 50_000;
        let now = Instant::now();
        let mut short = WindowLights {
            hover: Some((7, Light::Mode)),
            ..WindowLights::default()
        };
        short.sessions.insert(7, SessionLights::default());
        short.sessions.get_mut(&7).unwrap().refused =
            Some((Light::Fast, Refusal::Draft, now + Duration::from_secs(60)));
        let mut long = WindowLights {
            hover: short.hover,
            ..WindowLights::default()
        };
        long.sessions = short.sessions.clone();
        for session in 100..1_100 {
            long.sessions.insert(session, SessionLights::default());
        }
        assert_eq!(short.fingerprint(now), long.fingerprint(now));

        let measure = |w: &WindowLights| {
            let start = Instant::now();
            for _ in 0..SAMPLES {
                std::hint::black_box(w.fingerprint(now));
            }
            start.elapsed().as_nanos() / SAMPLES
        };
        eprintln!(
            "Claude lights fingerprint: 1 session={} ns/call, 1001 sessions={} ns/call",
            measure(&short),
            measure(&long)
        );
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
        w.observe_rows(7, rows, lights::read_screen)
    }

    /// The lights read behind the reader's panic fence: a panicking reader
    /// folds NOTHING in — the lights keep what they showed, a mode cycle in
    /// flight takes no step (no shift+tab queued, its press count and mode
    /// untouched) — and a toggle's own reading is refused (`None`: nothing is
    /// typed). Controls: the same screen through the real readers DOES step
    /// the cycle, and a toggle's reading of it is a composer, empty.
    #[test]
    fn a_reader_panic_folds_nothing_in_and_types_nothing() {
        let now = Instant::now();
        let rule = "\u{2500}".repeat(60);
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&rule, MANUAL, ""));
        start(&mut w, Light::Mode, Drive::CycleMode, Mode::Manual, now);
        let shown = w.sessions[&7].shown;
        let boom = |_: &[String]| -> Option<Screen> { panic!("stand-in reader panic") };
        let plan = screen(&rule, PLAN, "");
        assert_eq!(w.observe_rows(7, &plan, boom), Step::default());
        let s = &w.sessions[&7];
        assert_eq!(s.shown, shown, "the lights keep what they showed");
        let p = s.pending.as_ref().expect("the toggle is still in flight");
        assert_eq!((p.presses, p.mode_at_press), (1, Mode::Manual));
        // A toggle's reading.
        let dims = vec![false; plan.len()];
        assert!(
            fenced_screen_now(7, &plan, || -> Option<(Screen, bool)> {
                panic!("stand-in reader panic")
            })
            .is_none()
        );
        let (_, empty) = fenced_screen_now(7, &plan, || read_screen_now(&plan, (1, 2), &dims))
            .expect("the real reader sees the composer");
        assert!(empty);
        // Control: the real reader wakes the loop on the same screen.
        assert!(observe(&mut w, &plan).wake);
    }

    fn start(w: &mut WindowLights, light: Light, drive: Drive, mode: Mode, now: Instant) {
        let s = w.sessions.entry(7).or_default();
        s.pending = Some(Pending {
            light,
            from: s.shown.state(light),
            drive,
            started: mode,
            mode_at_press: mode,
            seen: Vec::new(),
            presses: 1,
            deadline: now + Duration::from_millis(lights::SETTLE_MS),
            next_poll: None,
            held_mid_turn: false,
            echoes_floor: 0,
            stale_notice: None,
            last_said: None,
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
        start(
            &mut w,
            Light::Mode,
            Drive::CycleMode,
            Mode::AcceptEdits,
            now,
        );
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
            matches!(w.sessions[&7].notice, Some((Light::Mode, _, None))),
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
        start(&mut w, Light::Mode, Drive::CycleMode, Mode::Manual, now);
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
        start(&mut w, Light::Mode, Drive::CycleMode, Mode::Manual, now);
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
            .block(7, blank(), &lights::Expect::default(), due, true)
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
        start(&mut w, Light::Mode, Drive::CycleMode, Mode::Manual, now);
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
            .block(7, blank(), &lights::Expect::default(), two, true)
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
        start(&mut w, Light::Mode, Drive::CycleMode, Mode::Manual, now);
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
        start(
            &mut timely,
            Light::Mode,
            Drive::CycleMode,
            Mode::Manual,
            now,
        );
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
        start(
            &mut arrived,
            Light::Mode,
            Drive::CycleMode,
            Mode::Manual,
            now,
        );
        assert_eq!(
            arrived.advance(7, engine(&rule, BYPASS).as_ref(), due),
            Advance::Arrived
        );
        assert!(arrived.sessions[&7].pending.is_none());
    }

    /// A cycle that holds neither bypass nor auto (manual → accept edits →
    /// plan → manual): the search comes back round to MANUAL and stops — the
    /// mode the person had — saying neither is in this cycle and naming the
    /// mode, never pressing on into another lap.
    #[test]
    fn a_cycle_without_an_expected_mode_is_refused_after_one_lap_back_where_it_began() {
        let now = Instant::now();
        let rule = "\u{2500}".repeat(60);
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&rule, MANUAL, ""));
        start(&mut w, Light::Mode, Drive::CycleMode, Mode::Manual, now);
        for (row, mode) in [(ACCEPT, Mode::AcceptEdits), (PLAN, Mode::Plan)] {
            assert_eq!(
                w.advance(7, engine(&rule, row).as_ref(), now),
                Advance::Press { expect: mode },
                "{mode:?}: press on"
            );
        }
        assert_eq!(
            w.advance(7, engine(&rule, MANUAL).as_ref(), now),
            Advance::Lapped,
            "a lap: stop"
        );
        let s = &w.sessions[&7];
        assert!(s.pending.is_none());
        assert!(matches!(
            s.refused,
            Some((Light::Mode, Refusal::NotInCycle(Mode::Manual), _))
        ));
        assert_eq!(s.shown.mode, Some(Mode::Manual), "manual, as before");
    }

    /// A slash command's frames queue nothing — its Enter is judged from the
    /// engine by the loop (`App::drain_claude_typed`), never off a frame — a
    /// frame whose tag moved only WAKES the loop, and the engine's read
    /// resolves it (`WindowLights::answer`).
    #[test]
    fn a_commands_frames_queue_nothing_and_its_tag_resolves_it() {
        let now = Instant::now();
        let bare = "\u{2500}".repeat(60);
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&bare, BYPASS, ""));
        start(
            &mut w,
            Light::Fast,
            Drive::Command {
                cmd: "/fast on",
                immediate: true,
            },
            Mode::Bypass,
            now,
        );
        assert!(!observe(&mut w, &screen(&bare, BYPASS, "/fast on")).wake);
        let tagged = format!("{} \u{21AF} \u{2500}", "\u{2500}".repeat(40));
        assert!(observe(&mut w, &screen(&tagged, BYPASS, "")).wake);
        assert!(w.sessions[&7].pending.is_some(), "a frame decides nothing");
        let end = w.answer(
            7,
            &read_of(LightState::On, Mode::Bypass, Default::default()),
            now,
        );
        assert_eq!(
            end,
            Some(CommandEnd {
                refusal: None,
                cost_auto: false
            })
        );
        assert!(w.sessions[&7].pending.is_none());
    }

    /// An engine read for [`WindowLights::answer`]: fast mode, the pill, the
    /// answers, nothing said of auto mode.
    fn read_of(fast: LightState, mode: Mode, answers: lights::FastAnswers) -> CommandRead {
        CommandRead {
            fast,
            mode: Some(mode),
            answers,
            auto_off: false,
        }
    }

    /// `/fast on` in flight in session 7, fast mode off, from `mode`, with
    /// `answers` on screen at the click.
    fn fast_in_flight(mode: Mode, answers: lights::FastAnswers, now: Instant) -> WindowLights {
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&"\u{2500}".repeat(60), BYPASS, ""));
        start(
            &mut w,
            Light::Fast,
            Drive::Command {
                cmd: "/fast on",
                immediate: true,
            },
            mode,
            now,
        );
        let p = w.sessions.get_mut(&7).unwrap().pending.as_mut().unwrap();
        p.echoes_floor = answers.echoed;
        p.stale_notice = answers.notice;
        w
    }

    /// THE NOTIFICATION SLOT IS NOT COUNTED (the review of 2026-09-27): a
    /// notification up at the click is an earlier try's; a DIFFERENT one in
    /// its place, or any once the slot has emptied, is this try's — and a
    /// new transcript answer is never overruled by a stale notification.
    /// The same words replaced in place cannot be told from the old ones: the
    /// toggle waits, and its deadline says Claude's newest words, not "did
    /// not switch".
    #[test]
    fn a_notification_is_this_tries_only_once_it_changed_and_an_echo_outranks_a_stale_one() {
        use lights::{FastAnswer as A, FastAnswers, FastRefusal as R};
        let now = Instant::now();
        let checking = Some(A::Refused(R::Checking));
        let stale = FastAnswers {
            notice: checking,
            ..FastAnswers::default()
        };
        // The same notification still up: not an answer.
        let mut w = fast_in_flight(Mode::Bypass, stale, now);
        assert_eq!(
            w.answer(7, &read_of(LightState::Off, Mode::Bypass, stale), now),
            None
        );
        // A different one in the slot: this try's.
        let refused = FastAnswers {
            notice: Some(A::Refused(R::DisabledByOrg)),
            ..FastAnswers::default()
        };
        assert_eq!(
            w.answer(7, &read_of(LightState::Off, Mode::Bypass, refused), now),
            Some(CommandEnd {
                refusal: Some(R::DisabledByOrg),
                cost_auto: false
            })
        );
        // The slot emptied, then the same words again: this try's.
        let mut w = fast_in_flight(Mode::Bypass, stale, now);
        let empty = FastAnswers::default();
        assert_eq!(
            w.answer(7, &read_of(LightState::Off, Mode::Bypass, empty), now),
            None
        );
        assert_eq!(
            w.answer(7, &read_of(LightState::Off, Mode::Bypass, stale), now),
            Some(CommandEnd {
                refusal: Some(R::Checking),
                cost_auto: false
            })
        );
        // A new echo while the stale notification is still up: the echo
        // speaks, not the stale words.
        let mut w = fast_in_flight(Mode::Bypass, stale, now);
        let echoed = FastAnswers {
            echoed: 1,
            echo: Some(A::On),
            notice: checking,
        };
        assert_eq!(
            w.answer(7, &read_of(LightState::Off, Mode::Bypass, echoed), now),
            Some(CommandEnd {
                refusal: None,
                cost_auto: false
            }),
            "the echo's ON, not the stale refusal"
        );
        // Unresolvable to the end: the deadline says Claude's words.
        let mut w = fast_in_flight(Mode::Bypass, stale, now);
        assert_eq!(
            w.answer(7, &read_of(LightState::Off, Mode::Bypass, stale), now),
            None
        );
        let due = now + Duration::from_millis(lights::SETTLE_MS);
        assert_eq!(w.expire(due), (true, vec![7]));
        assert!(matches!(
            w.sessions[&7].refused,
            Some((Light::Fast, Refusal::Vendor(R::Checking), _))
        ));
    }

    /// Claude's words on a notice go WITH that notice: a fast notice that
    /// said "Claude switched the model" ends, and a later mode notice is the
    /// mode's own. (The retired separate `said` slot outlived its notice and
    /// titled the mode chip "Permission mode: on · Claude switched the
    /// model".)
    #[test]
    fn a_notices_words_never_title_another_light() {
        let now = Instant::now();
        let rule = "\u{2500}".repeat(60);
        let mut w = fast_in_flight(Mode::Bypass, lights::FastAnswers::default(), now);
        let moved = lights::FastAnswers {
            echoed: 1,
            echo: Some(lights::FastAnswer::OnModelMoved),
            notice: None,
        };
        assert!(
            w.answer(7, &read_of(LightState::On, Mode::Bypass, moved), now)
                .is_some()
        );
        let title = |w: &WindowLights, at: Instant| -> String {
            w.block(7, blank(), &lights::Expect::default(), at, true)
                .title
                .iter()
                .map(|c| c.ch)
                .collect()
        };
        assert_eq!(
            title(&w, now),
            "Fast mode: on \u{00B7} Claude switched the model  ",
            "control: the fast notice says Claude's words"
        );
        let later = now + REFUSAL_SHOWN;
        assert_eq!(w.expire(later), (true, Vec::new()));
        observe(&mut w, &screen(&rule, PLAN, ""));
        start(&mut w, Light::Mode, Drive::CycleMode, Mode::Plan, later);
        observe(&mut w, &screen(&rule, BYPASS, ""));
        let due = later + Duration::from_millis(lights::SETTLE_MS);
        assert_eq!(w.expire(due), (true, vec![7]), "arrived, by the deadline");
        assert_eq!(title(&w, due), "Permission mode: bypass  ");
    }

    /// FAST MODE THAT TOOK AUTO MODE OFF (Claude Code 2.1.283, under its
    /// server flag `tengu_auto_mode_config.disableFastMode`): a `/fast on`
    /// from auto that Claude answers ON, with the pill now manual or its own
    /// words on screen, is said on the fast light and returned for the
    /// host's latch. Control: the same answer from bypass costs nothing.
    #[test]
    fn fast_mode_that_took_auto_off_says_so() {
        let now = Instant::now();
        let on = lights::FastAnswers {
            echoed: 1,
            echo: Some(lights::FastAnswer::On),
            notice: None,
        };
        let mut w = fast_in_flight(Mode::Auto, lights::FastAnswers::default(), now);
        assert_eq!(
            w.answer(7, &read_of(LightState::On, Mode::Manual, on), now),
            Some(CommandEnd {
                refusal: None,
                cost_auto: true
            })
        );
        assert!(matches!(
            w.sessions[&7].notice,
            Some((Light::Fast, _, Some(said))) if said.contains("auto mode off")
        ));
        let mut w = fast_in_flight(Mode::Auto, lights::FastAnswers::default(), now);
        let read = CommandRead {
            auto_off: true,
            ..read_of(LightState::On, Mode::Auto, on)
        };
        assert_eq!(
            w.answer(7, &read, now).map(|end| end.cost_auto),
            Some(true),
            "Claude's own words"
        );
        let mut w = fast_in_flight(Mode::Bypass, lights::FastAnswers::default(), now);
        assert_eq!(
            w.answer(7, &read_of(LightState::On, Mode::Bypass, on), now),
            Some(CommandEnd {
                refusal: None,
                cost_auto: false
            }),
            "control: bypass"
        );
    }

    /// A toggle Claude never answers is given up at its deadline, the light
    /// says why, and the session is named so the refusal's end is timed.
    #[test]
    fn an_unanswered_toggle_is_refused_by_name() {
        let now = Instant::now();
        let rule = "\u{2500}".repeat(60);
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&rule, BYPASS, ""));
        start(
            &mut w,
            Light::Fast,
            Drive::Command {
                cmd: "/fast on",
                immediate: true,
            },
            Mode::Bypass,
            now,
        );
        assert_eq!(w.expire(now), (false, Vec::new()));
        assert_eq!(
            w.expire(now + Duration::from_millis(lights::SETTLE_MS)),
            (true, vec![7])
        );
        let s = &w.sessions[&7];
        assert!(s.pending.is_none());
        assert!(matches!(
            s.refused,
            Some((Light::Fast, Refusal::NoChange, _))
        ));
        let block = w.block(7, blank(), &lights::Expect::default(), now, true);
        let text: String = block.title.iter().map(|c| c.ch).collect();
        assert!(
            text.starts_with("Fast mode: Claude did not switch"),
            "{text:?}"
        );
    }

    /// DEVIATIONS ONLY: at rest — bypass or auto, fast on — the block is
    /// EMPTY. Plan mode and fast off each draw a labelled chip, one
    /// cell apart, a cell of margin after; the title comes APART from the
    /// chips (only the chips must fit); a box over the composer keeps the last
    /// reading rather than dropping the row.
    #[test]
    fn only_deviations_are_drawn_and_the_block_keeps_what_it_saw() {
        let now = Instant::now();
        let expect = lights::Expect::default();
        let lit = format!("{} workspace \u{21AF} \u{2500}", "\u{2500}".repeat(40));
        let mut w = WindowLights::default();
        for mode in [BYPASS, AUTO] {
            observe(&mut w, &screen(&lit, mode, ""));
            let rest = w.block(7, blank(), &expect, now, true);
            assert!(
                rest.lights.is_empty() && rest.cols.is_empty() && rest.title.is_empty(),
                "nothing at rest: {mode:?}"
            );
        }
        // Control: plan mode, and fast mode off.
        let bare = "\u{2500}".repeat(60);
        observe(&mut w, &screen(&bare, PLAN, ""));
        let block = w.block(7, blank(), &expect, now, true);
        let text: String = block.lights.iter().map(|c| c.ch).collect();
        assert_eq!(text, "\u{23F8} plan \u{25CB} fast");
        assert_eq!(block.cols, vec![(0..6, Light::Mode), (7..13, Light::Fast)]);
        assert!(block.title.is_empty(), "nothing asked for a title");
        assert_eq!(block.lights[0].fg, blank().fg, "plan, in the row's ink");
        assert_eq!(block.lights[7].fg, dim_of(blank()), "fast off, dim");
        w.observe(7, None);
        assert_eq!(
            w.block(7, blank(), &expect, now, true).lights,
            block.lights,
            "a covered composer keeps the last reading"
        );
        w.hover = Some((7, Light::Fast));
        let hovered = w.block(7, blank(), &expect, now, true);
        let title: String = hovered.title.iter().map(|c| c.ch).collect();
        assert_eq!(title, "Fast mode: off \u{00B7} click to turn on  ");
        assert_eq!(
            hovered.lights.len(),
            block.lights.len(),
            "the title is apart"
        );
        assert!(
            hovered.lights[7..13].iter().all(|c| c.bold),
            "the whole hovered chip is marked"
        );
        assert!(!hovered.lights[0].bold);
        // A latched refusal stops asking for fast mode.
        w.hover = None;
        assert!(
            !w.block(
                7,
                blank(),
                &lights::Expect {
                    fast: false,
                    ..expect
                },
                now,
                true
            )
            .shows(Light::Fast)
        );
    }

    /// Return toggles the SELECTED light, so the row marks and titles that
    /// one even while the pointer rests on another.
    #[test]
    fn the_keyboard_selection_outranks_a_resting_pointer() {
        let now = Instant::now();
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&"\u{2500}".repeat(60), BYPASS, ""));
        w.hover = Some((7, Light::Mode));
        w.selected = Some((7, Light::Fast));
        let block = w.block(7, blank(), &lights::Expect::default(), now, true);
        let title: String = block.title.iter().map(|c| c.ch).collect();
        assert!(title.starts_with("Fast mode:"), "{title:?}");
        let at = |l: Light| {
            block
                .cols
                .iter()
                .find(|(_, x)| *x == l)
                .unwrap_or_else(|| panic!("{l:?} is revealed"))
                .0
                .start
        };
        assert!(block.lights[at(Light::Fast)].bold);
        assert!(
            !block.lights[at(Light::Mode)].bold,
            "the pointer's, unmarked"
        );
        assert_eq!(
            block.lights[at(Light::Mode)].ch,
            '\u{23F5}',
            "the selection reveals the expected mode too"
        );
    }

    #[test]
    fn a_quiet_row_keys_as_zero() {
        let now = Instant::now();
        let mut w = WindowLights::default();
        assert_eq!(w.fingerprint(now), 0);
        w.hover = Some((7, Light::Fast));
        let a = w.fingerprint(now);
        w.hover = Some((7, Light::Mode));
        assert_ne!(a, 0);
        assert_ne!(a, w.fingerprint(now));
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
    use aterm_agent::harness::lights::{self, Light, LightState, Mode};
    use aterm_core::terminal::RenderCell;
    use aterm_session::sink::SinkWriter;
    use winit::event::{ElementState, MouseButton};

    use crate::{App, WindowId, term_lock};

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
    /// write to the socket still succeeds (measured 2026-09-27 on the gate:
    /// `FollowRejected` read a failed-write fixture's Enter as accepted, only
    /// under load with sibling tests running). So wait for the sink's own end
    /// to see the hang-up, which it reports only once every copy is gone
    /// (`POLLHUP`; a `shutdown` does not help — a send to a shut-down Unix
    /// socket peer still succeeds on macOS). Bounded, so a copy that never
    /// closes fails here by name instead of as a model mismatch.
    fn hang_up(app: &App, session: u64, reader: UnixStream) {
        let sink = app.pool.get(session).expect("session").ctx.sink.clone();
        drop(reader);
        let mut pfd = libc::pollfd {
            fd: sink.master(),
            events: libc::POLLIN,
            revents: 0,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
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

    /// Wait (bounded) for the light's initial paste to leave the ordered
    /// writer, so the follow-up reaches the real write instead of waiting
    /// behind it. The bytes arrive before the writer retires its slot.
    fn ordering_retired(app: &App, session: u64) {
        let sink = app.pool.get(session).expect("session").ctx.sink.clone();
        let deadline = Instant::now() + Duration::from_secs(2);
        while crate::app_input::paste_order::is_ordering(&sink) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            !crate::app_input::paste_order::is_ordering(&sink),
            "the follow-up reaches the real write"
        );
    }

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
    /// paste ON, as Claude Code always has it, so a pasted command reaches the
    /// PTY framed ([`PASTED`]).
    fn draw_claude(app: &App, session: u64, mode: &str) {
        let rule = "\u{2500}".repeat(cols(app, session));
        let screen = format!(
            "\x1b[?2004h\x1b[2J\x1b[19;1H\u{25CF} done\x1b[21;1H{rule}\x1b[22;1H\u{276F} \
             \x1b[23;1H{rule}\x1b[24;1H{mode}\x1b[22;3H"
        );
        feed(app, session, screen.as_bytes());
    }

    /// `/fast on` as the light's paste reaches Claude: bracketed.
    const PASTED: &[u8] = b"\x1b[200~/fast on\x1b[201~";

    /// Claude turns fast mode on: `↯` on the composer's top rule.
    fn set_fast(app: &App, session: u64) {
        let rule = "\u{2500}".repeat(cols(app, session) - 10);
        feed(
            app,
            session,
            format!("\x1b[21;1H{rule} \u{21AF} \u{2500}\x1b[22;3H").as_bytes(),
        );
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

    /// Where the painter put `light` this frame: (frame row, frame column).
    fn hit(app: &App, wid: WindowId, light: Light) -> (usize, usize) {
        let h = app.windows[&wid]
            .claude_lights
            .hits
            .iter()
            .find(|h| h.light == light)
            .unwrap_or_else(|| panic!("{light:?} is painted"));
        (h.frame_row, h.col)
    }

    fn cell_at(app: &App, wid: WindowId, (row, col): (usize, usize)) -> RenderCell {
        app.windows[&wid].input_scratch.cells[row][col]
    }

    fn row_text(app: &App, wid: WindowId, row: usize) -> String {
        app.windows[&wid].input_scratch.cells[row]
            .iter()
            .map(|c| c.ch)
            .collect()
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

    /// What the session's PTY received: up to 2 s for `want` bytes, then
    /// 30 ms more for any that should NOT have come.
    fn pty(reader: &mut UnixStream, want: usize) -> Vec<u8> {
        let mut got = Vec::new();
        let mut buf = [0u8; 256];
        let deadline = Instant::now() + Duration::from_secs(2);
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

    /// THE CHIPS NEVER TOUCH OTHER INK (main's 35d8c4ca7, a real render on
    /// 2.1.284, 2026-09-28): in auto mode Claude right-aligns `◐ medium ·
    /// /effort` on its mode row, and the chips then drawn on that row beside
    /// it read `/effort○ fast`. The chips are in the composer's bottom rule
    /// now, and Claude's row stays Claude's — so the MEASURED screen, drawn at
    /// its own 144 columns, keeps the tail exactly where Claude put it, and
    /// every chip in the rule has a blank cell on each side: never glued to a
    /// rule glyph, another chip, a title or a fact. In manual mode too, where
    /// two chips stand side by side. CONTROL: the rule's own glyphs are ink a
    /// chip must not touch, and they are there.
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
            assert!(app.apply_term_resize(wid, 24, 144));
            publish_facts(&app, session);
            draw_claude(&app, session, mode);
            frame(&mut app, wid);
            let claude = row_text(&app, wid, mode_row(&app, wid));
            assert_eq!(claude.trim_end(), mode, "Claude's row is whole");
            let hits = &app.windows[&wid].claude_lights.hits;
            assert!(!hits.is_empty(), "{mode:?}: a chip is drawn");
            let rule = footer_row(&app, wid);
            let text: Vec<char> = app.windows[&wid].input_scratch.cells[rule]
                .iter()
                .map(|c| c.ch)
                .collect();
            assert!(text.contains(&'\u{2500}'), "control: the rule's glyphs");
            for light in [Light::Mode, Light::Fast] {
                let cols: Vec<usize> = hits
                    .iter()
                    .filter(|h| h.light == light)
                    .map(|h| {
                        assert_eq!(h.frame_row, rule, "the chip is in the rule");
                        h.col
                    })
                    .collect();
                let (Some(&first), Some(&last)) = (cols.iter().min(), cols.iter().max()) else {
                    continue;
                };
                let row: String = text.iter().collect();
                assert_eq!(text[first - 1], ' ', "{light:?} before: {row:?}");
                assert_eq!(text[last + 1], ' ', "{light:?} after: {row:?}");
            }
        }
    }

    /// A CLICK ON THE MODE LIGHT in manual mode: exactly one shift+tab
    /// reaches the PTY; the light shows `◐` until Claude's screen answers; an
    /// answer that is not yet an expected mode is pressed through from the
    /// WAKE (never from the frame); and the bypass pill resolves it — and no
    /// press ever goes on from there. Run with the mouse untracked and
    /// tracked (SGR 1000/1006): a press the light takes is not a mouse
    /// report, and neither is its release. Controls: the gap between two
    /// lights is not a light, and a second click while one toggle is in
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
            let fast = hit(&app, wid, Light::Fast);
            let text = row_text(&app, wid, mode.0);
            assert!(
                text.contains("\u{25C6} Opus 5.5 xhigh"),
                "the footer is painted: {text:?}"
            );
            assert_eq!(cell_at(&app, wid, mode).ch, '\u{23F8}', "{text:?}");
            assert!(text.contains("\u{23F8} manual \u{25CB} fast"), "{text:?}");
            let claude = row_text(&app, wid, mode_row(&app, wid));
            assert!(
                claude.contains("\u{23F8} manual mode on"),
                "the chip never takes the pill's place: Claude's row is whole: {claude:?}"
            );
            assert_eq!(cell_at(&app, wid, fast).ch, '\u{25CB}', "{text:?}");
            assert!(pty(&mut reader, 0).is_empty(), "a frame types nothing");

            // Hover: the gap beside the light is not one; the light is.
            let (x, y) = px_of(&app, wid, (fast.0, fast.1 - 1));
            app.on_cursor_moved(wid, x, y);
            assert_eq!(app.windows[&wid].claude_lights.hover, None);
            let (x, y) = px_of(&app, wid, mode);
            app.on_cursor_moved(wid, x, y);
            assert_eq!(
                app.windows[&wid].claude_lights.hover,
                Some((session, Light::Mode))
            );
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
                    (p.light, p.from, p.started, p.presses),
                    (Light::Mode, LightState::Off, Mode::Manual, 1)
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
            assert_ne!(
                app.windows[&wid].claude_lights.fingerprint(Instant::now()),
                0
            );
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
                text.contains("\u{25CB} fast  bypass ") && text.contains(WHOLE_FACTS),
                "the hover's title, short in an 80-column pane beside the whole facts: {text:?}"
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
                .any(|h| h.light == Light::Mode),
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

    /// AT REST NOTHING IS DRAWN (owner, 2026-09-27: "I don't want to show the
    /// state of parameters that I always expect to be on"): bypass or auto,
    /// fast on — the rule carries the facts alone, no chip, nothing to hit,
    /// while the rule is still known to be on the glass (the keyboard's way
    /// to the lights); and Claude's own row under it, its pill and hint,
    /// is exactly as Claude drew it (owner, 2026-09-28). Control: fast off
    /// draws its chip.
    #[test]
    fn at_rest_the_rule_carries_the_facts_alone() {
        for mode in [BYPASS, AUTO] {
            let (mut app, wid, session, _reader) = app();
            publish_facts(&app, session);
            draw_claude(&app, session, mode);
            set_fast(&app, session);
            frame(&mut app, wid);
            let lights = &app.windows[&wid].claude_lights;
            assert!(lights.hits.is_empty(), "{mode:?}");
            assert_eq!(
                lights.footers.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
                vec![session],
                "{mode:?}"
            );
            let text = row_text(&app, wid, footer_row(&app, wid));
            assert!(text.contains("\u{25C6} Opus 5.5 xhigh"), "{text:?}");
            for gone in ["\u{25CB}", "\u{25CF}", "\u{23F5}"] {
                assert!(!text.contains(gone), "{gone:?} in {text:?}");
            }
            // Claude's own row is whole: its pill, its hint.
            let claude = row_text(&app, wid, mode_row(&app, wid));
            assert_eq!(claude.trim_end(), mode, "Claude's row, as drawn");
            // Control: fast mode off asks for fast mode.
            draw_claude(&app, session, mode);
            frame(&mut app, wid);
            assert!(
                app.windows[&wid]
                    .claude_lights
                    .hits
                    .iter()
                    .all(|h| h.light == Light::Fast)
                    && !app.windows[&wid].claude_lights.hits.is_empty(),
                "{mode:?}: fast off draws its chip, and only it"
            );
        }
    }

    /// THE KEYBOARD REACHES A LIGHT AT REST (macOS): with nothing drawn,
    /// `ctrl+shift+tab` reveals every known light — the expected mode
    /// included, `⏵⏵ bypass` — and Return on the expected mode presses
    /// nothing; the reveal goes with the selection.
    #[cfg(target_os = "macos")]
    #[test]
    fn ctrl_shift_tab_reveals_the_lights_at_rest() {
        use winit::event::KeyEvent;
        use winit::keyboard::SmolStr;
        use winit::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey};

        fn tap(app: &mut App, wid: WindowId, named: NamedKey, code: KeyCode, text: &str) {
            for state in [ElementState::Pressed, ElementState::Released] {
                app.on_key(
                    wid,
                    KeyEvent::synthetic_for_test(
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
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        set_fast(&app, session);
        frame(&mut app, wid);
        assert!(app.windows[&wid].claude_lights.hits.is_empty(), "at rest");
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
        tap(&mut app, wid, NamedKey::Tab, KeyCode::Tab, "\t");
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, Light::Mode))
        );
        assert!(pty(&mut reader, 0).is_empty(), "the chord is the lights'");
        frame(&mut app, wid);
        let text = row_text(&app, wid, footer_row(&app, wid));
        assert!(
            text.contains("\u{23F5}\u{23F5} bypass \u{25CF} fast"),
            "every known light, revealed: {text:?}"
        );
        tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
        assert!(
            pty(&mut reader, 0).is_empty(),
            "Return on the expected mode presses nothing"
        );
        frame(&mut app, wid);
        assert!(
            app.windows[&wid].claude_lights.hits.is_empty(),
            "the reveal went with the selection"
        );
    }

    /// THE KEYBOARD ROUTE (macOS: elsewhere `ctrl+shift+tab` is `prev_tab`):
    /// the chord selects the focused pane's lights in order and types
    /// nothing; Return toggles the selected light — one shift+tab, and NOT
    /// the Return; the selection lets go. Control: with nothing selected,
    /// Return is Claude's again.
    #[cfg(target_os = "macos")]
    #[test]
    fn ctrl_shift_tab_selects_a_light_and_return_toggles_it() {
        use winit::event::KeyEvent;
        use winit::keyboard::SmolStr;
        use winit::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey};

        fn key(named: NamedKey, code: KeyCode, text: &str, state: ElementState) -> KeyEvent {
            KeyEvent::synthetic_for_test(
                PhysicalKey::Code(code),
                Key::Named(named),
                Some(SmolStr::new(text)),
                KeyLocation::Standard,
                state,
                false,
            )
        }
        fn tap(app: &mut App, wid: WindowId, named: NamedKey, code: KeyCode, text: &str) {
            app.on_key(wid, key(named, code, text, ElementState::Pressed));
            app.on_key(wid, key(named, code, text, ElementState::Released));
        }
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
            Some((session, Light::Mode))
        );
        tap(&mut app, wid, NamedKey::Tab, KeyCode::Tab, "\t");
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, Light::Fast))
        );
        set_mods(&mut app, wid, ModifiersState::empty());
        tap(&mut app, wid, NamedKey::ArrowLeft, KeyCode::ArrowLeft, "");
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, Light::Mode)),
            "\u{2190} moves back"
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
        if let Some((_, until, _)) = app
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
                .any(|h| h.light == Light::Mode),
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
    /// stay whole and the title comes short (`switching…`); the chip it names
    /// does not move.
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
            text.contains("switching\u{2026}") && !text.contains("Permission mode"),
            "the title gives way first: short, beside the lights: {text:?}"
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
    /// comes whole, beside the whole facts. Control: the chips are all there.
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
                text.contains("\u{23F5}\u{23F5} accept edits \u{25CB} fast"),
                "{cols}: control: the chips: {text:?}"
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
        assert!(matches!(s.notice, Some((Light::Mode, _, _))));
    }

    /// REGRESSION (review 2026-09-25): a cycle that never reaches an
    /// expected mode (neither bypass nor auto: a session started without
    /// --dangerously-skip-permissions on an account without auto mode) is
    /// searched exactly ONE lap and the search stops back where it began, the
    /// person's mode restored, saying neither is in this session's cycle —
    /// and naming the mode.
    #[test]
    fn a_cycle_that_never_reaches_an_expected_mode_is_refused_after_one_lap_where_it_began() {
        let ring = [ACCEPT, PLAN, MANUAL];
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, ACCEPT);
        frame(&mut app, wid);
        let (x, y) = px_of(&app, wid, hit(&app, wid, Light::Mode));
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let mut at = 0;
        let mut presses = 0;
        while pty(&mut reader, lights::SHIFT_TAB.len()) == lights::SHIFT_TAB {
            presses += 1;
            at = (at + 1) % ring.len();
            redraw_mode_row(&app, session, ring[at]);
            frame(&mut app, wid);
            app.on_claude_footer_changed(session);
        }
        assert_eq!(presses, ring.len(), "one lap, no more");
        let s = &app.windows[&wid].claude_lights.sessions[&session];
        assert!(s.pending.is_none(), "the search stopped");
        assert!(matches!(
            s.refused,
            Some((
                Light::Mode,
                super::Refusal::NotInCycle(Mode::AcceptEdits),
                _
            ))
        ));
        assert_eq!(
            ring[at], ACCEPT,
            "a toggle that did not take leaves the person's mode where it was"
        );
    }

    /// REGRESSION (review 2026-09-25): a light selected with ctrl+shift+tab
    /// is let go by any mouse press, so clicking to another pane and back
    /// leaves no selection armed to take the next Return — Return submits
    /// Claude's prompt, as the person expects.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_mouse_press_lets_a_keyboard_selection_go() {
        use winit::event::KeyEvent;
        use winit::keyboard::{
            Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr,
        };
        fn key(named: NamedKey, code: KeyCode, text: &str, state: ElementState) -> KeyEvent {
            KeyEvent::synthetic_for_test(
                PhysicalKey::Code(code),
                Key::Named(named),
                Some(SmolStr::new(text)),
                KeyLocation::Standard,
                state,
                false,
            )
        }
        fn tap(app: &mut App, wid: WindowId, named: NamedKey, code: KeyCode, text: &str) {
            app.on_key(wid, key(named, code, text, ElementState::Pressed));
            app.on_key(wid, key(named, code, text, ElementState::Released));
        }
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
        // The half-width pane's rule has room for the revealed chips beside
        // the model: the first light, the mode's, is the one selected.
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, Light::Mode))
        );
        click(&mut app, wid, (strip + 5, other_col));
        assert_eq!(app.focused_session_id(wid), Some(other));
        click(&mut app, wid, (strip + 5, 3));
        assert_eq!(app.focused_session_id(wid), Some(session));
        assert!(pty(&mut reader, 0).is_empty());
        tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
        assert_eq!(pty(&mut reader, 1), b"\r", "Return submits Claude's prompt");
    }

    fn click(app: &mut App, wid: WindowId, light: Light) {
        let (x, y) = px_of(app, wid, hit(app, wid, light));
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    }

    /// Claude echoes a draft into the composer: the caret row reads
    /// `❯ <draft>` and the cursor sits right behind it.
    fn echo_draft(app: &App, session: u64, draft: &str) {
        let col = 3 + draft.chars().count();
        feed(
            app,
            session,
            format!("\x1b[22;1H\x1b[2K\u{276F} {draft}\x1b[22;{col}H").as_bytes(),
        );
    }

    /// Put `session`'s typed command past its deadline (or its ctrl+u past
    /// its read-back), as the timers would.
    fn past_due(app: &mut App, session: u64) {
        let past = Instant::now() - Duration::from_millis(1);
        let t = app.claude_typed.get_mut(&session).expect("a typed command");
        t.deadline = past;
        if t.cleared.is_some() {
            t.cleared = Some(past);
        }
    }

    /// Hold `session`'s typed command well BEFORE its deadline, however
    /// slowly the test reached its drain: a loaded machine (a whole workspace
    /// run, the tiered checks' `ty` spawns) can spend all of `SETTLE_MS`
    /// between the click and the drain, which then takes the deadline's
    /// take-back instead of the Return the case is about (measured
    /// 2026-09-27: the accepted case failed 25 of 96 runs under a 96-way
    /// overload, its drain on the ctrl+u path).
    fn not_due(app: &mut App, session: u64) {
        let t = app.claude_typed.get_mut(&session).expect("a typed command");
        t.deadline = Instant::now() + Duration::from_secs(60);
    }

    fn refusal(app: &App, wid: WindowId, session: u64) -> Option<(Light, super::Refusal)> {
        app.windows[&wid].claude_lights.sessions[&session]
            .refused
            .map(|(light, why, _)| (light, why))
    }

    fn project_admission(
        app: &App,
        wid: WindowId,
        session: u64,
        expected: &aterm_spec::interp::State,
    ) -> aterm_spec::interp::State {
        // Phase and receipt facts describe the driven action. Ownership and
        // the visible rejection notice come from the genuine App state.
        let mut observed = expected.clone();
        observed.insert("owned", i64::from(app.claude_typed.contains_key(&session)));
        observed.insert(
            "left",
            i64::from(refusal(app, wid, session) == Some((Light::Fast, super::Refusal::LeftTyped))),
        );
        observed
    }

    fn drive_admission_action<T>(action: &str, drive: impl FnOnce() -> T) -> T {
        use aterm_spec::xref;

        assert!(xref::reset_entered_anchors());
        let result = drive();
        let entered = xref::entered_anchor_ids();
        xref::disarm_entered_anchors();
        let anchor = xref::refinements()
            .find(|anchor| anchor.machine == "claude_light_admission" && anchor.action == action)
            .expect("the admission action has a shipping anchor");
        assert!(
            entered.contains(anchor.entry_id),
            "{action} missed its shipping function"
        );
        result
    }

    fn check_admission_transition(
        app: &App,
        wid: WindowId,
        session: u64,
        action: &str,
        state: &mut aterm_spec::interp::State,
    ) {
        let model = aterm_spec::derive::claude_light_admission_model();
        let before = state.clone();
        assert!(model.fire(action, state));
        let observed = project_admission(app, wid, session, state);
        let (ok, evidence) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &before,
            &observed,
            Some(action),
            "Claude light real input-admission conformance",
        );
        assert!(ok, "{action}: {evidence}");

        // Each failure's historical post-state must be rejected, independent
        // of whether its shipping anchor was entered.
        let mut historical = observed.clone();
        match action {
            "StartRejected" => {
                historical.insert("owned", 1);
            }
            "FollowRejected" => {
                historical.insert("left", 0);
            }
            _ => return,
        }
        let (ok, _) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &before,
            &historical,
            Some(action),
            "ignored admission receipt negative control",
        );
        assert!(!ok, "{action} must reject the historical ignored receipt");
    }

    /// A rejected light paste cannot own an identical, delayed echo from a
    /// person's earlier accepted paste. Both actual channel-failure paths
    /// produce zero bytes, no Typed record, no pending toggle, and no Enter.
    #[test]
    fn rejected_light_paste_never_claims_an_earlier_pastes_echo() {
        use crate::input::{InputEvent, InputOutcome, PasteFraming, Source};

        for unavailable in [false, true] {
            let (mut app, wid, session, mut reader) = app();
            publish_facts(&app, session);
            draw_claude(&app, session, BYPASS);
            frame(&mut app, wid);
            assert_eq!(
                app.input_to_session(
                    wid,
                    InputEvent::Paste("/fast on".into(), PasteFraming::AtDrain),
                    Source::Human,
                    Some(session),
                    crate::app_input::PressPhase::Initial,
                ),
                InputOutcome::Ok,
            );
            assert_eq!(
                pty(&mut reader, PASTED.len()),
                PASTED,
                "the person's earlier paste"
            );
            let sink = app.pool.get(session).unwrap().ctx.sink.clone();
            let rejection =
                crate::app_input::paste_order::reject_admission_for_test(&sink, unavailable);
            let mut state = aterm_spec::derive::claude_light_admission_model().init_state();
            drive_admission_action("StartRejected", || click(&mut app, wid, Light::Fast));
            check_admission_transition(&app, wid, session, "StartRejected", &mut state);
            assert!(
                app.windows[&wid].claude_lights.sessions[&session]
                    .pending
                    .is_none()
            );
            assert_eq!(
                refusal(&app, wid, session),
                Some((Light::Fast, super::Refusal::InputRejected))
            );
            assert!(
                pty(&mut reader, 0).is_empty(),
                "rejected paste: unavailable={unavailable}"
            );
            drop(rejection);

            // The engine receives the earlier paste's delayed echo. The
            // rejection stamped the same person clock as a successful click,
            // so only respecting its admission result protects this draft.
            echo_draft(&app, session, "/fast on");
            app.on_claude_footer_changed(session);
            assert!(
                pty(&mut reader, 0).is_empty(),
                "never submit someone else's echo"
            );
            assert!(app.claude_typed.is_empty());
        }
    }

    /// An already accepted, genuinely stalled paste is not an empty composer:
    /// the light refuses before adding its command behind that unseen draft.
    #[test]
    fn a_light_refuses_ownership_behind_a_real_queued_paste() {
        use crate::input::{InputEvent, InputOutcome, PasteFraming, Source};
        use std::io::Write;

        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writer.set_nonblocking(true).unwrap();
        let mut seeded = 0;
        loop {
            match writer.write(&[b'.'; 4096]) {
                Ok(n) if n > 0 => seeded += n,
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                other => panic!("fill owned socket: {other:?}"),
            }
        }
        writer.set_nonblocking(false).unwrap();
        let sink = Arc::new(SinkWriter::new_owned(writer.into()));
        let mut app = App::headless_for_test_with_sink(Arc::clone(&sink));
        let wid = WindowId(0);
        let session = app.focused_session_id(wid).unwrap();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        // Unframed, so the bytes read back are the paste's alone.
        feed(&app, session, b"\x1b[?2004l");
        frame(&mut app, wid);
        let body = 3 * 1024 * 1024; // Larger than the entire bounded spill.
        assert_eq!(
            app.input_to_session(
                wid,
                InputEvent::Paste("p".repeat(body), PasteFraming::AtDrain),
                Source::Human,
                Some(session),
                crate::app_input::PressPhase::Initial,
            ),
            InputOutcome::Ok
        );
        assert!(crate::app_input::paste_order::is_ordering(&sink));
        let mut state = aterm_spec::derive::claude_light_admission_model().init_state();
        drive_admission_action("StartRejected", || click(&mut app, wid, Light::Fast));
        check_admission_transition(&app, wid, session, "StartRejected", &mut state);
        assert_eq!(
            refusal(&app, wid, session),
            Some((Light::Fast, super::Refusal::InputPending))
        );
        let mut actual = vec![0; seeded + body];
        reader.read_exact(&mut actual).unwrap();
        assert!(actual[..seeded].iter().all(|byte| *byte == b'.'));
        assert!(actual[seeded..].iter().all(|byte| *byte == b'p'));
        reader.set_nonblocking(true).unwrap();
        assert!(
            pty(&mut reader, 0).is_empty(),
            "no light command joined the earlier paste"
        );
    }

    /// The initial paste really lands. Accepted Return completes once;
    /// a real closed-sink failure leaves a notice for Return and Ctrl-U.
    #[test]
    fn light_follow_up_admission_conforms_and_rejection_leaves_a_notice() {
        for (failed, take_back) in [(false, false), (true, false), (true, true)] {
            let (mut app, wid, session, reader) = app();
            let mut reader = Some(reader);
            publish_facts(&app, session);
            draw_claude(&app, session, BYPASS);
            frame(&mut app, wid);
            let mut state = aterm_spec::derive::claude_light_admission_model().init_state();
            drive_admission_action("StartAccepted", || click(&mut app, wid, Light::Fast));
            assert_eq!(pty(reader.as_mut().unwrap(), PASTED.len()), PASTED);
            check_admission_transition(&app, wid, session, "StartAccepted", &mut state);
            ordering_retired(&app, session);
            echo_draft(&app, session, "/fast on");
            if take_back {
                past_due(&mut app, session);
            } else {
                not_due(&mut app, session);
            }
            if failed {
                // Genuine PTY-sink write failure, no fake receipt.
                hang_up(&app, session, reader.take().expect("the observer"));
            }
            let action = if failed {
                "FollowRejected"
            } else {
                "FollowAccepted"
            };
            drive_admission_action(action, || app.on_claude_footer_changed(session));
            check_admission_transition(&app, wid, session, action, &mut state);
            if failed {
                assert!(
                    app.windows[&wid].claude_lights.sessions[&session]
                        .pending
                        .is_none()
                );
                let title: String = app.windows[&wid]
                    .claude_lights
                    .block(
                        session,
                        RenderCell::default(),
                        &lights::Expect::default(),
                        Instant::now(),
                        true,
                    )
                    .title
                    .iter()
                    .map(|cell| cell.ch)
                    .collect();
                assert!(title.contains("command left in the prompt"), "{title}");
            } else {
                assert_eq!(pty(reader.as_mut().unwrap(), 1), b"\r");
            }
            app.on_claude_footer_changed(session);
            if let Some(reader) = reader.as_mut() {
                assert!(
                    pty(reader, 0).is_empty(),
                    "no automatic retry after the decision"
                );
            }
            assert!(app.claude_typed.is_empty());
        }
    }

    /// While accepted ordered input remains, neither Return nor cleanup may
    /// join it. Draining resumes ordinary submission; expiry leaves a notice.
    #[test]
    fn light_follow_up_waits_for_ordered_input_or_leaves_it_at_deadline() {
        for expire in [false, true] {
            let (mut app, wid, session, mut reader) = app();
            publish_facts(&app, session);
            draw_claude(&app, session, BYPASS);
            frame(&mut app, wid);
            let mut state = aterm_spec::derive::claude_light_admission_model().init_state();
            drive_admission_action("StartAccepted", || click(&mut app, wid, Light::Fast));
            assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
            check_admission_transition(&app, wid, session, "StartAccepted", &mut state);
            echo_draft(&app, session, "/fast on");
            let sink = app.pool.get(session).unwrap().ctx.sink.clone();
            let pin = crate::app_input::paste_order::pin_ordering_for_test(&sink);
            drive_admission_action("Wait", || app.on_claude_footer_changed(session));
            check_admission_transition(&app, wid, session, "Wait", &mut state);
            assert!(
                pty(&mut reader, 0).is_empty(),
                "no Return while input remains ordered"
            );
            if expire {
                past_due(&mut app, session);
                drive_admission_action("FollowRejected", || app.on_claude_footer_changed(session));
                check_admission_transition(&app, wid, session, "FollowRejected", &mut state);
                assert!(
                    pty(&mut reader, 0).is_empty(),
                    "no Ctrl-U behind undelivered input"
                );
                drop(pin);
            } else {
                drop(pin);
                drive_admission_action("FollowAccepted", || app.on_claude_footer_changed(session));
                check_admission_transition(&app, wid, session, "FollowAccepted", &mut state);
                assert_eq!(
                    pty(&mut reader, 1),
                    b"\r",
                    "drained input resumes submission"
                );
            }
            app.on_claude_footer_changed(session);
            assert!(pty(&mut reader, 0).is_empty());
            assert!(app.claude_typed.is_empty());
        }
    }

    /// A CLICK ON THE FAST LIGHT pastes `/fast on` into the empty composer;
    /// the Enter waits until the ENGINE shows the composer holding exactly
    /// it, goes once, and the light resolves when Claude's `↯` tag shows —
    /// the frame that shows it only wakes the loop, which reads the engine.
    #[test]
    fn a_fast_click_submits_the_command_once_the_composer_holds_it() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        click(&mut app, wid, Light::Fast);
        assert_eq!(pty(&mut reader, PASTED.len()), PASTED, "the paste");
        app.on_claude_footer_changed(session);
        assert!(pty(&mut reader, 0).is_empty(), "no echo yet: no Enter");
        echo_draft(&app, session, "/fast on");
        app.on_claude_footer_changed(session);
        assert_eq!(pty(&mut reader, 1), b"\r", "the Enter, once it is there");
        app.on_claude_footer_changed(session);
        assert!(pty(&mut reader, 0).is_empty(), "once");
        assert!(app.claude_typed.is_empty());
        let rule = "\u{2500}".repeat(70);
        feed(
            &app,
            session,
            format!("\x1b[21;1H{rule} \u{21AF} \u{2500}\x1b[22;1H\x1b[2K\u{276F} \x1b[22;3H")
                .as_bytes(),
        );
        frame(&mut app, wid);
        assert!(
            app.windows[&wid].claude_lights.sessions[&session]
                .pending
                .is_some(),
            "a frame decides nothing"
        );
        app.on_claude_footer_changed(session);
        let s = &app.windows[&wid].claude_lights.sessions[&session];
        assert!(s.pending.is_none(), "the tag resolved it");
        assert_eq!(s.shown.fast, LightState::On);
    }

    /// A PERSON TYPES after the paste: the text is theirs now. No Enter (the
    /// old cursor-row guard `contains("/fast on")` passed `/fast onh` and
    /// submitted it), no ctrl+u at the deadline, and the light says the
    /// command is left in the prompt.
    #[test]
    fn a_person_typing_after_the_paste_stops_the_enter_and_the_take_back() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        click(&mut app, wid, Light::Fast);
        assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
        app.input_to_session(
            wid,
            crate::input::InputEvent::Text("h".into()),
            crate::input::Source::Human,
            Some(session),
            crate::app_input::PressPhase::Initial,
        );
        assert_eq!(pty(&mut reader, 1), b"h");
        echo_draft(&app, session, "/fast onh");
        app.on_claude_footer_changed(session);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "no Enter under a person's text"
        );
        assert!(
            app.claude_typed[&session].hands_off,
            "aterm types nothing more"
        );
        past_due(&mut app, session);
        app.on_claude_footer_changed(session);
        assert!(pty(&mut reader, 0).is_empty(), "no take-back either");
        assert!(app.claude_typed.is_empty());
        assert_eq!(
            refusal(&app, wid, session),
            Some((Light::Fast, super::Refusal::LeftTyped))
        );
        // The person's gesture alone decides it, even before any echo: the
        // same click with the person's key typed straight after it.
        let (mut app, wid, session, mut reader) = super::gesture_tests::app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        click(&mut app, wid, Light::Fast);
        assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
        echo_draft(&app, session, "/fast on");
        app.input_to_session(
            wid,
            crate::input::InputEvent::Text("h".into()),
            crate::input::Source::Human,
            Some(session),
            crate::app_input::PressPhase::Initial,
        );
        assert_eq!(pty(&mut reader, 1), b"h");
        app.on_claude_footer_changed(session);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "the screen still reads exactly the command, but a person has keyed since"
        );
    }

    /// UNANSWERED: the command sat in the composer past its deadline. It is
    /// taken back with ONE ctrl+u while the composer holds exactly it, and
    /// read back once: cleared, nothing more; still there, the light says so
    /// and nothing more is pressed.
    #[test]
    fn a_command_not_submitted_in_time_is_taken_back_once() {
        for honoured in [true, false] {
            let (mut app, wid, session, mut reader) = app();
            publish_facts(&app, session);
            draw_claude(&app, session, BYPASS);
            frame(&mut app, wid);
            click(&mut app, wid, Light::Fast);
            assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
            echo_draft(&app, session, "/fast on");
            past_due(&mut app, session);
            app.on_claude_footer_changed(session);
            assert_eq!(pty(&mut reader, 1), b"\x15", "one ctrl+u, not the Enter");
            if honoured {
                echo_draft(&app, session, "");
            }
            past_due(&mut app, session);
            app.on_claude_footer_changed(session);
            assert!(pty(&mut reader, 0).is_empty(), "never a second press");
            assert!(app.claude_typed.is_empty());
            let left = refusal(&app, wid, session)
                .is_some_and(|(_, why)| why == super::Refusal::LeftTyped);
            assert_eq!(left, !honoured, "honoured={honoured}");
            // Taken back, the toggle ends NOW — not after the answer's whole
            // window (`FAST_SETTLE_MS`) of "turning on…".
            let s = &app.windows[&wid].claude_lights.sessions[&session];
            assert!(s.pending.is_none(), "honoured={honoured}");
            if honoured {
                assert_eq!(
                    refusal(&app, wid, session),
                    Some((Light::Fast, super::Refusal::NoChange))
                );
            }
        }
    }

    /// THE TAKE-BACK'S PERSON FENCE: a person keyed the session (a character
    /// and its backspace) and the composer reads exactly the command again —
    /// the one shape a ctrl+u would clear. It is theirs to keep: no ctrl+u.
    #[test]
    fn a_person_keyed_prompt_is_never_taken_back() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        click(&mut app, wid, Light::Fast);
        assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
        for key in ["x", "\x7f"] {
            app.input_to_session(
                wid,
                crate::input::InputEvent::KeySequence(key.as_bytes().to_vec()),
                crate::input::Source::Human,
                Some(session),
                crate::app_input::PressPhase::Initial,
            );
        }
        assert_eq!(pty(&mut reader, 2), b"x\x7f");
        echo_draft(&app, session, "/fast on");
        past_due(&mut app, session);
        app.on_claude_footer_changed(session);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "no ctrl+u under a person's hand"
        );
        assert_eq!(
            refusal(&app, wid, session),
            Some((Light::Fast, super::Refusal::LeftTyped))
        );
    }

    /// The person pressed Return on the command themselves: it went, and
    /// the light switched — nothing is taken back and nothing is said.
    #[test]
    fn a_command_the_person_submitted_is_not_called_left() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        click(&mut app, wid, Light::Fast);
        assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
        app.input_to_session(
            wid,
            crate::input::InputEvent::KeySequence(b"\r".to_vec()),
            crate::input::Source::Human,
            Some(session),
            crate::app_input::PressPhase::Initial,
        );
        assert_eq!(pty(&mut reader, 1), b"\r");
        app.on_claude_footer_changed(session);
        let rule = "\u{2500}".repeat(70);
        feed(
            &app,
            session,
            format!("\x1b[21;1H{rule} \u{21AF} \u{2500}\x1b[22;1H\x1b[2K\u{276F} \x1b[22;3H")
                .as_bytes(),
        );
        past_due(&mut app, session);
        app.on_claude_footer_changed(session);
        assert!(pty(&mut reader, 0).is_empty());
        assert!(app.claude_typed.is_empty());
        assert_ne!(
            refusal(&app, wid, session).map(|(_, why)| why),
            Some(super::Refusal::LeftTyped)
        );
    }

    /// A driver's lease taken AFTER the paste stops the Enter too: the
    /// driver's `turn` would submit its prompt merged with the command.
    #[test]
    fn a_lease_taken_after_the_paste_stops_the_enter() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        click(&mut app, wid, Light::Fast);
        assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
        *app.pool
            .get(session)
            .expect("session")
            .ctx
            .turn_lease
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(crate::Lease::Turn {
            id: 7,
            driver: None,
            typing: true,
        });
        echo_draft(&app, session, "/fast on");
        app.on_claude_footer_changed(session);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "no Enter into a driven session"
        );
        past_due(&mut app, session);
        app.on_claude_footer_changed(session);
        assert!(pty(&mut reader, 0).is_empty(), "and no take-back");
        assert_eq!(
            refusal(&app, wid, session),
            Some((Light::Fast, super::Refusal::LeftTyped))
        );
    }

    /// However many footer wakes arrive, a waiting command keeps ONE poll
    /// armed: a wake before its instant arms nothing more.
    #[test]
    fn a_waiting_command_arms_one_poll() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        click(&mut app, wid, Light::Fast);
        assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
        assert!(app.claude_typed[&session].next_poll.is_some());
        // The real poll is TYPED_POLL away and can come due while a loaded
        // machine runs this loop: pin its instant ahead, so every wake below
        // is one before it (the mirror of `past_due`).
        let armed = Some(Instant::now() + Duration::from_secs(60));
        app.claude_typed
            .get_mut(&session)
            .expect("waiting")
            .next_poll = armed;
        for _ in 0..5 {
            app.on_claude_footer_changed(session);
        }
        assert_eq!(
            app.claude_typed[&session].next_poll, armed,
            "no poll re-armed early"
        );
        app.claude_typed
            .get_mut(&session)
            .expect("waiting")
            .next_poll = Some(Instant::now() - Duration::from_millis(1));
        let fired = Instant::now();
        app.on_claude_footer_changed(session);
        assert!(
            app.claude_typed[&session].next_poll >= Some(fired + super::TYPED_POLL),
            "the poll that fired is re-armed, once"
        );
    }

    /// A LATE ECHO: the prompt was still empty at the deadline, and the paste
    /// lands after it. One late look takes it back — never an Enter after the
    /// light said Claude did not switch. Control: an echo that never comes
    /// ends the record in silence.
    #[test]
    fn a_paste_echoed_after_the_deadline_is_taken_back() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        click(&mut app, wid, Light::Fast);
        assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
        past_due(&mut app, session);
        app.on_claude_footer_changed(session);
        assert!(app.claude_typed[&session].late, "one late look");
        echo_draft(&app, session, "/fast on");
        app.on_claude_footer_changed(session);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "no Enter during the late look"
        );
        past_due(&mut app, session);
        app.on_claude_footer_changed(session);
        assert_eq!(pty(&mut reader, 1), b"\x15", "the late echo is taken back");
        // Control: the echo never comes.
        let (mut app, wid, session, mut reader) = super::gesture_tests::app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        click(&mut app, wid, Light::Fast);
        assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
        for _ in 0..2 {
            past_due(&mut app, session);
            app.on_claude_footer_changed(session);
        }
        assert!(pty(&mut reader, 0).is_empty());
        assert!(app.claude_typed.is_empty(), "ended, once");
    }

    /// An empty screen does not license a late look while accepted input
    /// is still ordered. The original deadline leaves the text explicitly,
    /// and a later echo cannot resurrect submission or cleanup.
    #[test]
    fn queued_input_at_an_empty_deadline_never_arms_a_late_look() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        let mut state = aterm_spec::derive::claude_light_admission_model().init_state();
        drive_admission_action("StartAccepted", || click(&mut app, wid, Light::Fast));
        assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
        check_admission_transition(&app, wid, session, "StartAccepted", &mut state);
        let sink = app.pool.get(session).unwrap().ctx.sink.clone();
        let pin = crate::app_input::paste_order::pin_ordering_for_test(&sink);
        past_due(&mut app, session);
        drive_admission_action("FollowRejected", || app.on_claude_footer_changed(session));
        check_admission_transition(&app, wid, session, "FollowRejected", &mut state);
        assert!(
            app.claude_typed.is_empty(),
            "no late record behind ordered input"
        );
        assert!(pty(&mut reader, 0).is_empty(), "no Return or cleanup");
        drop(pin);
        echo_draft(&app, session, "/fast on");
        app.on_claude_footer_changed(session);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "the expired record stays ended"
        );
        assert_eq!(
            refusal(&app, wid, session),
            Some((Light::Fast, super::Refusal::LeftTyped))
        );
    }

    /// Input can become queued after the late look was armed. Its deadline
    /// must retain the same ordering guard as the first one: no Ctrl-U joins
    /// undelivered bytes, and the remaining command is acknowledged.
    #[test]
    fn queued_input_at_the_late_deadline_prevents_cleanup() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        let mut state = aterm_spec::derive::claude_light_admission_model().init_state();
        drive_admission_action("StartAccepted", || click(&mut app, wid, Light::Fast));
        assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
        check_admission_transition(&app, wid, session, "StartAccepted", &mut state);
        past_due(&mut app, session);
        drive_admission_action("Wait", || app.on_claude_footer_changed(session));
        check_admission_transition(&app, wid, session, "Wait", &mut state);
        assert!(app.claude_typed[&session].late);
        assert!(app.claude_typed[&session].next_poll.is_none());
        echo_draft(&app, session, "/fast on");
        app.on_claude_footer_changed(session);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "late echo never sends Enter"
        );
        let sink = app.pool.get(session).unwrap().ctx.sink.clone();
        let pin = crate::app_input::paste_order::pin_ordering_for_test(&sink);
        past_due(&mut app, session);
        drive_admission_action("FollowRejected", || app.on_claude_footer_changed(session));
        check_admission_transition(&app, wid, session, "FollowRejected", &mut state);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "no late cleanup behind ordered input"
        );
        assert!(app.claude_typed.is_empty());
        drop(pin);
        app.on_claude_footer_changed(session);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "drain does not retry expired cleanup"
        );
    }

    /// A late take-back's sink can fail too. A real closed socket must leave
    /// the explicit notice, not a fictitious successful Ctrl-U read-back.
    #[test]
    fn a_failed_late_cleanup_leaves_the_command_explicitly() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        let mut state = aterm_spec::derive::claude_light_admission_model().init_state();
        drive_admission_action("StartAccepted", || click(&mut app, wid, Light::Fast));
        assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
        check_admission_transition(&app, wid, session, "StartAccepted", &mut state);
        past_due(&mut app, session);
        drive_admission_action("Wait", || app.on_claude_footer_changed(session));
        check_admission_transition(&app, wid, session, "Wait", &mut state);
        assert!(app.claude_typed[&session].late);
        echo_draft(&app, session, "/fast on");
        ordering_retired(&app, session);
        hang_up(&app, session, reader);
        past_due(&mut app, session);
        drive_admission_action("FollowRejected", || app.on_claude_footer_changed(session));
        check_admission_transition(&app, wid, session, "FollowRejected", &mut state);
        assert!(
            app.claude_typed.is_empty(),
            "no read-back for a failed Ctrl-U"
        );
        assert_eq!(
            refusal(&app, wid, session),
            Some((Light::Fast, super::Refusal::LeftTyped))
        );
        app.on_claude_footer_changed(session);
        assert!(app.claude_typed.is_empty());
    }

    /// The two tests above flaked under load (the gate of 2026-09-27): a
    /// child another test was spawning held a copy of the observer across
    /// its close, so the follow-up write was really accepted. Force that
    /// interleaving — a live child keeps the observer open for a moment
    /// after this test drops its own — and each failed follow-up (Return,
    /// ctrl+u take-back, late cleanup) must still be rejected with the
    /// notice: [`hang_up`] waits the copy out instead of letting the write
    /// land in a socket it keeps connected. A `drop` alone fails this every
    /// time.
    #[test]
    fn a_failed_follow_up_is_rejected_though_a_child_briefly_holds_the_observer() {
        for (take_back, late) in [(false, false), (true, false), (false, true)] {
            let case = format!("take_back={take_back} late={late}");
            let (mut app, wid, session, mut reader) = app();
            publish_facts(&app, session);
            draw_claude(&app, session, BYPASS);
            frame(&mut app, wid);
            let mut state = aterm_spec::derive::claude_light_admission_model().init_state();
            drive_admission_action("StartAccepted", || click(&mut app, wid, Light::Fast));
            assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
            check_admission_transition(&app, wid, session, "StartAccepted", &mut state);
            if late {
                past_due(&mut app, session);
                drive_admission_action("Wait", || app.on_claude_footer_changed(session));
                check_admission_transition(&app, wid, session, "Wait", &mut state);
            }
            ordering_retired(&app, session);
            echo_draft(&app, session, "/fast on");
            if take_back || late {
                past_due(&mut app, session);
            } else {
                not_due(&mut app, session);
            }
            let copy = reader.try_clone().expect("a second reference");
            let mut holder = std::process::Command::new("/bin/sleep")
                .arg("0.3")
                .stdin(std::os::fd::OwnedFd::from(copy))
                .spawn()
                .expect("a child holding the observer");
            hang_up(&app, session, reader);
            // The drain runs while an unreaped child could still hold the
            // copy: only `hang_up`'s wait for the hang-up keeps its write
            // from landing (reaped first, even a bare drop would pass).
            drive_admission_action("FollowRejected", || app.on_claude_footer_changed(session));
            let _ = holder.wait();
            check_admission_transition(&app, wid, session, "FollowRejected", &mut state);
            assert!(app.claude_typed.is_empty(), "{case}: no read-back");
            assert_eq!(
                refusal(&app, wid, session),
                Some((Light::Fast, super::Refusal::LeftTyped)),
                "{case}"
            );
        }
    }

    /// Under a box (the composer not on screen) at the deadline nothing is
    /// pressed, and the light says the command may still be in the prompt.
    #[test]
    fn nothing_is_pressed_into_a_box_at_the_deadline() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        click(&mut app, wid, Light::Fast);
        assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
        feed(&app, session, b"\x1b[2J\x1b[10;1H Do you want to proceed?");
        past_due(&mut app, session);
        app.on_claude_footer_changed(session);
        assert!(pty(&mut reader, 0).is_empty());
        assert_eq!(
            refusal(&app, wid, session),
            Some((Light::Fast, super::Refusal::LeftTyped))
        );
    }

    /// A DRIVER holds the session's lease: a command light types nothing and
    /// says why; a mode light still presses (shift+tab never touches the
    /// prompt). Control: once the lease is gone, the command pastes.
    #[test]
    fn a_driven_session_refuses_a_command_light_but_not_a_mode_light() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, PLAN);
        frame(&mut app, wid);
        let set_lease = |app: &App, lease: Option<crate::Lease>| {
            let entry = app.pool.get(session).expect("session");
            *entry
                .ctx
                .turn_lease
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = lease;
        };
        set_lease(
            &app,
            Some(crate::Lease::Turn {
                id: 41,
                driver: None,
                typing: true,
            }),
        );
        click(&mut app, wid, Light::Fast);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "no paste into a driver's prompt"
        );
        assert_eq!(
            refusal(&app, wid, session),
            Some((Light::Fast, super::Refusal::Driven))
        );
        click(&mut app, wid, Light::Mode);
        assert_eq!(
            pty(&mut reader, 3),
            lights::SHIFT_TAB,
            "a mode light is free"
        );
        let past = Instant::now() - Duration::from_millis(1);
        if let Some(p) = app
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.claude_lights.sessions.get_mut(&session))
            .and_then(|s| s.pending.as_mut())
        {
            p.deadline = past;
        }
        app.on_claude_footer_changed(session);
        set_lease(&app, None);
        click(&mut app, wid, Light::Fast);
        assert_eq!(
            pty(&mut reader, PASTED.len()),
            PASTED,
            "control: no lease, the paste"
        );
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
        assert!(matches!(s.notice, Some((Light::Mode, _, _))));
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
        let screen = app.claude_screen_now(session).map(|(screen, _)| screen);
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
            || s.notice.is_some_and(|(_, until, _)| until > now)
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
            drive_mode_return_action("Click", || click(&mut app, wid, Light::Mode));
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
                    drive_mode_return_action("Press", || app.on_claude_footer_changed(session));
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
            drive_mode_return_action("Click", || click(&mut app, wid, Light::Mode));
            assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
            check_mode_return_transition(&app, wid, session, "Click", &mut state, None);
            redraw_mode_row(&app, session, ACCEPT);
            check_mode_return_transition(&app, wid, session, "Answer", &mut state, None);
            drive_mode_return_action("Press", || app.on_claude_footer_changed(session));
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
            drive_mode_return_action("Click", || click(&mut app, wid, Light::Mode));
            assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
            check_mode_return_transition(&app, wid, session, "Click", &mut state, None);
            for answer in [PLAN, MANUAL] {
                redraw_mode_row(&app, session, answer);
                check_mode_return_transition(&app, wid, session, "Answer", &mut state, None);
                drive_mode_return_action("Press", || app.on_claude_footer_changed(session));
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
            drive_mode_return_action("Click", || click(&mut app, wid, Light::Mode));
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

    /// Claude mid-turn: `esc to interrupt` on the mode row.
    const BUSY: &str = "  \u{23F5}\u{23F5} bypass permissions on \u{00B7} esc to interrupt";

    /// FAST MODE MID-TURN (owner, 2026-09-27: "fast mode toggle doesn't
    /// work"): Claude runs `/fast` mid-turn (`immediate`, measured on
    /// 2.1.283), so a click during a turn is NOT refused — the command is
    /// pasted, and its Enter goes as soon as the composer holds it, the turn
    /// still running. Negative control: a command Claude does NOT run
    /// mid-turn keeps its Enter until the turn ends (the record forced
    /// non-immediate), and nothing is pressed meanwhile.
    #[test]
    fn a_fast_click_mid_turn_is_typed_and_submitted_at_once() {
        for immediate in [true, false] {
            let (mut app, wid, session, mut reader) = app();
            publish_facts(&app, session);
            draw_claude(&app, session, BUSY);
            frame(&mut app, wid);
            click(&mut app, wid, Light::Fast);
            assert_eq!(
                pty(&mut reader, PASTED.len()),
                PASTED,
                "not refused for the turn"
            );
            assert_eq!(refusal(&app, wid, session), None);
            app.claude_typed.get_mut(&session).expect("typed").immediate = immediate;
            not_due(&mut app, session);
            echo_draft(&app, session, "/fast on");
            app.on_claude_footer_changed(session);
            if immediate {
                assert_eq!(pty(&mut reader, 1), b"\r", "the Enter, mid-turn");
            } else {
                assert!(
                    pty(&mut reader, 0).is_empty(),
                    "control: a non-immediate command waits for the turn"
                );
            }
        }
    }

    /// THE REAL IDLE SEQUENCE: after the Enter, Claude hides its composer
    /// while it asks whether fast mode is available (up to 8 s, the vendor's
    /// `Rdr`), then shows `↯`. The old 2.5 s deadline called that "Claude did
    /// not switch". Now the toggle is still in flight 4 s in — no refusal, no
    /// ctrl+u — and `↯` resolves it. Control: past `FAST_SETTLE_MS` with no
    /// answer at all, it is given up.
    #[test]
    fn a_fast_click_waits_out_claudes_availability_check() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        let clicked = Instant::now();
        click(&mut app, wid, Light::Fast);
        assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
        not_due(&mut app, session);
        echo_draft(&app, session, "/fast on");
        app.on_claude_footer_changed(session);
        assert_eq!(pty(&mut reader, 1), b"\r");
        // Claude applies it: no composer, a progress line.
        feed(
            &app,
            session,
            b"\x1b[2J\x1b[20;1H  Turning fast mode on\xe2\x80\xa6",
        );
        app.on_claude_footer_changed(session);
        frame(&mut app, wid);
        let four = clicked + Duration::from_millis(4000);
        assert_eq!(
            app.windows
                .get_mut(&wid)
                .unwrap()
                .claude_lights
                .expire(four),
            (false, Vec::new()),
            "4 s in, still waiting"
        );
        assert!(pty(&mut reader, 0).is_empty(), "no ctrl+u");
        let s = &app.windows[&wid].claude_lights.sessions[&session];
        assert!(s.pending.is_some() && s.refused.is_none());
        assert!(
            super::what_of(
                s,
                Light::Fast,
                None,
                false,
                &lights::Expect::default(),
                Instant::now()
            )
            .0
            .starts_with("turning on"),
            "the chip says it is turning on"
        );
        // Control: the deadline is fast mode's own, 10 s.
        let deadline = s.pending.as_ref().unwrap().deadline;
        assert!(deadline >= clicked + Duration::from_millis(lights::FAST_SETTLE_MS));
        // The composer back, `↯` on its rule: on.
        draw_claude(&app, session, BYPASS);
        set_fast(&app, session);
        app.on_claude_footer_changed(session);
        let s = &app.windows[&wid].claude_lights.sessions[&session];
        assert!(s.pending.is_none() && s.refused.is_none(), "resolved");
        assert_eq!(s.shown.fast, LightState::On);
        assert!(matches!(s.notice, Some((Light::Fast, _, _))));
    }

    /// The transcript's echo of `/fast on` and Claude's `⎿` answer under it,
    /// above the composer (a SYNTHETIC layout from the 2.1.283 strings).
    fn answer_fast(app: &App, session: u64, answer: &str) {
        feed(
            app,
            session,
            format!(
                "\x1b[16;1H\x1b[2K\u{276F} /fast on\x1b[17;1H\x1b[2K  \u{23BF}  {answer}\x1b[22;3H"
            )
            .as_bytes(),
        );
    }

    /// CLAUDE'S OWN REFUSAL, IN ITS WORDS: `Fast mode is not available`
    /// under the echo ends the toggle at once as that refusal, titled "not
    /// available in this Claude" — and, being the build's answer, it is
    /// LATCHED for this Claude build and model: once said, the fast chip
    /// stops asking there. Controls: a retry-later refusal (`Checking fast
    /// mode availability`) and a reason this build does not know (`Fast mode
    /// unavailable: …`, `FastRefusal::Other`) are said but not latched, and
    /// the chip keeps asking; the latch is one build's and one model's — a
    /// session on another model is asked again, and another build's refusal
    /// latched later does not undo this one.
    #[test]
    fn claudes_refusal_is_said_in_its_words_and_a_lasting_one_stops_the_asking() {
        for (answer, lasting) in [
            ("Fast mode is not available", true),
            ("Checking fast mode availability\u{2026}", false),
            ("Fast mode unavailable: something new", false),
        ] {
            let (mut app, wid, session, mut reader) = app();
            publish_facts(&app, session);
            draw_claude(&app, session, BYPASS);
            frame(&mut app, wid);
            click(&mut app, wid, Light::Fast);
            assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
            not_due(&mut app, session);
            echo_draft(&app, session, "/fast on");
            app.on_claude_footer_changed(session);
            assert_eq!(pty(&mut reader, 1), b"\r");
            echo_draft(&app, session, "");
            answer_fast(&app, session, answer);
            app.on_claude_footer_changed(session);
            let why = refusal(&app, wid, session);
            assert!(
                matches!(why, Some((Light::Fast, super::Refusal::Vendor(_)))),
                "{answer:?}: {why:?}"
            );
            frame(&mut app, wid);
            let text = row_text(&app, wid, footer_row(&app, wid));
            let words = if lasting {
                "not available in this Claude"
            } else if answer.starts_with("Checking") {
                "Claude is still checking; try again"
            } else {
                "Claude did not switch (see its message)"
            };
            assert!(text.contains(words), "{text:?}");
            assert_eq!(
                !app.claude_fast_latch.refused.is_empty(),
                lasting,
                "{answer:?}"
            );
            // The refusal's moment over.
            if let Some((_, _, until)) = app
                .windows
                .get_mut(&wid)
                .and_then(|ws| ws.claude_lights.sessions.get_mut(&session))
                .and_then(|s| s.refused.as_mut())
            {
                *until = Instant::now() - Duration::from_millis(1);
            }
            app.on_claude_footer_changed(session);
            // The pointer leaves the chip it clicked: a hovered chip stays
            // drawn, whatever it shows.
            app.clear_claude_light_hover(wid);
            frame(&mut app, wid);
            let asking = app.windows[&wid]
                .claude_lights
                .hits
                .iter()
                .any(|h| h.light == Light::Fast);
            assert_eq!(asking, !lasting, "{answer:?}: the chip asks again?");
            assert!(pty(&mut reader, 0).is_empty(), "no key after the Enter");
            if lasting {
                // Another build's refusal, latched later, leaves this one's.
                assert!(app.claude_fast_latch.refuse(
                    Some("2.1.998".into()),
                    Some("Opus 5.5".into()),
                    lights::FastRefusal::DisabledByOrg,
                ));
                frame(&mut app, wid);
                assert!(
                    !app.windows[&wid]
                        .claude_lights
                        .hits
                        .iter()
                        .any(|h| h.light == Light::Fast),
                    "this build's latch still holds"
                );
                // Control: the latch is the BUILD's and the MODEL's. A
                // session now on another model, then on another Claude
                // build, is asked for fast mode again, once.
                for (version, model) in [(None, "Sonnet 5"), (Some("2.1.999"), "Opus 5.5")] {
                    let entry = app.pool.get(session).expect("session");
                    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                    let mut facts = timeline.claude_footer().cloned().expect("facts");
                    facts.version = version.map(str::to_owned);
                    facts.model = Some(model.to_owned());
                    assert!(timeline.set_claude_footer(PGID, None, Some(facts)));
                    drop(timeline);
                    frame(&mut app, wid);
                    assert!(
                        app.windows[&wid]
                            .claude_lights
                            .hits
                            .iter()
                            .any(|h| h.light == Light::Fast),
                        "{model} on {version:?} is asked again"
                    );
                }
            }
        }
    }

    /// An answer from an EARLIER try, still on screen at the click, is not
    /// this toggle's: the click is still in flight after a wake. Control: a
    /// new answer under a new echo resolves it.
    #[test]
    fn an_earlier_answer_on_screen_is_not_this_clicks() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        answer_fast(&app, session, "Fast mode is currently unavailable");
        frame(&mut app, wid);
        click(&mut app, wid, Light::Fast);
        assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
        app.on_claude_footer_changed(session);
        let s = &app.windows[&wid].claude_lights.sessions[&session];
        assert!(
            s.pending.is_some() && s.refused.is_none(),
            "the old answer is not this one's"
        );
        // Control: Claude answers this try, under its own echo.
        feed(
            &app,
            session,
            "\x1b[18;1H\x1b[2K\u{276F} /fast on\x1b[19;1H\x1b[2K  \u{23BF}  Fast mode ON\x1b[22;3H"
                .as_bytes(),
        );
        app.on_claude_footer_changed(session);
        let s = &app.windows[&wid].claude_lights.sessions[&session];
        assert!(
            s.pending.is_none() && s.refused.is_none(),
            "this one's answer"
        );
        assert!(matches!(s.notice, Some((Light::Fast, _, _))));
    }

    /// WHO MAY STILL BE TYPING: a `turn` whose prompt a busy Claude has
    /// already taken does not refuse a fast click (its prompt is sent), and a
    /// lease live at the paste does not stop the Enter; a cooperative
    /// `lease` refuses it (its holder may type at any time). The idle `turn`
    /// is `a_driven_session_refuses_a_command_light_but_not_a_mode_light`.
    #[test]
    fn a_turn_already_taken_by_a_busy_claude_does_not_refuse_fast_but_a_lease_does() {
        let set_lease = |app: &App, session: u64, lease: Option<crate::Lease>| {
            *app.pool
                .get(session)
                .expect("session")
                .ctx
                .turn_lease
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = lease;
        };
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BUSY);
        frame(&mut app, wid);
        set_lease(
            &app,
            session,
            Some(crate::Lease::Turn {
                id: 41,
                driver: None,
                typing: true,
            }),
        );
        click(&mut app, wid, Light::Fast);
        assert_eq!(
            pty(&mut reader, PASTED.len()),
            PASTED,
            "the turn's prompt is sent"
        );
        not_due(&mut app, session);
        echo_draft(&app, session, "/fast on");
        app.on_claude_footer_changed(session);
        assert_eq!(pty(&mut reader, 1), b"\r", "the same turn, no new hand");
        // A cooperative lease: refused.
        let (mut app, wid, session, mut reader) = super::gesture_tests::app();
        publish_facts(&app, session);
        draw_claude(&app, session, BUSY);
        frame(&mut app, wid);
        set_lease(
            &app,
            session,
            Some(crate::Lease::Drive {
                holder: "manager".into(),
                expires_us: u64::MAX,
                conn: None,
                hard: false,
            }),
        );
        click(&mut app, wid, Light::Fast);
        assert!(pty(&mut reader, 0).is_empty());
        assert_eq!(
            refusal(&app, wid, session),
            Some((Light::Fast, super::Refusal::Driven))
        );
    }

    /// Claude Code's bottom block drawn `cols` wide — [`draw_claude`] for a
    /// window resized narrower — with fast mode ON on its rule.
    fn draw_claude_fast_cols(app: &App, session: u64, mode: &str, cols: usize) {
        let rule = "\u{2500}".repeat(cols);
        let fast_rule = format!("{} \u{21AF} \u{2500}", "\u{2500}".repeat(cols - 4));
        let screen = format!(
            "\x1b[?2004h\x1b[2J\x1b[19;1H\u{25CF} done\x1b[21;1H{fast_rule}\x1b[22;1H\u{276F} \
             \x1b[23;1H{rule}\x1b[24;1H{mode}\x1b[22;3H"
        );
        feed(app, session, screen.as_bytes());
    }

    /// NO SELECTION NOBODY CAN SEE (the review of 2026-09-27): the keyboard
    /// selects only a chip the pane has ROOM for, the reveal giving way
    /// first. At 40 columns, accept edits: the chip drawn at rest stays on
    /// the glass when the chord selects it (the whole reveal does not fit),
    /// and the walk does not step onto a chip that cannot be drawn beside
    /// it. The window narrowed to 30 columns, no chip fits: the standing
    /// selection is let go by the next key, and that Return is CLAUDE'S —
    /// never swallowed by a light off the glass. Control: at 30 columns a
    /// fresh chord selects nothing (and reaches Claude as no shift+tab), and
    /// Return is Claude's again.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_keyboard_never_selects_a_chip_the_pane_has_no_room_for() {
        use winit::event::KeyEvent;
        use winit::keyboard::{
            Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr,
        };
        fn tap(app: &mut App, wid: WindowId, named: NamedKey, code: KeyCode, text: &str) {
            for state in [ElementState::Pressed, ElementState::Released] {
                app.on_key(
                    wid,
                    KeyEvent::synthetic_for_test(
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
        fn chord(app: &mut App, wid: WindowId) {
            app.windows.get_mut(&wid).unwrap().mods =
                ModifiersState::CONTROL | ModifiersState::SHIFT;
            tap(app, wid, NamedKey::Tab, KeyCode::Tab, "\t");
            app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
        }
        const ACCEPT_SHORT: &str = "  \u{23F5}\u{23F5} accept edits on";
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        assert!(app.apply_term_resize(wid, 24, 40));
        draw_claude_fast_cols(&app, session, ACCEPT_SHORT, 40);
        frame(&mut app, wid);
        let at_rest = hit(&app, wid, Light::Mode);
        chord(&mut app, wid);
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, Light::Mode))
        );
        frame(&mut app, wid);
        let text = row_text(&app, wid, at_rest.0);
        assert!(
            text.contains("\u{23F5}\u{23F5} accept edits") && !text.contains("fast"),
            "the chip stays, the reveal gives way: {text:?}"
        );
        assert_eq!(hit(&app, wid, Light::Mode).0, at_rest.0);
        tap(&mut app, wid, NamedKey::ArrowRight, KeyCode::ArrowRight, "");
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, Light::Mode)),
            "no step onto a chip with no room beside it"
        );
        assert!(pty(&mut reader, 0).is_empty());
        // Narrower: no chip fits at all.
        assert!(app.apply_term_resize(wid, 24, 30));
        draw_claude_fast_cols(&app, session, ACCEPT_SHORT, 30);
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
    }

    /// THE ANSWER WINDOW OPENS AT THE ENTER (the review of 2026-09-27): the
    /// paste's echo may take most of a step, and Claude's up-to-8 s org check
    /// starts at the Enter — so the Enter sets the toggle's deadline a whole
    /// `FAST_SETTLE_MS` from itself. And the Enter is ATERM'S keystroke, not
    /// the person's: it leaves the person clock as the click left it, so the
    /// supervisor's `human_grace_s` is the click's alone. Controls: the click
    /// itself stamps the clock, and the deadline before the Enter is the one
    /// set short here.
    #[test]
    fn the_answer_window_opens_at_the_enter_and_the_enter_is_aterms_own() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        let clock = |app: &App| app.pool.get(session).unwrap().ctx.human_input.last();
        let before = clock(&app);
        click(&mut app, wid, Light::Fast);
        assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
        let after_click = clock(&app);
        assert_ne!(after_click, before, "control: the click is the person's");
        not_due(&mut app, session);
        // The echo was slow: the click's window is nearly spent.
        let short = Instant::now() + Duration::from_millis(100);
        app.windows
            .get_mut(&wid)
            .and_then(|ws| ws.claude_lights.sessions.get_mut(&session))
            .and_then(|s| s.pending.as_mut())
            .expect("in flight")
            .deadline = short;
        echo_draft(&app, session, "/fast on");
        let enter = Instant::now();
        app.on_claude_footer_changed(session);
        assert_eq!(pty(&mut reader, 1), b"\r");
        let deadline = app.windows[&wid].claude_lights.sessions[&session]
            .pending
            .as_ref()
            .expect("still in flight")
            .deadline;
        assert!(
            deadline >= enter + Duration::from_millis(lights::FAST_SETTLE_MS),
            "a whole answer window from the Enter"
        );
        assert_eq!(clock(&app), after_click, "the Enter stamps no person");
    }

    /// A VIEW SCROLLED INTO HISTORY keeps the answer poll armed (the review
    /// of 2026-09-27): the engine read skips a scrolled view, and nothing
    /// paints one — so the loop must keep looking, and read Claude's answer
    /// once the view is back. Control: the answer, read, resolves in
    /// Claude's words (not the deadline's "did not switch").
    #[test]
    fn a_scrolled_view_keeps_the_answer_poll_armed() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        feed(&app, session, "history\r\n".repeat(60).as_bytes());
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        click(&mut app, wid, Light::Fast);
        assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
        not_due(&mut app, session);
        echo_draft(&app, session, "/fast on");
        app.on_claude_footer_changed(session);
        assert_eq!(pty(&mut reader, 1), b"\r");
        echo_draft(&app, session, "");
        answer_fast(
            &app,
            session,
            "Fast mode has been disabled by your organization",
        );
        let term = app.pool.get(session).expect("session").term.clone();
        term_lock(&term).scroll_display(5);
        assert!(app.claude_rows_now(session).is_none(), "scrolled back");
        let poll = |app: &mut App| -> Option<Instant> {
            app.windows[&wid].claude_lights.sessions[&session]
                .pending
                .as_ref()
                .and_then(|p| p.next_poll)
        };
        let past = Instant::now() - Duration::from_millis(1);
        app.windows
            .get_mut(&wid)
            .and_then(|ws| ws.claude_lights.sessions.get_mut(&session))
            .and_then(|s| s.pending.as_mut())
            .expect("in flight")
            .next_poll = Some(past);
        app.on_claude_footer_changed(session);
        assert!(
            poll(&mut app).is_some_and(|at| at > Instant::now()),
            "the poll is armed again"
        );
        term_lock(&term).scroll_to_bottom();
        app.on_claude_footer_changed(session);
        assert_eq!(
            refusal(&app, wid, session),
            Some((
                Light::Fast,
                super::Refusal::Vendor(lights::FastRefusal::DisabledByOrg)
            ))
        );
    }

    /// The repaint key of a window: its footer term, as a frame computes it.
    fn footer_key(app: &mut App, wid: WindowId) -> u64 {
        let prepared = app.prepare_terminal_capture_grid_with_cursor_fx_and_plan_outcome(
            wid,
            crate::app_render::ComposedCursorFxClock::Advance(Instant::now()),
        );
        let crate::app_render::CapturePreparation::Ready((_, plan)) = prepared else {
            panic!("the headless capture must produce a frame");
        };
        app.claude_footer_fp(wid, &plan)
    }

    /// THE LATCH REPAINTS EVERY WINDOW (the review of 2026-09-27): a latch
    /// taken in one window changes what the fast chip says in every other,
    /// so it is a term of every window's repaint key — an idle pane does not
    /// keep a stale, clickable `○ fast`. Control: the same latch again is no
    /// change, and the key stays.
    #[test]
    fn a_latch_moves_every_windows_repaint_key() {
        let (mut app, wid, session, _reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        let before = footer_key(&mut app, wid);
        assert_eq!(footer_key(&mut app, wid), before, "a quiet key is stable");
        assert!(app.claude_fast_latch.refuse(
            None,
            Some("Opus 5.5".into()),
            lights::FastRefusal::NotAvailable
        ));
        let latched = footer_key(&mut app, wid);
        assert_ne!(latched, before, "the latch repaints");
        assert!(!app.claude_fast_latch.refuse(
            None,
            Some("Opus 5.5".into()),
            lights::FastRefusal::NotAvailable
        ));
        assert_eq!(footer_key(&mut app, wid), latched, "control: no change");
        frame(&mut app, wid);
        assert!(
            !app.windows[&wid]
                .claude_lights
                .hits
                .iter()
                .any(|h| h.light == Light::Fast),
            "and the chip is gone"
        );
    }

    /// FAST MODE THAT TOOK AUTO MODE OFF, END TO END: from auto, `/fast on`
    /// is answered ON and Claude moves the session to manual, saying why
    /// (`lights::AUTO_OFF_FOR_FAST`). The fast chip says so; the build is
    /// latched; and from then on, in auto mode, the fast chip is not drawn,
    /// and a click on it (the keyboard's reveal) is REFUSED — auto is never
    /// traded for fast again unseen. Control: in bypass the latch costs
    /// nothing, and the fast chip still asks.
    #[test]
    fn fast_mode_that_took_auto_off_is_never_traded_again_unseen() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, AUTO);
        frame(&mut app, wid);
        click(&mut app, wid, Light::Fast);
        assert_eq!(pty(&mut reader, PASTED.len()), PASTED);
        not_due(&mut app, session);
        echo_draft(&app, session, "/fast on");
        app.on_claude_footer_changed(session);
        assert_eq!(pty(&mut reader, 1), b"\r");
        echo_draft(&app, session, "");
        answer_fast(&app, session, "Fast mode ON");
        redraw_mode_row(
            &app,
            session,
            "  \u{23F8} manual mode on \u{00B7} auto mode unavailable while fast mode is on \u{00B7} run /fast off",
        );
        set_fast(&app, session);
        app.on_claude_footer_changed(session);
        let s = &app.windows[&wid].claude_lights.sessions[&session];
        assert!(s.pending.is_none());
        assert!(
            matches!(s.notice, Some((Light::Fast, _, Some(said))) if said.contains("auto mode off")),
            "{:?}",
            s.notice
        );
        assert!(app.claude_fast_latch.costs_auto.contains(&None));
        // Back in auto, fast off: not asked.
        draw_claude(&app, session, AUTO);
        if let Some(s) = app
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.claude_lights.sessions.get_mut(&session))
        {
            s.notice = None;
        }
        app.clear_claude_light_hover(wid);
        frame(&mut app, wid);
        assert!(
            !app.windows[&wid]
                .claude_lights
                .hits
                .iter()
                .any(|h| h.light == Light::Fast),
            "auto is not asked to trade"
        );
        app.toggle_claude_light(wid, session, Light::Fast);
        assert!(pty(&mut reader, 0).is_empty(), "nothing typed");
        assert_eq!(
            refusal(&app, wid, session),
            Some((Light::Fast, super::Refusal::CostsAuto))
        );
        // Control: bypass loses nothing to fast mode.
        draw_claude(&app, session, BYPASS);
        if let Some(s) = app
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.claude_lights.sessions.get_mut(&session))
        {
            s.refused = None;
        }
        frame(&mut app, wid);
        assert!(
            app.windows[&wid]
                .claude_lights
                .hits
                .iter()
                .any(|h| h.light == Light::Fast),
            "bypass: the fast chip asks"
        );
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
