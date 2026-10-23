// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keeping-rule sweep tests (R1–R4) over Settings ▸ Themes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use super::*;

/// A window's size, in points.
type Size = (u16, u16);

/// The windows the sweep draws, phone width to desktop.
const WINDOWS: [Size; 11] = [
    (178, 348),
    (300, 160),
    (444, 180),
    (444, 292),
    (444, 420),
    (584, 348),
    (864, 292),
    (864, 500),
    (1_095, 700),
    (1_144, 700),
    (1_284, 714),
];

const FILTER: &str = "settings/showcase/filter";
const PACK: &str = "settings/showcase/pack";
const CUSTOMIZE: &str = "settings/showcase/customize";
const DONE: &str = "settings/showcase/customize-done";
const SIDE: &str = "settings/showcase/side/dark";
const LIST: &str = "settings/showcase/list";
const SHEET_LIST: &str = "settings/showcase/sheet-list";
/// The reload warning and a status, each as long as a real one gets.
const WARNING: &str = "aterm.toml reload rejected: repair the invalid authored value before \
                       automatic reload can resume.";
const REFUSED: &str = "/Volumes/Shared Drive/Users//someone/dotfiles/config/aterm \
                       isn\u{2019}t writable \u{b7} check its permissions";

/// What the keyboard is on when a walk's first frame is drawn, and how it got there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Keyboard {
    /// Off the list: the list lands on the theme in use alone.
    Off,
    /// The chooser's filter, put there before the page's first frame.
    Filter,
    /// The Community disclosure, pressed open before the first frame.
    Pack,
    /// The Customize sheet's Done, Customize pressed before the first frame.
    SheetFirst,
    /// The sheet's Done, Customize pressed on the drawn page.
    SheetDrawn,
    /// Customize, the sheet opened on the drawn page and closed with Done.
    SheetClosed,
    /// The Dark | Light segment's Dark, pressed on the drawn page.
    Side,
}

impl Keyboard {
    fn control(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Filter => Some(FILTER),
            Self::Pack => Some(PACK),
            Self::SheetFirst | Self::SheetDrawn => Some(DONE),
            Self::SheetClosed => Some(CUSTOMIZE),
            Self::Side => Some(SIDE),
        }
    }

    /// Whether a press on the drawn page put the keyboard there.
    fn pressed_on_drawn(self) -> bool {
        matches!(self, Self::SheetDrawn | Self::SheetClosed | Self::Side)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Scenario {
    keyboard: Keyboard,
    /// aterm.toml's theme.
    theme: &'static str,
    /// The theme in use.
    in_use: &'static str,
}

impl std::fmt::Display for Scenario {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "{:?} on {}", self.keyboard, self.theme)
    }
}

/// Which walks a sweep takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Walks {
    /// Every walk from every start ([`walks_from`]), and random walks seeded with this.
    Every(u64),
    /// SHARED WITH OFF ON NORD: only the walks whose frames the theme in use's row decides.
    Landing,
}

/// What comes between two frames.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    /// Nothing: the frame drawn again.
    Again,
    /// The window made that size.
    Window(Size),
    /// The text size changed, 1× to 2× or back.
    Text,
    /// The aterm.toml reload warning came, or went.
    Warning,
    /// A status came, or went.
    Status,
    /// The wheel that many lines down the list (up, negative).
    Wheel(i32),
    /// The keyboard put on its control (off the list, on the theme in use's row).
    Focus,
    /// The Messages tick every Settings view gets each minute.
    Tick,
    /// A packages refresh's reply.
    Refresh,
}

impl Step {
    /// Whether the person did it.
    fn persons(self) -> bool {
        matches!(self, Self::Wheel(_) | Self::Focus)
    }

    /// The step that undoes this one, in a walk that started at `start`.
    fn back(self, start: Drawn) -> Self {
        match self {
            Self::Window(_) => Self::Window(start.window),
            Self::Wheel(lines) => Self::Wheel(-lines),
            step => step,
        }
    }
}

impl std::fmt::Display for Step {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Window((width, height)) => write!(out, "Window({width}x{height})"),
            step => write!(out, "{step:?}"),
        }
    }
}

/// What a frame is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Drawn {
    window: Size,
    /// The text size: 1× or 2×.
    scale: u8,
    warning: bool,
    status: bool,
}

impl std::fmt::Display for Drawn {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (width, height) = self.window;
        write!(out, "{width}x{height}@{}", self.scale)?;
        if self.warning {
            out.write_str("+warning")?;
        }
        if self.status {
            out.write_str("+status")?;
        }
        Ok(())
    }
}

impl Drawn {
    const fn bare(window: Size, scale: u8) -> Self {
        Self {
            window,
            scale,
            warning: false,
            status: false,
        }
    }

    fn after(self, step: Step) -> Self {
        match step {
            Step::Window(window) => Self { window, ..self },
            Step::Text => Self {
                scale: 3 - self.scale,
                ..self
            },
            Step::Warning => Self {
                warning: !self.warning,
                ..self
            },
            Step::Status => Self {
                status: !self.status,
                ..self
            },
            Step::Again | Step::Wheel(_) | Step::Focus | Step::Tick | Step::Refresh => self,
        }
    }
}

/// The rule a frame broke.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Rule {
    R1,
    R2,
    R3,
    R4,
}

/// How much of a control a screen shows.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kept {
    Whole,
    /// That many points of it, cut (none: not drawn).
    Part(f32),
}

impl Kept {
    /// Whether a screen that shows this much shows what `owed` asks.
    fn covers(self, owed: Self) -> bool {
        match (self, owed) {
            (Self::Whole, _) => true,
            (Self::Part(_), Self::Whole) => false,
            (Self::Part(shown), Self::Part(owed)) => shown + 0.5 >= owed,
        }
    }
}

impl std::fmt::Display for Kept {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Whole => out.write_str("whole"),
            Self::Part(shown) => write!(out, "{shown}pt"),
        }
    }
}

