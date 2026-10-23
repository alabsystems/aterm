// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typing path for Claude Code footer fact slash commands.

use std::time::{Duration, Instant};

use aterm_agent::harness::footer::{self, TopAnswer};
use aterm_agent::harness::lights;
use aterm_primer::CLAUDE_TOP_EFFORT_KEY;

use crate::control::{GuardedInput, InputFence, ScreenGen};
use crate::input::{Delivery, InputEvent, Source};
use crate::session_timeline::ForegroundStamp;
use crate::{App, WindowId};

/// How often a press in flight reads the engine (the lights' mode poll).
pub(crate) const SLASH_POLL: Duration = Duration::from_millis(80);
/// Quiet duration the composer must hold the whole command before Enter.
pub(crate) const QUIET: Duration = Duration::from_millis(600);
/// How long a write may take to read back.
pub(crate) const ECHO: Duration = Duration::from_secs(3);
/// How long Claude may take to answer the Enter.
pub(crate) const ANSWER: Duration = Duration::from_secs(4);
/// How long a stop's reason stays in the title slot.
pub(crate) const REFUSAL_SHOWN: Duration = Duration::from_secs(4);
/// How long a press's done notice stays.
pub(crate) const NOTICE_SHOWN: Duration = Duration::from_secs(2);
const MENU_ROWS: usize = 8;

/// What aterm types into Claude Code for a fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Slash {
    /// Session-only toggle of the top-effort key (`/effort <key> on` or `off`).
    TopEffort(bool),
    /// `/model`: Claude's ModelPicker.
    ModelPicker,
    /// `/effort`: Claude's EffortSlider.
    EffortPicker,
    /// `/compact`: compact the conversation context.
    Compact,
}

impl Slash {
    /// What is typed, the key spelled by its one const.
    #[must_use]
    pub(crate) fn text(self) -> String {
        match self {
            Slash::TopEffort(on) => format!(
                "/effort {CLAUDE_TOP_EFFORT_KEY} {}",
                if on { "on" } else { "off" }
            ),
            Slash::ModelPicker => "/model".to_owned(),
            Slash::EffortPicker => "/effort".to_owned(),
            Slash::Compact => "/compact".to_owned(),
        }
    }

    const fn bare(self) -> bool {
        !matches!(self, Slash::TopEffort(_))
    }

    /// Its part of a menu row's stable key (`fact-menu/<fact>/type/<this>`).
    #[must_use]
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Slash::TopEffort(true) => "key-on",
            Slash::TopEffort(false) => "key-off",
            Slash::ModelPicker => "model",
            Slash::EffortPicker => "effort",
            Slash::Compact => "compact",
        }
    }

    /// What the title slot says while it runs.
    #[must_use]
    pub(crate) fn pending(self) -> String {
        match self {
            Slash::TopEffort(on) => format!(
                "turning {CLAUDE_TOP_EFFORT_KEY} {}\u{2026}",
                if on { "on" } else { "off" }
            ),
            Slash::ModelPicker => "opening /model\u{2026}".to_owned(),
            Slash::EffortPicker => "opening /effort\u{2026}".to_owned(),
            Slash::Compact => "running /compact\u{2026}".to_owned(),
        }
    }

    /// What the title slot says once it is done (none for a picker, which covers the composer
    /// itself).
    #[must_use]
    pub(crate) fn done(self) -> Option<String> {
        match self {
            Slash::TopEffort(on) => Some(format!(
                "\u{2713} {CLAUDE_TOP_EFFORT_KEY} {} (this session)",
                if on { "on" } else { "off" }
            )),
            Slash::ModelPicker | Slash::EffortPicker => None,
            Slash::Compact => Some("\u{2713} /compact sent".to_owned()),
        }
    }
}

/// Whether Claude Code `version` takes the top-effort key's session toggle.
#[must_use]
pub(crate) fn takes_key_toggle(version: &str) -> bool {
    aterm_phase::anchors::newer_than(version, "2.1.303")
}

/// The key as Claude writes it at a sentence's start (`<Key> needs …`), built from the one const.
#[must_use]
pub(crate) fn key_capitalised() -> String {
    let mut chars = CLAUDE_TOP_EFFORT_KEY.chars();
    chars.next().map_or_else(String::new, |first| {
        format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
    })
}

