// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CLAUDE CODE LIGHTS, host side: a light in the rule under Claude Code's
//! input box, left of aterm's footer facts (`crate::claude_footer`) — the
//! permission mode (owner directions, 2026-09-24 and 2026-09-28). What it
//! means, where it is read from and which of Claude's own inputs flips it are
//! decided in `aterm_agent::harness::lights`; this module puts it on the
//! glass and carries the gestures. Fast mode has no chip of aterm's (owner,
//! 2026-09-30): Claude Code draws its own `↯` in its top rule.
//!
//! * DEVIATIONS ONLY: the light is drawn — as a labelled chip, `⏸ plan` —
//!   only while it shows what the owner does not expect
//!   (`lights::deviates`), while a toggle of it is in flight, a refusal or a
//!   just-switched notice for it is on show, or the pointer or the keyboard
//!   selection is on it. At rest, everything as expected, nothing is drawn:
//!   `claude_lights_block` is `None` and the rule carries the facts alone.
//!   The chip never replaces or covers Claude's own mode pill, which stays
//!   on Claude's row under the rule, whole: the mode chip is aterm's own
//!   click and keyboard target, in aterm's span of the rule. A rule too
//!   narrow for the chip beside the model draws none (the model gives way
//!   last, `footer::fit_rule`).
//! * HOVER the chip: its title and state replace nothing — they appear just
//!   right of the chip, where the rule has room: a hover's, a selection's or
//!   a toggle's title is the first thing a narrow rule gives up, short then
//!   gone, before any of the facts, while a REASON — where a mode return
//!   stopped — outranks the branch and the path, never the model or the
//!   effort (`footer::fit_rule`). CLICK it: it toggles.
//! * `ctrl+shift+tab` (macOS; elsewhere it is aterm's `prev_tab`) REVEALS
//!   the known light of the focused pane and selects it (Claude keeps its
//!   own shift+tab); while it is selected, Return or Space toggles it (in
//!   bypass or auto it does nothing), and Escape — or any other key, which
//!   then goes on to Claude as usual — lets go, and the light at rest goes
//!   with the selection. Only a chip the pane has ROOM for is ever selected
//!   (the reveal gives way first), and a selection whose chip is not on the
//!   glass is let go by the next key: a light nobody can see never takes a
//!   Return meant for Claude.
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