/// A frame, as the rules read it.
struct Frame {
    drawn: Drawn,
    screen: crate::native_ui::CompiledUi,
    /// The list's plan.
    plan: Option<scroll::ScrollPlan>,
    top: Option<usize>,
    /// Whether the screen draws a list.
    list: bool,
    /// Whether the list paints any of its stops in its room.
    paints: bool,
    /// Where the list it draws starts, and how tall it is.
    list_top: f32,
    room: f32,
}

impl Frame {
    fn listed(&self, key: &str) -> bool {
        self.list
            && self
                .plan
                .as_ref()
                .is_some_and(|plan| plan.stop_of(&UiKey::new(key)).is_some())
    }

    /// How much of `key` the screen paints.
    fn shown(&self, key: &str) -> Option<(f32, f32)> {
        self.screen
            .semantic(&UiKey::new(key))
            .map(|_| shown_of(&self.screen, key))
    }

    fn kept(&self, key: &str) -> Kept {
        match self.shown(key) {
            Some((shown, tall)) if shown + 0.5 >= tall => Kept::Whole,
            shown => Kept::Part(shown.map_or(0.0, |(shown, _)| shown)),
        }
    }

    /// Whether the screen draws the chooser's list.
    fn chooser(&self) -> bool {
        self.screen.semantic(&UiKey::new(LIST)).is_some()
    }

    /// Whether the chooser's row `key` is whole inside the chooser's list.
    fn in_list(&self, key: &str) -> bool {
        self.chooser() && whole_in_list(&self.screen, key)
    }

    /// Whether no screen shows `landing` whole with `kept` of `key` ([`crate::native_ui::ListRoom`]).
    fn apart(&self, landing: &str, key: &str, kept: Kept) -> bool {
        let rect = |key: &str| self.screen.semantic(&UiKey::new(key)).map(|node| node.rect);
        let (Some(row), Some(control)) = (rect(landing), rect(key)) else {
            return false;
        };
        if !self.listed(landing) || !self.listed(key) {
            return false;
        }
        let part = match kept {
            Kept::Whole => control.height,
            Kept::Part(part) => part.min(control.height),
        };
        let (top, bottom) = if control.y >= row.y {
            (row.y, (control.y + part).max(row.bottom()))
        } else {
            ((control.bottom() - part).min(row.y), row.bottom())
        };
        bottom - top > self.room + 0.5
    }

    /// The identity of the stop the screen starts at.
    fn first(&self) -> Option<UiKey> {
        let (plan, top) = self.plan.as_ref().zip(self.top)?;
        plan.identities(top).next().cloned()
    }

    /// Where the list lays `key` out on this screen.
    fn placed(&self, key: &str) -> Option<(f32, f32)> {
        let rect = self.screen.semantic(&UiKey::new(key))?.rect;
        self.listed(key)
            .then_some((rect.y - self.list_top, rect.height))
    }
}

/// What one screen of the list shows: of the keyboard's control ([`Frame::placed`]).
#[derive(Clone, Copy, Debug)]
struct Screenful {
    control: Option<Kept>,
    landing: bool,
    control_at: Option<(f32, f32)>,
    landing_at: Option<(f32, f32)>,
    room: f32,
}

/// EVERY SCREEN A LIST CAN DRAW.
struct Reach {
    screens: Vec<Screenful>,
    /// Whether any of its screens paints a stop of the list ([`Frame::paints`]).
    list: bool,
    /// The plain status's bar its screens draw ([`StatusBar`]).
    bar: Option<StatusBar>,
}

/// Plain status bar under the page and its minimum height ([`Reaches::cause`]).
#[derive(Clone, Copy, Debug)]
struct StatusBar {
    tall: f32,
    /// Its words on one line ([`plain_bar_layout`]'s least).
    least: f32,
    /// Docked in a compact toolbar, setting every line of its words ([`dock_words_height`]).
    docked: bool,
}

impl StatusBar {
    /// The bar `frame` draws for its plain status, if it draws one.
    fn of(
        runtime: &mut NativeRuntime,
        view: crate::native_app::ViewId,
        frame: &Frame,
    ) -> Option<Self> {
        let tall = frame
            .screen
            .semantic(&UiKey::new("settings/feedback"))
            .filter(|_| frame.drawn.status)?
            .rect
            .height;
        let docked = frame
            .screen
            .semantic(&UiKey::new("settings/status-dock"))
            .is_some();
        let cx = cx_of(frame.drawn);
        let state = view_state(runtime, view);
        let most = state.status_bar_most.replace(Some(0.0));
        let least = feedback_bar_height(
            state,
            SettingsWidth::for_viewport(cx.viewport.width),
            cx.viewport,
        );
        state.status_bar_most.set(most);
        Some(Self {
            tall,
            least,
            docked,
        })
    }

    /// Whether the code could make it shorter.
    fn above_least(self) -> bool {
        !self.docked && self.tall > self.least + 0.5
    }
}

impl Reach {
    /// The first scroll whose screen shows the keyboard's control as `owed` asks.
    fn covering(&self, owed: Kept) -> Option<usize> {
        self.screens
            .iter()
            .position(|screen| screen.control.is_some_and(|kept| kept.covers(owed)))
    }

    /// The first scroll whose screen shows the theme in use's row whole in the list.
    fn landing_with(&self, owed: Option<Kept>) -> Option<usize> {
        self.screens.iter().position(|screen| {
            screen.landing
                && owed.is_none_or(|owed| screen.control.is_some_and(|kept| kept.covers(owed)))
        })
    }