/// Why a press did not begin, or stopped short — each said in words ([`Self::says`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SlashRefusal {
    /// A press, or a mode return of the lights, is in flight for the session.
    InFlight,
    /// The session is halted (`fabric`).
    Halted,
    /// The session is parked for an update.
    Parked,
    /// Another driver's lease stands on the session.
    Driver,
    /// No composer to type into: a box, a picker, a scrolled view, the `!` shell box.
    Covered,
    /// A turn is live.
    Busy,
    /// The composer holds a draft.
    Draft,
    /// A survey is open.
    Survey,
    /// The composer is drawn but not taking text as Claude's own.
    NotReady,
    /// The probe `/` never read back: vim NORMAL mode takes keys as motions.
    Vim,
    /// The screen moved between the read and the write, twice, or Claude's frame was unfinished.
    Moved,
    /// The person typed since the press began.
    Person,
    /// The input seam took no write.
    InputRejected,
    /// What aterm typed stands in the composer, not run.
    LeftTyped,
    /// The composer took other text than aterm typed.
    Changed,
    /// Claude did not answer the Enter in time.
    NoAnswer,
    /// Claude answered that the key needs dynamic workflows.
    NeedsWorkflows,
    /// Claude answered that the key is not available on the model.
    Unavailable,
}

impl SlashRefusal {
    /// Refusal message for `text` left in the composer and optional `model`.
    #[must_use]
    pub(crate) fn says(self, text: &str, model: Option<&str>) -> String {
        let key = key_capitalised();
        let left = |why: &str| {
            if text.is_empty() {
                format!("{why}; nothing typed")
            } else {
                format!("{why}; {text} is left in the composer")
            }
        };
        match self {
            Self::InFlight => "aterm is already typing here".to_owned(),
            Self::Halted => left("this session is halted"),
            Self::Parked => left("this session is parked for an update"),
            Self::Driver => left("another driver holds this session"),
            Self::Covered => left("a dialog is open"),
            Self::Busy if text.is_empty() => {
                "Claude is working; click when the turn ends".to_owned()
            }
            Self::Busy => format!("Claude is working; {text} is left in the composer"),
            Self::Draft => left("the composer holds a draft"),
            Self::Survey => left("a survey is open"),
            Self::NotReady => left("the composer is not ready"),
            Self::Vim => {
                "the composer took no text (vim normal mode); nothing more sent".to_owned()
            }
            Self::Moved => "Claude was drawing; click again".to_owned(),
            Self::Person => "you typed; nothing more sent".to_owned(),
            Self::InputRejected => "input was not accepted; try again".to_owned(),
            Self::LeftTyped => format!("{text} is in the composer; press Enter to run it"),
            Self::Changed => "the composer changed; nothing more sent".to_owned(),
            Self::NoAnswer => format!("Claude did not answer {text}"),
            Self::NeedsWorkflows => format!("{key} needs dynamic workflows (see /config)"),
            Self::Unavailable => {
                format!("{key} isn't available on {}", model.unwrap_or("this model"))
            }
        }
    }
}

/// Where a press stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    /// The probe `/` is written; its echo is awaited.
    Probed,
    /// The whole command is written.
    Typed { quiet: Option<(Instant, u64)> },
    /// The Enter went; Claude's answer, or its picker, is awaited.
    Entered,
}

/// One press in flight ([`App::press_slash`]).
#[derive(Clone, Debug)]
pub(crate) struct SlashPress {
    pub(crate) wid: WindowId,
    pub(crate) slash: Slash,
    text: Box<str>,
    pub(crate) stage: Stage,
    /// The person's input count right after the press's first write.
    pub(crate) human_after: u64,
    /// The person's last gesture that reached the program as of the probe.
    pub(crate) reached_after: u64,
    /// Past this, the stage is given up.
    pub(crate) deadline: Instant,
    /// The one poll armed.
    next_poll: Option<Instant>,
    /// The process and the model it was pressed for (a refusal's latch).
    pub(crate) pgid: i32,
    /// The published foreground identity the click was admitted under.
    foreground: ForegroundStamp,
    model: Option<String>,
    /// How many writes it has made.
    pub(crate) writes: u8,
}

/// How a press ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlashEnd {
    Done,
    Stopped(SlashRefusal),
}

/// THE KEY LATCHED OFF: Claude refused it for this process and model, so nothing offers to turn it
/// on until the model moves (or the process).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeyLatch {
    pub(crate) pgid: i32,
    /// The footer's model when the refusal came (what a move is told by).
    pub(crate) model: Option<String>,
    /// The model's ID as Claude named it (`claude-sonnet-4-5`).
    pub(crate) named: Option<String>,
    pub(crate) answer: TopAnswer,
}

impl KeyLatch {
    /// Whether it stands for the process `pgid` running the footer's `model`.
    #[must_use]
    pub(crate) fn holds(&self, pgid: i32, model: Option<&str>) -> bool {
        self.pgid == pgid && self.model.as_deref() == model
    }
}

