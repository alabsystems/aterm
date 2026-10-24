// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keeping-rule sweep (R1–R5, stated on [`SettingsViewState::seat_scroll`]) over Settings ▸ Themes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use super::*;

type Size = (u16, u16);

/// The windows the walks start from: no two lay a scenario's list out alike.
const WINDOWS: [Size; 10] = [
    (178, 348),
    (300, 160),
    (444, 180),
    (444, 292),
    (444, 420),
    (584, 348),
    (864, 292),
    (864, 500),
    (1_095, 700),
    (1_284, 714),
];

/// Windows no rule was tuned on, which the random walks draw too.
const ODD_WINDOWS: [Size; 13] = [
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
    (1_268, 165),
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
const FOLLOW: &str = "settings/showcase/follow-system";
const OPEN_FOLDER: &str = "settings/showcase/open-folder";
const IMPORT: &str = "settings/showcase/import";
const SAVE_AS: &str = "settings/showcase/save-as";
const CURSOR_COLOR: &str = "settings/color-open/cursor_color";
const FOREGROUND: &str = "settings/control/foreground";
const PALETTE_FIRST: &str = "settings/showcase/palette/0";
const SEARCH: &str = "settings/search";
const BACKGROUND_WELL: &str = "settings/color-open/background";
const PALETTE_SLOT: &str = "settings/showcase/palette/9";
/// What the walks press: Customize, Done, a colour well, a palette slot and Save as.
const PRESSES: [&str; 5] = [CUSTOMIZE, DONE, BACKGROUND_WELL, PALETTE_SLOT, SAVE_AS];
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
    /// This control of the chooser, put there before the first frame.
    On(&'static str),
    /// This control of the Customize sheet, put there once Customize is
    /// pressed on the drawn page.
    Sheet(&'static str),
}

impl Keyboard {
    fn pressed_on_drawn(self) -> bool {
        matches!(
            self,
            Self::SheetDrawn | Self::SheetClosed | Self::Side | Self::Sheet(_)
        )
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
    /// The walks the keyboard's control decides ([`walk_from`]), and
    /// random walks.
    Control(u64),
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
    /// A press on this control, where the last frame laid it out.
    Press(&'static str),
}

impl Step {
    fn persons(self) -> bool {
        matches!(self, Self::Wheel(_) | Self::Focus | Self::Press(_)) || self.key()
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
            Self::Press(key) => write!(out, "Press({})", key.trim_start_matches("settings/")),
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
    /// under the list's top, off the screen too, its height, and how much of
    /// it the screen paints ([`Self::kept`]'s measure). `None` where it is not
    /// a stop of the list (pinned around it, or not drawn).
    fn placed(&self, key: &str) -> Option<Place> {
        let (rect, shown) = laid_of(&self.screen, key)?;
        self.listed(key)
            .then_some((rect.y - self.list_top, rect.height, shown))
    }

    /// The list it draws: how many stops, how tall in all.
    fn list_drawn(&self) -> Option<(usize, f32)> {
        self.plan
            .as_ref()
            .filter(|_| self.list)
            .map(|plan| (plan.len(), plan.span(0..plan.len())))
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
    control_at: Option<Place>,
    landing_at: Option<Place>,
    first_row_at: Option<Place>,
    room: f32,
    list_top: f32,
    /// The list it draws ([`Frame::list_drawn`]).
    listed: Option<(usize, f32)>,
    /// The chrome it drew in.
    chrome: Chrome,
}

/// EVERY SCREEN A LIST CAN DRAW THAT SHOWS WHAT A RULE READS, as a frame
/// measures it: the list drawn at its scrolls in a view opened as the walk's
/// was, in the same window, text size and bars, the keyboard on the same
/// control, the same filter and trial and the same reveal pending, held where
/// the scroll puts it — what geometry lets any keeping draw, measured on the
/// screens and never asked of the code that seats the list. Keyed by the
/// scroll each screen drew. ONLY WHERE NEEDED: around the listed stops its
/// rule reads (the keyboard's control, and the landing or the first row as
/// [`Reads`] says), out to the first screen on each side that lays it wholly
/// past the room; a list moves its rows up as it scrolls on, so no screen
/// past those shows it, which the screens drawn hold to ([`Sweep::measure`]:
/// where they do not, every scroll).
struct Reach {
    screens: BTreeMap<usize, Screenful>,
    /// Scrolls asked for that drew another, or none.
    strays: Vec<(usize, Option<usize>)>,
    /// The scrolls it looked at.
    looked: BTreeSet<usize>,
    limit: usize,
    list: bool,
    bar: Option<StatusBar>,
    /// Whether a status docked in the compact toolbar on its screens.
    docked: bool,
    /// Whether the keyboard's control is pinned around the list, not a stop
    /// of it.
    pinned: bool,
    /// How much of the keyboard's control the screens not drawn show: none
    /// of a listed one (wholly past the room), as much as every drawn screen
    /// does of one pinned around the list.
    elsewhere: Option<Kept>,
    /// The chrome its screens are held in ([`PAGE_CHROME_HELD`]): the one
    /// the key names, else the one the page chose for its first frame.
    chrome: PageChrome,
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

    /// Whether the page drew it otherwise than at its least: taller, or
    /// docked in the toolbar, where it takes the toolbar's controls' place.
    fn not_least(self) -> bool {
        self.docked || self.tall > self.least + 0.5
    }
}

/// EVERY CHROME THE PAGE CAN DRAW A FRAME AT `drawn` IN: each rung of its
/// ladder, and under a bar each with the bars' room given.
fn rungs(drawn: Drawn) -> Vec<PageChrome> {
    set_text_scale(f32::from(drawn.scale));
    let cx = cx_of(drawn);
    let bars = drawn.warning || drawn.status;
    PageChrome::ladder(SettingsWidth::for_viewport(cx.viewport.width), cx.viewport)
        .into_iter()
        .flat_map(|rung| [Some(rung), bars.then(|| rung.giving_bar_room())])
        .flatten()
        .collect()
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

    /// The scrolls whose screens were drawn in another chrome than the
    /// reach's: none, or the reach measures no one page.
    fn mixed(&self) -> Vec<usize> {
        self.screens
            .iter()
            .filter(|(_, screen)| screen.chrome != Chrome(self.chrome))
            .map(|(scroll, _)| *scroll)
            .collect()
    }

    /// The scrolls looked at that no screen drew.
    fn missing(&self) -> Vec<usize> {
        self.looked
            .iter()
            .copied()
            .filter(|scroll| !self.screens.contains_key(scroll))
            .collect()
    }

    /// `at` says the list lays it out — the laid-out rects say whether the
    /// room holds it, the paint how much of it a screen shows, as a frame
    /// counts it ([`Frame::kept`]): the frame draws no list; it is taller
    /// than the list's room, or owed a part, taller than the room and at best
    /// starts a screen at the list's top. `Err` where the room holds as much
    /// of it as the rule asks and no screen paints it so, or a scroll drew no
    /// screen to measure: no geometry says why, a fault. That the list
    /// scrolls by stops is no cause.
    fn cause(&self, at: Placed, owed: Kept) -> Result<String, String> {
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
        let Some(&((_, tall, ..), room)) = placed.first() else {
            return Ok("not laid out in the list on any screen".to_string());
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
            .map(|&((_, _, shown), _)| shown)
            .fold(0.0_f32, f32::max);
        let highest = placed
            .iter()
            .map(|&((top, ..), _)| top)
            .filter(|top| *top >= -0.5)
            .fold(f32::INFINITY, f32::min);
        if tall > room + 0.5 && highest <= 0.5 {
            return Ok(format!(
                "it taller than the room, at best at the list's top: {tall}pt in {room}pt, \
                 {most}pt painted"
            ));
        }
        if most + 0.5 < needs {
            Err(format!(
                "the room holds its {needs}pt, but the paint shows at most {most}pt of its \
                 {tall}pt on any of {} screens, at best {highest}pt under the list's top in \
                 {room}pt",
                self.screens.len()
            ))
        } else {
            Err(format!(
                "a screen paints it so: {:?} (top under the list's, tall, painted) by scroll",
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
    /// Whether a compact window's statuses stack under the page, never
    /// docked in the toolbar ([`STATUS_UNDOCKED`]).
    undocked: bool,
    /// The chooser's filter, and the theme on trial.
    filter: String,
    trial: Option<String>,
    /// What else the walk's view held that lays the page out.
    held: Held,
    /// The chrome the screens are held in ([`PAGE_CHROME_HELD`]), or none
    /// for the chrome the page chooses.
    chrome: Option<Chrome>,
    /// The row its rule reads besides the keyboard's control.
    reads: Reads,
}

/// The row a rule reads a reach for besides the keyboard's control: none
/// (R2, R4), the theme in use's (R3) or the chooser's first (R5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Reads {
    Control,
    Landing,
    FirstRow,
}

/// WHAT A WALK'S VIEW HOLDS THAT LAYS THE PAGE OUT, past what the scenario
/// opened it with and [`ReachOf`]'s other fields: the sheet open or shut,
/// the Community disclosure (Left and Right on it), the Keep menu, an open
/// picker, the side the segment highlights (read only while the theme
/// follows the system), the drafts the person typed in the sheet's fields,
/// and what they typed in Settings' search.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Held {
    search: String,
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
            search: state.search_input.value().to_string(),
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
        if !self.search.is_empty() {
            state.search_input = crate::native_text_input::TextInputState::new(self.search.clone());
            state.set_search(self.search.clone());
        }
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

/// Where a screen lays a stop out under the list's top: its top, its
/// height, and how much of it the screen paints.
type Placed = fn(&Screenful) -> Option<Place>;

/// [`Frame::placed`].
type Place = (f32, f32, f32);

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
            Keyboard::On(key) | Keyboard::Sheet(key) => Some(key.to_string()),
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
        let put = |runtime: &mut NativeRuntime| {
            let key = self.control.clone().unwrap_or_default();
            let _ = dispatch(
                runtime,
                instance,
                view,
                AppEvent::FocusChanged(Some(UiKey::new(key))),
            );
        };
        match self.scenario.keyboard {
            Keyboard::Off => {}
            Keyboard::Filter | Keyboard::Row(_) | Keyboard::On(_) => put(&mut runtime),
            Keyboard::Sheet(_) => {
                press(&mut runtime, CUSTOMIZE);
                put(&mut runtime);
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

    /// Where `reach`'s status's bar is not at its least, the reach `key`
    /// names under the bar at its least, and why: measured only where a
    /// rule asks what `reach` does not show.
    fn at_least(&self, key: &ReachOf, reach: &Reach) -> Option<(Arc<Reach>, String)> {
        reach.bar.filter(|bar| bar.not_least()).map(|bar| {
            let least = self.reach(&ReachOf {
                least: true,
                ..key.clone()
            });
            let under = format!(
                "the status's bar {}pt{}, its least {}pt, the page's choice",
                bar.tall,
                if bar.docked { " docked" } else { "" },
                bar.least
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

    /// R2 AND R5 OF A FRAME THAT SHOWS `now` of the keyboard's control
    /// where it owes `owed`, drawn in `chrome`: `Err` names a screen that
    /// shows it so (in any chrome the page can draw, [`rungs`], and under
    /// the bar at its least too) or, where none does, one that shows more of
    /// it than the frame; `Ok` names the geometry that excepts it.
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
        let least = self.at_least(key, &reach);
        if let Some((least, under)) = least.as_ref()
            && let Some(scroll) = least.covering(owed)
        {
            return Err(format!(
                "{under}: at its least the screen at scroll {scroll} shows it so"
            ));
        }
        // A status docked in the toolbar takes its controls' place: stacked
        // under the page instead, as the page can draw it.
        if reach.docked {
            let undocked = ReachOf {
                undocked: true,
                ..key.clone()
            };
            let stacked = self.reach(&undocked);
            let at_least = self.at_least(&undocked, &stacked);
            if let Some(scroll) = stacked.covering(owed).or_else(|| {
                at_least
                    .as_ref()
                    .and_then(|(least, _)| least.covering(owed))
            }) {
                return Err(format!(
                    "a status docked in the toolbar: stacked under the page, the screen at \
                     scroll {scroll} shows it so"
                ));
            }
        }
        let measured = [Some(chrome), chosen.as_ref().map(|chosen| chosen.chrome)];
        for rung in rungs(key.drawn) {
            if measured.contains(&Some(rung)) {
                continue;
            }
            let held = ReachOf {
                chrome: Some(Chrome(rung)),
                ..key.clone()
            };
            let other = self.reach(&held);
            if let Some(scroll) = other.covering(owed) {
                return Err(format!(
                    "the page drew it in {chrome:?}: in {rung:?}, a rung of its ladder, the \
                     screen at scroll {scroll} shows it so"
                ));
            }
            if let Some((least, under)) = self.at_least(&held, &other)
                && let Some(scroll) = least.covering(owed)
            {
                return Err(format!(
                    "the page drew it in {chrome:?}: in {rung:?}, a rung of its ladder, with \
                     {under}, at its least the screen at scroll {scroll} shows it so"
                ));
            }
        }
        let (base, under) = match least {
            Some((least, under)) => (least, format!(", with {under}")),
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
        if base.pinned {
            return Ok(format!(
                "pinned around the list, {} of it on every screen in every chrome the page can \
                 draw, stacked or docked, the bar at its least too{under}",
                base.most().map_or(Kept::Part(0.0), |(_, most)| most)
            ));
        }
        base.cause(|screen| screen.control_at, owed)
            .map(|cause| format!("{cause}{under}"))
    }

    fn measure(&self, key: &ReachOf) -> Reach {
        let drawn = key.drawn;
        STATUS_BAR_LEAST.with(|least| least.set(key.least));
        STATUS_UNDOCKED.with(|undocked| undocked.set(key.undocked));
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
        // Every screen in one chrome: the key's, else the page's choice.
        let chrome = key.chrome.map_or(first.chrome, |chrome| chrome.0);
        PAGE_CHROME_HELD.with(|held| held.set(Some(chrome)));
        let control = key.keyboard.as_deref();
        let mut reach = Reach {
            screens: BTreeMap::new(),
            strays: Vec::new(),
            looked: BTreeSet::new(),
            limit: 0,
            list: false,
            bar: None,
            docked: false,
            pinned: false,
            elsewhere: None,
            chrome,
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
            let at = if first.list { frame.top } else { Some(0) };
            if at != Some(top) {
                reach.strays.push((top, at));
            }
            // The bar as the reveal sizes it, which no scroll changes.
            if reach.bar.is_none() {
                reach.bar = StatusBar::of(&mut runtime, view, drawn, &frame.screen);
            }
            reach.list |= frame.paints;
            reach.docked |= frame
                .screen
                .semantic(&UiKey::new("settings/status-dock"))
                .is_some();
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
                listed: frame.list_drawn(),
                chrome: Chrome(frame.chrome),
            };
            Some(*reach.screens.entry(at?).or_insert(screen))
        };
        let _ = look(0, &mut reach);
        let plan = held.borrow_mut().take().flatten();
        let limit = plan.as_ref().map_or(0, scroll::ScrollPlan::limit);
        reach.limit = limit;
        let listed =
            |row: Option<&str>| row.and_then(|row| plan.as_ref()?.stop_of(&UiKey::new(row)));
        let scans: [(Option<usize>, Placed); 3] = [
            (listed(control), |screen| screen.control_at),
            (
                listed(Some(&self.landing)).filter(|_| key.reads == Reads::Landing),
                |screen| screen.landing_at,
            ),
            (
                listed(first.first_row.as_deref()).filter(|_| key.reads == Reads::FirstRow),
                |screen| screen.first_row_at,
            ),
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
                    if let Some((under, tall, ..)) = at(&screen) {
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
        reach.pinned = pinned;
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
        STATUS_UNDOCKED.with(|undocked| undocked.set(false));
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

/// `screen` but the settings sidebar, which scrolls on its own: what of it a
/// wheel on the list places.
fn but_the_sidebar(screen: &crate::native_ui::CompiledUi) -> crate::native_ui::CompiledUi {
    let mut screen = screen.clone();
    screen
        .paint
        .retain(|node| !navigation_scroll_target(&node.key));
    screen
        .hits
        .retain(|hit| !navigation_scroll_target(&hit.key));
    screen
        .semantics
        .retain(|node| !navigation_scroll_target(&node.key));
    screen
        .focus_order
        .retain(|key| !navigation_scroll_target(key));
    screen
        .list_pictures
        .retain(|picture| !navigation_scroll_target(&picture.key));
    screen
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
        // A press lands only on a control the last frame laid out, painted
        // or not (an accessibility press reaches one scrolled away).
        let pressed = match step {
            Some(Step::Press(key)) => last
                .as_ref()
                .is_some_and(|last| last.screen.semantic(&UiKey::new(key)).is_some())
                .then_some(key),
            _ => None,
        };
        if let Some(key) = pressed {
            let _ = act(&mut runtime, instance, view, key, None);
        }
        let persons = step.is_some_and(Step::persons)
            && (pressed.is_some() || !matches!(step, Some(Step::Press(_))));
        // The keyboard where the person's event left it: put on a control
        // it was not on, or left on the one it was.
        let mut moved = false;
        if persons {
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
            undocked: false,
            filter: walked_state.showcase.filter.value().to_string(),
            trial: trying(walked_state),
            held: Held::of(walked_state),
            chrome: None,
            reads: Reads::Control,
        };
        let kept_reach_of = ReachOf {
            reveal: reveal.map(|reveal| control.as_deref().map_or(reveal, UiKey::new)),
            ..reach_of.clone()
        };
        // Where the walk is, written only for a frame a rule faults.
        let at_frame = || {
            let walked = steps[..at]
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{scenario} from {start} [{walked}]: frame {at} at {drawn}")
        };
        let mut fault = |rule: Rule, said: String| {
            found.faults.push((rule, format!("{}: {said}", at_frame())));
        };
        // The reaches this frame's rules read, in its chrome, each with
        // its key.
        let judged = std::cell::RefCell::new(Vec::<(ReachOf, Arc<Reach>)>::new());
        let framed = |key: &ReachOf| {
            let (reach, chosen) = sweep.framed(key, frame.chrome);
            judged.borrow_mut().push((key.clone(), Arc::clone(&reach)));
            (reach, chosen)
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
            let _ = framed(&kept_reach_of);
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
            let (reach, chosen) = framed(&ReachOf {
                reads: Reads::FirstRow,
                ..kept_reach_of.clone()
            });
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
        let apart = !bar.is_some_and(StatusBar::not_least)
            && owed
                .as_ref()
                .zip(control.as_deref())
                .is_some_and(|((owed, _, _), key)| {
                    frame.kept(key).covers(*owed) && frame.apart(landing, key, *owed)
                });
        if opening && !apart && frame.listed(landing) && frame.chooser() && !frame.in_list(landing)
        {
            let landing_of = ReachOf {
                reads: Reads::Landing,
                ..reach_of.clone()
            };
            let (reach, chosen) = framed(&landing_of);
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
                    least = sweep.at_least(&landing_of, &reach);
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
        // Where only a screen seated partway into the control's own stop
        // shows it as R2 asks (a stop taller than the room), the frame that
        // starts there.
        let into_its_stop = |owed: Kept| {
            control.as_deref().is_some_and(|key| {
                frame.kept(key).covers(owed)
                    && frame.top.is_some()
                    && frame
                        .plan
                        .as_ref()
                        .and_then(|plan| plan.stop_of(&UiKey::new(key)))
                        == frame.top
                    && framed(&kept_reach_of).0.covering(owed).is_none()
            })
        };
        if let Some(wheel) = wheeled.as_ref()
            && drawn == wheel.drawn
            && frame.screen != wheel.screen
            && but_the_sidebar(&frame.screen) != but_the_sidebar(&wheel.screen)
        {
            let needs = owed
                .as_ref()
                .filter(|(owed, _, _)| !wheel.kept.is_some_and(|kept| kept.covers(*owed)))
                .map(|(owed, _, _)| {
                    let reach = framed(&kept_reach_of).0;
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
            }) || owed.as_ref().is_some_and(|(owed, _, _)| {
                !wheel.kept.is_some_and(|kept| kept.covers(*owed)) && into_its_stop(*owed)
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
                let reach = framed(&kept_reach_of).0;
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
            } || asks.is_some_and(into_its_stop);
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
        // A REACH JUDGES A FRAME BY ITS LIST: each one a rule read for this
        // frame lists the stops the frame lists, as tall, drew every screen in
        // the frame's chrome, and at the frame's scroll leaves the list the
        // frame's room — else its key left out something the view holds, and
        // it judges the frame by another page's screens. Under a status, a
        // reach for another reveal sizes the bar, and so the list, for that:
        // what its key holds is judged by the reach it names for the frame's.
        if frame.list
            && let Some(top) = frame.top
        {
            let mut read: Vec<Arc<Reach>> = Vec::new();
            for (key, reach) in judged.take() {
                let reach = if drawn.status && key.reveal != reach_of.reveal {
                    let key = ReachOf {
                        reveal: reach_of.reveal.clone(),
                        ..key
                    };
                    sweep.framed(&key, frame.chrome).0
                } else {
                    reach
                };
                if read.iter().any(|seen| Arc::ptr_eq(seen, &reach)) {
                    continue;
                }
                read.push(Arc::clone(&reach));
                found.compared += 1;
                let listed = frame.list_drawn();
                let at = reach.screens.get(&top);
                let lists = at
                    .or_else(|| reach.screens.values().next())
                    .and_then(|screen| screen.listed);
                let same_list = lists
                    .zip(listed)
                    .is_some_and(|((was, tall), (now, height))| {
                        was == now && (tall - height).abs() < 0.5
                    });
                let same_room = at.is_none_or(|screen| {
                    (screen.room - frame.room).abs() < 0.5
                        && (screen.list_top - frame.list_top).abs() < 0.5
                });
                let mixed = reach.mixed();
                if !same_list || !same_room || !mixed.is_empty() {
                    let at_frame = at_frame();
                    found.unlike.push(format!(
                        "{at_frame}: the reach it is judged by lists {lists:?} (stops, tall), the \
                         frame {listed:?}; at scroll {top} it leaves the list {:?} (room, top), the \
                         frame ({}, {}); its screens at {mixed:?} are in another chrome than {:?}",
                        at.map(|screen| (screen.room, screen.list_top)),
                        frame.room,
                        frame.list_top,
                        reach.chrome
                    ));
                }
            }
        }
        if let Some(key) = control.as_deref() {
            let now = frame.kept(key);
            left = match left {
                Some(was) if !persons && !now.covers(was.kept) => Some(was),
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

/// THE WALK FROM `start`: through the windows that follow its own round
/// `windows`, half the others (so between two windows each change, out and
/// back, is walked once, from one of them), the other text size, the warning
/// and a status, and back, in each of the ways below that `taken` names, and
/// the keys on the keyboard's control. ONE WALK, so a start opens one view:
/// its ways in an order that leaves each the keyboard where a walk of its own
/// would start it — nothing of the person's before the frames drawn again,
/// the keyboard on the scenario's control (off the list, nowhere) for the
/// wheels, the keys that move it last. Each change is drawn again, by nothing
/// or by a reply the person did not cause, and half the wheels, so a change
/// follows a wheel straight and after its frame drawn again; a wheel's place
/// is held through two changes and back, one of them drawn again (each change
/// is the one in the walks from half the windows), and under the text size or
/// a bar through another. A WHEEL, OR THE KEYBOARD PUT ON ITS CONTROL, IN
/// ANOTHER WINDOW is walked from that window's own start, which wheels and
/// puts the keyboard there and goes on to the windows that follow it: only the
/// text size and the bars, whose changed state no start of a walk is, are
/// wheeled and focused in from here.
fn walk_from(start: Drawn, windows: &[Size], keyboard: Keyboard, taken: Walks) -> Vec<Step> {
    use Step::{Again, Down, Focus, Left, Right, Status, Type, Up, Warning, Wheel};
    let at = windows
        .iter()
        .position(|&window| window == start.window)
        .unwrap_or_default();
    let count = windows.len();
    let follows = |offset: usize| 2 * offset < count || (2 * offset == count && 2 * at < count);
    let changes = (1..count)
        .filter(|&offset| follows(offset))
        .map(|offset| Step::Window(windows[(at + offset) % count]))
        .chain([Step::Text, Warning, Status])
        .collect::<Vec<_>>();
    let mut again = vec![Again];
    // A wheel down and back to the list's top, each drawn again.
    let mut wheel = vec![Wheel(8), Again, Wheel(-12), Again];
    let mut focus = vec![Focus, Again];
    let mut there = Vec::new();
    let mut refocus = Vec::new();
    // Which change of each wheel's pair is drawn again: the first in the
    // walks from every other window, the second in the rest.
    let carried = at % 2;
    let mut turned = at;
    let mut turn = |lines: i32| {
        turned += 1;
        if turned % 2 == 0 {
            vec![Wheel(lines), Again]
        } else {
            vec![Wheel(lines)]
        }
    };
    for (index, &change) in changes.iter().enumerate() {
        let lines = if index % 4 < 2 { 4 } else { -4 };
        let back = change.back(start);
        let drawn_again = match index % 4 {
            0 => Step::Tick,
            2 => Step::Refresh,
            _ => Again,
        };
        again.extend([change, drawn_again, back]);
        if index % 2 == 0 {
            wheel.extend(turn(lines));
        }
        wheel.push(change);
        if index % 2 == carried {
            wheel.push(Again);
        }
        wheel.push(back);
        focus.extend([change, back]);
        // Under a change, a wheel's place through another and back.
        let then = match change {
            Step::Text => Some(Status),
            Status => Some(Warning),
            Warning => Some(Step::Text),
            _ => None,
        };
        if let Some(then) = then {
            there.push(change);
            there.extend(turn(lines));
            there.extend([then, then, back]);
            refocus.extend(turn(lines));
            refocus.extend([change, Focus, back]);
        }
    }
    match taken {
        Walks::Opening => {
            again.extend(focus);
            return again;
        }
        Walks::Control(_) => {
            // The keyboard left on its control through each change and
            // back, a window's under a status every other time; the wheels
            // from one text size's start in each window and the changes under
            // them from the other's; the bars, and the keys.
            let mut walk = vec![Again];
            for (index, &change) in changes.iter().enumerate() {
                let back = change.back(start);
                if index % 2 == 1 && matches!(change, Step::Window(_)) {
                    walk.extend([Status, change, back, Status]);
                } else {
                    walk.extend([change, back]);
                }
            }
            walk.extend(if (at + usize::from(start.scale)) % 2 == 0 {
                wheel
            } else {
                there
            });
            walk.extend([
                Warning, Status, Again, Status, Warning, Left, Again, Right, Up, Again, Down, Down,
            ]);
            return walk;
        }
        Walks::Every(_) | Walks::Reviewed => {}
    }
    let mut walk = again;
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
        Focus, Warning, Status, Again, Status, Status, Warning, Focus, Status,
    ]);
    // R5: the keys, each drawn again, and the filter typed and erased.
    walk.extend([Down, Again, Up, Again, Up, Type, Again, Step::Erase, Again]);
    walk
}

/// `count` walks of `length` steps, each step drawn at random from every
/// kind and half the wheels drawn again: every third walk in the windows the
/// sweep draws and odd ones, the rest in windows of any size, seeded so the
/// walks are the same each run.
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
        .map(|walk| {
            let window = |next: &mut dyn FnMut(usize) -> usize| -> Size {
                if walk % 3 == 0 {
                    return all[next(all.len())];
                }
                let width = next(1_270) + 170;
                let height = next(800) + 150;
                (
                    u16::try_from(width).unwrap_or(u16::MAX),
                    u16::try_from(height).unwrap_or(u16::MAX),
                )
            };
            let start = Drawn::bare(window(&mut next), [1, 2][next(2)]);
            let mut steps = Vec::with_capacity(length + 1);
            while steps.len() < length {
                let step = match next(100) {
                    0..10 => Step::Again,
                    10..34 => Step::Window(window(&mut next)),
                    34..42 => Step::Text,
                    42..50 => Step::Warning,
                    50..59 => Step::Status,
                    59..76 => Step::Wheel([1, -1, 2, -2, 3, -3, 4, -4, 8, -8, 12, -12][next(12)]),
                    76..82 => Step::Focus,
                    82..90 => [
                        Step::Up,
                        Step::Down,
                        Step::Type,
                        Step::Erase,
                        Step::Left,
                        Step::Right,
                    ][next(6)],
                    90..92 => Step::Tick,
                    92..94 => Step::Refresh,
                    _ => Step::Press(PRESSES[next(PRESSES.len())]),
                };
                steps.push(step);
                if let Step::Wheel(_) = step
                    && next(2) == 0
                {
                    steps.push(Step::Again);
                }
            }
            (start, steps)
        })
        .collect()
}

#[expect(clippy::too_many_lines, reason = "each review's sequences, as found")]
fn reviewed() -> Vec<(Scenario, Drawn, Vec<Step>)> {
    use Step::{
        Again, Down, Erase, Focus, Left, Press, Refresh, Right, Status, Text, Tick, Type, Up,
        Warning, Wheel, Window,
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
        w1284x714,
    ] = WINDOWS;
    let w1144x700 = (1_144, 700);
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
    let follow = Scenario {
        keyboard: Keyboard::On(FOLLOW),
        ..filter
    };
    let following = Scenario {
        keyboard: Keyboard::On(FOLLOW),
        ..side
    };
    let folder = Scenario {
        keyboard: Keyboard::On(OPEN_FOLDER),
        ..filter
    };
    let import = Scenario {
        keyboard: Keyboard::On(IMPORT),
        ..filter
    };
    let background = Scenario {
        keyboard: Keyboard::Sheet("settings/control/background"),
        ..filter
    };
    let well = Scenario {
        keyboard: Keyboard::Sheet("settings/color-open/selection_foreground"),
        ..drawn
    };
    let field = Scenario {
        keyboard: Keyboard::Sheet("settings/control/selection_foreground"),
        ..drawn
    };
    let palette = Scenario {
        keyboard: Keyboard::Sheet("settings/showcase/palette/9"),
        ..drawn
    };
    let save_as = Scenario {
        keyboard: Keyboard::Sheet(SAVE_AS),
        ..drawn
    };
    let import_light = Scenario {
        keyboard: Keyboard::On(IMPORT),
        ..drawn
    };
    let customize = Scenario {
        keyboard: Keyboard::On(CUSTOMIZE),
        ..filter
    };
    let cursor_color = Scenario {
        keyboard: Keyboard::Sheet(CURSOR_COLOR),
        ..filter
    };
    let foreground = Scenario {
        keyboard: Keyboard::Sheet(FOREGROUND),
        ..first
    };
    let palette_first = Scenario {
        keyboard: Keyboard::Sheet(PALETTE_FIRST),
        ..filter
    };
    let search = Scenario {
        keyboard: Keyboard::On(SEARCH),
        ..filter
    };
    let w380x300 = (380, 300);
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
        // Round 9's review: Again after a wheel back to the list's top, which
        // chose the page's chrome again (R1).
        (background, at(w444x420, 1), vec![Wheel(-12), Again]),
        (drawn, at((900, 260), 1), vec![Warning, Wheel(-12), Again]),
        (palette, at((260, 500), 1), vec![Warning, Wheel(-12), Again]),
        (save_as, at((380, 300), 1), vec![Status, Wheel(-12), Again]),
        (
            save_as,
            at((380, 300), 1),
            vec![Status, Warning, Wheel(-12), Again],
        ),
        // Round 9's review: the chooser's footer under the bars, a switch
        // under its label that no stop the room holds showed, and one the
        // list's edge cut counted as none of it (R2, R5).
        (follow, at(w178x348, 2), vec![Warning, Status]),
        (follow, at(w178x348, 2), vec![Warning, Status, Left]),
        (folder, at(w178x348, 2), vec![Warning, Status]),
        (import, at(w178x348, 2), vec![Warning, Status]),
        (
            follow,
            at(w1144x700, 1),
            vec![Focus, Status, Window((380, 300))],
        ),
        (
            following,
            at((700, 250), 2),
            vec![Status, Window((480, 360))],
        ),
        // Round 9's review: a wheel's place under a status, the keyboard on
        // Done outside the list, moved by the warning (R4).
        (drawn, at((380, 300), 1), vec![Status, Wheel(-4), Warning]),
        // Round 9's review: a reach whose screens were drawn in another chrome
        // than the frame's.
        (save_as, at((380, 300), 1), vec![Wheel(1), Status]),
        // Round 9's review: a row the list draws as a picture, its cause
        // measured as shown.
        (
            pack,
            at((980, 420), 2),
            vec![
                Again,
                Status,
                Window(w300x160),
                Window((900, 260)),
                Window((200, 300)),
                Refresh,
                Right,
                Wheel(2),
                Erase,
                Warning,
                Down,
                Type,
                Window(w444x180),
            ],
        ),
        // Round 9's review: a colour row's well and the one over it in the
        // room a status leaves (R2, R5).
        (well, at((380, 300), 1), vec![Status]),
        (field, at(w444x420, 2), vec![Status, Up]),
        // Round 10's review: the text size there and back after a wheel,
        // which chose the sheet's chrome again (R4).
        (palette, at(w444x420, 1), vec![Wheel(-2), Text, Text]),
        (background, at(w444x420, 1), vec![Wheel(-2), Text, Text]),
        (field, at(w444x420, 1), vec![Wheel(-2), Text, Text]),
        (save_as, at(w444x420, 1), vec![Wheel(-2), Text, Text]),
        (palette, at((1_229, 268), 1), vec![Wheel(8), Text, Text]),
        (
            background,
            at((380, 300), 1),
            vec![Refresh, Focus, Again, Wheel(3), Text, Tick, Text],
        ),
        (
            palette,
            at(w444x420, 1),
            vec![Wheel(2), Wheel(-4), Text, Again, Text],
        ),
        (palette, at(w444x420, 1), vec![Text, Text]),
        // Round 10's review: a chrome kept, or chosen again, that cut the
        // keyboard's control under a bar (R2).
        (
            background,
            at((1_385, 640), 1),
            vec![Window((1_268, 165)), Refresh, Up, Warning, Tick, Again],
        ),
        (drawn, at((1_195, 191), 2), vec![Warning, Text]),
        // Round 10's review: the pack row under its heading at 2× text, and
        // the theme in use under a status before the first event (R2, R3).
        (
            pack,
            at((590, 727), 2),
            vec![
                Window((1_402, 633)),
                Again,
                Again,
                Again,
                Again,
                Again,
                Window((914, 533)),
                Window((504, 425)),
                Window((657, 155)),
            ],
        ),
        (
            filter_tokyo,
            at((320, 340), 2),
            vec![Window((339, 920)), Warning, Status],
        ),
        // Round 10's review, its walks at any window size (each one another
        // goes on from left to that one).
        (
            follow,
            at((756, 310), 2),
            vec![
                Wheel(-8),
                Wheel(4),
                Focus,
                Wheel(-4),
                Wheel(2),
                Again,
                Again,
                Again,
                Window((1_100, 837)),
                Window((176, 279)),
                Window((1_380, 468)),
                Window((997, 219)),
                Wheel(4),
                Again,
                Up,
                Window((914, 155)),
                Again,
            ],
        ),
        (
            follow,
            at((624, 446), 1),
            vec![
                Again,
                Window((881, 658)),
                Window((847, 710)),
                Focus,
                Again,
                Status,
                Text,
                Wheel(-2),
                Again,
                Up,
                Window((900, 651)),
                Text,
                Window((860, 175)),
                Window((1_010, 863)),
                Text,
                Refresh,
                Window((633, 562)),
                Window((536, 887)),
                Window((1_137, 159)),
            ],
        ),
        (
            pack,
            at((435, 165), 2),
            vec![
                Tick,
                Wheel(-8),
                Wheel(8),
                Again,
                Tick,
                Status,
                Status,
                Window((1_231, 412)),
                Warning,
                Status,
                Window((365, 294)),
                Window((1_203, 481)),
                Warning,
                Warning,
                Again,
                Window((1_340, 306)),
                Erase,
                Left,
                Status,
                Again,
                Window((1_204, 154)),
            ],
        ),
        (
            pack,
            at((1_273, 207), 1),
            vec![
                Right,
                Warning,
                Window((1_267, 806)),
                Warning,
                Status,
                Focus,
                Text,
                Window((275, 221)),
            ],
        ),
        (
            pack,
            at((398, 314), 2),
            vec![
                Wheel(1),
                Again,
                Wheel(2),
                Again,
                Again,
                Window((1_038, 547)),
                Window((1_004, 776)),
                Text,
                Again,
                Left,
                Window((604, 222)),
                Wheel(1),
                Wheel(-4),
                Window((1_238, 426)),
                Text,
                Wheel(2),
                Again,
                Warning,
                Window((1_134, 154)),
                Type,
            ],
        ),
        (
            well,
            at((390, 803), 2),
            vec![
                Window((1_201, 524)),
                Status,
                Wheel(4),
                Again,
                Window((859, 164)),
                Focus,
                Text,
                Window((1_406, 772)),
                Warning,
                Focus,
                Tick,
                Window((917, 848)),
                Window((998, 287)),
                Status,
                Focus,
                Window((1_064, 188)),
                Again,
            ],
        ),
        (
            well,
            at((918, 570), 1),
            vec![
                Wheel(8),
                Text,
                Window((314, 897)),
                Warning,
                Window((496, 306)),
                Wheel(-8),
                Text,
                Wheel(1),
                Again,
                Window((1_402, 598)),
                Window((1_132, 381)),
                Status,
                Again,
                Window((1_150, 165)),
                Status,
            ],
        ),
        (
            field,
            at((584, 306), 2),
            vec![
                Focus,
                Wheel(3),
                Text,
                Warning,
                Erase,
                Down,
                Window((404, 708)),
                Wheel(-1),
                Window((1_049, 241)),
                Focus,
            ],
        ),
        (
            field,
            at((1_040, 456), 2),
            vec![
                Wheel(-3),
                Window((1_341, 891)),
                Text,
                Left,
                Again,
                Warning,
                Again,
                Window((1_136, 183)),
            ],
        ),
        (
            palette,
            at((324, 946), 1),
            vec![
                Left,
                Wheel(8),
                Window((313, 777)),
                Window((1_401, 881)),
                Status,
                Status,
                Wheel(1),
                Status,
                Window((1_239, 246)),
                Tick,
                Warning,
            ],
        ),
        (
            palette,
            at((714, 228), 1),
            vec![
                Wheel(12),
                Again,
                Status,
                Wheel(1),
                Warning,
                Wheel(-4),
                Focus,
                Wheel(1),
                Window((1_411, 246)),
            ],
        ),
        (
            palette,
            at((1_252, 478), 1),
            vec![
                Window((460, 341)),
                Window((848, 558)),
                Window((412, 264)),
                Focus,
                Window((610, 863)),
                Wheel(4),
                Again,
                Focus,
                Window((761, 161)),
                Tick,
                Warning,
            ],
        ),
        (
            save_as,
            at((1_065, 181), 1),
            vec![
                Window((1_355, 585)),
                Warning,
                Focus,
                Status,
                Wheel(2),
                Again,
                Up,
                Focus,
                Window((969, 161)),
                Focus,
                Focus,
                Text,
                Again,
                Down,
                Again,
                Window((467, 915)),
                Status,
                Text,
                Window((1_367, 245)),
                Again,
            ],
        ),
        (
            save_as,
            at((678, 609), 1),
            vec![
                Window((642, 700)),
                Window((1_311, 804)),
                Focus,
                Wheel(3),
                Wheel(-2),
                Again,
                Text,
                Warning,
                Wheel(3),
                Again,
                Wheel(2),
                Erase,
                Window((910, 724)),
                Again,
                Window((203, 221)),
                Erase,
            ],
        ),
        (
            drawn,
            at((433, 850), 2),
            vec![
                Wheel(8),
                Wheel(-2),
                Window((1_271, 939)),
                Window((396, 943)),
                Text,
                Warning,
                Window((984, 602)),
                Again,
                Again,
                Status,
                Status,
                Window((1_055, 170)),
            ],
        ),
        (
            drawn,
            at((647, 845), 2),
            vec![
                Warning,
                Again,
                Text,
                Text,
                Window((834, 287)),
                Window((964, 809)),
                Right,
                Window((1_195, 191)),
                Text,
                Again,
            ],
        ),
        (
            drawn,
            at((1_259, 213), 1),
            vec![
                Status,
                Status,
                Refresh,
                Window((366, 789)),
                Type,
                Again,
                Warning,
                Text,
                Wheel(-8),
                Window((412, 885)),
                Wheel(-1),
                Window((812, 745)),
                Window((806, 840)),
                Again,
                Text,
                Wheel(-8),
                Again,
                Again,
                Wheel(-2),
                Status,
                Warning,
                Again,
                Status,
                Warning,
            ],
        ),
        (
            drawn,
            at((357, 369), 1),
            vec![
                Status,
                Right,
                Wheel(-12),
                Again,
                Status,
                Text,
                Right,
                Status,
                Wheel(-2),
                Window((290, 469)),
                Window((1_300, 625)),
                Status,
                Text,
                Status,
                Window((730, 475)),
                Tick,
                Window((1_352, 891)),
                Wheel(-3),
                Again,
                Window((1_322, 167)),
                Status,
                Warning,
                Erase,
            ],
        ),
        (
            row,
            at((245, 935), 2),
            vec![
                Warning,
                Window((450, 282)),
                Text,
                Focus,
                Type,
                Window((994, 234)),
                Window((957, 623)),
                Window((948, 423)),
                Again,
                Window((468, 153)),
                Tick,
                Type,
                Status,
                Tick,
                Status,
                Window((315, 304)),
                Window((687, 842)),
                Wheel(4),
                Again,
                Window((1_122, 766)),
                Wheel(-1),
                Warning,
                Warning,
            ],
        ),
        (
            import_light,
            at((967, 946), 1),
            vec![
                Tick,
                Tick,
                Down,
                Warning,
                Wheel(-12),
                Status,
                Wheel(1),
                Text,
                Status,
                Window((696, 819)),
                Status,
                Tick,
                Wheel(4),
                Warning,
                Status,
                Up,
                Again,
                Text,
                Text,
                Window((509, 625)),
                Up,
                Status,
            ],
        ),
        (
            cursor_color,
            at((421, 550), 2),
            vec![
                Down,
                Focus,
                Window((940, 289)),
                Again,
                Status,
                Warning,
                Erase,
                Window((230, 612)),
                Tick,
                Wheel(-4),
                Again,
                Left,
                Window((479, 859)),
                Again,
                Wheel(-4),
                Tick,
                Up,
                Window((1_289, 166)),
                Text,
                Status,
                Text,
                Text,
            ],
        ),
        (
            cursor_color,
            at((1_001, 255), 2),
            vec![
                Window((1_115, 582)),
                Window((477, 798)),
                Wheel(-3),
                Again,
                Warning,
                Status,
                Status,
                Wheel(1),
                Again,
                Warning,
                Focus,
                Wheel(-12),
                Text,
                Wheel(-8),
                Status,
                Status,
                Window((982, 865)),
                Tick,
                Window((1_342, 695)),
                Window((338, 524)),
                Window((909, 802)),
                Window((824, 249)),
                Warning,
                Window((1_086, 176)),
            ],
        ),
        (
            foreground,
            at((1_044, 937), 2),
            vec![
                Right,
                Again,
                Text,
                Window((968, 576)),
                Window((893, 213)),
                Window((525, 699)),
                Warning,
                Window((1_008, 826)),
                Again,
                Wheel(-3),
                Again,
                Again,
                Wheel(-3),
                Window((889, 397)),
                Wheel(-8),
                Wheel(-12),
                Again,
                Window((1_093, 204)),
                Window((1_359, 166)),
                Left,
                Up,
            ],
        ),
        (
            foreground,
            at((412, 430), 1),
            vec![
                Wheel(-3),
                Warning,
                Status,
                Wheel(-3),
                Again,
                Window((656, 353)),
                Again,
                Warning,
                Warning,
                Status,
                Refresh,
                Focus,
                Window((954, 165)),
            ],
        ),
        (
            palette_first,
            at((190, 571), 2),
            vec![
                Warning,
                Window((709, 900)),
                Focus,
                Focus,
                Focus,
                Window((706, 404)),
                Again,
                Wheel(3),
                Focus,
                Window((230, 736)),
                Focus,
                Window((345, 602)),
                Text,
                Tick,
                Again,
                Status,
                Focus,
                Down,
                Status,
                Focus,
                Window((1_398, 541)),
                Window((765, 406)),
                Window((1_209, 180)),
            ],
        ),
        // Round 10's walks at any window size: the landing under a status
        // before the first event (R3), a reveal for a stop the screen chosen
        // did not draw (R1, R2), a key under a held chrome and a colour row
        // taller than the room the bars leave (R2), and the settings sidebar,
        // which scrolls on its own, under a wheel's place (R4).
        (
            row,
            at((520, 691), 2),
            vec![
                Status,
                Window((1_265, 589)),
                Window((921, 178)),
                Warning,
                Tick,
                Window((481, 876)),
            ],
        ),
        (
            following,
            at((899, 851), 2),
            vec![Status, Window((238, 515)), Again],
        ),
        (
            drawn,
            at((935, 539), 1),
            vec![
                Warning,
                Wheel(3),
                Wheel(-12),
                Status,
                Again,
                Erase,
                Again,
                Again,
                Window((858, 201)),
                Wheel(1),
                Down,
            ],
        ),
        (
            background,
            at((1_292, 803), 1),
            vec![
                Refresh,
                Wheel(12),
                Again,
                Wheel(-4),
                Again,
                Again,
                Warning,
                Window((530, 685)),
                Type,
                Again,
                Wheel(-4),
                Again,
                Wheel(-3),
                Again,
                Left,
                Status,
                Tick,
                Window((266, 308)),
                Window((804, 667)),
                Down,
                Right,
                Window((266, 152)),
                Again,
            ],
        ),
        (
            background,
            at((896, 315), 2),
            vec![
                Again,
                Up,
                Warning,
                Window((452, 287)),
                Again,
                Window((670, 406)),
                Again,
                Down,
                Status,
                Window((567, 904)),
                Wheel(-8),
                Again,
                Window((563, 195)),
                Text,
                Again,
                Up,
                Refresh,
                Text,
            ],
        ),
        (
            customize,
            at((822, 429), 2),
            vec![Warning, Window((172, 856)), Status],
        ),
        (
            cursor_color,
            at((405, 777), 1),
            vec![
                Focus,
                Window((397, 300)),
                Tick,
                Status,
                Wheel(8),
                Window((308, 336)),
                Left,
                Wheel(-2),
                Warning,
                Wheel(-4),
                Again,
                Status,
                Window((473, 372)),
                Warning,
                Tick,
                Wheel(-2),
                Wheel(-4),
                Again,
                Status,
                Tick,
                Text,
                Window((1_049, 887)),
                Window((862, 922)),
                Window((531, 187)),
            ],
        ),
        (
            foreground,
            at((713, 208), 1),
            vec![
                Wheel(-4),
                Again,
                Wheel(-1),
                Wheel(-12),
                Text,
                Right,
                Status,
                Window((388, 423)),
                Warning,
                Window((1_221, 701)),
                Wheel(3),
                Again,
                Status,
                Window((1_387, 711)),
                Window((1_009, 305)),
                Down,
                Window((425, 220)),
                Text,
                Warning,
                Text,
                Status,
                Warning,
                Again,
            ],
        ),
        (
            pack,
            at((809, 189), 1),
            vec![
                Window((1_055, 452)),
                Wheel(-1),
                Again,
                Window((1_321, 159)),
                Window((1_080, 758)),
                Text,
                Window((654, 462)),
                Wheel(-3),
                Warning,
                Window((1_395, 916)),
                Refresh,
                Again,
                Wheel(-2),
                Down,
                Wheel(2),
                Again,
                Again,
                Text,
                Wheel(-1),
                Again,
                Warning,
                Warning,
            ],
        ),
        // Round 11's review: a window back at a size it was drawn at, which
        // recalled the chrome it drew there though that cut the keyboard's
        // control and another rung kept it whole (R2).
        (
            save_as,
            at(w380x300, 1),
            vec![Status, Window((381, 300)), Window(w380x300)],
        ),
        (
            save_as,
            at(w380x300, 1),
            vec![Status, Window(w444x420), Window(w380x300)],
        ),
        (
            background,
            at(w380x300, 1),
            vec![Status, Window((381, 300)), Window(w380x300)],
        ),
        (
            background,
            at(w380x300, 1),
            vec![Status, Window(w444x420), Window(w380x300)],
        ),
        (
            background,
            at(w380x300, 1),
            vec![Status, Window((600, 200)), Window(w380x300)],
        ),
        (
            background,
            at(w380x300, 1),
            vec![Status, Window((520, 400)), Window(w380x300)],
        ),
        (
            drawn,
            at(w380x300, 1),
            vec![
                Status,
                Press(BACKGROUND_WELL),
                Again,
                Window((381, 300)),
                Window(w380x300),
            ],
        ),
        // Round 11's review: a chrome that cut the keyboard's control where
        // a richer rung, or a leaner one seated partway into the control's
        // stop, showed it whole (R2).
        (well, at((931, 186), 1), vec![Status, Warning]),
        (well, at((931, 186), 1), vec![Warning, Status]),
        (well, at((931, 186), 1), vec![Focus, Status, Warning]),
        (save_as, at(w380x300, 1), vec![Status]),
        (
            customize,
            at(w380x300, 1),
            vec![Press(CUSTOMIZE), Status, Press(BACKGROUND_WELL)],
        ),
        (
            customize,
            at(w380x300, 1),
            vec![Press(CUSTOMIZE), Status, Press(SAVE_AS)],
        ),
        // Round 11's review: the keyboard on Settings' search in the
        // toolbar, which a status's bar covered, or the toolbar's dock took
        // the place of (R2).
        (search, at(w178x348, 2), vec![Focus, Warning, Status]),
        (search, at(w178x348, 2), vec![Wheel(8), Warning, Status]),
        (search, at(w178x348, 2), vec![Wheel(1), Warning]),
        (search, at(w444x420, 1), vec![Warning, Status]),
        // Round 11's review: a colour well, which no words name, crossed by
        // the list's floor or bottom edge, owed what it painted (R2).
        (
            drawn,
            at((380, 164), 1),
            vec![
                Status,
                Press(BACKGROUND_WELL),
                Again,
                Window((380, 172)),
                Window((380, 164)),
            ],
        ),
        (
            drawn,
            at((444, 262), 2),
            vec![
                Status,
                Press(BACKGROUND_WELL),
                Again,
                Window((444, 270)),
                Window((444, 262)),
            ],
        ),
        // Round 11's review, widened: a rung that shows a cut control no
        // less than the screen before did, where none shows it whole, and a
        // status docked in the compact toolbar that took more of the window
        // than its bars stacked at their least (R2).
        (
            drawn,
            at((380, 318), 1),
            vec![
                Status,
                Press(BACKGROUND_WELL),
                Again,
                Window((380, 310)),
                Window((380, 302)),
            ],
        ),
        (
            drawn,
            at((300, 360), 2),
            vec![Status, Press(BACKGROUND_WELL), Again, Warning],
        ),
        (follow, at((300, 360), 2), vec![Status, Again, Warning]),
        // A walk at any window size: the pack row, taller than the room a
        // status leaves, owed more of it than a stop's screen shows (R2).
        (
            import_light,
            at((876, 803), 1),
            vec![
                Warning,
                Window((437, 673)),
                Up,
                Status,
                Up,
                Warning,
                Text,
                Wheel(1),
                Wheel(-1),
                Window((1_240, 151)),
            ],
        ),
        // Random walks: back at the wheel's window and bars, the list where
        // the wheel left it and the sidebar scrolled on its own, its rows off
        // its screen counted among the list's stops (R4).
        (
            drawn,
            at((1_259, 213), 1),
            vec![
                Status,
                Status,
                Refresh,
                Window((366, 789)),
                Type,
                Again,
                Warning,
                Text,
                Wheel(-8),
                Window((412, 885)),
                Wheel(-1),
                Window((812, 745)),
                Window((806, 840)),
                Again,
                Text,
                Wheel(-8),
                Again,
                Again,
                Wheel(-2),
                Status,
                Warning,
                Again,
                Status,
                Warning,
            ],
        ),
        (
            row,
            at((245, 935), 2),
            vec![
                Warning,
                Window((450, 282)),
                Text,
                Focus,
                Type,
                Window((994, 234)),
                Window((957, 623)),
                Window((948, 423)),
                Again,
                Window((468, 153)),
                Tick,
                Type,
                Status,
                Tick,
                Status,
                Window((315, 304)),
                Window((687, 842)),
                Wheel(4),
                Again,
                Window((1_122, 766)),
                Wheel(-1),
                Warning,
                Warning,
            ],
        ),
    ]
}

/// The workers each scenario's walks share: more than other sweeps', so the
/// scenario whose list is longest (Pack's) is not the run's long pole.
const KEEPING_WORKERS: usize = 12;

/// `scenario`'s frames held to the keeping rules: the walks through every
/// change and back ([`walk_from`]) that `taken` names, random walks, and the
/// walks reviews found ([`reviewed`]).
fn sweep(scenario: Scenario, taken: Walks) {
    let started = std::time::Instant::now();
    let sweep = Sweep::new(scenario);
    let mut walks = Vec::new();
    let mut starts = Vec::new();
    for scale in [1, 2].into_iter().filter(|_| taken != Walks::Reviewed) {
        for window in WINDOWS {
            let start = Drawn::bare(window, scale);
            starts.push(start.to_string());
            walks.push((start, walk_from(start, &WINDOWS, scenario.keyboard, taken)));
        }
    }
    match taken {
        Walks::Every(seed) => walks.extend(random_walks(seed, 12, 18)),
        Walks::Control(seed) => walks.extend(random_walks(seed, 6, 18)),
        Walks::Opening | Walks::Reviewed => {}
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
fn the_keeping_rules_hold_with_the_keyboard_on_import_over_github_light() {
    sweep(
        Scenario {
            keyboard: Keyboard::On(IMPORT),
            theme: "GitHub Light",
            in_use: "GitHub Light",
        },
        Walks::Reviewed,
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_customize() {
    sweep(
        Scenario {
            keyboard: Keyboard::On(CUSTOMIZE),
            theme: "Nord",
            in_use: "Nord",
        },
        Walks::Reviewed,
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_the_cursor_colour_well_of_the_sheet() {
    sweep(
        Scenario {
            keyboard: Keyboard::Sheet(CURSOR_COLOR),
            theme: "Nord",
            in_use: "Nord",
        },
        Walks::Reviewed,
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_the_foreground_field_of_the_sheet() {
    sweep(
        Scenario {
            keyboard: Keyboard::Sheet(FOREGROUND),
            theme: "Dracula",
            in_use: "Dracula",
        },
        Walks::Reviewed,
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_the_first_palette_slot_of_the_sheet() {
    sweep(
        Scenario {
            keyboard: Keyboard::Sheet(PALETTE_FIRST),
            theme: "Nord",
            in_use: "Nord",
        },
        Walks::Reviewed,
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_follow_system() {
    sweep(
        Scenario {
            keyboard: Keyboard::On(FOLLOW),
            theme: "Nord",
            in_use: "Nord",
        },
        Walks::Control(10),
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_follow_system_while_it_follows() {
    sweep(
        Scenario {
            keyboard: Keyboard::On(FOLLOW),
            theme: "dark:Dracula,light:GitHub Light",
            in_use: "GitHub Light",
        },
        Walks::Control(11),
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_the_themes_folder() {
    sweep(
        Scenario {
            keyboard: Keyboard::On(OPEN_FOLDER),
            theme: "Nord",
            in_use: "Nord",
        },
        Walks::Control(12),
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_import() {
    sweep(
        Scenario {
            keyboard: Keyboard::On(IMPORT),
            theme: "Nord",
            in_use: "Nord",
        },
        Walks::Control(13),
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_a_colour_field_of_the_sheet() {
    sweep(
        Scenario {
            keyboard: Keyboard::Sheet("settings/control/background"),
            theme: "Nord",
            in_use: "Nord",
        },
        Walks::Control(14),
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_a_colour_well_of_the_sheet() {
    sweep(
        Scenario {
            keyboard: Keyboard::Sheet("settings/color-open/selection_foreground"),
            theme: "GitHub Light",
            in_use: "GitHub Light",
        },
        Walks::Control(15),
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_the_last_colour_field_of_the_sheet() {
    sweep(
        Scenario {
            keyboard: Keyboard::Sheet("settings/control/selection_foreground"),
            theme: "GitHub Light",
            in_use: "GitHub Light",
        },
        Walks::Control(16),
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_a_palette_slot_of_the_sheet() {
    sweep(
        Scenario {
            keyboard: Keyboard::Sheet("settings/showcase/palette/9"),
            theme: "GitHub Light",
            in_use: "GitHub Light",
        },
        Walks::Control(17),
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_save_as_in_the_sheet() {
    sweep(
        Scenario {
            keyboard: Keyboard::Sheet(SAVE_AS),
            theme: "GitHub Light",
            in_use: "GitHub Light",
        },
        Walks::Control(18),
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_settings_search() {
    sweep(
        Scenario {
            keyboard: Keyboard::On(SEARCH),
            theme: "Nord",
            in_use: "Nord",
        },
        Walks::Control(19),
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
        Walks::Control(3),
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
        Walks::Control(5),
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

/// A redraw takes the least bar's decision; a test build decides again, uncounted, to check it.
#[test]
fn a_redraw_takes_the_least_bar_a_frame_at_its_inputs_and_record_decided() {
    let taken = || LEAST_DECIDED.with(std::cell::Cell::get);
    let sweep = Sweep::new(Scenario {
        keyboard: Keyboard::Pack,
        theme: "Nord",
        in_use: "Nord",
    });
    let start = Drawn {
        status: true,
        ..Drawn::bare((444, 420), 1)
    };
    let (mut runtime, instance, view) = sweep.opened(start);
    let _ = draw(&mut runtime, instance, view, start);
    for _ in 0..2 {
        let _ = compile_settings_view(&runtime, instance, view, &cx_of(start));
    }
    let before = taken();
    for _ in 0..3 {
        let _ = compile_settings_view(&runtime, instance, view, &cx_of(start));
    }
    assert_eq!(
        taken() - before,
        3,
        "each redraw of the settled page took the least bar's decision"
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

/// A frame of a walk whose page work is counted: drawn after a step, or drawn again unchanged.
#[derive(Clone, Copy)]
enum Then {
    Step(Step),
    Redraw,
}

/// The page work of each frame of a walk from `start`, the first the page opened at.
fn page_work(
    scenario: Scenario,
    start: Drawn,
    frames: &[Then],
) -> Vec<(String, crate::work_counts::SettingsPageWork)> {
    let sweep = Sweep::new(scenario);
    let (mut runtime, instance, view) = sweep.opened(start);
    let _ = crate::work_counts::take_settings_page_work();
    let mut drawn = start;
    let _ = draw(&mut runtime, instance, view, drawn);
    let mut work = vec![(
        format!("opened at {drawn}"),
        crate::work_counts::take_settings_page_work(),
    )];
    for then in frames {
        let frame = match *then {
            Then::Step(step) => {
                drawn = drawn.after(step);
                let _ = draw(&mut runtime, instance, view, drawn);
                format!("{step} to {drawn}")
            }
            Then::Redraw => {
                let _ = compile_settings_view(&runtime, instance, view, &cx_of(drawn));
                format!("redrawn at {drawn}")
            }
        };
        work.push((frame, crate::work_counts::take_settings_page_work()));
    }
    work
}

/// A FRAME BUILDS THE THEMES PAGE NO MORE OFTEN, AND LAYS ITS BARS OUT NO MORE, THAN 006cded90b
/// DID FOR THE SAME INPUT, before the give-way: a redraw, one under a status, a live resize with the
/// keyboard on a sheet's control, a status coming and going in a compact window under the warning
/// and the keyboard on a theme's row each build `[pages, lists measured, docks, bars]` at most.
#[test]
fn a_frame_builds_the_themes_page_no_more_often_than_before_the_give_way() {
    let nord = |keyboard| Scenario {
        keyboard,
        theme: "Nord",
        in_use: "Nord",
    };
    let status = |window, scale| Drawn {
        status: true,
        ..Drawn::bare(window, scale)
    };
    let walks = [
        (
            nord(Keyboard::Off),
            Drawn::bare((864, 500), 1),
            vec![(None, [1, 0, 0, 0]), (Some(Then::Redraw), [1, 0, 0, 0])],
        ),
        (
            nord(Keyboard::Pack),
            status((864, 500), 1),
            vec![
                (None, [2, 0, 0, 3]),
                (Some(Then::Step(Step::Again)), [1, 0, 0, 3]),
                (Some(Then::Redraw), [1, 0, 0, 3]),
                (Some(Then::Redraw), [1, 0, 0, 2]),
            ],
        ),
        (
            nord(Keyboard::Pack),
            status((444, 420), 1),
            vec![
                (None, [3, 2, 0, 3]),
                (Some(Then::Step(Step::Again)), [1, 2, 0, 3]),
                (Some(Then::Redraw), [1, 2, 0, 3]),
                (Some(Then::Redraw), [1, 0, 0, 2]),
            ],
        ),
        (
            nord(Keyboard::Sheet("settings/control/background")),
            status((444, 420), 2),
            vec![
                (None, [2, 1, 7, 4]),
                (Some(Then::Step(Step::Window((443, 420)))), [3, 0, 6, 3]),
                (Some(Then::Step(Step::Window((442, 420)))), [3, 0, 6, 3]),
                (Some(Then::Redraw), [2, 0, 6, 3]),
                (Some(Then::Redraw), [2, 0, 6, 3]),
            ],
        ),
        (
            nord(Keyboard::On("settings/showcase/card/catppuccin-mocha")),
            Drawn::bare((244, 440), 1),
            vec![
                (None, [6, 0, 0, 0]),
                (Some(Then::Step(Step::Warning)), [2, 4, 0, 0]),
                (Some(Then::Step(Step::Status)), [1, 7, 6, 4]),
                (Some(Then::Redraw), [1, 2, 6, 4]),
                (Some(Then::Redraw), [1, 2, 6, 4]),
                (Some(Then::Step(Step::Status)), [1, 0, 0, 0]),
            ],
        ),
        (
            nord(Keyboard::Row("Dracula")),
            Drawn::bare((300, 360), 1),
            vec![
                (None, [3, 0, 4, 0]),
                (Some(Then::Step(Step::Status)), [2, 3, 7, 4]),
                (Some(Then::Step(Step::Again)), [2, 0, 6, 4]),
                (Some(Then::Redraw), [2, 0, 6, 4]),
                (Some(Then::Redraw), [2, 0, 6, 3]),
                (Some(Then::Step(Step::Status)), [1, 0, 4, 0]),
            ],
        ),
    ];
    let mut over = Vec::new();
    for (scenario, start, frames) in walks {
        let thens = frames
            .iter()
            .filter_map(|(then, _)| *then)
            .collect::<Vec<_>>();
        let work = page_work(scenario, start, &thens);
        for ((frame, work), (_, [pages, measures, docks, bars])) in work.into_iter().zip(&frames) {
            let at_most = crate::work_counts::SettingsPageWork {
                pages: *pages,
                measures: *measures,
                docks: *docks,
                bars: *bars,
            };
            if work.pages > at_most.pages
                || work.measures > at_most.measures
                || work.docks > at_most.docks
                || work.bars > at_most.bars
            {
                over.push(format!(
                    "{scenario}, {frame}: {work:?}, at most {at_most:?}"
                ));
            }
        }
    }
    assert!(
        over.is_empty(),
        "frames over their page work:\n{}",
        over.join("\n")
    );
}