    /// WHY NO SCREEN SHOWS what a rule asks of a stop of the list ([`scroll::ListFoot::Whole`]).
    fn cause(
        &self,
        at: fn(&Screenful) -> Option<(f32, f32)>,
        owed: Kept,
    ) -> Result<String, String> {
        if !self.list {
            return Ok("no list: the frame's room holds none of its stops".to_string());
        }
        let placed = self
            .screens
            .iter()
            .filter_map(|screen| at(screen).map(|place| (place, screen.room)))
            .collect::<Vec<_>>();
        let held = || {
            format!(
                "the room holds it so on one: {:?} (top under the list's, tall) by scroll",
                placed.iter().map(|&(place, _)| place).collect::<Vec<_>>()
            )
        };
        let &((_, tall), room) = placed.first().ok_or_else(held)?;
        let needs = match owed {
            Kept::Whole => tall,
            Kept::Part(part) => part,
        };
        if needs > room + 0.5 {
            return Ok(format!("taller than the room: {needs}pt in {room}pt"));
        }
        let most = placed
            .iter()
            .map(|&((top, tall), room)| (top + tall).min(room) - top.max(0.0))
            .fold(0.0_f32, f32::max);
        let highest = placed
            .iter()
            .map(|&((top, _), _)| top)
            .filter(|top| *top >= -0.5)
            .fold(f32::INFINITY, f32::min);
        if most + 0.5 < needs {
            Ok(format!(
                "the list scrolls by stops: the room holds at most {most}pt of its {tall}pt on \
                 any of {} screens, at best {highest}pt under the list's top in {room}pt",
                self.screens.len()
            ))
        } else {
            Err(held())
        }
    }
}

/// What a reach is measured for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ReachOf {
    drawn: Drawn,
    /// Whether the keyboard is on the theme in use's row.
    on_landing: bool,
    /// Pending reveal that a status bar makes room for ([`SettingsViewState::note_status_room`]).
    reveal: Option<UiKey>,
    /// Whether a status's bar is drawn at its least ([`StatusBar`], [`STATUS_BAR_LEAST`]).
    least: bool,
}

/// The reaches a sweep has measured.
struct Reaches {
    scenario: Scenario,
    landing: String,
    seen: Mutex<HashMap<ReachOf, Arc<OnceLock<Arc<Reach>>>>>,
}

impl Reaches {
    /// The reach `key` names, measured once.
    fn of(&self, key: &ReachOf) -> Arc<Reach> {
        let cell = self
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(key.clone())
            .or_default()
            .clone();
        cell.get_or_init(|| Arc::new(self.measure(key))).clone()
    }

    /// Why no screen of `key` shows what a rule asks ([`Reach::cause`], [`StatusBar`]).
    fn cause(
        &self,
        key: &ReachOf,
        at: fn(&Screenful) -> Option<(f32, f32)>,
        owed: Kept,
        shows: impl Fn(&Reach) -> Option<usize>,
    ) -> Result<String, String> {
        let reach = self.of(key);
        let Some(bar) = reach.bar.filter(|bar| bar.above_least()) else {
            let docked = reach.bar.is_some_and(|bar| bar.docked);
            return reach.cause(at, owed).map(|cause| {
                if docked {
                    format!("{cause}, the status docked in the toolbar")
                } else {
                    cause
                }
            });
        };
        let least = self.of(&ReachOf {
            least: true,
            ..key.clone()
        });
        let under = format!(
            "the status's bar {}pt, its least {}pt, the page's choice",
            bar.tall, bar.least
        );
        match shows(&least) {
            Some(scroll) => Err(format!(
                "{under}: at its least the screen at scroll {scroll} shows it so"
            )),
            None => least
                .cause(at, owed)
                .map(|cause| format!("{cause}, with {under}")),
        }
    }

    fn measure(&self, key: &ReachOf) -> Reach {
        let (drawn, on_landing, reveal) = (key.drawn, key.on_landing, &key.reveal);
        STATUS_BAR_LEAST.with(|least| least.set(key.least));
        let (_service, mut runtime, instance, view) = opened(self.scenario, drawn);
        let control = if on_landing {
            let _ = dispatch(
                &mut runtime,
                instance,
                view,
                AppEvent::FocusChanged(Some(UiKey::new(self.landing.as_str()))),
            );
            Some(self.landing.as_str())
        } else {
            self.scenario.keyboard.control()
        };
        let first = draw(&mut runtime, instance, view, drawn);
        let limit = first
            .plan
            .as_ref()
            .filter(|_| first.list)
            .map_or(0, scroll::ScrollPlan::limit);
        let mut frames = Vec::new();
        for top in 0..=limit {
            if first.list {
                let state = view_state(&mut runtime, view);
                state.scroll_page_to(top);
                state.page_reveal.clone_from(reveal);
                state.reveal_as = scroll::Reveal::Hold;
                state.drop_list_nudge();
            }
            let frame = draw(&mut runtime, instance, view, drawn);
            assert!(
                !first.list || frame.top == Some(top),
                "{}: the reach at {drawn} draws scroll {top}, not {:?}",
                self.scenario,
                frame.top
            );
            frames.push(frame);
        }
        let bar = StatusBar::of(&mut runtime, view, &frames[0]);
        STATUS_BAR_LEAST.with(|least| least.set(false));
        Reach {
            list: frames.iter().any(|frame| frame.paints),
            bar,
            screens: frames
                .iter()
                .map(|frame| Screenful {
                    control: control.map(|key| frame.kept(key)),
                    landing: frame.in_list(&self.landing),
                    control_at: control.and_then(|key| frame.placed(key)),
                    landing_at: frame.placed(&self.landing),
                    room: frame.room,
                })
                .collect(),
        }
    }
}

/// What walks found.
#[derive(Default)]
struct Findings {
    faults: Vec<(Rule, String)>,
    /// Each frame geometry kept a rule from asking anything of: the rule and the frame, then why.
    exceptions: Vec<String>,
    frames: usize,
}

fn view_state(
    runtime: &mut NativeRuntime,
    view: crate::native_app::ViewId,
) -> &mut SettingsViewState {
    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
        unreachable!()
    };
    state
}

fn cx_of(drawn: Drawn) -> ViewCx<'static> {
    let (width, height) = drawn.window;
    view_cx_at(f32::from(width), f32::from(height))
}

