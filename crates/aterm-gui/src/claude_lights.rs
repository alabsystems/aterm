// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CLAUDE CODE LIGHTS, host side: a row of lights at the right end of
//! aterm's Claude Code footer (`crate::claude_footer`) — auto-approve, auto
//! mode, fast mode (owner direction, 2026-09-24). What each light means,
//! where it is read from and which of Claude's own inputs flips it are
//! decided in `aterm_agent::harness::lights`; this module puts them on the
//! glass and carries the gestures.
//!
//! * HOVER a light: its title and state replace nothing — they appear just
//!   left of the lights. CLICK it: it toggles.
//! * `ctrl+shift+tab` (macOS; elsewhere it is aterm's `prev_tab`) selects the
//!   focused pane's next light and shows its title (Claude keeps its own
//!   shift+tab); while one is selected, Return or
//!   Space toggles it, ←/→ move, and Escape — or any other key, which then
//!   goes on to Claude as usual — lets go.
//! * A toggle is Claude's own input, and it is READ BACK: the light shows
//!   `◐` until Claude's screen says it switched, and says so in its title if
//!   Claude did not within `harness::lights::SETTLE_MS`. A slash command is
//!   pasted only into an EMPTY composer, never while a driver holds the
//!   session's lease, and it is SUBMITTED only while the composer holds
//!   exactly it (`lights::composer_holds`) and no person has touched the
//!   session since aterm typed it (`HumanInputStamp::last`). Not submitted in
//!   time, it is TAKEN BACK — one ctrl+u, under the same two conditions, read
//!   back once — and anything else leaves it where it is, the light saying so:
//!   aterm never types into, or deletes, what may be a person's.
//! * Input never leaves from the render path: a frame that reads a step's
//!   result queues the next step and wakes the loop (`Wake::ClaudeFooter`),
//!   whose handler sends it ([`App::drain_claude_lights`]).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use aterm_agent::harness::lights::{self, Drive, Light, LightState, Mode, Screen};
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
    /// The mode cycle came back round without the mode: this session's
    /// shift+tab cycle does not hold it. The person's mode is restored.
    NotInCycle,
    /// Claude is mid-turn; a slash command waits for it to finish.
    Busy,
    /// Claude's screen moved (a box opened) before the next press could go.
    Moved,
    /// The mode in front (don't ask) is one Claude's shift+tab cycle never
    /// comes back to, so no cycle could restore it: set it in Claude.
    OutsideCycle,
    /// The slash command aterm typed is still in the composer: a person
    /// touched the session before it went, or it could not be taken back.
    LeftTyped,
    /// A driver holds the session's lease (a `turn` in flight, or a `lease`):
    /// a slash command must not land in its prompt.
    Driven,
    /// The input seam accepted nothing: no toggle owns the command.
    InputRejected,
    /// Earlier accepted input has not yet left the session's ordered FIFO.
    InputPending,
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
    deadline: Instant,
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
#[derive(Debug, Clone)]
struct SessionLights {
    /// The last state each light was SEEN in — kept while a box covers the
    /// composer, so the row does not flicker grey under every dialog.
    shown: [LightState; 3],
    pending: Option<Pending>,
    refused: Option<(Light, Refusal, Instant)>,
}

impl Default for SessionLights {
    fn default() -> Self {
        Self {
            shown: [LightState::Unknown; 3],
            pending: None,
            refused: None,
        }
    }
}

/// A step the loop sends after the frame that asked for it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Out {
    /// shift+tab, the next press of a mode cycle — sent only while the live
    /// screen still shows the composer in `expect`, the mode this press was
    /// decided on (a box that opened since the frame gets nothing), and only
    /// while the toggle that asked for it is still in flight.
    ShiftTab { expect: Mode },
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
    hits: Vec<LightHit>,
    hover: Option<(u64, Light)>,
    selected: Option<(u64, Light)>,
    sessions: HashMap<u64, SessionLights>,
    outbox: Vec<(u64, Out)>,
}