use aterm_agent::harness::lights::{self, Light, LightState, Mode, Reading, Screen};
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
                    || s.notice.is_some_and(|(_, until)| until > now)
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
            if let Some((light, until)) = s.notice
                && until > now
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
        let Some(screen) =
            crate::reader_guard::read_or_none(session, "Claude Code lights", rows, || read(rows))
        else {
            return Step::default();
        };
        self.observe(session, screen.as_ref())
    }

    /// Fold one frame's reading of `session`'s screen in: remember what each
    /// light shows. A toggle is NEVER stepped here: a frame that shows
    /// Claude's answer — the pill moved on — only wakes the loop, which reads
    /// the ENGINE and decides ([`App::advance_claude_mode_returns`]) — the
    /// same decision whether or not a frame is painted. What the caller must
    /// do: wake the loop.
    fn observe(&mut self, session: u64, screen: Option<&Screen>) -> Step {
        let s = self.sessions.entry(session).or_default();
        s.shown.fold(&lights::read(screen));
        let Some(p) = s.pending.as_ref() else {
            return Step::default();
        };
        // Claude answered the last press (or the pill moved on its own): the
        // loop reads the engine and decides.
        Step {
            wake: screen.is_some_and(|screen| screen.mode.is_some_and(|m| m != p.mode_at_press)),
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
    fn block(&self, session: u64, blank: RenderCell, now: Instant, reveal: bool) -> Block {
        self.block_with(
            session,
            blank,
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
    fn block_with(
        &self,
        session: u64,
        blank: RenderCell,
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
            .filter(|(_, until)| *until > now)
            .map(|(light, _)| light);
        let pending = s.pending.as_ref().map(|p| p.light);
        let drawn = |light: Light| {
            lights::deviates(light, &s.shown)
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
            let (what, why) = what_of(s, light, refused, focus.is_some_and(|(_, l)| l == light));
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

/// A chip's glyph, word and ink. The mode by its own pill glyph and word, in
/// its hue where Claude gives it one (bypass, auto) and the row's ink
/// otherwise; a light whose mode is not known as a dim `◌` and its word. A
/// toggle in flight shows `◐`.
fn chip(
    light: Light,
    shown: &Reading,
    switching: bool,
    ink: [u8; 3],
    dim: [u8; 3],
) -> (&'static str, &'static str, [u8; 3]) {
    let glyph = |own: &'static str| if switching { "\u{25D0}" } else { own };
    match shown.mode {
        Some(mode) => (glyph(mode.glyph()), mode.word(), mode.hue().unwrap_or(ink)),
        None => (glyph("\u{25CC}"), light.word(), dim),
    }
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

    /// A block of `lights` (the chips' cells, as one chip) and its titles,
    /// for the rule's layout tests (`claude_footer`).
    pub(crate) fn of_chips(
        lights: Vec<RenderCell>,
        title: Vec<RenderCell>,
        short_title: Vec<RenderCell>,
        reason: bool,
    ) -> Block {
        Block {
            cols: vec![(0..lights.len(), Light::Mode)],
            title,
            short_title,
            reason,
            lights,
        }
    }
}

impl App {
    /// Read one Claude Code pane's screen for its lights: remember what each
    /// shows, and wake the loop when a toggle in flight was answered. Called
    /// for EVERY Claude Code pane each frame, painted footer or not.
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
        let now = Instant::now();
        [true, false]
            .into_iter()
            .map(|reveal| ws.claude_lights.block(session, blank, now, reveal))
            .find(|block| block.lights.len() <= room)
            .filter(|block| !block.cols.is_empty())
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
    /// the focused pane's next light; with one selected, Return/Space toggle
    /// and Escape lets go. Any other key lets go and is NOT consumed.
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
        let step = |from: Option<Light>| -> Option<Light> {
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
            Some((Light::Mode, Refusal::NotShown, now + Duration::from_secs(1)));
        assert_ne!(w.fingerprint(now), hovered);
        assert_eq!(w.fingerprint(now + Duration::from_secs(2)), hovered);

        w.hover = None;
        assert_eq!(w.fingerprint(now + Duration::from_secs(2)), 0);
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
        let boom = |_: &[String]| -> Option<Screen> { panic!("stand-in reader panic") };
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
            .block(7, blank(), due, true)
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
            .block(7, blank(), two, true)
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

    /// DEVIATIONS ONLY: at rest — bypass or auto — the block is EMPTY. Plan
    /// mode draws a labelled chip, a cell of margin after; the title comes
    /// APART from the chip (only the chip must fit); a box over the composer
    /// keeps the last reading rather than dropping the row.
    #[test]
    fn only_deviations_are_drawn_and_the_block_keeps_what_it_saw() {
        let now = Instant::now();
        let bare = "\u{2500}".repeat(60);
        let mut w = WindowLights::default();
        for mode in [BYPASS, AUTO] {
            observe(&mut w, &screen(&bare, mode, ""));
            let rest = w.block(7, blank(), now, true);
            assert!(
                rest.lights.is_empty() && rest.cols.is_empty() && rest.title.is_empty(),
                "nothing at rest: {mode:?}"
            );
        }
        // Control: plan mode.
        observe(&mut w, &screen(&bare, PLAN, ""));
        let block = w.block(7, blank(), now, true);
        let text: String = block.lights.iter().map(|c| c.ch).collect();
        assert_eq!(text, "\u{23F8} plan");
        assert_eq!(block.cols, vec![(0..6, Light::Mode)]);
        assert!(block.title.is_empty(), "nothing asked for a title");
        assert_eq!(block.lights[0].fg, blank().fg, "plan, in the row's ink");
        w.observe(7, None);
        assert_eq!(
            w.block(7, blank(), now, true).lights,
            block.lights,
            "a covered composer keeps the last reading"
        );
        w.hover = Some((7, Light::Mode));
        let hovered = w.block(7, blank(), now, true);
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
    }

    /// The keyboard selection reveals the light at rest and marks it: in
    /// bypass, the chip names the mode.
    #[test]
    fn the_keyboard_selection_reveals_the_light_at_rest() {
        let now = Instant::now();
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&"\u{2500}".repeat(60), BYPASS, ""));
        assert!(w.block(7, blank(), now, true).cols.is_empty(), "at rest");
        w.selected = Some((7, Light::Mode));
        let block = w.block(7, blank(), now, true);
        let title: String = block.title.iter().map(|c| c.ch).collect();
        assert_eq!(title, "Permission mode: bypass  ");
        assert_eq!(
            block.lights[0].ch, '\u{23F5}',
            "the expected mode, revealed"
        );
        assert!(block.lights[0].bold);
        assert!(
            w.block(7, blank(), now, false).shows(Light::Mode),
            "the selection itself is drawn without the reveal"
        );
    }

    #[test]
    fn a_quiet_row_keys_as_zero() {
        let now = Instant::now();
        let mut w = WindowLights::default();
        assert_eq!(w.fingerprint(now), 0);
        w.hover = Some((7, Light::Mode));
        let a = w.fingerprint(now);
        w.selected = Some((7, Light::Mode));
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
    use aterm_agent::harness::lights::{self, Light, Mode};
    use aterm_core::terminal::RenderCell;
    use aterm_session::sink::SinkWriter;
    use winit::event::{ElementState, MouseButton};

    use crate::{App, WindowId, term_lock};

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
    /// write to the socket still succeeds (measured 2026-09-27 on the gate:
    /// `FollowRejected` read a failed-write fixture's Enter as accepted, only
    /// under load with sibling tests running). So wait for the sink's own end
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
    /// glyph, a title or a fact. CONTROL: the rule's own glyphs are ink a
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
            if mode == measured {
                // Auto mode is as expected: no chip at rest. Reveal it.
                app.windows.get_mut(&wid).unwrap().claude_lights.selected =
                    Some((session, Light::Mode));
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
            for light in Light::ALL {
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
                text.contains("\u{23F5}\u{23F5} bypass  bypass ") && text.contains(WHOLE_FACTS),
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
    /// state of parameters that I always expect to be on"): bypass or auto —
    /// the rule carries the facts alone, no chip, nothing
    /// to hit, while the rule is still known to be on the glass (the
    /// keyboard's way to the lights); and Claude's own row under it, its pill
    /// and hint, is exactly as Claude drew it (owner, 2026-09-28).
    #[test]
    fn at_rest_the_rule_carries_the_facts_alone() {
        for mode in [BYPASS, AUTO] {
            let (mut app, wid, session, _reader) = app();
            publish_facts(&app, session);
            draw_claude(&app, session, mode);
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
        }
    }

    /// THE KEYBOARD REACHES A LIGHT AT REST (macOS): with nothing drawn,
    /// `ctrl+shift+tab` reveals the known light — the expected mode,
    /// `⏵⏵ bypass` — and Return on the expected mode presses nothing; the
    /// reveal goes with the selection.
    #[cfg(target_os = "macos")]
    #[test]
    fn ctrl_shift_tab_reveals_the_lights_at_rest() {
        use winit::keyboard::{KeyCode, ModifiersState, NamedKey};

        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
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
            app.windows[&wid].claude_lights.hits.is_empty(),
            "the reveal went with the selection"
        );
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
            Some((session, Light::Mode))
        );
        tap(&mut app, wid, NamedKey::Tab, KeyCode::Tab, "\t");
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, Light::Mode)),
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
    /// Return is Claude's again.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_keyboard_never_selects_a_chip_the_pane_has_no_room_for() {
        use winit::keyboard::{KeyCode, ModifiersState, NamedKey};
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
        draw_claude_cols(&app, session, ACCEPT_SHORT, 40);
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