/// A view on the scenario's theme with the keyboard where it says.
fn opened(
    scenario: Scenario,
    start: Drawn,
) -> (
    VersionedConfigService,
    NativeRuntime,
    crate::native_app::AppInstanceId,
    crate::native_app::ViewId,
) {
    set_text_scale(f32::from(start.scale));
    let (service, mut runtime, instance, view) =
        themes_view(&format!("theme = \"{}\"\n", scenario.theme));
    let press = |runtime: &mut NativeRuntime, key: &str| {
        let _ = compile_settings_view(runtime, instance, view, &cx_of(start));
        let _ = act(runtime, instance, view, key, None);
    };
    match scenario.keyboard {
        Keyboard::Off => {}
        Keyboard::Filter => {
            let _ = dispatch(
                &mut runtime,
                instance,
                view,
                AppEvent::FocusChanged(Some(UiKey::new(FILTER))),
            );
        }
        Keyboard::Pack => {
            let _ = act(&mut runtime, instance, view, PACK, None);
        }
        Keyboard::SheetFirst => {
            let _ = act(&mut runtime, instance, view, CUSTOMIZE, None);
        }
        Keyboard::SheetDrawn => press(&mut runtime, CUSTOMIZE),
        Keyboard::SheetClosed => {
            press(&mut runtime, CUSTOMIZE);
            press(&mut runtime, DONE);
        }
        Keyboard::Side => press(&mut runtime, SIDE),
    }
    (service, runtime, instance, view)
}

/// Draw a frame as `drawn` says.
fn draw(
    runtime: &mut NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    drawn: Drawn,
) -> Frame {
    set_text_scale(f32::from(drawn.scale));
    let bars = view_state(runtime, view);
    bars.config_watch_status = drawn.warning.then(|| WARNING.to_string());
    bars.feedback = drawn.status.then(|| REFUSED.to_string());
    let screen = compile_settings_view(runtime, instance, view, &cx_of(drawn));
    let state = state(runtime, view);
    let (key, list) = [LIST, SHEET_LIST]
        .into_iter()
        .find_map(|key| {
            screen
                .semantic(&UiKey::new(key))
                .map(|list| (key, list.rect))
        })
        .unzip();
    let paints = key.zip(list).is_some_and(|(key, list)| {
        screen.paint.iter().any(|node| {
            let top = node.rect.y.max(node.clip.y).max(list.y);
            let bottom = node
                .rect
                .bottom()
                .min(node.clip.bottom())
                .min(list.bottom());
            !node.key.as_str().starts_with(key)
                && bottom - top > 0.5
                && node.rect.x >= list.x - 0.5
                && node.rect.right() <= list.right() + 0.5
        })
    });
    Frame {
        drawn,
        plan: state.scroll_plan(),
        top: state
            .scroll_record
            .borrow()
            .as_ref()
            .map(scroll::ScrollRecord::top),
        list: list.is_some(),
        paints,
        list_top: list.map_or(0.0, |list| list.y),
        room: list.map_or(0.0, |list| list.height),
        screen,
    }
}

/// What R2 keeps of the keyboard's control.
#[derive(Clone, Copy, Debug)]
struct Left {
    kept: Kept,
    at: usize,
    drawn: Drawn,
}

/// A wheel's screen, which R4 holds until an event of the person's.
struct Wheeled {
    at: usize,
    drawn: Drawn,
    screen: crate::native_ui::CompiledUi,
    first: Option<UiKey>,
    /// How much of the keyboard's control it showed.
    kept: Option<Kept>,
    /// The list it drew and the stop it started at: the wheel's place.
    place: Option<(scroll::ScrollPlan, usize)>,
    /// Whole keyboard control and stops under its first ([`SettingsViewState::seat_scroll`]).
    held: Option<(String, usize)>,
}