fn index(light: Light) -> usize {
    Light::ALL.iter().position(|l| *l == light).unwrap_or(0)
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
                s.pending.is_some() || s.refused.is_some_and(|(_, _, until)| until > now)
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
        }
        h.finish() | 1
    }

    /// Forget every painted light; the painter records this frame's.
    pub(crate) fn clear_hits(&mut self) {
        self.hits.clear();
    }

    fn light_at(&self, frame_row: usize, col: usize) -> Option<(u64, Light)> {
        self.hits
            .iter()
            .find(|h| h.frame_row == frame_row && h.col == col)
            .map(|h| (h.session, h.light))
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
        now: Instant,
    ) -> Step {
        let Some(screen) = crate::reader_guard::read_or_none(
            &format!("{session}"),
            "Claude Code lights",
            rows,
            || read(rows),
        ) else {
            return Step::default();
        };
        self.observe(session, screen.as_ref(), now)
    }

    /// Fold one frame's reading of `session`'s screen in: remember what each
    /// light shows, and move a toggle in flight along — done, the next press
    /// of a cycle, or given up. (A command's Enter is judged from the engine
    /// by the loop, `App::drain_claude_lights`, not from a frame.) What the
    /// caller must do: wake the loop to send a queued step, and time a
    /// refusal's end.
    fn observe(&mut self, session: u64, screen: Option<&Screen>, now: Instant) -> Step {
        let states = lights::states(screen);
        let s = self.sessions.entry(session).or_default();
        for (slot, state) in s.shown.iter_mut().zip(states) {
            if state != LightState::Unknown {
                *slot = state;
            }
        }
        let Some(p) = s.pending.as_mut() else {
            return Step::default();
        };
        let light = p.light;
        let now_state = s.shown[index(light)];
        let mut queued = None;
        let mut done = false;
        let mut lapped = false;
        match (&p.drive, screen) {
            (Drive::CycleMode { until }, Some(screen)) => match screen.mode {
                Some(mode) if lights::cycle_done(*until, p.started, mode) => done = true,
                Some(mode) if mode != p.mode_at_press => {
                    // Claude answered the last press with a mode that is not
                    // the one wanted. A mode shown before is a LAP — this
                    // session's cycle does not hold the target (it has three
                    // to five modes, `lights::MAX_MODE_PRESSES`) — and back at
                    // the start is exactly where the person had it: stop.
                    if mode == p.started
                        || p.seen.contains(&mode)
                        || p.presses >= lights::MAX_MODE_PRESSES
                    {
                        lapped = true;
                    } else {
                        p.presses += 1;
                        p.seen.push(mode);
                        p.mode_at_press = mode;
                        queued = Some(Out::ShiftTab { expect: mode });
                    }
                }
                // Not answered yet, or a pill this build cannot read (which
                // takes no step): wait, the deadline is the backstop.
                _ => {}
            },
            (Drive::Command(_), _) => {
                if now_state != p.from && now_state != LightState::Unknown {
                    done = true;
                }
            }
            (Drive::CycleMode { .. }, None) => {}
        }
        if done {
            s.pending = None;
            s.refused = None;
        }
        if lapped {
            s.refused = Some((light, Refusal::NotInCycle, now + REFUSAL_SHOWN));
            s.pending = None;
        }
        let step = Step {
            queued: queued.is_some(),
            refused: lapped,
        };
        if let Some(out) = queued {
            self.outbox.push((session, out));
        }
        step
    }

    /// Give up on every toggle past its deadline: whether anything changed,
    /// and the sessions whose refusal starts now (the caller times its end —
    /// a refusal no frame ends would stay on an idle screen).
    fn expire(&mut self, now: Instant) -> (bool, Vec<u64>) {
        let mut any = false;
        let mut refused = Vec::new();
        for (session, s) in &mut self.sessions {
            if let Some(p) = &s.pending
                && p.deadline <= now
            {
                s.refused = Some((p.light, Refusal::NoChange, now + REFUSAL_SHOWN));
                s.pending = None;
                refused.push(*session);
                any = true;
            }
            if s.refused.is_some_and(|(_, _, until)| until <= now) {
                s.refused = None;
                any = true;
            }
        }
        (any, refused)
    }

    /// `session`'s light block: its TITLE (when a light is selected, hovered,
    /// switching or refused — empty otherwise) and its LIGHTS (the three
    /// lights, then one cell of margin), with the column of each light within
    /// the lights. Apart, because only the lights must fit: a pane too narrow
    /// for the title keeps its lights and drops the title.
    fn block(&self, session: u64, blank: RenderCell, now: Instant) -> Block {
        let s = self.sessions.get(&session).cloned().unwrap_or_default();
        // The keyboard selection outranks a resting pointer: Return toggles
        // the SELECTED light, so the row must mark that one.
        let focus = self
            .selected
            .filter(|(id, _)| *id == session)
            .or(self.hover.filter(|(id, _)| *id == session));
        let refused = s.refused.filter(|(_, _, until)| *until > now);
        let titled = focus
            .map(|(_, l)| l)
            .or(s.pending.as_ref().map(|p| p.light))
            .or(refused.map(|(l, _, _)| l));
        let dim = dim_of(blank);
        let mut title = Vec::new();
        let mut short_title = Vec::new();
        if let Some(light) = titled {
            let state = s.shown[index(light)];
            let what = match (refused, &s.pending) {
                (Some((l, why, _)), _) if l == light => match why {
                    Refusal::NotShown => "not on screen right now",
                    Refusal::Draft => "clear the prompt first",
                    Refusal::NoChange => "Claude did not switch",
                    Refusal::NotInCycle => "not in this session's cycle",
                    Refusal::Busy => "wait for Claude to finish",
                    Refusal::Moved => "screen changed, try again",
                    Refusal::OutsideCycle => "leave don't-ask in Claude first",
                    Refusal::LeftTyped => "command left in the prompt",
                    Refusal::Driven => "a driver has the session",
                    Refusal::InputRejected => "input was not accepted; try again",
                    Refusal::InputPending => "wait for queued input",
                },
                (_, Some(p)) if p.light == light => "switching\u{2026}",
                _ => match (light, state) {
                    (_, LightState::On) => "on",
                    (_, LightState::Off) => "off",
                    (_, LightState::Unknown) => "not on screen right now",
                },
            };
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
        let mut cells = Vec::with_capacity(2 * Light::ALL.len());
        let mut cols = Vec::with_capacity(Light::ALL.len());
        for (i, light) in Light::ALL.into_iter().enumerate() {
            if i > 0 {
                cells.push(blank);
            }
            let state = s.shown[i];
            let switching = s.pending.as_ref().is_some_and(|p| p.light == light);
            let mut c = blank;
            (c.ch, c.fg) = match (switching, state) {
                (true, _) => ('\u{25D0}', light.hue()),
                (false, LightState::On) => ('\u{25CF}', light.hue()),
                (false, LightState::Off) => ('\u{25CB}', dim),
                (false, LightState::Unknown) => ('\u{25CC}', dim),
            };
            if focus.is_some_and(|(_, l)| l == light) {
                c.bold = true;
                c.underline = UnderlineStyle::Single;
            }
            cols.push((cells.len(), light));
            cells.push(c);
        }
        cells.push(blank);
        Block {
            title,
            short_title,
            lights: cells,
            cols,
        }
    }
}

/// What one frame's reading of a pane asks of the host.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Step {
    /// A step was queued: wake the loop to send it.
    queued: bool,
    /// A refusal began: time its end.
    refused: bool,
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