/// One read of a session's engine, under one hold of its lock.
#[derive(Clone, Debug)]
pub(crate) struct EngineRead {
    pub(crate) rows: Vec<String>,
    pub(crate) cursor: (usize, usize),
    dim: Vec<bool>,
    pub(crate) generation: ScreenGen,
    human: u64,
}

impl EngineRead {
    fn dim_at(&self, row: usize) -> bool {
        self.dim.get(row).copied().unwrap_or(false)
    }
}

/// The composer as a press reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Composer {
    top: usize,
    bottom: usize,
    caret: usize,
    /// Its text, its rows joined and every run of blanks one blank.
    text: String,
    /// Empty by the upgrade's rule (a dim suggestion is none).
    empty: bool,
    /// The cursor stands inside its text: typed, not suggested.
    typed: bool,
}

#[cfg(test)]
impl Composer {
    /// Its text and whether the cursor proves it typed (the conformance projection's read, a unix
    /// test's).
    #[cfg(unix)]
    pub(crate) fn text_and_typed(&self) -> (&str, bool) {
        (&self.text, self.typed)
    }
}

fn squashed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The composer on `read`, where its frame is drawn and its caret row lies strictly between its
/// rules.
#[must_use]
pub(crate) fn composer_of(read: &EngineRead) -> Option<Composer> {
    let rows = &read.rows;
    let (top, bottom) = aterm_phase::phase::composer_rules(rows)?;
    let (caret, lines) = aterm_phase::phase::composer_draft(rows)?;
    if !(top < caret && caret < bottom && rows[caret].starts_with('\u{276F}')) {
        return None;
    }
    let empty = aterm_agent::harness::upgrade::composer_is_empty(
        rows,
        Some(read.cursor),
        read.dim_at(caret),
    );
    let (row, col) = read.cursor;
    let typed = (caret..bottom).contains(&row) && !(row == caret && col == 2);
    Some(Composer {
        top,
        bottom,
        caret,
        text: squashed(&lines.join(" ")),
        empty,
        typed,
    })
}

/// Slash menu drawn over `top` or under `bottom` as a command is typed.
#[must_use]
pub(crate) fn slash_menu(rows: &[String], top: usize, bottom: usize) -> Option<Option<String>> {
    let over = rows[top.saturating_sub(MENU_ROWS)..top].iter().rev();
    let under = rows.get(bottom + 1..).unwrap_or(&[]).iter().take(MENU_ROWS);
    menu_block(over).or_else(|| menu_block(under))
}

fn menu_block<'a>(rows: impl Iterator<Item = &'a String>) -> Option<Option<String>> {
    let mut entries = false;
    let mut pointers = 0;
    let mut pointed = None;
    for row in rows {
        let text = row.trim_start();
        if text.is_empty() || row.len() == text.len() {
            break;
        }
        let word = |t: &str| t.split_whitespace().next().map(str::to_owned);
        if let Some(rest) = text.strip_prefix('\u{276F}') {
            let rest = rest.trim_start();
            if rest.starts_with('/') {
                entries = true;
                pointers += 1;
                pointed = word(rest);
            }
        } else if text.starts_with('/') {
            entries = true;
        }
    }
    entries.then(|| pointed.filter(|_| pointers == 1))
}

fn view_of(read: &EngineRead, c: &Composer) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::hash::DefaultHasher::new();
    read.rows[c.top.saturating_sub(MENU_ROWS)..=c.bottom].hash(&mut h);
    slash_menu(&read.rows, c.top, c.bottom).hash(&mut h);
    read.cursor.hash(&mut h);
    h.finish()
}

fn live_turn(screen: Option<&lights::Screen>, rows: &[String]) -> bool {
    screen.is_some_and(|s| s.busy && !aterm_phase::phase::background_only(rows))
}

/// THE CLICK'S CHECKS ON THE ENGINE, before the probe `/` is written.
pub(crate) fn ready(read: &EngineRead) -> Result<usize, SlashRefusal> {
    let rows = &read.rows;
    let screen = lights::read_screen(rows).ok_or(SlashRefusal::Covered)?;
    if screen.mode.is_none() || screen.vim_insert {
        return Err(SlashRefusal::NotReady);
    }
    if live_turn(Some(&screen), rows) {
        return Err(SlashRefusal::Busy);
    }
    let c = composer_of(read).ok_or(SlashRefusal::NotReady)?;
    if !c.empty {
        return Err(SlashRefusal::Draft);
    }
    if aterm_phase::survey_block(rows).is_some() {
        return Err(SlashRefusal::Survey);
    }
    if read.cursor.0 != c.caret {
        return Err(SlashRefusal::NotReady);
    }
    Ok(c.caret)
}