/// Walk `steps` from a view opened at `start`, each rule asserted on every frame.
#[expect(
    clippy::too_many_lines,
    reason = "the four rules read each frame together, each with what it keeps of the walk"
)]
fn walk(reaches: &Reaches, start: Drawn, steps: &[Step]) -> Findings {
    let scenario = reaches.scenario;
    let landing = reaches.landing.as_str();
    let mut found = Findings::default();
    let (_service, mut runtime, instance, view) = opened(scenario, start);
    let mut control = scenario.keyboard.control().map(str::to_string);
    let mut opening = !scenario.keyboard.pressed_on_drawn();
    let mut last: Option<Frame> = None;
    let mut left: Option<Left> = None;
    let mut wheeled: Option<Wheeled> = None;
    for at in 0..=steps.len() {
        let step = at.checked_sub(1).map(|index| steps[index]);
        let focused = control.is_none() && step == Some(Step::Focus);
        let revision = u64::try_from(at).unwrap_or(u64::MAX).saturating_add(1);
        let event = match step {
            Some(Step::Wheel(lines)) => Some(AppEvent::ScrollAt {
                lines,
                key: UiKey::new(LIST),
            }),
            Some(Step::Focus) => {
                let key = control.get_or_insert_with(|| landing.to_string());
                Some(AppEvent::FocusChanged(Some(UiKey::new(key.as_str()))))
            }
            Some(Step::Tick) => Some(AppEvent::MessagesChanged { revision }),
            Some(Step::Refresh) => Some(AppEvent::PackagesChanged { revision }),
            _ => None,
        };
        if let Some(event) = event {
            let _ = dispatch(&mut runtime, instance, view, event);
        }
        if step.is_some_and(Step::persons) {
            opening = false;
            wheeled = None;
        }
        let drawn = last
            .as_ref()
            .zip(step)
            .map_or(start, |(last, step)| last.drawn.after(step));
        let frame = draw(&mut runtime, instance, view, drawn);
        found.frames += 1;
        let reveal = state(&runtime, view).page_reveal.clone();
        let on_landing = control.as_deref() == Some(landing);
        let reach_of = ReachOf {
            drawn,
            on_landing,
            reveal: reveal.clone(),
            least: false,
        };
        let kept_reach_of = ReachOf {
            reveal: reveal.map(|reveal| control.as_deref().map_or(reveal, UiKey::new)),
            ..reach_of.clone()
        };
        let walked = steps[..at]
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let at_frame = format!("{scenario} from {start} [{walked}]: frame {at} at {drawn}");
        let mut fault = |rule: Rule, said: String| {
            found.faults.push((rule, format!("{at_frame}: {said}")));
        };
        if let Some(step @ (Step::Again | Step::Tick | Step::Refresh)) = step
            && let Some(last) = last.as_ref()
            && frame.screen != last.screen
        {
            fault(
                Rule::R1,
                format!(
                    "drawn again after {step}, no event of the person's between, another \
                     screen: it started at {:?}, now at {:?}",
                    last.first(),
                    frame.first()
                ),
            );
        }
        let owed = control.as_deref().and_then(|_| match step {
            None => Some((Kept::Whole, "its first screen".to_string())),
            Some(Step::Wheel(_)) => None,
            Some(Step::Focus) if focused => {
                Some((Kept::Whole, "the keyboard put on it".to_string()))
            }
            _ => left.map(|left| {
                (
                    left.kept,
                    format!("what frame {} at {} left the person", left.at, left.drawn),
                )
            }),
        });
        if let (Some(key), Some((owed, why))) = (control.as_deref(), owed.as_ref())
            && !frame.kept(key).covers(*owed)
        {
            let reach = reaches.of(&kept_reach_of);
            let how = if frame.listed(key) {
                "listed"
            } else {
                "pinned"
            };
            match (
                reach.covering(*owed),
                reaches.cause(
                    &kept_reach_of,
                    |screen| screen.control_at,
                    *owed,
                    |least| least.covering(*owed),
                ),
            ) {
                (Some(scroll), _) => fault(
                    Rule::R2,
                    format!(
                        "{key} ({how}) {owed} as {why}, {} now ({:?} shown, tall), though the \
                         screen at scroll {scroll} shows it so",
                        frame.kept(key),
                        frame.shown(key)
                    ),
                ),
                (None, Ok(cause)) => found
                    .exceptions
                    .push(format!("R2 at {drawn}: {key} {owed}, {cause}")),
                (None, Err(why_not)) => fault(
                    Rule::R2,
                    format!(
                        "{key} ({how}) {owed} as {why}, {} now and on no screen of the list, \
                         though no geometry says why: {why_not}",
                        frame.kept(key)
                    ),
                ),
            }
        }
        let apart = owed
            .as_ref()
            .zip(control.as_deref())
            .is_some_and(|((owed, _), key)| {
                frame.kept(key).covers(*owed) && frame.apart(landing, key, *owed)
            });
        if opening && !apart && frame.listed(landing) && frame.chooser() && !frame.in_list(landing)
        {
            let reach = reaches.of(&reach_of);
            let asks = |reach: &Reach| {
                owed.as_ref()
                    .map(|(owed, _)| *owed)
                    .filter(|owed| reach.covering(*owed).is_some())
            };
            if let Some(scroll) = reach.landing_with(asks(&reach)) {
                fault(
                    Rule::R3,
                    format!(
                        "before the person's first event, the theme in use's {landing} is {:?} \
                         (shown, tall), not whole in the list, though the screen at scroll \
                         {scroll} shows it whole with the keyboard's control as R2 asks ({})",
                        frame.shown(landing),
                        asks(&reach).map_or("nothing".to_string(), |owed| format!(
                            "{owed} of {}",
                            control.as_deref().unwrap_or_default()
                        ))
                    ),
                );
            } else if reach.landing_with(None).is_none() {
                match reaches.cause(
                    &reach_of,
                    |screen| screen.landing_at,
                    Kept::Whole,
                    |least| least.landing_with(asks(least)),
                ) {
                    Ok(cause) => found
                        .exceptions
                        .push(format!("R3 at {drawn}: {landing}, {cause}")),
                    Err(why_not) => fault(
                        Rule::R3,
                        format!(
                            "before the person's first event, the theme in use's {landing} is \
                             whole on no screen of the list, though no geometry says why: \
                             {why_not}"
                        ),
                    ),
                }
            }
        }
        if let Some(wheel) = wheeled.as_ref()
            && drawn == wheel.drawn
            && frame.screen != wheel.screen
        {
            let needs = owed.as_ref().is_some_and(|(owed, _)| {
                !wheel.kept.is_some_and(|kept| kept.covers(*owed))
                    && reaches.of(&kept_reach_of).covering(*owed).is_some()
            });
            if !needs {
                fault(
                    Rule::R4,
                    format!(
                        "back where the wheel turned (frame {}), another screen: it starts at \
                         {:?}, the wheel's at {:?}",
                        wheel.at,
                        frame.first(),
                        wheel.first
                    ),
                );
            }
        }
        if let Some(wheel) = wheeled.as_ref()
            && drawn != wheel.drawn
            && frame.list
            && let Some((was, top)) = wheel.place.as_ref()
            && let Some(plan) = frame.plan.as_ref()
            && let Some(stop) = wheel
                .held
                .as_ref()
                .and_then(|(key, under)| {
                    plan.stop_of(&UiKey::new(key.as_str()))
                        .map(|stop| stop.saturating_sub(*under))
                })
                .or_else(|| {
                    if *top == 0 {
                        return Some(0);
                    }
                    (*top..was.len()).find_map(|stop| {
                        was.identities(stop)
                            .find_map(|id| plan.stop_of_identity(id))
                            .map(|index| index.saturating_sub(stop - top))
                    })
                })
            && let place = stop.min(plan.limit())
            && let asks = owed.as_ref().map(|(owed, _)| *owed)
            && !(frame.top == Some(place)
                && asks
                    .zip(control.as_deref())
                    .is_none_or(|(owed, key)| frame.kept(key).covers(owed)))
        {
            let reach = asks.map(|_| reaches.of(&kept_reach_of));
            let shows = |scroll: usize| {
                reach.as_ref().zip(asks).is_none_or(|(reach, owed)| {
                    reach
                        .screens
                        .get(scroll)
                        .and_then(|screen| screen.control)
                        .is_some_and(|kept| kept.covers(owed))
                })
            };
            let showing = (0..=plan.limit()).filter(|&scroll| shows(scroll));
            let nearest = showing
                .clone()
                .map(|scroll| scroll.abs_diff(place))
                .min()
                .filter(|_| !shows(place));
            let held = match nearest {
                Some(far) => frame
                    .top
                    .is_some_and(|top| shows(top) && top.abs_diff(place) == far),
                None => frame.top == Some(place),
            };
            if !held {
                fault(
                    Rule::R4,
                    format!(
                        "after the wheel (frame {} at {}), no event of the person's since, the \
                         list is at scroll {:?} ({:?}), not the wheel's place, scroll {place} \
                         ({:?}){}",
                        wheel.at,
                        wheel.drawn,
                        frame.top,
                        frame.first(),
                        wheel.first,
                        nearest.map_or(String::new(), |far| format!(
                            ", or {far} from it where R2 needs the move: the screens that show \
                             the keyboard's control as it asks are {:?}",
                            showing.collect::<Vec<_>>()
                        ))
                    ),
                );
            }
        }
        if let Some(key) = control.as_deref() {
            let now = frame.kept(key);
            left = match left {
                Some(was) if !step.is_some_and(Step::persons) && !now.covers(was.kept) => Some(was),
                _ => Some(Left {
                    kept: now,
                    at,
                    drawn,
                }),
            };
        }
        if matches!(step, Some(Step::Wheel(_))) {
            wheeled = Some(Wheeled {
                at,
                drawn,
                screen: frame.screen.clone(),
                first: frame.first(),
                kept: control.as_deref().map(|key| frame.kept(key)),
                place: frame.plan.clone().zip(frame.top).filter(|_| frame.list),
                held: control
                    .as_deref()
                    .filter(|key| frame.listed(key) && frame.kept(key) == Kept::Whole)
                    .zip(frame.plan.as_ref().zip(frame.top))
                    .and_then(|(key, (plan, top))| {
                        let stop = plan.stop_of(&UiKey::new(key))?;
                        Some((key.to_string(), stop.checked_sub(top)?))
                    }),
            });
        }
        last = Some(frame);
    }
    found
}

