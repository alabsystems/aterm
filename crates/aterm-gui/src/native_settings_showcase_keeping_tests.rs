// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keeping-rule sweep (R1–R5, stated on [`SettingsViewState::seat_scroll`]) over Settings ▸ Themes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use super::*;

type Size = (u16, u16);

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

/// Windows no rule was tuned on, which the random walks draw too.
const ODD_WINDOWS: [Size; 12] = [
    (230, 420),
    (300, 292),
    (380, 300),
    (444, 240),
    (520, 400),
    (600, 200),
    (640, 480),
    (700, 250),
    (760, 330),
    (980, 420),
    (1_144, 462),
    (1_440, 900),
];

const FILTER: &str = "settings/showcase/filter";
const PACK: &str = "settings/showcase/pack";
const CUSTOMIZE: &str = "settings/showcase/customize";
const DONE: &str = "settings/showcase/customize-done";
const SIDE: &str = "settings/showcase/side/dark";
const LIST: &str = "settings/showcase/list";
const SHEET_LIST: &str = "settings/showcase/sheet-list";
const ROW: &str = "settings/showcase/card/";
const WARNING: &str = "aterm.toml reload rejected: repair the invalid authored value before \
                       automatic reload can resume.";
const REFUSED: &str = "/Volumes/Shared Drive/Users//someone/dotfiles/config/aterm \
                       isn\u{2019}t writable \u{b7} check its permissions";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Keyboard {
    Off,
    Filter,
    Pack,
    SheetFirst,
    SheetDrawn,
    SheetClosed,
    Side,
    /// This theme's row, put there before the first frame: one next to the
    /// theme in use's.
    Row(&'static str),
}

impl Keyboard {
    fn pressed_on_drawn(self) -> bool {
        matches!(self, Self::SheetDrawn | Self::SheetClosed | Self::Side)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Scenario {
    keyboard: Keyboard,
    theme: &'static str,
    in_use: &'static str,
}

impl std::fmt::Display for Scenario {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "{:?} on {}", self.keyboard, self.theme)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Walks {
    /// Every walk from every start ([`walk_from`]), and random walks
    Every(u64),
    /// Only the walks whose frames the theme in use's row decides: the list
    /// opened and drawn through every change before the person's first
    /// event (R1, R3), and the keyboard put on its control (R2). The four Off
    /// sweeps differ in that row alone, the same list with no landing and no
    /// control the row decides after a wheel or a key, so Off on Nord's
    /// other walks stand for the other three's.
    Opening,
    /// Only the walks reviews found ([`reviewed`]).
    Reviewed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    Again,
    Window(Size),
    Text,
    Warning,
    Status,
    Wheel(i32),
    Focus,
    /// Up, Down, an "o" typed and Backspace, Left and Right, each the
    /// person's key.
    Up,
    Down,
    Type,
    Erase,
    Left,
    Right,
    Tick,
    Refresh,
}

impl Step {
    fn persons(self) -> bool {
        matches!(self, Self::Wheel(_) | Self::Focus) || self.key()
    }

    /// Whether it is a key the person pressed (R5).
    fn key(self) -> bool {
        matches!(
            self,
            Self::Up | Self::Down | Self::Type | Self::Erase | Self::Left | Self::Right
        )
    }

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Drawn {
    window: Size,
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
            _ => self,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Rule {
    R1,
    R2,
    R3,
    R4,
    R5,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kept {
    Whole,
    Part(f32),
}

impl Kept {
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

struct Frame {
    drawn: Drawn,
    screen: crate::native_ui::CompiledUi,
    plan: Option<scroll::ScrollPlan>,
    top: Option<usize>,
    list: bool,
    paints: bool,
    list_top: f32,
    room: f32,
    /// The chooser's first row under its filter: its first result.
    first_row: Option<String>,
    /// The chrome the page drew in: one it chose, or held.
    chrome: PageChrome,
}

impl Frame {
    fn listed(&self, key: &str) -> bool {
        self.list
            && self
                .plan
                .as_ref()
                .is_some_and(|plan| plan.stop_of(&UiKey::new(key)).is_some())
    }

    /// How much of `key` the screen paints, and how tall it is laid out;
    /// `None` where the screen does not lay it out.
    fn shown(&self, key: &str) -> Option<(f32, f32)> {
        laid_of(&self.screen, key).map(|(rect, shown)| (shown, rect.height))
    }

    fn kept(&self, key: &str) -> Kept {
        match self.shown(key) {
            Some((shown, tall)) if shown + 0.5 >= tall => Kept::Whole,
            shown => Kept::Part(shown.map_or(0.0, |(shown, _)| shown)),
        }
    }

    fn chooser(&self) -> bool {
        self.screen.semantic(&UiKey::new(LIST)).is_some()
    }

    fn in_list(&self, key: &str) -> bool {
        self.chooser() && whole_in_list(&self.screen, key)
    }

    /// frame's laid-out rects — the list holds every row it has, one off its
    /// screen at the bounds scrolling gives it ([`crate::native_ui::ListRoom`]),
    /// so what lies between the two is the same on every screen.
    fn apart(&self, landing: &str, key: &str, kept: Kept) -> bool {
        let (Some((row, _)), Some((control, _))) =
            (laid_of(&self.screen, landing), laid_of(&self.screen, key))
        else {
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

    fn first(&self) -> Option<UiKey> {
        let (plan, top) = self.plan.as_ref().zip(self.top)?;
        plan.identities(top).next().cloned()
    }

    /// Where the list lays `key` out on this screen: its laid-out rect's top
    /// under the list's top, off the screen too, and its height. `None`
    /// where it is not a stop of the list (pinned around it, or not drawn).
    fn placed(&self, key: &str) -> Option<(f32, f32)> {
        let (rect, _) = laid_of(&self.screen, key)?;
        self.listed(key)
            .then_some((rect.y - self.list_top, rect.height))
    }
}

/// whether it shows the theme in use's row and the chooser's first row whole
/// in the list; and where the list lays the control, the landing and the
/// first row out on it ([`Frame::placed`]), under a room that tall.
#[derive(Clone, Copy, Debug)]
struct Screenful {
    control: Option<Kept>,
    landing: bool,
    first_row: bool,
    control_at: Option<(f32, f32)>,
    landing_at: Option<(f32, f32)>,
    first_row_at: Option<(f32, f32)>,
    room: f32,
    list_top: f32,
}

/// EVERY SCREEN A LIST CAN DRAW THAT SHOWS WHAT A RULE READS, as a frame
/// measures it: the list drawn at its scrolls in a view opened as the walk's
/// was, in the same window, text size and bars, the keyboard on the same
/// control, the same filter and trial and the same reveal pending, held where
/// the scroll puts it — what geometry lets any keeping draw, measured on the
/// screens and never asked of the code that seats the list. Keyed by the
/// scroll each screen drew. ONLY WHERE NEEDED: around each listed stop a rule
/// reads (the keyboard's control, the landing, the first row), out to the
/// first screen on each side that lays it wholly past the room; a list moves
/// its rows up as it scrolls on, so no screen past those shows it, which the
/// screens drawn hold to ([`Sweep::measure`]: where they do not, every scroll).
struct Reach {
    screens: BTreeMap<usize, Screenful>,
    /// Scrolls asked for that drew another, or none.
    strays: Vec<(usize, Option<usize>)>,
    /// The scrolls it looked at.
    looked: BTreeSet<usize>,
    limit: usize,
    list: bool,
    bar: Option<StatusBar>,
    /// How much of the keyboard's control the screens not drawn show: none
    /// of a listed one (wholly past the room), as much as every drawn screen
    /// does of one pinned around the list.
    elsewhere: Option<Kept>,
    /// The chrome its screens drew in.
    chrome: PageChrome,
    /// The list its screens draw: how many stops, how tall in all.
    listed: Option<(usize, f32)>,
}

/// ([`Sweep::judge`]).
#[derive(Clone, Copy, Debug)]
struct StatusBar {
    tall: f32,
    least: f32,
    docked: bool,
}

impl StatusBar {
    /// The bar `screen` draws for its plain status, if it draws one:
    /// measured with the view as that frame drew it.
    fn of(
        runtime: &mut NativeRuntime,
        view: crate::native_app::ViewId,
        drawn: Drawn,
        screen: &crate::native_ui::CompiledUi,
    ) -> Option<Self> {
        let tall = screen
            .semantic(&UiKey::new("settings/feedback"))
            .filter(|_| drawn.status)?
            .rect
            .height;
        let docked = screen
            .semantic(&UiKey::new("settings/status-dock"))
            .is_some();
        let cx = cx_of(drawn);
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

    fn above_least(self) -> bool {
        !self.docked && self.tall > self.least + 0.5
    }
}

impl Reach {
    /// A scroll whose screen shows the keyboard's control as `owed` asks.
    fn covering(&self, owed: Kept) -> Option<usize> {
        self.find(|screen| screen.control.is_some_and(|kept| kept.covers(owed)))
    }

    fn find(&self, shows: impl Fn(&Screenful) -> bool) -> Option<usize> {
        self.screens
            .iter()
            .find(|(_, screen)| shows(screen))
            .map(|(scroll, _)| *scroll)
    }

    /// The screen that shows the most of the keyboard's control, and how
    /// much.
    fn most(&self) -> Option<(usize, Kept)> {
        let mut best: Option<(usize, Kept)> = None;
        for (scroll, screen) in &self.screens {
            let Some(kept) = screen.control else {
                continue;
            };
            if best.is_none_or(|(_, most)| !most.covers(kept)) {
                best = Some((*scroll, kept));
            }
        }
        best
    }

    /// What R2 asks of the keyboard's control on these screens: `owed`
    /// where one shows it so, else as much as the one that shows the most.
    fn asked(&self, owed: Kept) -> Kept {
        if self.covering(owed).is_some() {
            return owed;
        }
        self.most().map_or(Kept::Part(0.0), |(_, most)| most)
    }

    /// A scroll whose screen shows the theme in use's row whole in the list,
    /// and the keyboard's control as `asked`.
    fn landing_with(&self, asked: Option<Kept>) -> Option<usize> {
        self.find(|screen| {
            screen.landing
                && asked.is_none_or(|asked| screen.control.is_some_and(|kept| kept.covers(asked)))
        })
    }

    /// A scroll whose screen shows the chooser's first row whole in the
    /// list, and the keyboard's control as `asked`.
    fn first_row_with(&self, asked: Kept) -> Option<usize> {
        self.find(|screen| {
            screen.first_row && screen.control.is_some_and(|kept| kept.covers(asked))
        })
    }

    /// How much of the keyboard's control the screen at `scroll` shows:
    /// none where a scroll looked at drew another.
    fn control_at(&self, scroll: usize) -> Option<Kept> {
        match self.screens.get(&scroll) {
            Some(screen) => screen.control,
            None if self.looked.contains(&scroll) => None,
            None => self.elsewhere,
        }
    }

    /// The scrolls looked at that no screen drew.
    fn missing(&self) -> Vec<usize> {
        self.looked
            .iter()
            .copied()
            .filter(|scroll| !self.screens.contains_key(scroll))
            .collect()
    }

    /// `at` says the list lays it out — measured on the laid-out rects
    /// alone: the frame draws no list; it is taller than the list's room;
    /// or it is pinned around the list, and the room the page leaves it at
    /// the bar's least cuts it. `Err` where the room holds as much of it as
    /// the rule asks and no screen draws it so, or a scroll drew no screen
    /// to measure: no geometry says why, a fault. That the list scrolls by
    /// stops is no cause: a stop is grouped for the room it is seated in,
    /// under a status the room the bar leaves, and a head the room cannot
    /// hold over a row scrolls away with the rows, so a screen can start at
    /// the stop (at 444x180 and 2× text, COLORS' 40pt over foreground's 56pt
    /// in 84pt, and at 300x160 under a status the built-in section's caption
    /// over its first theme, 64pt in 48pt, were excepted so).
    fn cause(
        &self,
        at: fn(&Screenful) -> Option<(f32, f32)>,
        owed: Kept,
    ) -> Result<String, String> {
        if !self.list {
            return Ok("no list: the frame's room holds none of its stops".to_string());
        }
        let missing = self.missing();
        if !missing.is_empty() {
            return Err(format!(
                "no geometry measured at scrolls {missing:?}: asked, the reach drew {:?}",
                self.strays
            ));
        }
        let placed = self
            .screens
            .values()
            .filter_map(|screen| at(screen).map(|place| (place, screen.room)))
            .collect::<Vec<_>>();
        let Some(&((_, tall), room)) = placed.first() else {
            return Ok(match self.most() {
                Some((_, most)) => {
                    format!("pinned around the list, {most} of it on every screen")
                }
                None => "not drawn on any screen".to_string(),
            });
        };
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
            Err(format!(
                "the room holds its {needs}pt, but at most {most}pt of its {tall}pt on any of \
                 {} screens, at best {highest}pt under the list's top in {room}pt",
                self.screens.len()
            ))
        } else {
            Err(format!(
                "the room holds it so on one: {:?} (top under the list's, tall) by scroll",
                placed.iter().map(|&(place, _)| place).collect::<Vec<_>>()
            ))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ReachOf {
    drawn: Drawn,
    /// The keyboard's control.
    keyboard: Option<String>,
    reveal: Option<UiKey>,
    least: bool,
    /// The chooser's filter, and the theme on trial.
    filter: String,
    trial: Option<String>,
    /// What else the walk's view held that lays the page out.
    held: Held,
    /// The chrome the screens are held in ([`PAGE_CHROME_HELD`]), or none
    /// for the chrome the page chooses.
    chrome: Option<Chrome>,
}

/// WHAT A WALK'S VIEW HOLDS THAT LAYS THE PAGE OUT, past what the scenario
/// opened it with and [`ReachOf`]'s other fields: the sheet open or shut,
/// the Community disclosure (Left and Right on it), the Keep menu, an open
/// picker, the side the segment highlights (read only while the theme
/// follows the system), and the drafts the person typed in the sheet's
/// fields.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Held {
    sheet_open: bool,
    pack_open: bool,
    keep_more: bool,
    color_picker: Option<&'static str>,
    choice_picker: Option<crate::native_settings::ChoicePicker>,
    slot: Option<crate::native_settings::showcase_page::SlotEditor>,
    save: Option<crate::native_settings::showcase_page::SaveAsState>,
    highlight_side: Option<aterm_types::Appearance>,
    editing_field: Option<String>,
    field_inputs: BTreeMap<String, String>,
}

impl std::hash::Hash for Held {
    fn hash<H: std::hash::Hasher>(&self, hasher: &mut H) {
        format!("{self:?}").hash(hasher);
    }
}

impl Held {
    fn of(state: &SettingsViewState) -> Self {
        Self {
            sheet_open: state.showcase.sheet_open,
            pack_open: state.showcase.pack_open,
            keep_more: state.showcase.keep_more,
            color_picker: state.color_picker,
            choice_picker: state.choice_picker.clone(),
            slot: state.showcase.slot.clone(),
            save: state.showcase.save.clone(),
            highlight_side: state.showcase_highlight_side(),
            editing_field: state.editing_field.clone(),
            field_inputs: state
                .field_inputs
                .iter()
                .map(|(field, input)| (field.clone(), input.value().to_string()))
                .collect(),
        }
    }

    /// `state`, a view opened as the scenario says, made to hold this.
    fn put(&self, state: &mut SettingsViewState) {
        state.showcase.sheet_open = self.sheet_open;
        state.set_showcase_pack_open(self.pack_open);
        state.showcase.keep_more = self.keep_more;
        state.color_picker = self.color_picker;
        state.choice_picker.clone_from(&self.choice_picker);
        state.showcase.slot.clone_from(&self.slot);
        state.showcase.save.clone_from(&self.save);
        state.showcase.highlight_side = self.highlight_side;
        state.editing_field.clone_from(&self.editing_field);
        state.field_inputs = self
            .field_inputs
            .iter()
            .map(|(field, value)| {
                (
                    field.clone(),
                    crate::native_text_input::TextInputState::new(value.clone()),
                )
            })
            .collect();
        state.invalidate_result_page_limit();
    }
}

/// A page chrome as a key.
#[derive(Clone, Copy, Debug)]
struct Chrome(PageChrome);

impl PartialEq for Chrome {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for Chrome {}

impl std::hash::Hash for Chrome {
    fn hash<H: std::hash::Hasher>(&self, hasher: &mut H) {
        format!("{:?}", self.0).hash(hasher);
    }
}

/// A scenario's sweep: its landing and control, and the reaches it has
/// measured.
struct Sweep {
    scenario: Scenario,
    landing: String,
    /// The keyboard's control as the scenario puts it.
    control: Option<String>,
    /// The scenario's configuration, read once: every view opens on it.
    snapshot: crate::native_config_service::ConfigSnapshot,
    seen: Mutex<HashMap<ReachOf, Arc<OnceLock<Arc<Reach>>>>>,
    /// The screens its reaches drew.
    screens: AtomicUsize,
}

/// Where a screen lays a stop out under the list's top: its top and height.
type Placed = fn(&Screenful) -> Option<(f32, f32)>;

/// A view a walk or a reach draws.
type Opened = (
    NativeRuntime,
    crate::native_app::AppInstanceId,
    crate::native_app::ViewId,
);

impl Sweep {
    fn new(scenario: Scenario) -> Self {
        let (service, runtime, _instance, view) =
            themes_view(&format!("theme = \"{}\"\n", scenario.theme));
        let control = match scenario.keyboard {
            Keyboard::Off => None,
            Keyboard::Filter => Some(FILTER.to_string()),
            Keyboard::Pack => Some(PACK.to_string()),
            Keyboard::SheetFirst | Keyboard::SheetDrawn => Some(DONE.to_string()),
            Keyboard::SheetClosed => Some(CUSTOMIZE.to_string()),
            Keyboard::Side => Some(SIDE.to_string()),
            Keyboard::Row(theme) => Some(card_action(&runtime, view, theme)),
        };
        Self {
            scenario,
            landing: card_action(&runtime, view, scenario.in_use),
            control,
            snapshot: service.snapshot(),
            seen: Mutex::new(HashMap::new()),
            screens: AtomicUsize::new(0),
        }
    }

    /// A view on the scenario's theme with the keyboard where it says: a
    /// press on the drawn page after a frame at `start`.
    fn opened(&self, start: Drawn) -> Opened {
        set_text_scale(f32::from(start.scale));
        let mut state = SettingsViewState::from_snapshot(&self.snapshot).expect("snapshot");
        state.navigate(SettingsRoute::Themes);
        let (mut runtime, instance, view) =
            setup_with(UpdateState::from_status(1, "0.1.0", None, false), state);
        let press = |runtime: &mut NativeRuntime, key: &str| {
            let _ = compile_settings_view(runtime, instance, view, &cx_of(start));
            let _ = act(runtime, instance, view, key, None);
        };
        match self.scenario.keyboard {
            Keyboard::Off => {}
            Keyboard::Filter | Keyboard::Row(_) => {
                let key = self.control.clone().unwrap_or_default();
                let _ = dispatch(
                    &mut runtime,
                    instance,
                    view,
                    AppEvent::FocusChanged(Some(UiKey::new(key))),
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
        (runtime, instance, view)
    }

    fn reach(&self, key: &ReachOf) -> Arc<Reach> {
        let cell = self
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(key.clone())
            .or_default()
            .clone();
        cell.get_or_init(|| Arc::new(self.measure(key))).clone()
    }

    /// Where `reach`'s status's bar is above its least, the reach `key`
    /// names under the bar at its least, and why: measured only where a
    /// rule asks what `reach` does not show.
    fn at_least(&self, key: &ReachOf, reach: &Reach) -> Option<(Arc<Reach>, String)> {
        reach.bar.filter(|bar| bar.above_least()).map(|bar| {
            let least = self.reach(&ReachOf {
                least: true,
                ..key.clone()
            });
            let under = format!(
                "the status's bar {}pt, its least {}pt, the page's choice",
                bar.tall, bar.least
            );
            (least, under)
        })
    }

    /// THE SCREENS OF A FRAME DRAWN IN `chrome`: the reach `key` names in
    /// the chrome the page chooses, and where the frame drew another (one
    /// it held), the reach in the frame's, with the page's as the choice
    /// the code had too, as the bar at its least is ([`Sweep::at_least`]).
    fn framed(&self, key: &ReachOf, chrome: PageChrome) -> (Arc<Reach>, Option<Arc<Reach>>) {
        let chosen = self.reach(key);
        if chosen.chrome == chrome {
            return (chosen, None);
        }
        let held = self.reach(&ReachOf {
            chrome: Some(Chrome(chrome)),
            ..key.clone()
        });
        (held, Some(chosen))
    }

    /// The reach `key` names, where some frame has had it measured.
    fn measured(&self, key: &ReachOf) -> Option<Arc<Reach>> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .and_then(|cell| cell.get().cloned())
    }

    /// R2 AND R5 OF A FRAME THAT SHOWS `now` of the keyboard's control
    /// where it owes `owed`, drawn in `chrome`: `Err` names a screen that
    /// shows it so (in the chrome the page chooses and under the bar at its
    /// least too) or, where none does, one that shows more of it than the
    /// frame; `Ok` names the geometry that excepts it.
    fn judge(
        &self,
        key: &ReachOf,
        chrome: PageChrome,
        now: Kept,
        owed: Kept,
    ) -> Result<String, String> {
        let (reach, chosen) = self.framed(key, chrome);
        if let Some(scroll) = reach.covering(owed) {
            return Err(format!("though the screen at scroll {scroll} shows it so"));
        }
        if let Some(scroll) = chosen.as_ref().and_then(|chosen| chosen.covering(owed)) {
            return Err(format!(
                "the page held its chrome, {:?}: in the one it chooses, {:?}, the screen at \
                 scroll {scroll} shows it so",
                chrome,
                chosen.as_ref().map(|chosen| chosen.chrome)
            ));
        }
        let (base, under) = match self.at_least(key, &reach) {
            Some((least, under)) => {
                if let Some(scroll) = least.covering(owed) {
                    return Err(format!(
                        "{under}: at its least the screen at scroll {scroll} shows it so"
                    ));
                }
                (least, format!(", with {under}"))
            }
            None if reach.bar.is_some_and(|bar| bar.docked) => {
                (reach, ", the status docked in the toolbar".to_string())
            }
            None => (reach, String::new()),
        };
        if let Some((scroll, most)) = base.most()
            && !now.covers(most)
        {
            return Err(format!(
                "no screen shows it so, but the screen at scroll {scroll} shows {most} of \
                 it{under}: not the screen that shows the most"
            ));
        }
        base.cause(|screen| screen.control_at, owed)
            .map(|cause| format!("{cause}{under}"))
    }

    fn measure(&self, key: &ReachOf) -> Reach {
        let drawn = key.drawn;
        STATUS_BAR_LEAST.with(|least| least.set(key.least));
        PAGE_CHROME_HELD.with(|held| held.set(key.chrome.map(|chrome| chrome.0)));
        let (mut runtime, instance, view) = self.opened(drawn);
        if Held::of(view_state(&mut runtime, view)) != key.held {
            key.held.put(view_state(&mut runtime, view));
        }
        if key.keyboard != self.control
            && let Some(keyboard) = key.keyboard.as_deref()
        {
            let _ = dispatch(
                &mut runtime,
                instance,
                view,
                AppEvent::FocusChanged(Some(UiKey::new(keyboard))),
            );
        }
        let state = view_state(&mut runtime, view);
        if !key.filter.is_empty() {
            state.showcase.filter =
                crate::native_text_input::TextInputState::new(key.filter.clone());
            state.invalidate_result_page_limit();
        }
        if let Some(id) = key.trial.as_deref() {
            let _ = state.highlight_card(id);
        }
        let first = draw(&mut runtime, instance, view, drawn);
        let control = key.keyboard.as_deref();
        let mut reach = Reach {
            screens: BTreeMap::new(),
            strays: Vec::new(),
            looked: BTreeSet::new(),
            limit: 0,
            list: false,
            bar: None,
            elsewhere: None,
            chrome: first.chrome,
            listed: None,
        };
        // The plan a held screen measures, which the first, drawn before the
        // reveal held the list, need not.
        let held = std::cell::RefCell::new(None);
        let mut look = |top: usize, reach: &mut Reach| -> Option<Screenful> {
            if !reach.looked.insert(top) {
                let at = reach
                    .strays
                    .iter()
                    .find(|(asked, _)| *asked == top)
                    .map_or(Some(top), |(_, at)| *at);
                return reach.screens.get(&at?).copied();
            }
            if first.list {
                let state = view_state(&mut runtime, view);
                state.scroll_page_to(top);
                state.page_reveal.clone_from(&key.reveal);
                state.reveal_as = scroll::Reveal::Hold;
                state.drop_list_nudge();
            }
            let frame = draw(&mut runtime, instance, view, drawn);
            self.screens.fetch_add(1, Ordering::Relaxed);
            if reach.looked.len() == 1 {
                reach.chrome = frame.chrome;
            }
            let at = if first.list { frame.top } else { Some(0) };
            if at != Some(top) {
                reach.strays.push((top, at));
            }
            // The bar as the reveal sizes it, which no scroll changes.
            if reach.bar.is_none() {
                reach.bar = StatusBar::of(&mut runtime, view, drawn, &frame.screen);
            }
            reach.list |= frame.paints;
            held.borrow_mut()
                .get_or_insert_with(|| frame.plan.clone().filter(|_| frame.list));
            let screen = Screenful {
                control: control.map(|key| frame.kept(key)),
                landing: frame.in_list(&self.landing),
                first_row: frame
                    .first_row
                    .as_deref()
                    .is_some_and(|row| frame.in_list(row)),
                control_at: control.and_then(|key| frame.placed(key)),
                landing_at: frame.placed(&self.landing),
                first_row_at: frame.first_row.as_deref().and_then(|row| frame.placed(row)),
                room: frame.room,
                list_top: frame.list_top,
            };
            Some(*reach.screens.entry(at?).or_insert(screen))
        };
        let _ = look(0, &mut reach);
        let plan = held.borrow_mut().take().flatten();
        reach.listed = plan
            .as_ref()
            .map(|plan| (plan.len(), plan.span(0..plan.len())));
        let limit = plan.as_ref().map_or(0, scroll::ScrollPlan::limit);
        reach.limit = limit;
        let listed =
            |row: Option<&str>| row.and_then(|row| plan.as_ref()?.stop_of(&UiKey::new(row)));
        let scans: [(Option<usize>, Placed); 3] = [
            (listed(control), |screen| screen.control_at),
            (listed(Some(&self.landing)), |screen| screen.landing_at),
            (listed(first.first_row.as_deref()), |screen| {
                screen.first_row_at
            }),
        ];
        // Out from the screen that starts at each listed stop, while the
        // stop's rect lies in the room and its rows move up as the scroll
        // goes on; where they do not, or a scroll draws no screen of its
        // own, every scroll.
        let mut every = plan.is_none();
        for (stop, at) in scans {
            let Some(stop) = stop else {
                continue;
            };
            let start = stop.min(limit);
            for ahead in [false, true] {
                let mut last: Option<f32> = None;
                let mut top = start;
                loop {
                    let Some(screen) = look(top, &mut reach) else {
                        every = true;
                        break;
                    };
                    if let Some((under, tall)) = at(&screen) {
                        every |= last.is_some_and(|last| {
                            if ahead {
                                under > last + 0.5
                            } else {
                                under < last - 0.5
                            }
                        });
                        last = Some(under);
                        let past = if ahead {
                            under + tall <= 0.5
                        } else {
                            under >= screen.room - 0.5
                        };
                        if past {
                            break;
                        }
                    }
                    match (ahead, top) {
                        (true, top) if top >= limit => break,
                        (false, 0) => break,
                        (true, _) => top += 1,
                        (false, _) => top -= 1,
                    }
                }
            }
        }
        // A control the list does not hold, pinned around it, shows alike on
        // every screen: the list's last screen too, and every scroll where
        // the screens drawn show it otherwise or list it.
        let pinned = control.is_some() && scans[0].0.is_none();
        if pinned {
            let _ = look(limit, &mut reach);
            let one = reach
                .screens
                .values()
                .next()
                .and_then(|screen| screen.control);
            every |= reach
                .screens
                .values()
                .any(|screen| screen.control != one || screen.control_at.is_some());
        }
        if every {
            for top in 0..=limit {
                let _ = look(top, &mut reach);
            }
        }
        reach.elsewhere = if pinned {
            reach
                .screens
                .values()
                .next()
                .and_then(|screen| screen.control)
        } else {
            control.map(|_| Kept::Part(0.0))
        };
        STATUS_BAR_LEAST.with(|least| least.set(false));
        PAGE_CHROME_HELD.with(|held| held.set(None));
        reach
    }
}

#[derive(Default)]
struct Findings {
    faults: Vec<(Rule, String)>,
    exceptions: Vec<String>,
    /// Each frame a reach of its own key draws another list for, and how
    /// many frames were held to one.
    unlike: Vec<String>,
    compared: usize,
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
        first_row: state
            .showcase
            .listed()
            .first()
            .map(|slug| format!("{ROW}{slug}")),
        chrome: state.page_chrome.get(),
        screen,
    }
}

/// The theme on trial, if one is.
fn trying(state: &SettingsViewState) -> Option<String> {
    match &state.showcase.trial {
        ShowcaseTrial::Trying { id } => Some(id.clone()),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug)]
struct Left {
    kept: Kept,
    at: usize,
    drawn: Drawn,
}

struct Wheeled {
    at: usize,
    drawn: Drawn,
    screen: crate::native_ui::CompiledUi,
    first: Option<UiKey>,
    kept: Option<Kept>,
    place: Option<(scroll::ScrollPlan, usize)>,
    /// The keyboard's control, where its stop is one the screen shows
    /// ([`scroll::ScrollPlan::shown`]), and how many stops under its first:
    /// what holds the place ([`SettingsViewState::seat_scroll`]).
    held: Option<(String, usize)>,
}

/// How far from `place` the nearest scroll up to `limit` is whose screen
/// `shows` what R2 asks, and those that do, where `place`'s does not.
fn nearest(
    limit: usize,
    place: usize,
    shows: impl Fn(usize) -> bool,
) -> Option<(usize, Vec<usize>)> {
    if shows(place) {
        return None;
    }
    let showing = (0..=limit)
        .filter(|&scroll| shows(scroll))
        .collect::<Vec<_>>();
    let far = showing.iter().map(|scroll| scroll.abs_diff(place)).min()?;
    Some((far, showing))
}

#[expect(
    clippy::too_many_lines,
    reason = "the five rules read each frame together, each with what it keeps of the walk"
)]
fn walk(sweep: &Sweep, start: Drawn, steps: &[Step]) -> Findings {
    let scenario = sweep.scenario;
    let landing = sweep.landing.as_str();
    let mut found = Findings::default();
    let (mut runtime, instance, view) = sweep.opened(start);
    // What the view holds as the scenario opened it.
    let opened = Held::of(state(&runtime, view));
    let mut control = sweep.control.clone();
    let mut opening = !scenario.keyboard.pressed_on_drawn();
    let mut last: Option<Frame> = None;
    let mut left: Option<Left> = None;
    let mut wheeled: Option<Wheeled> = None;
    for at in 0..=steps.len() {
        let step = at.checked_sub(1).map(|index| steps[index]);
        let revision = u64::try_from(at).unwrap_or(u64::MAX).saturating_add(1);
        let filter_before = state(&runtime, view).showcase.filter.value().to_string();
        let event = match step {
            Some(Step::Wheel(lines)) => Some(AppEvent::ScrollAt {
                lines,
                key: UiKey::new(LIST),
            }),
            Some(Step::Focus) => Some(AppEvent::FocusChanged(Some(UiKey::new(
                control.as_deref().unwrap_or(landing),
            )))),
            Some(Step::Up) => Some(AppEvent::StepFocus(-1)),
            Some(Step::Down) => Some(AppEvent::StepFocus(1)),
            Some(Step::Type) => Some(AppEvent::TextInput(TextInputEvent::Commit("o".to_string()))),
            Some(Step::Erase) => Some(AppEvent::TextInput(TextInputEvent::Backspace)),
            Some(Step::Left) => Some(AppEvent::TextInput(TextInputEvent::Left { extend: false })),
            Some(Step::Right) => Some(AppEvent::TextInput(TextInputEvent::Right { extend: false })),
            Some(Step::Tick) => Some(AppEvent::MessagesChanged { revision }),
            Some(Step::Refresh) => Some(AppEvent::PackagesChanged { revision }),
            _ => None,
        };
        if let Some(event) = event {
            let _ = dispatch(&mut runtime, instance, view, event);
        }
        // The keyboard where the person's event left it: put on a control
        // it was not on, or left on the one it was.
        let mut moved = false;
        if step.is_some_and(Step::persons) {
            let now = state(&runtime, view)
                .common
                .last_focus
                .as_ref()
                .map(|key| key.as_str().to_string())
                .filter(|key| !key.starts_with("settings/nav/"));
            if now.is_some() && now != control {
                moved = true;
                control = now;
            }
            opening = false;
            wheeled = None;
        }
        let typed = state(&runtime, view).showcase.filter.value() != filter_before;
        let drawn = last
            .as_ref()
            .zip(step)
            .map_or(start, |(last, step)| last.drawn.after(step));
        let frame = draw(&mut runtime, instance, view, drawn);
        let bar = StatusBar::of(&mut runtime, view, drawn, &frame.screen);
        found.frames += 1;
        let walked_state = state(&runtime, view);
        let reveal = walked_state.page_reveal.clone();
        let reach_of = ReachOf {
            drawn,
            keyboard: control.clone(),
            reveal: reveal.clone(),
            least: false,
            filter: walked_state.showcase.filter.value().to_string(),
            trial: trying(walked_state),
            held: Held::of(walked_state),
            chrome: None,
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
        // R2 and R5: what this frame owes the keyboard's control, and why.
        let owed = control.as_deref().and_then(|_| match step {
            None => Some((Kept::Whole, "its first screen".to_string(), Rule::R2)),
            Some(Step::Wheel(_)) => None,
            Some(_) if moved => Some((Kept::Whole, "the keyboard put on it".to_string(), Rule::R2)),
            Some(step) => left.map(|left| {
                (
                    left.kept,
                    format!("what frame {} at {} left the person", left.at, left.drawn),
                    if step.key() { Rule::R5 } else { Rule::R2 },
                )
            }),
        });
        if let (Some(key), Some((owed, why, rule))) = (control.as_deref(), owed.as_ref())
            && !frame.kept(key).covers(*owed)
        {
            let how = if frame.listed(key) {
                "listed"
            } else {
                "pinned"
            };
            match sweep.judge(&kept_reach_of, frame.chrome, frame.kept(key), *owed) {
                Ok(cause) => found
                    .exceptions
                    .push(format!("{rule:?} at {drawn}: {key} {owed}, {cause}")),
                Err(said) => fault(
                    *rule,
                    format!(
                        "{key} ({how}) {owed} as {why}, {} now ({:?} shown, tall), {said}",
                        frame.kept(key),
                        frame.shown(key)
                    ),
                ),
            }
        }
        // R5: a filter change shows the filter and then the first results,
        // wherever a screen shows the first result whole with the filter as
        // R5 asks.
        if typed
            && control.as_deref() == Some(FILTER)
            && frame.listed(FILTER)
            && let Some(first) = frame.first_row.as_deref()
            && !frame.in_list(first)
            && let Some((owed, _, _)) = owed.as_ref()
        {
            let (reach, chosen) = sweep.framed(&kept_reach_of, frame.chrome);
            let shows = |reach: &Reach| reach.first_row_with(reach.asked(*owed));
            let screen = shows(&reach)
                .map(|scroll| format!("the screen at scroll {scroll}"))
                .or_else(|| {
                    chosen.as_deref().and_then(shows).map(|scroll| {
                        format!("in the chrome the page chooses, the screen at scroll {scroll}")
                    })
                });
            if let Some(screen) = screen {
                fault(
                    Rule::R5,
                    format!(
                        "the filter changed, its first result {first} {:?} (shown, tall), not \
                         whole in the list, though {screen} shows it whole with the filter as R5 \
                         asks",
                        frame.shown(first)
                    ),
                );
            }
        }
        // control as R2 asks, the status's bar at its least — none does
        // where this one shows the control so, its bar at its least, and the
        // two lie further apart than the room.
        let apart = !bar.is_some_and(StatusBar::above_least)
            && owed
                .as_ref()
                .zip(control.as_deref())
                .is_some_and(|((owed, _, _), key)| {
                    frame.kept(key).covers(*owed) && frame.apart(landing, key, *owed)
                });
        if opening && !apart && frame.listed(landing) && frame.chooser() && !frame.in_list(landing)
        {
            let (reach, chosen) = sweep.framed(&reach_of, frame.chrome);
            let asks = |reach: &Reach| owed.as_ref().map(|(owed, _, _)| reach.asked(*owed));
            let shows = |reach: &Reach| reach.landing_with(asks(reach));
            let mut least = None;
            let both = shows(&reach)
                .map(|scroll| format!("the screen at scroll {scroll}"))
                .or_else(|| {
                    chosen.as_deref().and_then(shows).map(|scroll| {
                        format!("in the chrome the page chooses, the screen at scroll {scroll}")
                    })
                })
                .or_else(|| {
                    least = sweep.at_least(&reach_of, &reach);
                    least.as_ref().and_then(|(least, under)| {
                        shows(least).map(|scroll| {
                            format!("{under}: at its least the screen at scroll {scroll}")
                        })
                    })
                });
            let base = least.as_ref().map_or(&reach, |(least, _)| least);
            if let Some(both) = both {
                fault(
                    Rule::R3,
                    format!(
                        "before the person's first event, the theme in use's {landing} is {:?} \
                         (shown, tall), not whole in the list, though {both} shows it whole with \
                         the keyboard's control as R2 asks ({})",
                        frame.shown(landing),
                        asks(base).map_or("nothing".to_string(), |owed| format!(
                            "{owed} of {}",
                            control.as_deref().unwrap_or_default()
                        ))
                    ),
                );
            } else if base.landing_with(None).is_none() {
                match base.cause(|screen| screen.landing_at, Kept::Whole) {
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
        // than R2 asks (what it owes where a screen shows that, else the most
        // a screen shows), the nearest of the screens that show it so.
        if let Some(wheel) = wheeled.as_ref()
            && drawn == wheel.drawn
            && frame.screen != wheel.screen
        {
            let needs = owed
                .as_ref()
                .filter(|(owed, _, _)| !wheel.kept.is_some_and(|kept| kept.covers(*owed)))
                .map(|(owed, _, _)| {
                    let reach = sweep.framed(&kept_reach_of, frame.chrome).0;
                    let asked = reach.asked(*owed);
                    (reach, asked)
                })
                .filter(|(_, asked)| !wheel.kept.is_some_and(|kept| kept.covers(*asked)));
            let held = needs.is_some_and(|(reach, asked)| {
                let shows = |scroll: usize| {
                    reach
                        .control_at(scroll)
                        .is_some_and(|kept| kept.covers(asked))
                };
                match (wheel.place.as_ref(), frame.top) {
                    (Some((plan, place)), Some(top)) => {
                        match nearest(plan.limit(), *place, shows) {
                            Some((far, _)) => shows(top) && top.abs_diff(*place) == far,
                            None => top == *place,
                        }
                    }
                    _ => frame.top.is_some_and(shows),
                }
            });
            if !held {
                fault(
                    Rule::R4,
                    format!(
                        "back where the wheel turned (frame {}), another screen: it starts at \
                         {:?} (scroll {:?}), the wheel's at {:?}, and it is not the screen \
                         nearest the wheel's that shows the keyboard's control as R2 asks",
                        wheel.at,
                        frame.first(),
                        frame.top,
                        wheel.first,
                    ),
                );
            }
        }
        // [`SettingsViewState::seat_scroll`] reads it: by the keyboard's
        // control where the wheel's screen showed its stop, as many stops
        // the list's top where the wheel left it at its top. But where that
        // screen shows the keyboard's control less than R2 asks and another
        // shows it so, the nearest of those: no further than R2 needs.
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
            && let asks = owed.as_ref().map(|(owed, _, _)| *owed)
            && !(frame.top == Some(place)
                && asks
                    .zip(control.as_deref())
                    .is_none_or(|(owed, key)| frame.kept(key).covers(owed)))
        {
            let reach = asks.map(|owed| {
                let reach = sweep.framed(&kept_reach_of, frame.chrome).0;
                let asked = reach.asked(owed);
                (reach, asked)
            });
            let shows = |scroll: usize| {
                reach.as_ref().is_none_or(|(reach, asked)| {
                    reach
                        .control_at(scroll)
                        .is_some_and(|kept| kept.covers(*asked))
                })
            };
            let nearer = nearest(plan.limit(), place, shows);
            let held = match nearer.as_ref() {
                Some((far, _)) => frame
                    .top
                    .is_some_and(|top| shows(top) && top.abs_diff(place) == *far),
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
                        nearer.map_or(String::new(), |(far, showing)| format!(
                            ", or {far} from it where R2 needs the move: the screens that show \
                             the keyboard's control as it asks are {showing:?}"
                        ))
                    ),
                );
            }
        }
        // A REACH DRAWS THE FRAME'S LIST: one measured for this frame's key,
        // by any walk, lists the stops the frame lists, as tall, and at the
        // frame's scroll leaves the list the frame's room where the frame
        // has it — else its key left out something the view holds, and it
        // judges frames by another page's screens. Measured for every frame
        // whose view holds what the scenario did not open it with. Under a
        // status the bar sizes itself by the control's seat, which a held
        // screen need not share, and a reach for another reveal sizes it
        // for that.
        if frame.list
            && !drawn.status
            && reach_of == kept_reach_of
            && let Some(top) = frame.top
            && let Some(reach) = if reach_of.held == opened {
                sweep
                    .measured(&kept_reach_of)
                    .filter(|reach| reach.chrome == frame.chrome)
                    .or_else(|| {
                        sweep.measured(&ReachOf {
                            chrome: Some(Chrome(frame.chrome)),
                            ..kept_reach_of.clone()
                        })
                    })
            } else {
                Some(sweep.framed(&kept_reach_of, frame.chrome).0)
            }
        {
            found.compared += 1;
            let listed = frame
                .plan
                .as_ref()
                .map(|plan| (plan.len(), plan.span(0..plan.len())));
            let same_list = reach
                .listed
                .zip(listed)
                .is_some_and(|((was, tall), (now, height))| {
                    was == now && (tall - height).abs() < 0.5
                });
            let same_room = reach.screens.get(&top).is_none_or(|screen| {
                (screen.room - frame.room).abs() < 0.5
                    && (screen.list_top - frame.list_top).abs() < 0.5
            });
            if !same_list || !same_room {
                found.unlike.push(format!(
                    "{at_frame}: the reach of its key lists {:?} (stops, tall), the frame {listed:?}; \
                     at scroll {top} it leaves the list {:?} (room, top), the frame ({}, {})",
                    reach.listed,
                    reach
                        .screens
                        .get(&top)
                        .map(|screen| (screen.room, screen.list_top)),
                    frame.room,
                    frame.list_top
                ));
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
                    .filter(|_| frame.list)
                    .zip(frame.plan.as_ref().zip(frame.top))
                    .and_then(|(key, (plan, top))| {
                        let stop = plan.stop_of(&UiKey::new(key))?;
                        plan.shown(top)
                            .contains(&stop)
                            .then(|| Some((key.to_string(), stop.checked_sub(top)?)))
                            .flatten()
                    }),
            });
        }
        last = Some(frame);
    }
    found
}

fn distinct_windows(sweep: &Sweep, scale: u8) -> Vec<Size> {
    let mut seen: Vec<Vec<Option<scroll::ScrollPlan>>> = Vec::new();
    let mut windows = Vec::new();
    for window in WINDOWS {
        let start = Drawn::bare(window, scale);
        let (mut runtime, instance, view) = sweep.opened(start);
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

/// THE WALK FROM `start`: through each of `windows` but its own, the other
/// text size, the warning and a status, and back, in each of the ways below
/// that `taken` names, and the keys on the keyboard's control. ONE WALK, so a
/// start opens one view: its ways in an order that leaves each the keyboard
/// where a walk of its own would start it — nothing of the person's before
/// the frames drawn again, the keyboard on the scenario's control (off the
/// list, nowhere) for the wheels, the keys that move it last. Each change is
/// drawn again, by nothing or by a reply the person did not cause; a wheel's
/// place is held through two changes and back, one of them drawn again (each
/// change is the one in the walks from half the windows). A WHEEL, OR THE KEYBOARD PUT
/// ON ITS CONTROL, IN ANOTHER WINDOW and back here is walked from that
/// window's own start, which wheels and puts the keyboard there and goes to
/// every window from it: only the text size and the bars, whose changed
/// state no start of a walk is, are wheeled and focused in from here.
fn walk_from(start: Drawn, windows: &[Size], keyboard: Keyboard, taken: Walks) -> Vec<Step> {
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
    // Which change of each wheel's pair is drawn again: the first in the
    // walks from every other window, the second in the rest.
    let carried = windows
        .iter()
        .position(|&window| window == start.window)
        .unwrap_or_default()
        % 2;
    for (index, change) in changes.enumerate() {
        let lines = if index % 4 < 2 { 4 } else { -4 };
        let back = change.back(start);
        let drawn_again = match index % 4 {
            0 => Step::Tick,
            2 => Step::Refresh,
            _ => Step::Again,
        };
        again.extend([change, drawn_again, back]);
        if index % 2 == 0 {
            wheel.push(Step::Wheel(lines));
        }
        wheel.push(change);
        if index % 2 == carried {
            wheel.push(Step::Again);
        }
        wheel.push(back);
        focus.extend([change, back]);
        if !matches!(change, Step::Window(_)) {
            there.extend([change, Step::Wheel(lines), back]);
            refocus.extend([Step::Wheel(lines), change, Step::Focus, back]);
        }
    }
    let mut walk = again;
    if taken == Walks::Opening {
        walk.extend(focus);
        return walk;
    }
    walk.extend(wheel);
    if keyboard != Keyboard::Off {
        walk.extend(there);
    }
    walk.extend(refocus);
    walk.extend(focus);
    // R2 under a status: the bar over the warning and alone, the keyboard
    // put on its control under it, and gone, where a short window's room is
    // shorter than the control.
    walk.extend([
        Step::Focus,
        Step::Warning,
        Step::Status,
        Step::Again,
        Step::Status,
        Step::Status,
        Step::Warning,
        Step::Focus,
        Step::Status,
    ]);
    // R5: the keys, each drawn again, and the filter typed and erased.
    walk.extend([
        Step::Down,
        Step::Again,
        Step::Up,
        Step::Again,
        Step::Up,
        Step::Type,
        Step::Again,
        Step::Erase,
        Step::Again,
    ]);
    walk
}

/// `count` walks of `length` steps from random windows, those the sweep
/// draws and odd ones, each step drawn at random from every kind, seeded so
/// the walks are the same each run.
fn random_walks(seed: u64, count: usize, length: usize) -> Vec<(Drawn, Vec<Step>)> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = move |below: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        usize::try_from(state % u64::try_from(below).unwrap_or(u64::MAX)).unwrap_or_default()
    };
    let all = WINDOWS
        .iter()
        .chain(&ODD_WINDOWS)
        .copied()
        .collect::<Vec<_>>();
    (0..count)
        .map(|_| {
            let start = Drawn::bare(all[next(all.len())], [1, 2][next(2)]);
            let steps = (0..length)
                .map(|_| match next(100) {
                    0..11 => Step::Again,
                    11..36 => Step::Window(all[next(all.len())]),
                    36..45 => Step::Text,
                    45..53 => Step::Warning,
                    53..62 => Step::Status,
                    62..80 => Step::Wheel([1, -1, 2, -2, 3, -3, 4, -4, 8, -8, 12, -12][next(12)]),
                    80..86 => Step::Focus,
                    86..95 => [Step::Up, Step::Down, Step::Type, Step::Erase][next(4)],
                    95..98 => Step::Tick,
                    _ => Step::Refresh,
                })
                .collect();
            (start, steps)
        })
        .collect()
}

#[expect(clippy::too_many_lines, reason = "each review's sequences, as found")]
fn reviewed() -> Vec<(Scenario, Drawn, Vec<Step>)> {
    use Step::{
        Again, Down, Erase, Focus, Left, Refresh, Right, Status, Text, Tick, Type, Up, Warning,
        Wheel, Window,
    };
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
    let row = Scenario {
        keyboard: Keyboard::Row("Dracula"),
        ..filter
    };
    let row_after = Scenario {
        keyboard: Keyboard::Row("Tokyo Night"),
        ..filter
    };
    let row_default = Scenario {
        keyboard: Keyboard::Row("Default"),
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
    let filter_tokyo = Scenario {
        theme: "Tokyo Night",
        in_use: "Tokyo Night",
        ..filter
    };
    let latte = Scenario {
        keyboard: Keyboard::Row("Catppuccin Latte"),
        ..drawn
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
        // A status over a pinned control (the filter, and
        // the strip's Done beside the sheet), which the sweep counted whole
        // by its clipped rect.
        (filter, at(w444x420, 2), vec![Focus, Status]),
        (filter, at(w444x420, 2), vec![Wheel(8), Status]),
        (first, at((640, 480), 2), vec![Status]),
        // A status that took the keyboard's control out of
        // view, the list sent back to a place no wheel had put it.
        (closed, at(w444x292, 2), vec![Window((600, 200)), Status]),
        (row, at((600, 200), 1), vec![Window((380, 300)), Status]),
        // The keyboard on the row next to the theme in
        // use's before the first frame, which a screen shows with it (R3),
        // under the bar at its least too.
        (row, at(w1284x714, 2), vec![Again]),
        (row, at(w178x348, 1), vec![Status, Text]),
        // Reaches in windows no rule was tuned on.
        (
            drawn,
            at((380, 300), 1),
            vec![Wheel(4), Status, Again, Status, Wheel(-4), Status],
        ),
        // The person's own keys, the keyboard left on its
        // control (R5).
        (filter, at((230, 420), 2), vec![Up]),
        (closed, at(w300x160, 1), vec![Up]),
        (side, at(w444x180, 1), vec![Up, Up]),
        (filter, at(w178x348, 1), vec![Down, Type]),
        // The keyboard on a row by the theme in use's
        // before the first frame, a landing the keyboard's row held off the
        // screen on a frame measured again (R3).
        (
            row_after,
            at(w1284x714, 2),
            vec![Window(w178x348), Window(w1284x714)],
        ),
        (row_after, at(w584x348, 2), vec![Text]),
        (row_after, at(w178x348, 2), vec![Text]),
        (row_default, at(w444x292, 2), vec![Window((980, 420))]),
        (pack, at((1_440, 900), 2), vec![Text]),
        // A status over a row taller than the room, which
        // the bar at its least shows 64pt of, its words whole (R2).
        (row_after, at(w178x348, 2), vec![Focus, Warning, Status]),
        (row_after, at(w178x348, 2), vec![Warning, Status, Focus]),
        (closed, at(w178x348, 2), vec![Focus, Warning, Status]),
        (closed, at(w178x348, 2), vec![Status, Status]),
        // A filter change that held the chrome the short
        // list of results had chosen, the long list back with less room (R5).
        (
            filter,
            at(w444x292, 1),
            vec![Type, Window((380, 300)), Erase],
        ),
        (
            filter,
            at(w444x180, 1),
            vec![Type, Window((380, 300)), Erase],
        ),
        (
            filter,
            at(w584x348, 1),
            vec![Type, Window((380, 300)), Erase],
        ),
        (
            filter,
            at(w444x292, 1),
            vec![Warning, Type, Window((380, 300)), Erase],
        ),
        (
            filter,
            at(w444x292, 1),
            vec![Wheel(4), Type, Window((380, 300)), Erase],
        ),
        (
            filter_tokyo,
            at(w444x292, 1),
            vec![Type, Window((380, 300)), Erase],
        ),
        (
            filter_tokyo,
            at(w584x348, 1),
            vec![Down, Type, Window((380, 300)), Erase, Again, Warning],
        ),
        // A long name wrapped at 2× text, its row in the
        // list's faded foot, which the code counted shown and the paint did
        // not (R3, R4).
        (latte, at((330, 260), 1), vec![Text, Status]),
        (
            latte,
            at(w178x348, 1),
            vec![
                Wheel(-2),
                Window((1_440, 900)),
                Status,
                Again,
                Window(w444x292),
                Again,
                Again,
                Window((980, 420)),
                Window((380, 300)),
                Again,
                Text,
                Again,
                Window((330, 260)),
            ],
        ),
        (
            latte,
            at((480, 360), 2),
            vec![
                Status,
                Warning,
                Wheel(-4),
                Window(w864x500),
                Refresh,
                Window((200, 300)),
                Window((1_000, 600)),
                Wheel(1),
                Window(w178x348),
            ],
        ),
        // Up on Customize, which the last screen showed
        // cut, scrolled it out of view (R5).
        (closed, at((600, 200), 2), vec![Status, Focus, Up]),
        (
            closed,
            at(w444x292, 2),
            vec![Window((600, 200)), Status, Focus, Up],
        ),
        // The wheel's place over a re-layout, the filter
        // its screen listed (R4).
        (
            filter_tokyo,
            at((300, 292), 2),
            vec![
                Status,
                Window((300, 292)),
                Window((720, 540)),
                Window(w584x348),
                Status,
                Left,
                Status,
                Wheel(-3),
                Type,
                Focus,
                Wheel(-4),
                Again,
                Again,
                Warning,
            ],
        ),
        // What a view holds past the scenario's opening
        // that lays the page out — the Community disclosure Left shuts and
        // Right opens, a draft typed in the sheet's foreground field — which
        // a reach must hold too ([`Held`]).
        (
            pack,
            at(w444x292, 1),
            vec![
                Left,
                Again,
                Status,
                Window(w864x500),
                Right,
                Again,
                Text,
                Left,
                Wheel(-4),
                Window(w444x180),
                Status,
            ],
        ),
        (
            pack,
            at(w178x348, 2),
            vec![
                Left,
                Warning,
                Window(w1095x700),
                Right,
                Wheel(8),
                Window(w178x348),
            ],
        ),
        (
            first,
            at(w444x292, 1),
            vec![
                Down,
                Type,
                Again,
                Status,
                Window(w444x180),
                Text,
                Again,
                Erase,
                Window(w864x500),
                Warning,
            ],
        ),
        (
            drawn,
            at(w178x348, 2),
            vec![
                Down,
                Type,
                Type,
                Window(w584x348),
                Wheel(4),
                Window(w178x348),
            ],
        ),
        // A control the room holds, whole on no screen of
        // a list that scrolls by stops — foreground under the caption COLORS
        // repeats over every screen of its rows (R2, R5), and under a status
        // the built-in section's caption over its first theme, a stop grouped
        // for the room before the bar, 64pt in 48pt (R2, R3); Latte under a
        // status is the long name's first walk above.
        (drawn, at(w444x292, 2), vec![Down, Window(w444x180)]),
        (drawn, at(w444x180, 2), vec![Down, Again]),
        (drawn, at(w178x348, 2), vec![Down, Type]),
        (off, at(w300x160, 1), vec![Status, Again]),
        (row_default, at(w300x160, 1), vec![Status, Again]),
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

/// The workers each scenario's walks share: more than other sweeps', so the
/// scenario whose list is longest (Pack's) is not the run's long pole.
const KEEPING_WORKERS: usize = 12;

/// the walks through every change and back ([`walk_from`]) that `taken`
fn sweep(scenario: Scenario, taken: Walks) {
    let started = std::time::Instant::now();
    let sweep = Sweep::new(scenario);
    let mut walks = Vec::new();
    let mut starts = Vec::new();
    for scale in [1, 2].into_iter().filter(|_| taken != Walks::Reviewed) {
        let windows = distinct_windows(&sweep, scale);
        for &window in &windows {
            let start = Drawn::bare(window, scale);
            starts.push(start.to_string());
            walks.push((start, walk_from(start, &windows, scenario.keyboard, taken)));
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
    on_n_workers(
        KEEPING_WORKERS,
        &walks,
        "keeping walks",
        |(start, steps)| {
            let walked = walk(&sweep, *start, steps);
            let mut found = found.lock().unwrap_or_else(PoisonError::into_inner);
            found.faults.extend(walked.faults);
            found.exceptions.extend(walked.exceptions);
            found.unlike.extend(walked.unlike);
            found.compared += walked.compared;
            found.frames += walked.frames;
        },
    );
    let mut found = found.into_inner().unwrap_or_else(PoisonError::into_inner);
    found.faults.sort_by_key(|(rule, _)| *rule);
    let mut excepted = found.exceptions.clone();
    excepted.sort_unstable();
    excepted.dedup();
    let (reaches, in_held) = {
        let seen = sweep.seen.lock().unwrap_or_else(PoisonError::into_inner);
        (
            seen.len(),
            seen.keys().filter(|key| key.chrome.is_some()).count(),
        )
    };
    eprintln!(
        "{scenario}: {} walks from {starts:?}, {} frames ({} held to the reach of their key) and \
         {reaches} reaches ({in_held} in a chrome the page held; {} screens) in {:?}; geometry \
         excepted {} frames:\n  {}",
        walks.len(),
        found.frames,
        found.compared,
        sweep.screens.load(Ordering::Relaxed),
        started.elapsed(),
        found.exceptions.len(),
        excepted.join("\n  ")
    );
    assert!(
        found.unlike.is_empty(),
        "{scenario}: {} frames are judged by reaches that draw another list:\n{}",
        found.unlike.len(),
        found
            .unlike
            .iter()
            .take(6)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    let rules = [Rule::R1, Rule::R2, Rule::R3, Rule::R4, Rule::R5];
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
fn the_keeping_rules_hold_with_the_keyboard_on_the_row_next_to_the_theme_in_use() {
    sweep(
        Scenario {
            keyboard: Keyboard::Row("Dracula"),
            theme: "Nord",
            in_use: "Nord",
        },
        Walks::Every(8),
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_the_row_after_the_theme_in_use() {
    sweep(
        Scenario {
            keyboard: Keyboard::Row("Tokyo Night"),
            theme: "Nord",
            in_use: "Nord",
        },
        Walks::Every(9),
    );
}

#[test]
fn the_keeping_rules_hold_before_the_first_event_with_the_keyboard_on_default() {
    sweep(
        Scenario {
            keyboard: Keyboard::Row("Default"),
            theme: "Nord",
            in_use: "Nord",
        },
        Walks::Opening,
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_a_long_name_wrapped_at_2x() {
    sweep(
        Scenario {
            keyboard: Keyboard::Row("Catppuccin Latte"),
            theme: "GitHub Light",
            in_use: "GitHub Light",
        },
        Walks::Opening,
    );
}

#[test]
fn the_keeping_rules_hold_on_the_filter_over_tokyo_night() {
    sweep(
        Scenario {
            keyboard: Keyboard::Filter,
            theme: "Tokyo Night",
            in_use: "Tokyo Night",
        },
        Walks::Reviewed,
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
        Walks::Opening,
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
        Walks::Opening,
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
        Walks::Opening,
    );
}