/// What a press in flight does next, decided from one engine read ([`decide`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Nothing yet.
    Wait,
    /// The composer's view moved: the quiet window starts again here.
    Quiet(u64),
    /// The probe read back: write the rest, fenced at the cursor's row.
    Rest { row: usize },
    /// Quiet with the whole command: Enter, fenced at the cursor's row.
    Enter { row: usize },
    /// Claude answered as asked, or its picker is up.
    Done,
    /// It stops, named; a refusal Claude answered latches the key.
    Stop {
        why: SlashRefusal,
        left: String,
        latch: Option<(TopAnswer, Option<String>)>,
    },
}

/// THE WAKE'S DECISION for `press` from the engine as `read` shows it at `now`.
#[must_use]
pub(crate) fn decide(press: &SlashPress, read: Option<&EngineRead>, now: Instant) -> Step {
    let late = now >= press.deadline;
    let stop = |why: SlashRefusal, left: &str| Step::Stop {
        why,
        left: left.to_owned(),
        latch: None,
    };
    let Some(read) = read else {
        return match press.stage {
            _ if !late => Step::Wait,
            Stage::Probed => stop(SlashRefusal::Covered, "/"),
            _ => stop(SlashRefusal::NoAnswer, &press.text),
        };
    };
    let screen = lights::read_screen(&read.rows);
    let composer = screen.as_ref().and_then(|_| composer_of(read));
    let busy = live_turn(screen.as_ref(), &read.rows);
    let survey = aterm_phase::survey_block(&read.rows).is_some();
    match press.stage {
        Stage::Probed => match composer {
            Some(c) if c.typed && c.text == "/" && (busy || survey) => {
                if late {
                    let why = if busy {
                        SlashRefusal::Busy
                    } else {
                        SlashRefusal::Survey
                    };
                    stop(why, "/")
                } else {
                    Step::Wait
                }
            }
            Some(c) if c.typed && c.text == "/" => Step::Rest { row: read.cursor.0 },
            Some(c) if c.empty => {
                if late {
                    stop(SlashRefusal::Vim, "")
                } else {
                    Step::Wait
                }
            }
            Some(_) => stop(SlashRefusal::Changed, ""),
            None if late => stop(SlashRefusal::Covered, "/"),
            None => Step::Wait,
        },
        Stage::Typed { quiet } => {
            let Some(c) = composer else {
                return stop(SlashRefusal::LeftTyped, &press.text);
            };
            if c.empty {
                return if late {
                    stop(SlashRefusal::Changed, "")
                } else {
                    Step::Wait
                };
            }
            if !(c.typed && c.text == *press.text) {
                // The rest's echo still on its way: the text so far is the
                // command's own head.
                if c.typed && press.text.starts_with(c.text.as_str()) && !late {
                    return Step::Wait;
                }
                return stop(SlashRefusal::LeftTyped, &c.text);
            }
            let menu = slash_menu(&read.rows, c.top, c.bottom);
            let menu_ok = match menu {
                None => true,
                Some(pointed) => press.slash.bare() && pointed.as_deref() == Some(&*press.text),
            };
            if late {
                return stop(SlashRefusal::LeftTyped, &press.text);
            }
            let view = view_of(read, &c);
            match quiet {
                Some((since, seen)) if seen == view => {
                    if menu_ok && !busy && !survey && now.duration_since(since) >= QUIET {
                        Step::Enter { row: read.cursor.0 }
                    } else {
                        Step::Wait
                    }
                }
                _ => Step::Quiet(view),
            }
        }
        Stage::Entered => {
            let held = composer
                .as_ref()
                .is_some_and(|c| c.typed && c.text == *press.text);
            let answered = match press.slash {
                Slash::TopEffort(want) if !held => composer.as_ref().and_then(|c| {
                    let tag = footer::top_rule_tag(&read.rows[c.top]).is_some();
                    if tag == want {
                        return Some(Step::Done);
                    }
                    let reply = footer::top_effort_reply(&read.rows, c.top)?;
                    match reply.answer {
                        TopAnswer::On if want => Some(Step::Done),
                        TopAnswer::Off if !want => Some(Step::Done),
                        TopAnswer::NeedsWorkflows => Some(Step::Stop {
                            why: SlashRefusal::NeedsWorkflows,
                            left: String::new(),
                            latch: Some((reply.answer, None)),
                        }),
                        TopAnswer::Unavailable => Some(Step::Stop {
                            why: SlashRefusal::Unavailable,
                            left: String::new(),
                            latch: Some((reply.answer, reply.model)),
                        }),
                        _ => None,
                    }
                }),
                Slash::ModelPicker | Slash::EffortPicker if screen.is_none() => Some(Step::Done),
                Slash::Compact if !held => Some(Step::Done),
                _ => None,
            };
            match answered {
                Some(step) => step,
                None if late && held => stop(SlashRefusal::LeftTyped, &press.text),
                None if late => stop(SlashRefusal::NoAnswer, &press.text),
                None => Step::Wait,
            }
        }
    }
}