/// The windows whose list no earlier one's measures as it does at `scale`.
fn distinct_windows(scenario: Scenario, scale: u8) -> Vec<Size> {
    let mut seen: Vec<Vec<Option<scroll::ScrollPlan>>> = Vec::new();
    let mut windows = Vec::new();
    for window in WINDOWS {
        let start = Drawn::bare(window, scale);
        let (_service, mut runtime, instance, view) = opened(scenario, start);
        let measures = [Step::Again, Step::Warning, Step::Status, Step::Text]
            .into_iter()
            .map(|step| draw(&mut runtime, instance, view, start.after(step)))
            .map(|frame| frame.plan.filter(|_| frame.list))
            .collect::<Vec<_>>();
        if !seen.contains(&measures) {
            seen.push(measures);
            windows.push(window);
        }
    }
    windows
}

/// The walks from `start`.
fn walks_from(start: Drawn, windows: &[Size], keyboard: Keyboard, taken: Walks) -> Vec<Vec<Step>> {
    let changes = windows
        .iter()
        .filter(|&&other| other != start.window)
        .map(|&other| Step::Window(other))
        .chain([Step::Text, Step::Warning, Step::Status]);
    let mut again = vec![Step::Again];
    let mut wheel = Vec::new();
    let mut focus = vec![Step::Focus, Step::Again];
    let mut there = Vec::new();
    let mut refocus = Vec::new();
    for (index, change) in changes.enumerate() {
        let lines = if index % 2 == 0 { 4 } else { -4 };
        let back = change.back(start);
        match index % 4 {
            0 => again.push(Step::Tick),
            2 => again.push(Step::Refresh),
            _ => {}
        }
        again.extend([change, Step::Again, back]);
        wheel.extend([Step::Wheel(lines), change, Step::Again, back]);
        focus.extend([change, back]);
        there.extend([change, Step::Wheel(lines), back]);
        refocus.extend([Step::Wheel(lines), change, Step::Focus, back]);
    }
    let mut walks = vec![again, focus];
    if taken == Walks::Landing {
        return walks;
    }
    walks.extend([wheel, refocus]);
    if keyboard != Keyboard::Off {
        walks.push(there);
    }
    walks
}

/// `count` walks of `length` steps from random windows.
fn random_walks(seed: u64, count: usize, length: usize) -> Vec<(Drawn, Vec<Step>)> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = move |below: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        usize::try_from(state % u64::try_from(below).unwrap_or(u64::MAX)).unwrap_or_default()
    };
    (0..count)
        .map(|_| {
            let start = Drawn::bare(WINDOWS[next(WINDOWS.len())], [1, 2][next(2)]);
            let steps = (0..length)
                .map(|_| match next(100) {
                    0..12 => Step::Again,
                    12..40 => Step::Window(WINDOWS[next(WINDOWS.len())]),
                    40..50 => Step::Text,
                    50..59 => Step::Warning,
                    59..68 => Step::Status,
                    68..88 => Step::Wheel([4, -4, 8, -8, 2][next(5)]),
                    88..95 => Step::Focus,
                    95..98 => Step::Tick,
                    _ => Step::Refresh,
                })
                .collect();
            (start, steps)
        })
        .collect()
}