/// The lights' reading of a pane's rows, one frame's.
fn read_lights(rows: &[String]) -> Option<Screen> {
    lights::read_screen(rows)
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

/// `blank`'s ink halfway to its ground: the off light and the title.
fn dim_of(blank: RenderCell) -> [u8; 3] {
    let mix = |a: u8, b: u8| ((u16::from(a) + u16::from(b)) / 2) as u8;
    [
        mix(blank.fg[0], blank.bg[0]),
        mix(blank.fg[1], blank.bg[1]),
        mix(blank.fg[2], blank.bg[2]),
    ]
}

/// Post the footer wake for `session` after `delay` — a toggle's deadline
/// and a refusal's end are times the grid has no damage to announce.
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

/// What the painter needs for one pane: the title and the lights (see
/// [`WindowLights::block`]) and where each light sits in the lights.
pub(crate) struct Block {
    /// `Light: what` — or empty.
    pub(crate) title: Vec<RenderCell>,
    /// `what` alone, for a pane with no room for the whole title.
    pub(crate) short_title: Vec<RenderCell>,
    pub(crate) lights: Vec<RenderCell>,
    cols: Vec<(usize, Light)>,
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
            .observe_rows(session, rows, read_lights, Instant::now());
        if step.queued {
            crate::claude_footer::post_changed(session);
        }
        if step.refused {
            wake_after(session, REFUSAL_SHOWN);
        }
    }

    /// The light block to paint at the end of `session`'s footer row.
    pub(crate) fn claude_lights_block(
        &self,
        wid: WindowId,
        session: u64,
        blank: RenderCell,
    ) -> Option<Block> {
        let ws = self.windows.get(&wid)?;
        Some(ws.claude_lights.block(session, blank, Instant::now()))
    }

    /// Record where `block`'s lights landed: frame row `frame_row` (window
    /// grid row `term_row`), the LIGHTS starting at frame column `col`.
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
                .extend(block.cols.iter().map(|&(at, light)| LightHit {
                    frame_row,
                    term_row,
                    col: col + at,
                    session,
                    light,
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
        let hit = ws.claude_lights.light_at(row, col)?;
        let term_row = ws
            .claude_lights
            .hits
            .iter()
            .find(|h| h.frame_row == row && h.col == col)
            .map(|h| h.term_row)?;
        (!self.chrome_owns_terminal_row(wid, term_row)).then_some(hit)
    }

    /// The pointer left the window: no light is hovered any more.
    pub(crate) fn clear_claude_light_hover(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid)
            && ws.claude_lights.hover.take().is_some()
            && let Some(w) = ws.os_window.as_ref()
        {
            w.request_redraw();
        }
    }

    /// A mouse press anywhere lets a keyboard selection go: the person has
    /// moved on (a click to another pane and back must not leave a selected
    /// light waiting to swallow the next Return).
    pub(crate) fn clear_claude_light_selection(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid)
            && ws.claude_lights.selected.take().is_some()
            && let Some(w) = ws.os_window.as_ref()
        {
            w.request_redraw();
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
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
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
    /// on screen, or a slash command would land on a draft, on a turn in
    /// flight or in a driver's prompt. Decided from the ENGINE's screen as it
    /// is now, not from the last painted frame: a mode that changed since is
    /// the one the toggle starts from.
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
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
            wake_after(session, REFUSAL_SHOWN);
        };
        let reading = self.claude_screen_now(session);
        let driven = self.session_driven(session);
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
        let from = lights::states(Some(&screen))[index(light)];
        let Some(drive) = lights::drive(light, from) else {
            refuse(ws, Refusal::NotShown);
            return;
        };
        if matches!(drive, Drive::Command(_)) {
            if !empty {
                refuse(ws, Refusal::Draft);
                return;
            }
            // Whether Claude runs or queues a slash command typed mid-turn is
            // not measured; a light that flips a turn later would be wrong in
            // the meantime and invite the opposite click. Wait for the turn.
            if screen.busy {
                refuse(ws, Refusal::Busy);
                return;
            }
            // A driver's `turn` pastes and submits into this composer: a
            // command typed now could be sent with its text. A mode light
            // stays free — shift+tab never touches the prompt.
            if driven {
                refuse(ws, Refusal::Driven);
                return;
            }
            if ordering {
                // The empty screen can precede an earlier accepted paste's
                // echo. Do not add our command behind that unread draft.
                refuse(ws, Refusal::InputPending);
                return;
            }
        }
        // Don't ask is outside Claude's shift+tab cycle (it only ever leads on
        // to manual): a cycle from there could never come back to it, so the
        // person's mode could not be restored. Refuse before pressing.
        if matches!(drive, Drive::CycleMode { .. }) && screen.mode == Some(lights::Mode::DontAsk) {
            refuse(ws, Refusal::OutsideCycle);
            return;
        }
        // A mode light is only ever clicked while its pill is read (`from` is
        // Unknown otherwise), so the mode is there to start the cycle from.
        let Some(mode) = screen.mode.or(match &drive {
            Drive::Command(_) => Some(lights::Mode::Manual),
            Drive::CycleMode { .. } => None,
        }) else {
            refuse(ws, Refusal::NotShown);
            return;
        };
        let first = match &drive {
            Drive::CycleMode { .. } => {
                crate::input::InputEvent::KeySequence(lights::SHIFT_TAB.to_vec())
            }
            Drive::Command(cmd) => crate::input::InputEvent::Paste(
                (*cmd).to_owned(),
                crate::input::PasteFraming::AtDrain,
            ),
        };
        let settle = Duration::from_millis(lights::SETTLE_MS);
        let command = match &drive {
            Drive::Command(cmd) => Some(*cmd),
            Drive::CycleMode { .. } => None,
        };
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
        s.shown[index(light)] = from;
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
        });
        if let Some(w) = ws.os_window.as_ref() {
            w.request_redraw();
        }
        wake_after(session, settle);
        if let Some(cmd) = command
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
                    hands_off: false,
                    deadline: now + settle,
                    next_poll: Some(now + TYPED_POLL),
                    cleared: None,
                    late: false,
                },
            );
            wake_after(session, TYPED_POLL);
        }
    }

    /// Whether a driver holds `session`'s lease now: a `turn` in flight, or a
    /// `lease` not yet lapsed (`Lease::is_live`).
    fn session_driven(&self, session: u64) -> bool {
        self.pool.get(session).is_some_and(|entry| {
            entry
                .ctx
                .turn_lease
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
                .is_some_and(|lease| lease.is_live(crate::metrics::now_us()))
        })
    }

    /// `session`'s screen read straight from its engine ([`read_screen_now`],
    /// behind the reader's panic fence) — `None` without a composer on
    /// screen or on a reader panic. The engine's lock is held only to copy
    /// the rows out; the reader runs after it is dropped.
    fn claude_screen_now(&self, session: u64) -> Option<(Screen, bool)> {
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
        // Each row's column 2 drawn DIM: on the caret row, a placeholder
        // suggestion, not a draft — the `cell` verb's reading, which
        // `composer_is_empty` wants.
        let dim_col2: Vec<bool> = (0..t.rows())
            .map(|row| {
                t.grid()
                    .row_at_screen(row)
                    .and_then(|cells| cells.get(2))
                    .is_some_and(|cell| cell.flags().contains(aterm_core::grid::CellFlags::DIM))
            })
            .collect();
        drop(t);
        fenced_screen_now(session, &rows, || read_screen_now(&rows, cursor, &dim_col2))
    }

    /// The footer wake's lights half: give up on toggles past their
    /// deadline (and time the end of the refusal that says so), then send
    /// every queued step against the LIVE screen — a shift+tab only while the
    /// composer still shows the mode the press was decided on (a permission
    /// box that opened since the frame must not take a shift+tab as its
    /// answer), a Return only while the cursor row still holds the command it
    /// submits.
    pub(crate) fn drain_claude_lights(&mut self) {
        let now = Instant::now();
        let mut sends: Vec<(WindowId, u64, Out)> = Vec::new();
        for (wid, ws) in &mut self.windows {
            let (changed, refused) = ws.claude_lights.expire(now);
            if changed && let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
            for session in refused {
                wake_after(session, REFUSAL_SHOWN);
            }
            sends.extend(
                ws.claude_lights
                    .outbox
                    .drain(..)
                    .map(|(session, out)| (*wid, session, out)),
            );
        }
        for (wid, session, out) in sends {
            let Out::ShiftTab { expect } = out;
            // A step whose toggle ended since the frame queued it (expired
            // just above, resolved, or refused) is not sent.
            let live = self.windows.get(&wid).is_some_and(|ws| {
                ws.claude_lights
                    .sessions
                    .get(&session)
                    .is_some_and(|s| s.pending.is_some())
            });
            if !live {
                continue;
            }
            let still = self
                .claude_screen_now(session)
                .is_some_and(|(screen, _)| screen.mode == Some(expect));
            if !still {
                self.abandon_claude_light(wid, session, Refusal::Moved);
                continue;
            }
            let outcome = self.input_to_session(
                wid,
                crate::input::InputEvent::KeySequence(lights::SHIFT_TAB.to_vec()),
                crate::input::Source::Human,
                Some(session),
                crate::app_input::PressPhase::Initial,
            );
            if outcome != crate::input::InputOutcome::Ok {
                self.abandon_claude_light(wid, session, Refusal::InputRejected);
            }
        }
        self.drain_claude_typed(now);
    }

    /// The typed commands' half of the footer wake: for each slash command a
    /// light typed, read the session's composer from the ENGINE and decide —
    /// submit it (the composer holds exactly it, Claude is idle, and no other
    /// hand has come to the session since), wait, or at the deadline take it
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
                }
                continue;
            }
            let hands_off = t.hands_off || view.stamp != t.stamp || self.session_driven(session);
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
            if !ordering && view.holds && !view.busy {
                let accepted = self.send_typed(
                    session,
                    crate::input::InputEvent::KeySequence(b"\r".to_vec()),
                );
                self.claude_typed.remove(&session);
                if accepted.is_none() {
                    // Do not silently forget an unsubmitted command or retry
                    // later under a composer that another gesture may change.
                    self.leave_claude_typed(session, t.light, now);
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

    /// Send one of a typed command's keys to `session` through the input
    /// seam (as the click's input, like the paste), and return the person
    /// clock right after acceptance — aterm's own write, not a person's gesture.
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
            crate::input::Source::Human,
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
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
        }
        wake_after(session, REFUSAL_SHOWN);
    }

    /// `session`'s composer as the typed command `typed` sees it, read from the
    /// engine (the reader behind its panic fence; a panic reads as
    /// unreadable) — `None` when the session is gone.
    fn typed_view(&self, session: u64, typed: &Typed) -> Option<TypedView> {
        let entry = self.pool.get(session)?;
        let stamp = entry.ctx.human_input.last();
        let t = crate::term_lock(&entry.term);
        if t.grid().display_offset() > 0 {
            return Some(TypedView::unreadable(stamp));
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
        drop(t);
        let read = crate::reader_guard::read_or_none(
            &format!("{session}"),
            "Claude Code typed command",
            &rows,
            || {
                read_screen_now(&rows, cursor, &dim_col2).map(|(screen, empty)| {
                    let now = lights::states(Some(&screen))[index(typed.light)];
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
        if let Some(w) = ws.os_window.as_ref() {
            w.request_redraw();
        }
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
            && (!select_chord || ws.claude_lights.hits.is_empty())
        {
            return false;
        }
        let Some(session) = self.focused_session_id(wid) else {
            return false;
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        let painted = ws.claude_lights.hits.iter().any(|h| h.session == session);
        // A selection that is not the focused pane's, or whose lights are not
        // on the glass (a box covers the composer, the pane narrowed), is let
        // go rather than left armed to swallow a later Return.
        let stale = ws.claude_lights.selected.is_some_and(|(id, _)| {
            id != session || !ws.claude_lights.hits.iter().any(|h| h.session == id)
        });
        if stale {
            ws.claude_lights.selected = None;
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
        }
        // macOS only: everywhere else `ctrl+shift+tab` is aterm's default
        // `prev_tab` (`keybinding::PLATFORM_DEFAULT_PAIRS`), and this gate runs
        // before the keybindings. The lights stay clickable on every platform.
        let step = |from: Option<Light>, forward: bool| {
            let n = Light::ALL.len();
            let at = from.map_or(if forward { n - 1 } else { 0 }, index);
            Light::ALL[if forward {
                (at + 1) % n
            } else {
                (at + n - 1) % n
            }]
        };
        let selected = ws
            .claude_lights
            .selected
            .filter(|(id, _)| *id == session)
            .map(|(_, l)| l);
        let redraw = |ws: &crate::WindowState| {
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
        };
        if select_chord && painted {
            ws.claude_lights.selected = Some((session, step(selected, true)));
            redraw(ws);
            return true;
        }
        let Some(light) = selected else {
            return false;
        };
        let plain = !mods.control_key() && !mods.alt_key() && !mods.super_key();
        match &ev.logical_key {
            Key::Named(NamedKey::Enter | NamedKey::Space) if plain => {
                ws.claude_lights.selected = None;
                redraw(ws);
                self.toggle_claude_light(wid, session, light);
                true
            }
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowLeft) if plain => {
                let forward = matches!(ev.logical_key, Key::Named(NamedKey::ArrowRight));
                ws.claude_lights.selected = Some((session, step(Some(light), forward)));
                redraw(ws);
                true
            }
            Key::Named(NamedKey::Escape) => {
                ws.claude_lights.selected = None;
                redraw(ws);
                true
            }
            _ => {
                ws.claude_lights.selected = None;
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
            hover: Some((7, Light::AutoMode)),
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
            hover: Some((7, Light::AutoMode)),
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
        w.observe_rows(7, rows, read_lights, Instant::now())
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
        observe(&mut w, &screen(&rule, BYPASS, ""));
        start(
            &mut w,
            Light::AutoMode,
            Drive::CycleMode {
                until: Some(Mode::Auto),
            },
            Mode::Bypass,
            now,
        );
        let shown = w.sessions[&7].shown;
        let boom = |_: &[String]| -> Option<Screen> { panic!("stand-in reader panic") };
        let plan = screen(&rule, PLAN, "");
        assert_eq!(w.observe_rows(7, &plan, boom, now), Step::default());
        assert!(w.outbox.is_empty(), "no step off an unread screen");
        let s = &w.sessions[&7];
        assert_eq!(s.shown, shown, "the lights keep what they showed");
        let p = s.pending.as_ref().expect("the toggle is still in flight");
        assert_eq!((p.presses, p.mode_at_press), (1, Mode::Bypass));
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
        // Control: the real reader steps the cycle on the same screen.
        assert!(observe(&mut w, &plan).queued);
        assert_eq!(w.outbox, vec![(7, Out::ShiftTab { expect: Mode::Plan })]);
    }

    fn start(w: &mut WindowLights, light: Light, drive: Drive, mode: Mode, now: Instant) {
        let s = w.sessions.entry(7).or_default();
        s.pending = Some(Pending {
            light,
            from: s.shown[index(light)],
            drive,
            started: mode,
            mode_at_press: mode,
            seen: Vec::new(),
            presses: 1,
            deadline: now + Duration::from_millis(lights::SETTLE_MS),
        });
    }

    /// A mode cycle presses again only when Claude ANSWERED the last press,
    /// and stops at the mode it wanted.
    #[test]
    fn a_mode_cycle_presses_until_its_mode_shows() {
        let now = Instant::now();
        let rule = "\u{2500}".repeat(60);
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&rule, BYPASS, ""));
        start(
            &mut w,
            Light::AutoMode,
            Drive::CycleMode {
                until: Some(Mode::Auto),
            },
            Mode::Bypass,
            now,
        );
        assert!(
            !observe(&mut w, &screen(&rule, BYPASS, "")).queued,
            "no answer yet: no second press"
        );
        assert!(
            observe(&mut w, &screen(&rule, PLAN, "")).queued,
            "answered with plan: press again"
        );
        assert_eq!(w.outbox, vec![(7, Out::ShiftTab { expect: Mode::Plan })]);
        w.outbox.clear();
        assert!(!observe(&mut w, &screen(&rule, AUTO, "")).queued);
        assert!(w.sessions[&7].pending.is_none(), "auto reached");
        assert_eq!(w.sessions[&7].shown[index(Light::AutoMode)], LightState::On);
    }

    /// The owner's own cycle, measured on 2.1.283 without bypass: auto →
    /// manual → accept edits → plan → auto. Clicking auto-approve there must
    /// come back round to AUTO and stop — the mode the person had — saying
    /// the mode is not in this cycle, never pressing on into another mode.
    #[test]
    fn a_mode_the_cycle_lacks_is_refused_after_one_lap_back_where_it_began() {
        let now = Instant::now();
        let rule = "\u{2500}".repeat(60);
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&rule, AUTO, ""));
        start(
            &mut w,
            Light::AutoApprove,
            Drive::CycleMode {
                until: Some(Mode::Bypass),
            },
            Mode::Auto,
            now,
        );
        for (row, mode) in [
            (MANUAL, Mode::Manual),
            (ACCEPT, Mode::AcceptEdits),
            (PLAN, Mode::Plan),
        ] {
            let step = observe(&mut w, &screen(&rule, row, ""));
            assert!(step.queued && !step.refused, "{mode:?}: press on");
            assert_eq!(w.outbox.pop(), Some((7, Out::ShiftTab { expect: mode })));
        }
        let step = observe(&mut w, &screen(&rule, AUTO, ""));
        assert_eq!(
            step,
            Step {
                queued: false,
                refused: true
            },
            "a lap: stop, and time the refusal's end"
        );
        let s = &w.sessions[&7];
        assert!(s.pending.is_none());
        assert!(matches!(
            s.refused,
            Some((Light::AutoApprove, Refusal::NotInCycle, _))
        ));
        assert_eq!(
            s.shown[index(Light::AutoMode)],
            LightState::On,
            "auto, as before"
        );
    }

    /// A slash command's frames queue nothing — its Enter is judged from the
    /// engine by the loop (`App::drain_claude_typed`), never off a frame —
    /// and it is done when the tag moves.
    #[test]
    fn a_commands_frames_queue_nothing_and_its_tag_resolves_it() {
        let now = Instant::now();
        let bare = "\u{2500}".repeat(60);
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&bare, BYPASS, ""));
        start(
            &mut w,
            Light::Fast,
            Drive::Command("/fast on"),
            Mode::Bypass,
            now,
        );
        assert!(!observe(&mut w, &screen(&bare, BYPASS, "/fast on")).queued);
        assert!(w.outbox.is_empty());
        let tagged = format!("{} \u{21AF} \u{2500}", "\u{2500}".repeat(40));
        observe(&mut w, &screen(&tagged, BYPASS, ""));
        assert!(w.sessions[&7].pending.is_none());
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
            Drive::Command("/fast on"),
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
        let block = w.block(7, blank(), now);
        let text: String = block.title.iter().map(|c| c.ch).collect();
        assert!(
            text.starts_with("Fast mode: Claude did not switch"),
            "{text:?}"
        );
    }

    /// Three lights, one cell apart, each in its own hue when on; the title
    /// comes APART from the lights (only the lights must fit); a box over the
    /// composer keeps the last states rather than greying the row.
    #[test]
    fn the_block_is_three_lights_and_keeps_what_it_saw() {
        let now = Instant::now();
        // An effort tag (`workspace` stands in for the vendor's word) beside
        // fast mode's `↯`: the tag lights nothing, `↯` lights fast mode.
        let rule = format!("{} workspace \u{21AF} \u{2500}", "\u{2500}".repeat(40));
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&rule, BYPASS, ""));
        let block = w.block(7, blank(), now);
        assert!(block.title.is_empty(), "nothing asked for a title");
        let text: String = block.lights.iter().map(|c| c.ch).collect();
        assert_eq!(text, "\u{25CF} \u{25CB} \u{25CF} ");
        assert_eq!(
            block.cols.iter().map(|(at, _)| *at).collect::<Vec<_>>(),
            [0, 2, 4]
        );
        assert_eq!(block.lights[4].fg, Light::Fast.hue());
        w.observe(7, None, now);
        assert_eq!(
            w.block(7, blank(), now).lights,
            block.lights,
            "a covered composer keeps the last reading"
        );
        w.hover = Some((7, Light::Fast));
        let hovered = w.block(7, blank(), now);
        let title: String = hovered.title.iter().map(|c| c.ch).collect();
        assert_eq!(title, "Fast mode: on  ");
        assert_eq!(
            hovered.lights.len(),
            block.lights.len(),
            "the title is apart"
        );
        let fast = hovered
            .cols
            .iter()
            .find(|(_, l)| *l == Light::Fast)
            .unwrap()
            .0;
        assert!(hovered.lights[fast].bold, "the hovered light is marked");
    }

    /// Return toggles the SELECTED light, so the row marks and titles that
    /// one even while the pointer rests on another.
    #[test]
    fn the_keyboard_selection_outranks_a_resting_pointer() {
        let now = Instant::now();
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&"\u{2500}".repeat(60), BYPASS, ""));
        w.hover = Some((7, Light::AutoMode));
        w.selected = Some((7, Light::Fast));
        let block = w.block(7, blank(), now);
        let title: String = block.title.iter().map(|c| c.ch).collect();
        assert!(title.starts_with("Fast mode:"), "{title:?}");
        let at = |l: Light| block.cols.iter().find(|(_, x)| *x == l).unwrap().0;
        assert!(block.lights[at(Light::Fast)].bold);
        assert!(!block.lights[at(Light::AutoMode)].bold);
    }

    #[test]
    fn a_quiet_row_keys_as_zero() {
        let now = Instant::now();
        let mut w = WindowLights::default();
        assert_eq!(w.fingerprint(now), 0);
        w.hover = Some((7, Light::Fast));
        let a = w.fingerprint(now);
        w.hover = Some((7, Light::AutoMode));
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
                repo_read_denied: None,
                version: None,
            }),
        ));
    }

    fn feed(app: &App, session: u64, bytes: &[u8]) {
        let term = app.pool.get(session).expect("session").term.clone();
        term_lock(&term).process(bytes);
    }

    /// Claude Code's bottom block on the 24x80 engine: a transcript row, the
    /// composer between its two rules with an EMPTY draft and the cursor at
    /// its caret (row 21, column 2), and `mode` on the last row.
    fn draw_claude(app: &App, session: u64, mode: &str) {
        let rule = "\u{2500}".repeat(80);
        let screen = format!(
            "\x1b[2J\x1b[19;1H\u{25CF} done\x1b[21;1H{rule}\x1b[22;1H\u{276F} \
             \x1b[23;1H{rule}\x1b[24;1H{mode}\x1b[22;3H"
        );
        feed(app, session, screen.as_bytes());
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

    /// A CLICK ON THE AUTO-MODE LIGHT: exactly one shift+tab reaches the
    /// PTY; the light shows `◐` until Claude's screen answers; an answer
    /// that is not auto mode is pressed through from the WAKE (never from
    /// the frame); and the auto-mode pill resolves it. Run with the mouse
    /// untracked and tracked (SGR 1000/1006): a press the light takes is not
    /// a mouse report, and neither is its release. Controls: the gap between
    /// two lights is not a light, and a second click while one toggle is in
    /// flight types nothing.
    #[test]
    fn a_click_on_the_auto_mode_light_sends_one_shift_tab_and_reads_the_answer_back() {
        for tracking in [false, true] {
            let (mut app, wid, session, mut reader) = app();
            publish_facts(&app, session);
            draw_claude(&app, session, BYPASS);
            if tracking {
                feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
            }
            frame(&mut app, wid);
            let auto = hit(&app, wid, Light::AutoMode);
            let bypass = hit(&app, wid, Light::AutoApprove);
            let text = row_text(&app, wid, auto.0);
            assert!(
                text.contains("\u{25C6} Opus 5.5 xhigh"),
                "the footer is painted: {text:?}"
            );
            assert_eq!(cell_at(&app, wid, bypass).ch, '\u{25CF}', "{text:?}");
            assert_eq!(cell_at(&app, wid, auto).ch, '\u{25CB}', "{text:?}");
            assert!(pty(&mut reader, 0).is_empty(), "a frame types nothing");

            // Hover: the gap beside the light is not one; the light is.
            let (x, y) = px_of(&app, wid, (auto.0, auto.1 - 1));
            app.on_cursor_moved(wid, x, y);
            assert_eq!(app.windows[&wid].claude_lights.hover, None);
            let (x, y) = px_of(&app, wid, auto);
            app.on_cursor_moved(wid, x, y);
            assert_eq!(
                app.windows[&wid].claude_lights.hover,
                Some((session, Light::AutoMode))
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
                    (Light::AutoMode, LightState::Off, Mode::Bypass, 1)
                );
            }
            app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
            app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
            assert!(pty(&mut reader, 0).is_empty(), "one toggle in flight");

            // The glass says it is switching; no answer yet, no second press.
            frame(&mut app, wid);
            let auto = hit(&app, wid, Light::AutoMode);
            assert_eq!(cell_at(&app, wid, auto).ch, '\u{25D0}');
            let text = row_text(&app, wid, auto.0);
            assert!(text.contains("Auto mode: switching\u{2026}"), "{text:?}");
            assert_ne!(
                app.windows[&wid].claude_lights.fingerprint(Instant::now()),
                0
            );
            app.on_claude_footer_changed(session);
            assert!(pty(&mut reader, 0).is_empty(), "no answer, no second press");

            // Claude answers with plan mode: the frame QUEUES the next press,
            // and only the footer wake sends it.
            redraw_mode_row(&app, session, PLAN);
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

            // Claude answers with auto mode: resolved, lit, nothing more sent.
            redraw_mode_row(&app, session, AUTO);
            frame(&mut app, wid);
            app.on_claude_footer_changed(session);
            assert!(pty(&mut reader, 0).is_empty());
            let s = &app.windows[&wid].claude_lights.sessions[&session];
            assert!(s.pending.is_none() && s.refused.is_none(), "resolved");
            let auto = hit(&app, wid, Light::AutoMode);
            let bypass = hit(&app, wid, Light::AutoApprove);
            let cell = cell_at(&app, wid, auto);
            assert_eq!((cell.ch, cell.fg), ('\u{25CF}', Light::AutoMode.hue()));
            assert_eq!(cell_at(&app, wid, bypass).ch, '\u{25CB}');
            let text = row_text(&app, wid, auto.0);
            assert!(
                text.contains("Auto mode: on"),
                "the hover's title: {text:?}"
            );
        }
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

        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);

        set_mods(
            &mut app,
            wid,
            ModifiersState::CONTROL | ModifiersState::SHIFT,
        );
        tap(&mut app, wid, NamedKey::Tab, KeyCode::Tab, "\t");
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, Light::AutoApprove))
        );
        tap(&mut app, wid, NamedKey::Tab, KeyCode::Tab, "\t");
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, Light::AutoMode))
        );
        assert!(pty(&mut reader, 0).is_empty(), "the chord is the lights'");
        frame(&mut app, wid);
        let auto = hit(&app, wid, Light::AutoMode);
        let text = row_text(&app, wid, auto.0);
        assert!(text.contains("Auto mode: off"), "{text:?}");
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
        assert_eq!(cell_at(&app, wid, auto).ch, '\u{25D0}');

        redraw_mode_row(&app, session, AUTO);
        frame(&mut app, wid);
        app.on_claude_footer_changed(session);
        assert!(pty(&mut reader, 0).is_empty());
        assert!(
            app.windows[&wid].claude_lights.sessions[&session]
                .pending
                .is_none()
        );
        let auto = hit(&app, wid, Light::AutoMode);
        assert_eq!(cell_at(&app, wid, auto).ch, '\u{25CF}');
        let text = row_text(&app, wid, auto.0);
        assert!(
            !text.contains("Auto mode:"),
            "nothing asks for a title: {text:?}"
        );

        // Control: nothing selected, so Return is Claude's.
        tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
        assert_eq!(pty(&mut reader, 1), b"\r");
    }

    /// REGRESSION (review 2026-09-25): in the default 80-column pane a
    /// PENDING auto-approve toggle's title (`Auto-approve (bypass
    /// permissions): switching…  `, 47 cells) never pushes the lights off
    /// the row — only the lights must fit; the title comes along where it
    /// fits too, and beside three lights it does (the reason-only fallback is
    /// held by the don't-ask refusal below, whose title does not).
    #[test]
    fn a_pending_auto_approve_toggle_keeps_its_lights_in_an_80_column_pane() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, AUTO);
        frame(&mut app, wid);
        let approve = hit(&app, wid, Light::AutoApprove);
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
            text.contains("Auto-approve (bypass permissions): switching\u{2026}"),
            "room for the whole title beside three lights: {text:?}"
        );
    }

    /// Don't ask is outside Claude's shift+tab cycle — no cycle could come
    /// back to it — so a mode light clicked there presses NOTHING and says
    /// where the mode is set.
    #[test]
    fn a_mode_light_in_dont_ask_mode_presses_nothing() {
        const DONT_ASK: &str = "  \u{23F5}\u{23F5} don't ask on (shift+tab to cycle)";
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, DONT_ASK);
        frame(&mut app, wid);
        let (x, y) = px_of(&app, wid, hit(&app, wid, Light::AutoApprove));
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        assert!(pty(&mut reader, 0).is_empty(), "no shift+tab");
        let s = &app.windows[&wid].claude_lights.sessions[&session];
        assert!(s.pending.is_none());
        assert!(matches!(
            s.refused,
            Some((Light::AutoApprove, super::Refusal::OutsideCycle, _))
        ));
        // Its whole title does not fit beside the lights at 80 columns, so the
        // reason comes alone, the light marked beside it.
        frame(&mut app, wid);
        let approve = hit(&app, wid, Light::AutoApprove);
        let text = row_text(&app, wid, approve.0);
        assert!(
            text.contains("leave don't-ask in Claude first") && !text.contains("Auto-approve"),
            "no room for the whole title: the reason alone: {text:?}"
        );
    }

    /// REGRESSION (review 2026-09-25): a mode the session's cycle never
    /// reaches (bypass, in a session started without
    /// --dangerously-skip-permissions — the owner's own four-mode cycle,
    /// measured on 2.1.283) is searched for exactly ONE lap and the search
    /// stops back where it began, the person's mode restored, saying the mode
    /// is not in this session's cycle.
    #[test]
    fn a_mode_the_cycle_never_reaches_is_refused_after_one_lap_where_it_began() {
        const ACCEPT: &str = "  \u{23F5}\u{23F5} accept edits on (shift+tab to cycle)";
        const MANUAL: &str = "  \u{23F8} manual mode on \u{00B7} ? for shortcuts";
        let ring = [ACCEPT, PLAN, AUTO, MANUAL];
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, ACCEPT);
        frame(&mut app, wid);
        let (x, y) = px_of(&app, wid, hit(&app, wid, Light::AutoApprove));
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
            Some((Light::AutoApprove, super::Refusal::NotInCycle, _))
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
        let cols = usize::from(term_lock(&app.pool.get(session).unwrap().term).cols());
        publish_facts(&app, session);
        let rule = "\u{2500}".repeat(cols);
        feed(&app, session, format!(
            "\x1b[2J\x1b[21;1H{rule}\x1b[22;1H\u{276F} \x1b[23;1H{rule}\x1b[24;1H  \u{23F5}\u{23F5} bypass permissions on\x1b[22;3H"
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
            Some((session, Light::AutoApprove))
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
                pty(&mut reader, 8),
                b"/fast on",
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
            assert_eq!(pty(reader.as_mut().unwrap(), 8), b"/fast on");
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
                    .block(session, RenderCell::default(), Instant::now())
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
            assert_eq!(pty(&mut reader, 8), b"/fast on");
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
    /// it, goes once, and the light resolves when Claude's `↯` tag shows.
    #[test]
    fn a_fast_click_submits_the_command_once_the_composer_holds_it() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        click(&mut app, wid, Light::Fast);
        assert_eq!(pty(&mut reader, 8), b"/fast on", "the paste");
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
        let s = &app.windows[&wid].claude_lights.sessions[&session];
        assert!(s.pending.is_none(), "the tag resolved it");
        assert_eq!(s.shown[super::index(Light::Fast)], LightState::On);
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
        assert_eq!(pty(&mut reader, 8), b"/fast on");
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
        assert_eq!(pty(&mut reader, 8), b"/fast on");
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
            assert_eq!(pty(&mut reader, 8), b"/fast on");
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
        assert_eq!(pty(&mut reader, 8), b"/fast on");
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
        assert_eq!(pty(&mut reader, 8), b"/fast on");
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
        assert_eq!(pty(&mut reader, 8), b"/fast on");
        *app.pool
            .get(session)
            .expect("session")
            .ctx
            .turn_lease
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(crate::Lease::Turn {
            id: 7,
            driver: None,
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
        assert_eq!(pty(&mut reader, 8), b"/fast on");
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
        assert_eq!(pty(&mut reader, 8), b"/fast on");
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
        assert_eq!(pty(&mut reader, 8), b"/fast on");
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
        assert_eq!(pty(&mut reader, 8), b"/fast on");
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
        assert_eq!(pty(&mut reader, 8), b"/fast on");
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
        assert_eq!(pty(&mut reader, 8), b"/fast on");
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
            assert_eq!(pty(&mut reader, 8), b"/fast on");
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
        assert_eq!(pty(&mut reader, 8), b"/fast on");
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
        draw_claude(&app, session, BYPASS);
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
        click(&mut app, wid, Light::AutoMode);
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
            pty(&mut reader, 8),
            b"/fast on",
            "control: no lease, the paste"
        );
    }

    /// A step a frame queued is not sent once its toggle has ended: the next
    /// press of a cycle whose deadline passed before the wake stays unsent.
    #[test]
    fn a_queued_press_of_an_ended_toggle_is_not_sent() {
        let (mut app, wid, session, mut reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        click(&mut app, wid, Light::AutoMode);
        assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
        redraw_mode_row(&app, session, PLAN);
        frame(&mut app, wid);
        assert!(
            !app.windows[&wid].claude_lights.outbox.is_empty(),
            "the frame queued the next press"
        );
        let past = Instant::now() - Duration::from_millis(1);
        app.windows
            .get_mut(&wid)
            .and_then(|ws| ws.claude_lights.sessions.get_mut(&session))
            .and_then(|s| s.pending.as_mut())
            .expect("in flight")
            .deadline = past;
        app.on_claude_footer_changed(session);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "the ended toggle presses nothing"
        );
    }
}