fn slash_holds(ctx: &crate::SessionCtx) -> Option<SlashRefusal> {
    if crate::fabric::halt_refusal(ctx, "send").is_some() {
        return Some(SlashRefusal::Halted);
    }
    if ctx.update_parked.is_parked() {
        return Some(SlashRefusal::Parked);
    }
    let driven = crate::harness_host::driver_holds(
        ctx.turn_lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref(),
        crate::metrics::now_us(),
        &crate::harness_host::holder(),
    );
    driven.then_some(SlashRefusal::Driver)
}

impl App {
    /// `session`'s engine read once ([`EngineRead`]).
    pub(crate) fn slash_engine_read(&self, session: u64) -> Option<EngineRead> {
        self.slash_engine_look(session).ok()
    }

    fn slash_engine_look(&self, session: u64) -> Result<EngineRead, SlashRefusal> {
        let entry = self.pool.get(session).ok_or(SlashRefusal::Covered)?;
        let human = entry.ctx.human_input.seq();
        let t = crate::term_lock(&entry.term);
        if t.grid().display_offset() > 0 {
            return Err(SlashRefusal::Covered);
        }
        if crate::frame_unfinished_now(&t) {
            return Err(SlashRefusal::Moved);
        }
        let rows: Vec<String> = (0..usize::from(t.rows()))
            .map(|r| crate::claude_lights::row_text(&t, r))
            .collect();
        let cursor = t.cursor();
        let dim: Vec<bool> = (0..t.rows())
            .map(|row| {
                t.grid()
                    .row_at_screen(row)
                    .and_then(|cells| cells.get(2))
                    .is_some_and(|cell| cell.flags().contains(aterm_core::grid::CellFlags::DIM))
            })
            .collect();
        Ok(EngineRead {
            rows,
            cursor: (usize::from(cursor.row), usize::from(cursor.col)),
            dim,
            generation: crate::control::screen_gen(&t),
            human,
        })
    }

    /// Whether `session`'s engine shows the top-effort key's tag on its composer's top rule now.
    #[must_use]
    pub(crate) fn engine_top_tag(&self, session: u64) -> Option<bool> {
        let read = self.slash_engine_read(session)?;
        crate::reader_guard::read_or_none(session, "Claude Code top rule", &read.rows, || {
            let (top, _) = aterm_phase::phase::composer_rules(&read.rows)?;
            Some(footer::top_rule_tag(&read.rows[top]).is_some())
        })
        .flatten()
    }

    /// Whether a slash press is in flight for `session`.
    #[must_use]
    pub(crate) fn slash_in_flight(&self, session: u64) -> bool {
        self.claude_facts
            .get(&session)
            .is_some_and(|f| f.press.is_some())
    }