/// EACH SEQUENCE A REVIEW FOUND BY HAND, walked as found.
fn reviewed() -> Vec<(Scenario, Drawn, Vec<Step>)> {
    use Step::{Again, Focus, Refresh, Status, Text, Tick, Warning, Wheel, Window};
    let [
        w178x348,
        w300x160,
        w444x180,
        w444x292,
        w444x420,
        w584x348,
        w864x292,
        w864x500,
        w1095x700,
        w1144x700,
        w1284x714,
    ] = WINDOWS;
    let filter = Scenario {
        keyboard: Keyboard::Filter,
        theme: "Nord",
        in_use: "Nord",
    };
    let pack = Scenario {
        keyboard: Keyboard::Pack,
        ..filter
    };
    let first = Scenario {
        keyboard: Keyboard::SheetFirst,
        theme: "Dracula",
        in_use: "Dracula",
    };
    let drawn = Scenario {
        keyboard: Keyboard::SheetDrawn,
        theme: "GitHub Light",
        in_use: "GitHub Light",
    };
    let closed = Scenario {
        keyboard: Keyboard::SheetClosed,
        ..first
    };
    let side = Scenario {
        keyboard: Keyboard::Side,
        theme: "dark:Dracula,light:GitHub Light",
        in_use: "GitHub Light",
    };
    let off = Scenario {
        keyboard: Keyboard::Off,
        ..filter
    };
    let at = Drawn::bare;
    vec![
        (filter, at(w864x292, 1), vec![Wheel(4), Warning]),
        (
            filter,
            at(w1284x714, 2),
            vec![Window(w444x180), Window(w1284x714)],
        ),
        (
            filter,
            at(w1284x714, 2),
            vec![Window(w584x348), Window(w1284x714)],
        ),
        (drawn, at(w178x348, 1), vec![Wheel(-4), Status, Text, Again]),
        (drawn, at(w178x348, 1), vec![Wheel(-4), Text, Status, Again]),
        (
            drawn,
            at(w864x292, 2),
            vec![Wheel(8), Window(w178x348), Status, Again],
        ),
        (
            drawn,
            at(w864x292, 2),
            vec![Wheel(8), Window(w178x348), Again, Status, Again, Again],
        ),
        (
            drawn,
            at(w864x292, 2),
            vec![
                Wheel(8),
                Window(w178x348),
                Warning,
                Again,
                Warning,
                Status,
                Again,
            ],
        ),
        (
            first,
            at(w864x292, 2),
            vec![Wheel(8), Window(w178x348), Status, Again],
        ),
        (drawn, at(w444x292, 2), vec![Status, Again, Status]),
        (first, at(w444x292, 2), vec![Status, Again, Status]),
        (filter, at(w444x292, 2), vec![Status, Again, Status]),
        (
            filter,
            at(w444x180, 1),
            vec![Wheel(4), Window(w864x292), Focus, Window(w444x180)],
        ),
        (
            filter,
            at(w444x180, 1),
            vec![Wheel(4), Window(w864x292), Wheel(4), Window(w444x180)],
        ),
        (
            filter,
            at(w444x180, 1),
            vec![Wheel(4), Window(w864x292), Window(w444x180)],
        ),
        (
            closed,
            at(w178x348, 1),
            vec![Focus, Again, Window(w444x292), Window(w178x348), Again],
        ),
        (
            closed,
            at(w178x348, 1),
            vec![Window(w444x292), Window(w178x348)],
        ),
        (
            filter,
            at(w178x348, 1),
            vec![
                Wheel(4),
                Window(w300x160),
                Window(w178x348),
                Wheel(-4),
                Window(w444x180),
                Window(w178x348),
                Wheel(4),
                Window(w444x292),
                Window(w178x348),
            ],
        ),
        (
            pack,
            at(w300x160, 1),
            vec![Status, Text, Text, Wheel(8), Window(w1284x714)],
        ),
        (
            side,
            at(w444x292, 2),
            vec![
                Wheel(-8),
                Status,
                Window(w444x292),
                Again,
                Window(w1144x700),
                Window(w1095x700),
                Window(w584x348),
                Again,
                Window(w864x292),
                Window(w584x348),
                Window(w178x348),
                Text,
                Window(w300x160),
                Text,
                Again,
                Status,
            ],
        ),
        (off, at(w1144x700, 1), vec![Tick, Window((1_144, 462))]),
        (off, at(w1144x700, 1), vec![Refresh, Window((1_144, 462))]),
        (
            closed,
            at(w584x348, 2),
            vec![
                Window(w864x292),
                Status,
                Wheel(8),
                Window(w178x348),
                Focus,
                Window(w864x500),
                Window(w444x420),
                Wheel(-8),
                Warning,
            ],
        ),
        (
            filter,
            at(w444x420, 2),
            vec![
                Again,
                Window(w444x420),
                Focus,
                Status,
                Window(w300x160),
                Wheel(-4),
                Again,
                Window(w300x160),
                Window(w1144x700),
                Status,
                Focus,
                Window(w300x160),
            ],
        ),
        (
            filter,
            at(w864x292, 1),
            vec![
                Text,
                Window(w444x180),
                Again,
                Wheel(-8),
                Window(w300x160),
                Warning,
                Window(w300x160),
                Text,
                Warning,
                Again,
                Window(w864x292),
                Wheel(4),
                Window(w300x160),
            ],
        ),
        (
            drawn,
            at(w444x420, 1),
            vec![
                Status,
                Window(w444x420),
                Wheel(4),
                Warning,
                Text,
                Status,
                Again,
                Window(w864x292),
                Again,
                Wheel(8),
                Window(w178x348),
                Again,
                Warning,
                Status,
                Again,
            ],
        ),
        (
            off,
            at(w444x292, 2),
            vec![Warning, Status, Window(w444x180)],
        ),
        (
            off,
            at(w864x500, 2),
            vec![
                Again,
                Window(w444x292),
                Again,
                Window(w864x500),
                Focus,
                Window(w444x292),
                Window(w864x500),
                Status,
                Window(w444x180),
                Warning,
            ],
        ),
        (
            closed,
            at(w584x348, 1),
            vec![
                Focus,
                Again,
                Text,
                Focus,
                Warning,
                Status,
                Window(w444x180),
                Again,
                Text,
                Again,
                Focus,
                Text,
                Warning,
                Wheel(-4),
                Text,
                Text,
            ],
        ),
        (first, at(w178x348, 2), vec![Status]),
        (first, at(w444x292, 2), vec![Status]),
        (closed, at(w178x348, 2), vec![Status]),
        (off, at(w178x348, 2), vec![Status]),
        (
            first,
            at(w584x348, 1),
            vec![
                Wheel(-4),
                Window(w1095x700),
                Text,
                Warning,
                Wheel(-8),
                Text,
                Wheel(-4),
                Window(w178x348),
                Status,
                Text,
            ],
        ),
        (
            filter,
            at(w584x348, 1),
            vec![
                Tick,
                Again,
                Wheel(-4),
                Warning,
                Wheel(2),
                Window(w864x292),
                Again,
                Focus,
                Focus,
                Window(w178x348),
                Window(w178x348),
                Text,
                Status,
            ],
        ),
        (
            closed,
            at(w444x420, 1),
            vec![
                Window(w444x420),
                Focus,
                Text,
                Wheel(8),
                Wheel(-4),
                Tick,
                Wheel(2),
                Window(w584x348),
                Window(w1144x700),
                Again,
                Window(w584x348),
                Status,
                Window(w178x348),
                Warning,
            ],
        ),
        (side, at(w444x420, 1), vec![Window(w178x348)]),
        (
            filter,
            at(w178x348, 1),
            vec![Window(w300x160), Window(w444x180), Window(w444x292)],
        ),
        (
            filter,
            at(w444x292, 1),
            vec![Wheel(4), Window(w584x348), Window(w444x292)],
        ),
        (
            first,
            at(w178x348, 1),
            vec![Wheel(4), Window(w584x348), Window(w178x348)],
        ),
    ]
}

/// Sweep a scenario ([`walks_from`], [`reviewed`]).
fn sweep(scenario: Scenario, taken: Walks) {
    let started = std::time::Instant::now();
    let landing = {
        let (_service, runtime, _instance, view) =
            themes_view(&format!("theme = \"{}\"\n", scenario.theme));
        card_action(&runtime, view, scenario.in_use)
    };
    let reaches = Reaches {
        scenario,
        landing,
        seen: Mutex::new(HashMap::new()),
    };
    let mut walks = Vec::new();
    let mut starts = Vec::new();
    for scale in [1, 2] {
        let windows = distinct_windows(scenario, scale);
        for &window in &windows {
            let start = Drawn::bare(window, scale);
            starts.push(start.to_string());
            walks.extend(
                walks_from(start, &windows, scenario.keyboard, taken)
                    .into_iter()
                    .map(|steps| (start, steps)),
            );
        }
    }
    if let Walks::Every(seed) = taken {
        walks.extend(random_walks(seed, 16, 16));
    }
    walks.extend(
        reviewed()
            .into_iter()
            .filter(|(seen, _, _)| *seen == scenario)
            .map(|(_, start, steps)| (start, steps)),
    );
    let found = Mutex::new(Findings::default());
    on_workers(&walks, "keeping walks", |(start, steps)| {
        let walked = walk(&reaches, *start, steps);
        let mut found = found.lock().unwrap_or_else(PoisonError::into_inner);
        found.faults.extend(walked.faults);
        found.exceptions.extend(walked.exceptions);
        found.frames += walked.frames;
    });
    let mut found = found.into_inner().unwrap_or_else(PoisonError::into_inner);
    found.faults.sort_by_key(|(rule, _)| *rule);
    let mut excepted = found.exceptions.clone();
    excepted.sort_unstable();
    excepted.dedup();
    eprintln!(
        "{scenario}: {} walks from {starts:?}, {} frames and {} reaches in {:?}; geometry \
         excepted {} frames:\n  {}",
        walks.len(),
        found.frames,
        reaches
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len(),
        started.elapsed(),
        found.exceptions.len(),
        excepted.join("\n  ")
    );
    let rules = [Rule::R1, Rule::R2, Rule::R3, Rule::R4];
    let count = |rule: Rule| {
        found
            .faults
            .iter()
            .filter(|(seen, _)| *seen == rule)
            .count()
    };
    assert!(
        found.faults.is_empty(),
        "{scenario}: {} of {} frames break a keeping rule ({}); the first of each:\n{}",
        found.faults.len(),
        found.frames,
        rules
            .map(|rule| format!("{rule:?} {}", count(rule)))
            .join(", "),
        rules
            .iter()
            .flat_map(|rule| {
                found
                    .faults
                    .iter()
                    .filter(move |(seen, _)| seen == rule)
                    .take(3)
                    .map(|(rule, said)| format!("{rule:?}: {said}"))
            })
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_the_filter() {
    sweep(
        Scenario {
            keyboard: Keyboard::Filter,
            theme: "Nord",
            in_use: "Nord",
        },
        Walks::Every(1),
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_a_pack_row() {
    sweep(
        Scenario {
            keyboard: Keyboard::Pack,
            theme: "Nord",
            in_use: "Nord",
        },
        Walks::Every(2),
    );
}

#[test]
fn the_keeping_rules_hold_with_customize_pressed_before_the_first_frame() {
    sweep(
        Scenario {
            keyboard: Keyboard::SheetFirst,
            theme: "Dracula",
            in_use: "Dracula",
        },
        Walks::Every(3),
    );
}

#[test]
fn the_keeping_rules_hold_with_customize_pressed_on_the_drawn_page() {
    sweep(
        Scenario {
            keyboard: Keyboard::SheetDrawn,
            theme: "GitHub Light",
            in_use: "GitHub Light",
        },
        Walks::Every(4),
    );
}

#[test]
fn the_keeping_rules_hold_with_the_customize_sheet_closed_with_done() {
    sweep(
        Scenario {
            keyboard: Keyboard::SheetClosed,
            theme: "Dracula",
            in_use: "Dracula",
        },
        Walks::Every(5),
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_the_dark_light_segment() {
    sweep(
        Scenario {
            keyboard: Keyboard::Side,
            theme: "dark:Dracula,light:GitHub Light",
            in_use: "GitHub Light",
        },
        Walks::Every(6),
    );
}

#[test]
fn the_keeping_rules_hold_off_the_list_on_nord() {
    sweep(
        Scenario {
            keyboard: Keyboard::Off,
            theme: "Nord",
            in_use: "Nord",
        },
        Walks::Every(7),
    );
}

#[test]
fn the_keeping_rules_hold_off_the_list_on_dracula() {
    sweep(
        Scenario {
            keyboard: Keyboard::Off,
            theme: "Dracula",
            in_use: "Dracula",
        },
        Walks::Landing,
    );
}

#[test]
fn the_keeping_rules_hold_off_the_list_on_github_light() {
    sweep(
        Scenario {
            keyboard: Keyboard::Off,
            theme: "GitHub Light",
            in_use: "GitHub Light",
        },
        Walks::Landing,
    );
}

#[test]
fn the_keeping_rules_hold_off_the_list_on_one_dark() {
    sweep(
        Scenario {
            keyboard: Keyboard::Off,
            theme: "One Dark",
            in_use: "One Dark",
        },
        Walks::Landing,
    );
}