    fn slash_write(
        &self,
        session: u64,
        read: &EngineRead,
        row: usize,
        human: u64,
        foreground: &ForegroundStamp,
        ev: InputEvent,
    ) -> GuardedInput {
        let Some(entry) = self.pool.get(session) else {
            return GuardedInput::Pressed(Delivery::Failed);
        };
        let pattern = aterm_agent::supervise::policy::row_guard(&read.rows[row]);
        let Ok(Some(guard)) = crate::control::compile_guard(Some(&pattern)) else {
            return GuardedInput::Skipped;
        };
        #[cfg(test)]
        BETWEEN_READ_AND_WRITE.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook();
            }
        });
        crate::control::input_if_fenced_observed(
            &entry.term,
            &entry.ctx,
            ev,
            &InputFence {
                generation: Some(read.generation),
                human: Some(human),
                foreground: Some(foreground),
                guard: Some(&*guard),
                guard_at_cursor: true,
                ..InputFence::default()
            },
            crate::ReadOn::EventLoop,
            None,
        )
    }

    /// THE CLICK'S HALF: begin typing `slash` into `session` for a gesture in window `wid`.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_slash_press",
            action = "Click",
            project = "claude_lights::slash_tests::project_slash_press"
        )
    )]
    pub(crate) fn press_slash(
        &mut self,
        wid: WindowId,
        session: u64,
        slash: Slash,
    ) -> Result<(), SlashRefusal> {
        let result = self.begin_slash(wid, session, slash);
        if let Err(why) = result {
            // Said where the person looks; a press already in flight keeps
            // its own words.
            if why != SlashRefusal::InFlight {
                self.note_fact_said(wid, session, why.says("", None), REFUSAL_SHOWN);
            }
        }
        result
    }

    fn begin_slash(
        &mut self,
        wid: WindowId,
        session: u64,
        slash: Slash,
    ) -> Result<(), SlashRefusal> {
        let now = Instant::now();
        let Some(entry) = self.pool.get(session) else {
            return Err(SlashRefusal::NotReady);
        };
        let ctx = entry.ctx.clone();
        let (claude, pgid, model, foreground) = {
            let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            (
                crate::harness_host::agent_of(tl.agent()) == Some(aterm_phase::Program::Claude),
                tl.agent().program_pgid,
                tl.claude_footer().and_then(|f| f.model.clone()),
                tl.foreground_stamp(),
            )
        };
        if !claude {
            return Err(SlashRefusal::NotReady);
        }
        let Some(foreground) = foreground else {
            return Err(SlashRefusal::NotReady);
        };
        if self.slash_in_flight(session) || self.mode_return_in_flight(session) {
            return Err(SlashRefusal::InFlight);
        }
        if let Some(why) = slash_holds(&ctx) {
            return Err(why);
        }
        let probe = InputEvent::KeySequence(b"/".to_vec());
        for attempt in 0..2 {
            let read = self.slash_engine_look(session)?;
            let caret = crate::reader_guard::read_or_none(
                session,
                "Claude Code live screen",
                &read.rows,
                || ready(&read),
            )
            .unwrap_or(Err(SlashRefusal::NotReady))?;
            match self.slash_write(
                session,
                &read,
                caret,
                read.human,
                &foreground,
                probe.clone(),
            ) {
                GuardedInput::Changed if attempt == 0 => continue,
                GuardedInput::Changed => return Err(SlashRefusal::Moved),
                GuardedInput::Person => return Err(SlashRefusal::Person),
                GuardedInput::Skipped => return Err(SlashRefusal::NotReady),
                GuardedInput::Pressed(Delivery::Full) => {}
                GuardedInput::Pressed(Delivery::PartialInDoubt { accepted }) => {
                    aterm_log::warn!(
                        "session {session}: a footer press's probe was written in part \
                         (accepted={accepted})"
                    );
                    return Err(SlashRefusal::InputRejected);
                }
                GuardedInput::Pressed(Delivery::BusyZero | Delivery::Failed) => {
                    return Err(SlashRefusal::InputRejected);
                }
            }
            self.note_person_input(session, &probe, Source::Human);
            let counts = ctx.human_input.counts();
            self.claude_facts.entry(session).or_default().press = Some(SlashPress {
                wid,
                slash,
                text: slash.text().into(),
                stage: Stage::Probed,
                human_after: counts.seq,
                reached_after: counts.reached,
                deadline: now + ECHO,
                next_poll: Some(now + SLASH_POLL),
                pgid,
                foreground,
                model,
                writes: 1,
            });
            self.claude_facts.entry(session).or_default().ended = None;
            self.set_fact_said(wid, session, slash.pending(), ECHO + QUIET + ECHO + ANSWER);
            crate::claude_lights::wake_after(session, SLASH_POLL);
            return Ok(());
        }
        Err(SlashRefusal::Moved)
    }

    /// THE WAKE'S HALF (the rest of the command, the Enter, Claude's answer).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_slash_press",
            action = "Type",
            project = "claude_lights::slash_tests::project_slash_press"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_slash_press",
            action = "Enter",
            project = "claude_lights::slash_tests::project_slash_press"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_slash_press",
            action = "Done",
            project = "claude_lights::slash_tests::project_slash_press"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_slash_press",
            action = "Stop",
            project = "claude_lights::slash_tests::project_slash_press"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "claude_slash_press",
            action = "Rest",
            project = "claude_lights::slash_tests::project_slash_press"
        )
    )]
    pub(crate) fn advance_slash_presses(&mut self, now: Instant) {
        for facts in self.claude_facts.values_mut() {
            if facts
                .ended
                .as_ref()
                .is_some_and(|(_, _, until)| *until <= now)
            {
                facts.ended = None;
            }
        }
        let sessions: Vec<u64> = self
            .claude_facts
            .iter()
            .filter(|(_, f)| f.press.is_some())
            .map(|(session, _)| *session)
            .collect();
        for session in sessions {
            self.advance_slash_press(session, now);
        }
    }

    fn advance_slash_press(&mut self, session: u64, now: Instant) {
        let Some(press) = self
            .claude_facts
            .get(&session)
            .and_then(|f| f.press.clone())
        else {
            return;
        };
        let Some(entry) = self.pool.get(session) else {
            if let Some(f) = self.claude_facts.get_mut(&session) {
                f.press = None;
            }
            return;
        };
        if entry
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .foreground_stamp()
            != Some(press.foreground)
        {
            self.end_slash(session, SlashEnd::Stopped(SlashRefusal::Changed), "", None);
            return;
        }
        let counts = entry.ctx.human_input.counts();
        let holds = (press.stage != Stage::Entered)
            .then(|| slash_holds(&entry.ctx))
            .flatten();
        if counts.reached > press.reached_after {
            self.end_slash(session, SlashEnd::Stopped(SlashRefusal::Person), "", None);
            return;
        }
        if let Some(why) = holds {
            let left = if press.stage == Stage::Probed {
                "/".to_owned()
            } else {
                press.text.to_string()
            };
            self.end_slash(session, SlashEnd::Stopped(why), &left, None);
            return;
        }
        let mut press = press;
        if counts.seq != press.human_after {
            press.human_after = counts.seq;
            self.with_press(session, |p| p.human_after = counts.seq);
        }
        let read = self.slash_engine_read(session);
        let step = read
            .as_ref()
            .and_then(|r| {
                crate::reader_guard::read_or_none(
                    session,
                    "Claude Code live screen",
                    &r.rows,
                    || decide(&press, Some(r), now),
                )
            })
            .unwrap_or_else(|| decide(&press, None, now));
        match step {
            Step::Wait => {}
            Step::Quiet(view) => {
                self.with_press(session, |p| {
                    p.stage = Stage::Typed {
                        quiet: Some((now, view)),
                    }
                });
            }
            Step::Rest { row } => {
                let Some(read) = read else { return };
                let rest = press.text.get(1..).unwrap_or("").as_bytes().to_vec();
                match self.slash_write(
                    session,
                    &read,
                    row,
                    press.human_after,
                    &press.foreground,
                    InputEvent::KeySequence(rest),
                ) {
                    GuardedInput::Pressed(Delivery::Full) => self.with_press(session, |p| {
                        p.stage = Stage::Typed { quiet: None };
                        p.deadline = now + ECHO;
                        p.writes += 1;
                    }),
                    GuardedInput::Changed | GuardedInput::Skipped | GuardedInput::Person => {}
                    GuardedInput::Pressed(_) => {
                        self.end_slash(
                            session,
                            SlashEnd::Stopped(SlashRefusal::InputRejected),
                            "",
                            None,
                        );
                        return;
                    }
                }
            }
            Step::Enter { row } => {
                let Some(read) = read else { return };
                let Some(enter) = crate::control::parse_key("enter") else {
                    return;
                };
                match self.slash_write(
                    session,
                    &read,
                    row,
                    press.human_after,
                    &press.foreground,
                    enter,
                ) {
                    GuardedInput::Pressed(Delivery::Full) => {
                        self.with_press(session, |p| {
                            p.stage = Stage::Entered;
                            p.deadline = now + ANSWER;
                            p.writes += 1;
                        });
                        self.request_footer_now(session, press.pgid);
                    }
                    GuardedInput::Changed | GuardedInput::Skipped | GuardedInput::Person => {
                        self.with_press(session, |p| p.stage = Stage::Typed { quiet: None });
                    }
                    GuardedInput::Pressed(_) => {
                        self.end_slash(
                            session,
                            SlashEnd::Stopped(SlashRefusal::LeftTyped),
                            &press.text,
                            None,
                        );
                        return;
                    }
                }
            }
            Step::Done => {
                self.end_slash(session, SlashEnd::Done, "", None);
                return;
            }
            Step::Stop { why, left, latch } => {
                self.end_slash(session, SlashEnd::Stopped(why), &left, latch);
                return;
            }
        }
        let due = self
            .claude_facts
            .get_mut(&session)
            .and_then(|f| f.press.as_mut())
            .is_some_and(|p| {
                let due = p.next_poll.is_none_or(|at| now >= at);
                if due {
                    p.next_poll = Some(now + SLASH_POLL);
                }
                due
            });
        if due {
            crate::claude_lights::wake_after(session, SLASH_POLL);
        }
    }

    fn with_press(&mut self, session: u64, f: impl FnOnce(&mut SlashPress)) {
        if let Some(p) = self
            .claude_facts
            .get_mut(&session)
            .and_then(|facts| facts.press.as_mut())
        {
            f(p);
        }
    }

    fn end_slash(
        &mut self,
        session: u64,
        end: SlashEnd,
        left: &str,
        latch: Option<(TopAnswer, Option<String>)>,
    ) {
        let now = Instant::now();
        let Some(press) = self
            .claude_facts
            .get_mut(&session)
            .and_then(|f| f.press.take())
        else {
            return;
        };
        let named = latch.as_ref().and_then(|(_, named)| named.clone());
        if let Some((answer, named)) = latch {
            self.claude_facts.entry(session).or_default().latch = Some(KeyLatch {
                pgid: press.pgid,
                model: press.model.clone(),
                named,
                answer,
            });
        }
        let shown = match end {
            SlashEnd::Done => NOTICE_SHOWN,
            SlashEnd::Stopped(_) => REFUSAL_SHOWN,
        };
        self.claude_facts.entry(session).or_default().ended = Some((press.slash, end, now + shown));
        let words = match end {
            SlashEnd::Done => press.slash.done(),
            SlashEnd::Stopped(why) => Some(why.says(left, named.as_deref())),
        };
        let woken = match words {
            Some(words) => self.note_fact_said(press.wid, session, words, shown),
            None => {
                self.clear_fact_said(press.wid, session);
                false
            }
        };
        if end == SlashEnd::Done {
            // The key turned on here exists for this process: its ghost may
            // stand in for Claude's tag once it is off again.
            if press.slash == Slash::TopEffort(true) {
                self.claude_facts.entry(session).or_default().top_seen = Some(press.pgid);
            }
            self.request_footer_now(session, press.pgid);
        }
        if !woken {
            crate::claude_lights::wake_after(session, shown);
        }
        self.refresh_fact_readings(session);
        self.refresh_fact_menu(session);
        self.redraw_claude_footer(session);
    }

    fn request_footer_now(&self, session: u64, pgid: i32) {
        #[cfg(test)]
        {
            let _ = (session, pgid);
            FOOTER_ASKED.with(|asked| asked.set(asked.get() + 1));
        }
        #[cfg(not(test))]
        if let Some(entry) = self.pool.get(session) {
            crate::claude_footer::request_footer(
                session,
                &entry.ctx.timeline,
                pgid,
                &self.footer_log_wake(),
            );
        }
    }
}

/// `claude_slash_press` environment actions, each waived where the Tier-1
/// test (`claude_lights::slash_tests`) drives it on the real engine.
#[cfg(all(any(test, debug_assertions), unix))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_slash_press",
    action = "Echo",
    reason = "Claude Code's own step: its composer draws what was typed. The Tier-1 test feeds \
              the echo to the real engine and projects the App state."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_slash_press",
    action = "Settle",
    reason = "The clock: the composer's view holds still for the quiet window. The Tier-1 test \
              advances the press past it."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_slash_press",
    action = "PersonTypes",
    reason = "The person's own key into the composer: the Tier-1 test counts one on the real \
              session's person clock and feeds its echo."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_slash_press",
    action = "PersonMoves",
    reason = "The person's own key that leaves the composer's text: the Tier-1 test counts one \
              on the real session's person clock."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_slash_press",
    action = "Clears",
    reason = "The person clears the composer in Claude Code: aterm never erases it."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_slash_press",
    action = "VimNormal",
    reason = "The person's editor mode in Claude Code's settings: the Tier-1 test draws the \
              measured vim NORMAL composer, which echoes nothing."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_slash_press",
    action = "BoxOpens",
    reason = "Claude Code's own step: a box or a picker over the composer. The Tier-1 tests \
              draw them on the real engine and check nothing is typed into one."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_slash_press",
    action = "BoxCloses",
    reason = "Claude Code's own step: the box answered and the composer back."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_slash_press",
    action = "TurnStarts",
    reason = "Claude Code's own step: a turn starts. The Tier-1 test draws a live turn on the \
              real engine."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "claude_slash_press",
    action = "TurnEnds",
    reason = "Claude Code's own step: the turn ends."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn slash_press_environment_waivers() {}

#[cfg(test)]
thread_local! {
    /// What a test does between the click's read of the engine and its write.
    pub(crate) static BETWEEN_READ_AND_WRITE: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
    /// The footer reads a press asked for, in a test build.
    pub(crate) static FOOTER_ASKED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}
