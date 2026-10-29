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
const MOCHA: &str = "settings/showcase/card/catppuccin-mocha";
/// What the walks press: Customize, Done, a colour well, a palette slot, Save as and a theme's row.
const PRESSES: [&str; 6] = [
    CUSTOMIZE,
    DONE,
    BACKGROUND_WELL,
    PALETTE_SLOT,
    SAVE_AS,
    MOCHA,
];
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
    /// This theme's row, put there before the first frame: one next to the theme in use's.
    Row(&'static str),
    /// This control of the chooser, put there before the first frame.
    On(&'static str),
    /// This control of the Customize sheet, put there once Customize is pressed on the drawn page.
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
    /// Every walk from every start ([`walk_from`]), and random walks from this seed.
    Every(u64),
    /// Only walks the landing's row decides: the opening (R1, R3) and the keyboard put on it (R2).
    Opening,
    /// Only the regression walks ([`reviewed`]).
    Reviewed,
    /// The walks the keyboard's control decides ([`walk_from`]), and random walks.
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
    /// Up, Down, an "o" typed and Backspace, Left and Right, each the person's key.
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
    /// How wide the list is: a control no wider is never cut at a side.
    list_wide: f32,
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

    /// How much of `key` the screen paints, and how tall it is laid out; `None` where not laid out.
    fn shown(&self, key: &str) -> Option<(f32, f32)> {
        laid_of(&self.screen, key).map(|(rect, shown)| (shown * self.across(key), rect.height))
    }

    /// The share of `key`'s width the screen paints: a control cut at a side is not whole.
    fn across(&self, key: &str) -> f32 {
        let node = self
            .screen
            .paint
            .iter()
            .find(|node| node.key.as_str() == key);
        node.filter(|node| node.rect.width > 0.0)
            .map_or(1.0, |node| {
                let wide = node.rect.right().min(node.clip.right()) - node.rect.x.max(node.clip.x);
                if wide + 0.5 >= node.rect.width {
                    1.0
                } else {
                    wide.max(0.0) / node.rect.width
                }
            })
    }

    /// The width of `key` painted where a side cuts it though the list is as wide; else `None`.
    fn cut_across(&self, key: &str) -> Option<f32> {
        let (rect, shown) = laid_of(&self.screen, key)?;
        let across = self.across(key);
        (shown > 0.5 && across < 1.0 && rect.width <= self.list_wide + 0.5)
            .then_some(across * rect.width)
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
        self.chooser() && whole_in_list(&self.screen, key) && self.across(key) >= 1.0
    }

    fn first(&self) -> Option<UiKey> {
        let (plan, top) = self.plan.as_ref().zip(self.top)?;
        plan.identities(top).next().cloned()
    }

    /// Where the list lays `key` out here: its top, height and how much is painted; none off it.
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

/// One screen of a reach: how much of the control, landing and first row it shows, and where.
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

/// EVERY SCREEN A LIST DRAWS SHOWING WHAT A RULE READS, held at each scroll ([`Sweep::measure`]).
#[derive(Clone)]
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
    /// Whether the keyboard's control is pinned around the list, not a stop of it.
    pinned: bool,
    /// What undrawn screens show of the control: none of a listed one; of a pinned one, as drawn.
    elsewhere: Option<Kept>,
    /// The chrome its screens are held in ([`PAGE_CHROME_HELD`]): the key's, else the first's.
    chrome: PageChrome,
    /// The screen the first frame of the freshly opened page seats.
    seated: Screenful,
}

/// A plain status's bar under the page as a frame draws it, and the least it can be. The bar's
/// height is the code's choice down to that least (it shortens for the control the person is on,
/// down to its words' one line), so a room it leaves is geometry only as the bar at its least
/// leaves it ([`Sweep::judge`]).
#[derive(Clone, Copy, Debug)]
struct StatusBar {
    tall: f32,
    least: f32,
    docked: bool,
}

impl StatusBar {
    /// The bar `screen` draws for its plain status, if any, measured with the view its frame drew.
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

    /// Whether the page drew it otherwise than at its least: taller, or docked in the toolbar.
    fn not_least(self) -> bool {
        self.docked || self.tall > self.least + 0.5
    }
}

/// EVERY CHROME A FRAME AT `drawn` CAN BE DRAWN IN: each rung, and under a bar each giving room.
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

    /// The screen that shows the most of the keyboard's control, and how much.
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

    /// What R2 asks of the control here: `owed` where a screen shows it so, else the most shown.
    fn asked(&self, owed: Kept) -> Kept {
        if self.covering(owed).is_some() {
            return owed;
        }
        self.most().map_or(Kept::Part(0.0), |(_, most)| most)
    }

    /// A scroll whose screen shows the landing's row whole in the list and the control as `asked`.
    fn landing_with(&self, asked: Option<Kept>) -> Option<usize> {
        self.find(|screen| {
            screen.landing
                && asked.is_none_or(|asked| screen.control.is_some_and(|kept| kept.covers(asked)))
        })
    }

    /// Whether the seated screen shows the landing's row whole and the control as `asked`.
    fn seats_landing_with(&self, asked: Option<Kept>) -> bool {
        self.seated.landing
            && asked.is_none_or(|asked| self.seated.control.is_some_and(|kept| kept.covers(asked)))
    }

    /// A scroll whose screen shows the first row whole in the list and the control as `asked`.
    fn first_row_with(&self, asked: Kept) -> Option<usize> {
        self.find(|screen| {
            screen.first_row && screen.control.is_some_and(|kept| kept.covers(asked))
        })
    }

    /// How much of the control the screen at `scroll` shows: none where that scroll drew another.
    fn control_at(&self, scroll: usize) -> Option<Kept> {
        match self.screens.get(&scroll) {
            Some(screen) => screen.control,
            None if self.looked.contains(&scroll) => None,
            None => self.elsewhere,
        }
    }

    /// Scrolls whose screens were drawn in a chrome not the reach's: none, or no page is measured.
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

    /// Why no screen shows `at`'s place as `owed` asks, by geometry; `Err` (a fault) if it fits.
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
    /// Whether compact window statuses stack under the page, never docked ([`STATUS_UNDOCKED`]).
    undocked: bool,
    /// The chooser's filter.
    filter: String,
    /// What else the walk's view held that lays the page out.
    held: Held,
    /// The chrome screens are held in ([`PAGE_CHROME_HELD`]), or none for the page's own choice.
    chrome: Option<Chrome>,
    /// The row its rule reads besides the keyboard's control.
    reads: Reads,
}

/// The row a rule reads beside the control: none (R2, R4), the landing (R3) or first (R5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Reads {
    Control,
    Landing,
    FirstRow,
    /// The landing's row beside the control as much as asked: the screens that show the control.
    Beside(Option<u32>),
}

/// WHAT A WALK'S VIEW HOLDS THAT LAYS THE PAGE OUT past the scenario and [`ReachOf`]'s fields.
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
    /// Each key an edit laid over the config: its field's value and its authored one.
    laid_over: BTreeMap<String, (Option<String>, Option<String>)>,
    /// The theme on trial, and what its Keeps and settling left the chooser.
    trial: ShowcaseTrial,
    settled: Option<showcase_page::Settled>,
    pending_select: Option<showcase_page::PendingSelect>,
    pending_keep: Option<showcase_page::PendingKeep>,
    error: Option<String>,
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
            laid_over: state
                .underlay
                .keys()
                .map(|key| {
                    let seed = state.field_by_key(key).and_then(|field| field.seed.clone());
                    (key.clone(), (seed, state.raw_value(key)))
                })
                .collect(),
            trial: state.showcase.trial.clone(),
            settled: state.showcase.settled.clone(),
            pending_select: state.showcase.pending_select.clone(),
            pending_keep: state.showcase.pending_keep.clone(),
            error: state.showcase.error.clone(),
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
        state.showcase.trial.clone_from(&self.trial);
        state.showcase.settled.clone_from(&self.settled);
        state
            .showcase
            .pending_select
            .clone_from(&self.pending_select);
        state.showcase.pending_keep.clone_from(&self.pending_keep);
        state.showcase.error.clone_from(&self.error);
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
        for (key, (seed, raw)) in &self.laid_over {
            let under = crate::native_settings::Underlay {
                raw: state.raw_value(key),
                seed: state.field_by_key(key).map(|field| field.seed.clone()),
            };
            state.underlay.insert(key.clone(), under);
            match raw {
                Some(raw) => state.raw_values.insert(key.clone(), raw.clone()),
                None => state.raw_values.remove(key),
            };
            if let Some(index) = state.field_index_of(key) {
                state.legacy.fields[index].seed.clone_from(seed);
            }
        }
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

/// A scenario's sweep: its landing and control, and the reaches it has measured.
struct Sweep {
    scenario: Scenario,
    landing: String,
    /// The keyboard's control as the scenario puts it.
    control: Option<String>,
    /// The scenario's configuration, read once: every view opens on it.
    snapshot: crate::native_config_service::ConfigSnapshot,
    seen: Mutex<HashMap<ReachOf, Arc<OnceLock<Arc<Reach>>>>>,
    /// Reaches measured, keyed by the list their chrome lays out: key less chrome, plan and bar.
    alike: Mutex<HashMap<String, Arc<Reach>>>,
    /// The screens its reaches drew.
    screens: AtomicUsize,
}

/// Where a screen lays a stop under the list's top: its top, its height and how much is painted.
type Placed = fn(&Screenful) -> Option<Place>;

/// Where a stop is laid out under the list's top: its top, its height, and how much of it the
/// screen paints ([`Frame::placed`]).
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
            alike: Mutex::new(HashMap::new()),
            screens: AtomicUsize::new(0),
        }
    }

    /// A view on the scenario's theme, the keyboard where it says: a press on the page at `start`.
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

    /// Where `reach`'s bar is not at its least, the reach `key` names under its least, and why.
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

    /// THE SCREENS OF A FRAME DRAWN IN `chrome`: the page's choice, and the frame's if it held one.
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

    /// EVERY CHROME A FRAME AT `key` CAN BE DRAWN IN, visited until `visit` takes one ([`rungs`]).
    fn each_reach(
        &self,
        key: &ReachOf,
        chrome: PageChrome,
        stacked: bool,
        narrowed: impl Fn(&ReachOf) -> ReachOf,
        mut visit: impl FnMut(&str, &Reach) -> bool,
    ) -> bool {
        let (_, chosen) = self.framed(key, chrome);
        let held = |rung: PageChrome| ReachOf {
            chrome: Some(Chrome(rung)),
            ..key.clone()
        };
        let mut keys = vec![(
            String::new(),
            chosen
                .as_ref()
                .map_or_else(|| key.clone(), |_| held(chrome)),
            false,
        )];
        let mut drawn = vec![chrome];
        if let Some(chosen) = chosen {
            keys.push((
                format!(
                    "the page held its chrome, {chrome:?}: in the one it chooses, {:?}, ",
                    chosen.chrome
                ),
                key.clone(),
                false,
            ));
            drawn.push(chosen.chrome);
        }
        keys.extend(
            rungs(key.drawn)
                .into_iter()
                .filter(|rung| !drawn.contains(rung))
                .map(|rung| {
                    (
                        format!(
                            "the page drew it in {chrome:?}: in {rung:?}, a rung of its ladder, "
                        ),
                        held(rung),
                        true,
                    )
                }),
        );
        let mut at = 0;
        while let Some((said, of, rung)) = keys.get(at).cloned() {
            at += 1;
            let of = if rung { narrowed(&of) } else { of };
            let reach = self.reach(&of);
            if visit(&said, &reach) {
                return true;
            }
            if let Some((least, under)) = self.at_least(&of, &reach)
                && visit(&format!("{said}{under}: at its least "), &least)
            {
                return true;
            }
            if stacked && at == 1 && reach.docked {
                keys.insert(
                    1,
                    (
                        "a status docked in the toolbar: stacked under the page, ".to_string(),
                        ReachOf {
                            undocked: true,
                            ..of
                        },
                        false,
                    ),
                );
            }
        }
        false
    }

    /// R2 AND R5 OF A FRAME SHOWING `now` of the control, owing `owed`: `Err` names one with more.
    fn judge(
        &self,
        key: &ReachOf,
        chrome: PageChrome,
        now: Kept,
        owed: Kept,
    ) -> Result<String, String> {
        let mut covered = None;
        let mut most: Option<(String, usize, Kept)> = None;
        self.each_reach(key, chrome, true, ReachOf::clone, |said, reach| {
            if let Some(scroll) = reach.covering(owed) {
                covered = Some(format!("{said}the screen at scroll {scroll} shows it so"));
                return true;
            }
            if let Some((scroll, kept)) = reach.most()
                && most.as_ref().is_none_or(|(_, _, best)| !best.covers(kept))
            {
                most = Some((said.to_string(), scroll, kept));
            }
            false
        });
        if let Some(covered) = covered {
            return Err(covered);
        }
        if let Some((said, scroll, most)) = most
            && !now.covers(most)
        {
            return Err(format!(
                "no screen shows it so, but {said}the screen at scroll {scroll} shows {most} of \
                 it: not the screen that shows the most"
            ));
        }
        let (reach, chosen) = self.framed(key, chrome);
        let held = ReachOf {
            chrome: chosen.map(|_| Chrome(chrome)),
            ..key.clone()
        };
        let (base, under) = match self.at_least(&held, &reach) {
            Some((least, under)) => (least, format!(", with {under}")),
            None => (reach, String::new()),
        };
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
        let first = draw(&mut runtime, instance, view, drawn);
        // The most room the list can have in this chrome: its body under the bar at its least.
        let body_least = matches!(key.reads, Reads::Beside(_)).then(|| {
            let state = view_state(&mut runtime, view);
            let cx = cx_of(drawn);
            let most = state.status_bar_most.replace(Some(0.0));
            let body = settings_list_body_height(
                state,
                SettingsWidth::for_viewport(cx.viewport.width),
                cx.viewport,
            );
            state.status_bar_most.set(most);
            body
        });
        // Every screen in one chrome: the key's, else the page's choice.
        let chrome = key.chrome.map_or(first.chrome, |chrome| chrome.0);
        PAGE_CHROME_HELD.with(|held| held.set(Some(chrome)));
        let control = key.keyboard.as_deref();
        let screen_of = |frame: &Frame| Screenful {
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
            seated: screen_of(&first),
        };
        // The plan a held screen measures; one drawn before the reveal held the list need not.
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
            let screen = screen_of(&frame);
            Some(*reach.screens.entry(at?).or_insert(screen))
        };
        // No screen in a chrome whose list cannot hold the row and the control both shows them.
        if let Reads::Beside(part) = key.reads
            && let Some((plan, control)) = first.plan.as_ref().filter(|_| first.list).zip(control)
            && plan
                .room_beside(
                    &UiKey::new(self.landing.as_str()),
                    &UiKey::new(control),
                    part.map(f32::from_bits),
                )
                .zip(body_least)
                .is_some_and(|(need, body)| need > body + 1.0)
        {
            STATUS_BAR_LEAST.with(|least| least.set(false));
            STATUS_UNDOCKED.with(|undocked| undocked.set(false));
            PAGE_CHROME_HELD.with(|held| held.set(None));
            return reach;
        }
        // The row whole on the screen at its stop, and beside the control on the upper of the two.
        let tops = |plan: &scroll::ScrollPlan| {
            let listed = |row: &str| plan.stop_of(&UiKey::new(row));
            let (limit, row) = (plan.limit(), listed(&self.landing)?);
            let alone = (key.reads == Reads::Landing).then_some(row.min(limit));
            Some((
                control
                    .and_then(listed)
                    .map_or(row, |stop| stop.min(row))
                    .min(limit),
                alone,
            ))
        };
        let looked =
            |(upper, alone): (usize, Option<usize>)| [Some(upper), alone].into_iter().flatten();
        // These first where the first frame lists the row: held ones listing it alike need none.
        let early = match key.reads {
            Reads::Landing | Reads::Beside(_) => {
                first.plan.as_ref().filter(|_| first.list).and_then(&tops)
            }
            _ => None,
        };
        for top in early.into_iter().flat_map(looked) {
            let _ = look(top, &mut reach);
        }
        let early = early.filter(|asked| {
            reach.strays.is_empty()
                && held
                    .borrow()
                    .as_ref()
                    .and_then(Option::as_ref)
                    .and_then(&tops)
                    == Some(*asked)
        });
        if early.is_none() {
            let _ = look(0, &mut reach);
        }
        let plan = held.borrow_mut().take().flatten();
        // A chrome that lays the list out as one measured already draws its screens alike.
        let first_screen = format!("{:?}", reach.screens.get(&0));
        let alike = early.is_none().then(|| {
            format!(
                "{:?}|{plan:?}|{}|{:?}|{}",
                ReachOf {
                    chrome: None,
                    ..key.clone()
                },
                first_screen.split(", chrome:").next().unwrap_or_default(),
                reach.bar,
                reach.docked
            )
        });
        let seen = alike.as_ref().and_then(|alike| {
            self.alike
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(alike)
                .cloned()
        });
        if let Some(seen) = seen {
            STATUS_BAR_LEAST.with(|least| least.set(false));
            STATUS_UNDOCKED.with(|undocked| undocked.set(false));
            PAGE_CHROME_HELD.with(|held| held.set(None));
            let seated = reach.seated;
            let mut reach = Reach::clone(&seen);
            for screen in reach.screens.values_mut() {
                screen.chrome = Chrome(chrome);
            }
            reach.chrome = chrome;
            reach.seated = seated;
            return reach;
        }
        let limit = plan.as_ref().map_or(0, scroll::ScrollPlan::limit);
        reach.limit = limit;
        let listed =
            |row: Option<&str>| row.and_then(|row| plan.as_ref()?.stop_of(&UiKey::new(row)));
        let beside = early.or_else(|| {
            plan.as_ref()
                .filter(|_| matches!(key.reads, Reads::Landing | Reads::Beside(_)))
                .and_then(&tops)
                .filter(|asked| {
                    looked(*asked).all(|top| look(top, &mut reach).is_some())
                        && reach.strays.is_empty()
                })
        });
        // A row read beside the control shows beside it only on the screens that show the row.
        let scans: [(Option<usize>, Placed); 3] = [
            (
                listed(control).filter(|_| key.reads != Reads::Landing),
                |screen| screen.control_at,
            ),
            (
                listed(Some(&self.landing)).filter(|_| {
                    key.reads == Reads::Landing
                        || (matches!(key.reads, Reads::Beside(_)) && listed(control).is_none())
                }),
                |screen| screen.landing_at,
            ),
            (
                listed(first.first_row.as_deref()).filter(|_| key.reads == Reads::FirstRow),
                |screen| screen.first_row_at,
            ),
        ];
        // From each listed stop's screen while its rect is in the room, rising; else every scroll.
        let mut every = plan.is_none();
        for (stop, at) in scans.map(|(stop, at)| (stop.filter(|_| beside.is_none()), at)) {
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
        // A pinned control shows alike on every screen, the last too, and where drawn ones differ.
        let pinned = control.is_some() && listed(control).is_none();
        reach.pinned = pinned;
        if pinned && beside.is_none() {
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
        if let Some(alike) = alike {
            self.alike
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(alike)
                .or_insert_with(|| Arc::new(reach.clone()));
        }
        reach
    }
}

#[derive(Default)]
struct Findings {
    faults: Vec<(Rule, String)>,
    exceptions: Vec<String>,
    /// Each frame a reach of its own key draws another list for, and how many were held to one.
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
        list_wide: list.map_or(0.0, |list| list.width),
        first_row: state
            .showcase
            .listed()
            .first()
            .map(|slug| format!("{ROW}{slug}")),
        chrome: state.page_chrome.get(),
        screen,
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
    /// The keyboard's control where the screen shows its stop, and how many stops under its first.
    held: Option<(String, usize)>,
}

/// `screen` but the settings sidebar, which scrolls on its own: what a wheel on the list places.
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

/// How far `place` is from the nearest scroll up to `limit` that `shows` what R2 asks, and which.
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
        // A press lands only on a control the last frame laid out, painted or not.
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
        // The keyboard where the event left it: put on a new control, or left where it was.
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
        // The reaches this frame's rules read, in its chrome, each with its key.
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
            // A pinned head shorter than the control excuses nothing: unpinned, the list holds it.
            let head_pinned =
                frame.chooser() && frame.shown(FILTER).is_some() && !frame.listed(FILTER);
            match sweep.judge(&kept_reach_of, frame.chrome, frame.kept(key), *owed) {
                Ok(cause) if head_pinned && cause.starts_with("taller than the room") => fault(
                    *rule,
                    format!("{key} ({how}) {owed} as {why}, under a pinned head: {cause}"),
                ),
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
        // R2: the keyboard's listed control, painted at all, is whole across in a list as wide.
        if let Some(key) = control.as_deref()
            && frame.listed(key)
            && let Some(wide) = frame.cut_across(key)
        {
            fault(
                Rule::R2,
                format!(
                    "{key} (listed) cut at a side: {wide}pt of it across in a list {}pt wide",
                    frame.list_wide
                ),
            );
        }
        // R5: a filter change shows the filter, then its first result whole where both can show.
        if typed
            && control.as_deref() == Some(FILTER)
            && let Some(first) = frame.first_row.as_deref()
            && !frame.in_list(first)
            && let Some((owed, _, _)) = owed.as_ref()
        {
            let first_of = ReachOf {
                reads: Reads::FirstRow,
                ..kept_reach_of.clone()
            };
            let (reach, chosen) = framed(&first_of);
            let shows = |reach: &Reach| reach.first_row_with(reach.asked(*owed));
            // Judged over the rungs as R3 is: another rung, or the bar at its least, at any seat.
            let rungs = || {
                let mut found = None;
                sweep.each_reach(
                    &first_of,
                    frame.chrome,
                    false,
                    ReachOf::clone,
                    |said, reach| {
                        found = shows(reach)
                            .map(|scroll| format!("{said}the screen at scroll {scroll}"));
                        found.is_some()
                    },
                );
                found
            };
            let screen = shows(&reach)
                .map(|scroll| format!("the screen at scroll {scroll}"))
                .or_else(|| {
                    chosen.as_deref().and_then(shows).map(|scroll| {
                        format!("in the chrome the page chooses, the screen at scroll {scroll}")
                    })
                })
                .or_else(rungs);
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
        // R3: before any event the landing's row whole wherever a rung shows it so by the control.
        if opening && frame.listed(landing) && frame.chooser() && !frame.in_list(landing) {
            let landing_of = ReachOf {
                reads: Reads::Landing,
                ..reach_of.clone()
            };
            let _ = framed(&landing_of);
            let asks = owed
                .as_ref()
                .zip(control.as_deref())
                .map(|((owed, _, _), key)| {
                    let now = frame.kept(key);
                    if now.covers(*owed) { *owed } else { now }
                });
            let (mut both, alone) = (None, Cell::new(false));
            // Once a screen shows the landing alone, only screens showing the control are measured.
            let beside = |of: &ReachOf| ReachOf {
                reads: match asks {
                    Some(Kept::Whole) if alone.get() => Reads::Beside(None),
                    Some(Kept::Part(part)) if alone.get() && part > 1.5 => {
                        Reads::Beside(Some(part.to_bits()))
                    }
                    _ => of.reads,
                },
                ..of.clone()
            };
            sweep.each_reach(&landing_of, frame.chrome, false, beside, |said, reach| {
                alone.set(alone.get() || reach.landing_with(None).is_some());
                // The frame's chrome at any seat; another by the screen it seats (a give-way).
                both = if said.is_empty() {
                    reach
                        .landing_with(asks)
                        .map(|scroll| format!("the screen at scroll {scroll}"))
                } else {
                    reach
                        .seats_landing_with(asks)
                        .then(|| format!("{said}the screen it seats"))
                };
                both.is_some()
            });
            let alone = alone.get();
            if let Some(both) = both {
                fault(
                    Rule::R3,
                    format!(
                        "before the person's first event, the theme in use's {landing} is {:?} \
                         (shown, tall), not whole in the list, though {both} shows it whole with \
                         the keyboard's control as R2 asks ({})",
                        frame.shown(landing),
                        asks.map_or("nothing".to_string(), |owed| format!(
                            "{owed} of {}",
                            control.as_deref().unwrap_or_default()
                        ))
                    ),
                );
            } else if !alone {
                let (reach, _) = sweep.framed(&landing_of, frame.chrome);
                match reach.cause(|screen| screen.landing_at, Kept::Whole) {
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
        // R4 holds a frame at the control's stop, taller than the room, that no screen beats.
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
        // R4 after a re-layout: the wheel's place as seat_scroll reads it, or the nearest R2 takes.
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
        // A REACH JUDGES A FRAME BY ITS LIST: each reach read lists its stops as tall, in its room.
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

/// THE WALK FROM `start` through each change `taken` names and back, each drawn again, in one view.
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
    // Which of each wheel's pair is drawn again: the first from every other window, else the other.
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
            // The keyboard on its control through each change and back, then wheels, bars and keys.
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
    // R2 under a status: the bar over the warning and alone, keyboard on its control, short window.
    walk.extend([
        Focus, Warning, Status, Again, Status, Status, Warning, Focus, Status,
    ]);
    // R5: the keys, each drawn again, and the filter typed and erased.
    walk.extend([Down, Again, Up, Again, Up, Type, Again, Step::Erase, Again]);
    walk
}

/// `count` seeded walks of `length` random steps from any bars, half the wheels drawn again.
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
                let width = next(2_800) + 170;
                let height = next(800) + 150;
                (
                    u16::try_from(width).unwrap_or(u16::MAX),
                    u16::try_from(height).unwrap_or(u16::MAX),
                )
            };
            let start = Drawn {
                warning: next(3) == 0,
                status: next(2) == 0,
                ..Drawn::bare(window(&mut next), [1, 2][next(2)])
            };
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
    let mocha = Scenario {
        keyboard: Keyboard::On(MOCHA),
        ..filter
    };
    let one_dark = Scenario {
        keyboard: Keyboard::Row("One Dark"),
        ..drawn
    };
    let solarized = Scenario {
        keyboard: Keyboard::Row("Solarized Dark"),
        ..filter
    };
    let nord_over_dracula = Scenario {
        keyboard: Keyboard::Row("Nord"),
        ..first
    };
    let w380x300 = (380, 300);
    let at = Drawn::bare;
    let warned = |window, scale| Drawn {
        warning: true,
        ..at(window, scale)
    };
    let statused = |window, scale| Drawn {
        status: true,
        ..at(window, scale)
    };
    let warned_statused = |window, scale| Drawn {
        warning: true,
        ..statused(window, scale)
    };
    vec![
        // Before any event, the rung showing the landing whole with the keyboard's control (R3).
        (row_default, at(w864x292, 1), vec![]),
        (row, at((520, 691), 2), vec![Status]),
        // The chooser's pins leave the list a stop of what a status's bar at its most leaves (R3).
        (row, statused((244, 440), 1), vec![]),
        // A status under a held give-way chrome leaves it to the page, the landing whole (R3).
        (row_after, at((238, 668), 2), vec![Status]),
        // A theme row pressed twice is kept, which the reaches hold too.
        (
            row,
            Drawn {
                warning: true,
                ..statused((307, 539), 2)
            },
            vec![Press(MOCHA), Press(MOCHA), Window((173, 394))],
        ),
        // The rungs that give a status's bar room are R3's rungs with no bar up too.
        (row, warned((244, 440), 1), vec![Warning]),
        (latte, warned((244, 440), 1), vec![Warning]),
        // The chooser's pins read a status's bar at its most, which no settling moves (R1).
        (filter_tokyo, statused((244, 440), 1), vec![Down, Tick]),
        (
            pack,
            at(w864x292, 1),
            vec![
                Again,
                Window(w864x500),
                Tick,
                Window(w864x292),
                Window(w1095x700),
                Again,
                Window(w864x292),
                Window(w1284x714),
            ],
        ),
        // Where no rung shows the keyboard's control as owed, the one showing the most of it (R2).
        (
            drawn,
            at((380, 318), 1),
            vec![Status, Press(BACKGROUND_WELL)],
        ),
        // A filter just changed, its first result cut, yields to the rung that shows it whole (R5).
        (filter_tokyo, warned((2_937, 226), 1), vec![Type, Erase]),
        // A first result shown whole at the list's top stays revealed when drawn again (R1).
        (
            filter_tokyo,
            at((178, 348), 2),
            vec![Type, Window((980, 420)), Erase, Again],
        ),
        // A pinned filter's change shows its first result beside it in a rung giving way (R5).
        (
            filter,
            warned((2_884, 321), 1),
            vec![
                Wheel(-8),
                Warning,
                Window((2_336, 701)),
                Window((2_172, 268)),
                Refresh,
                Window((1_320, 805)),
                Again,
                Window((387, 789)),
                Focus,
                Wheel(12),
                Again,
                Focus,
                Right,
                Window((2_474, 564)),
                Wheel(-8),
                Window((1_619, 225)),
                Wheel(-8),
                Press(BACKGROUND_WELL),
                Refresh,
                Warning,
                Type,
                Tick,
                Wheel(1),
                Erase,
            ],
        ),
        // A warning over a press's status gives way for the control its bar's side cuts (R2).
        (
            drawn,
            warned((2_765, 332), 2),
            vec![
                Right,
                Down,
                Focus,
                Text,
                Window((2_329, 175)),
                Press(PALETTE_SLOT),
                Window((1_027, 703)),
                Warning,
                Press(PALETTE_SLOT),
                Window((2_241, 919)),
                Window((2_111, 270)),
                Status,
                Warning,
            ],
        ),
        // A wheel's place gives way to the screen showing the most of the keyboard's control (R2).
        (
            closed,
            at((1_407, 841), 2),
            vec![Status, Wheel(-12), Window((189, 200))],
        ),
        // A rung whose stop-led screens paint a wheel no more is searched where R2 seats partway.
        (
            pack,
            warned_statused((413, 1_050), 1),
            vec![
                Text,
                Window((1_356, 130)),
                Press(CUSTOMIZE),
                Warning,
                Press(CURSOR_COLOR),
                Warning,
                Text,
                Status,
            ],
        ),
        (
            background,
            warned((2_598, 662), 1),
            vec![
                Text,
                Warning,
                Text,
                Window((1_408, 176)),
                Status,
                Text,
                Press(CURSOR_COLOR),
                Warning,
                Text,
            ],
        ),
        // A text size's change owes what the last screen showed, as the new size lays it out (R2).
        (folder, at((1_411, 151), 2), vec![Warning, Text]),
        (import, at((1_411, 151), 2), vec![Warning, Text]),
        (import_light, at((1_411, 151), 2), vec![Warning, Text]),
        (save_as, at((1_240, 151), 2), vec![Warning, Text]),
        (
            save_as,
            at((1_240, 151), 2),
            vec![Warning, Wheel(3), Again, Text],
        ),
        (filter, warned((1_411, 151), 2), vec![Text]),
        (folder, warned((1_411, 151), 2), vec![Text]),
        (background, warned((1_411, 151), 2), vec![Text]),
        (row, warned((1_411, 151), 2), vec![Text]),
        // A press that moves the keyboard after the opening shows the control it moved to (R2).
        (
            first,
            at(w864x292, 1),
            vec![Type, Window((1_268, 165)), Press(DONE)],
        ),
        // A status docks only where that leaves the keyboard's row as much as stacking does (R2).
        (mocha, at((244, 440), 1), vec![Warning, Status]),
        // A filter's change shows its first result in a chrome whose list holds a row (R5).
        (
            filter_tokyo,
            at((654, 788), 1),
            vec![
                Status,
                Warning,
                Status,
                Text,
                Status,
                Window((1_094, 193)),
                Wheel(2),
                Text,
                Type,
            ],
        ),
        // The chooser pins its head only where a status's bar leaves the list a stop under it (R2).
        (
            import_light,
            at((300, 360), 2),
            vec![Status, Wheel(3), Text],
        ),
        (
            filter_tokyo,
            statused((989, 715), 2),
            vec![Window((363, 309)), Text],
        ),
        // Pins and stacked rows keep the tallest row as the list's width lays it out (R2, R3).
        (row, statused((368, 438), 2), vec![]),
        (row, statused((210, 350), 1), vec![]),
        (
            import_light,
            at((562, 396), 1),
            vec![
                Window((2_571, 815)),
                Status,
                Focus,
                Down,
                Text,
                Erase,
                Window((2_570, 815)),
                Window((373, 472)),
            ],
        ),
        // An open picker a status's bar would cut is sized so the bar leaves its wheel whole (R2).
        (
            save_as,
            statused((2_218, 159), 2),
            vec![Press(BACKGROUND_WELL)],
        ),
        (
            well,
            statused((1_421, 946), 2),
            vec![Press(BACKGROUND_WELL), Window((2_945, 164))],
        ),
        (
            customize,
            Drawn {
                warning: true,
                ..statused((419, 267), 2)
            },
            vec![
                Press(CUSTOMIZE),
                Press(BACKGROUND_WELL),
                Up,
                Window((2_452, 198)),
                Warning,
            ],
        ),
        // A bar gone and none up, a rung giving its room is tried after every rung keeping it (R2).
        (
            save_as,
            statused((1_593, 928), 2),
            vec![Window((2_875, 138)), Status],
        ),
        // A reach holds the sheet's edits as the walked view does.
        (
            side,
            at((1_044, 672), 2),
            vec![
                Text,
                Press(CUSTOMIZE),
                Press(BACKGROUND_WELL),
                Text,
                Right,
                Text,
                Wheel(-8),
                Window((556, 449)),
            ],
        ),
        // The palette slot editor's tools inside a narrow window's list (R2).
        (
            palette,
            Drawn {
                status: true,
                ..warned(w178x348, 2)
            },
            vec![Press(PALETTE_SLOT), Down, Down],
        ),
        // A focused Close 0.5pt past the column keeps its line; a point past, it wraps (R2).
        (
            palette_first,
            warned((299, 1_422), 2),
            vec![Press(PALETTE_SLOT), Down, Down, Window((298, 1_422))],
        ),
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
        // A status over a pinned control (filter, strip's Done), counted whole by its clipped rect.
        (filter, at(w444x420, 2), vec![Focus, Status]),
        (filter, at(w444x420, 2), vec![Wheel(8), Status]),
        (first, at((640, 480), 2), vec![Status]),
        // A status took the control out of view, the list sent where no wheel had put it.
        (closed, at(w444x292, 2), vec![Window((600, 200)), Status]),
        (row, at((600, 200), 1), vec![Window((380, 300)), Status]),
        // The keyboard on the row next to the landing's before the first frame, shown with it (R3).
        (row, at(w1284x714, 2), vec![Again]),
        (row, at(w178x348, 1), vec![Status, Text]),
        // Reaches in windows no rule was tuned on.
        (
            drawn,
            at((380, 300), 1),
            vec![Wheel(4), Status, Again, Status, Wheel(-4), Status],
        ),
        // The person's own keys, the keyboard left on its control (R5).
        (filter, at((230, 420), 2), vec![Up]),
        (closed, at(w300x160, 1), vec![Up]),
        (side, at(w444x180, 1), vec![Up, Up]),
        (filter, at(w178x348, 1), vec![Down, Type]),
        // The keyboard by the landing's row before the first frame, the landing a frame late (R3).
        (
            row_after,
            at(w1284x714, 2),
            vec![Window(w178x348), Window(w1284x714)],
        ),
        (row_after, at(w584x348, 2), vec![Text]),
        (row_after, at(w178x348, 2), vec![Text]),
        (row_default, at(w444x292, 2), vec![Window((980, 420))]),
        (pack, at((1_440, 900), 2), vec![Text]),
        // A status over a row taller than the room: the least bar shows 64pt, words whole (R2).
        (row_after, at(w178x348, 2), vec![Focus, Warning, Status]),
        (row_after, at(w178x348, 2), vec![Warning, Status, Focus]),
        (closed, at(w178x348, 2), vec![Focus, Warning, Status]),
        (closed, at(w178x348, 2), vec![Status, Status]),
        // A filter change held a short result list's chrome, the long list back in less room (R5).
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
        // A long name wrapped at 2x in the faded foot, counted shown by code, not paint (R3, R4).
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
        // Up on Customize, which the last screen showed cut, scrolled it out of view (R5).
        (closed, at((600, 200), 2), vec![Status, Focus, Up]),
        (
            closed,
            at(w444x292, 2),
            vec![Window((600, 200)), Status, Focus, Up],
        ),
        // The wheel's place over a re-layout, the filter its screen listed (R4).
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
        // What a view holds past the opening that lays the page out, as a reach does ([`Held`]).
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
        // A control the room holds, whole on no screen of a list scrolling by stops (R2, R3, R5).
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
        // Round 9: Again after a wheel back to the top, which chose the page's chrome again (R1).
        (background, at(w444x420, 1), vec![Wheel(-12), Again]),
        (drawn, at((900, 260), 1), vec![Warning, Wheel(-12), Again]),
        (palette, at((260, 500), 1), vec![Warning, Wheel(-12), Again]),
        (save_as, at((380, 300), 1), vec![Status, Wheel(-12), Again]),
        (
            save_as,
            at((380, 300), 1),
            vec![Status, Warning, Wheel(-12), Again],
        ),
        // Round 9: the footer under bars, a switch no stop showed or the edge cut (R2, R5).
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
        // Round 9: a wheel's place under a status, the keyboard on Done, moved by the warning (R4).
        (drawn, at((380, 300), 1), vec![Status, Wheel(-4), Warning]),
        // Round 9's review: a reach whose screens were drawn in another chrome than the frame's.
        (save_as, at((380, 300), 1), vec![Wheel(1), Status]),
        // Round 9's review: a row the list draws as a picture, its cause measured as shown.
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
        // Round 9: a colour row's well and the one over it in the room a status leaves (R2, R5).
        (well, at((380, 300), 1), vec![Status]),
        (field, at(w444x420, 2), vec![Status, Up]),
        // Round 10: text size there and back after a wheel, choosing the sheet's chrome again (R4).
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
        // Round 10: a chrome kept or chosen again that cut the keyboard's control under a bar (R2).
        (
            background,
            at((1_385, 640), 1),
            vec![Window((1_268, 165)), Refresh, Up, Warning, Tick, Again],
        ),
        (drawn, at((1_195, 191), 2), vec![Warning, Text]),
        // Round 10: the pack row at 2x text, and the landing under a status at first (R2, R3).
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
        // Round 10's walks at any window size (each goes on from where the one before left).
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
        // Round 10: a landing under a status, undrawn reveals, held chromes, tall rows, sidebar.
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
        // Round 11: a window back at a drawn size, recalling a chrome that cut the control (R2).
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
        // Round 11's review: a chrome that cut the control where another rung showed it whole (R2).
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
        // Round 11: the keyboard on Settings' search, under a status's bar or docked over (R2).
        (search, at(w178x348, 2), vec![Focus, Warning, Status]),
        (search, at(w178x348, 2), vec![Wheel(8), Warning, Status]),
        (search, at(w178x348, 2), vec![Wheel(1), Warning]),
        (search, at(w444x420, 1), vec![Warning, Status]),
        // Round 11: a colour well crossed by the list's floor or edge, owed what it painted (R2).
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
        // Round 11, widened: no rung shows a cut control less; a docked status takes no more (R2).
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
        // Wheeled off follow-system, at 197x316 at 2× text its switch split from its row (R2).
        (
            following,
            at((1_048, 364), 2),
            vec![
                Wheel(-8),
                Status,
                Warning,
                Window((1_136, 886)),
                Text,
                Window((197, 316)),
                Text,
                Again,
            ],
        ),
        // Any window: the pack row taller than a status's room, owed more than a stop shows (R2).
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
        // A chrome told to cut the keyboard's control owes it what its own seat's screen does.
        (
            field,
            at((273, 187), 1),
            vec![
                Text,
                Press(BACKGROUND_WELL),
                Warning,
                Window((1_263, 816)),
                Status,
                Text,
                Window((1_371, 214)),
            ],
        ),
        // Random walks: back at the wheel's window and bars, the sidebar scrolled on its own (R4).
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
        // Round 8b: a give-way judged by the control painted never cuts a pinned Import (R3, R5).
        (import, at((994, 328), 1), vec![Status]),
        (import_light, statused((2_194, 443), 2), vec![]),
        // Before any event a give-way takes the judge's rung, told as its own list seats it (R3).
        (customize, warned((2_792, 278), 1), vec![]),
        (filter_tokyo, warned((980, 263), 1), vec![]),
        (row_after, at((1_282, 153), 1), vec![]),
        (mocha, at((240, 589), 1), vec![]),
        (latte, at((1_002, 150), 1), vec![]),
        (import_light, at((1_355, 735), 2), vec![]),
        // An open picker that its sheet's room overflows, docked or stacked (R2).
        (
            filter_tokyo,
            at((443, 672), 1),
            vec![
                Press(CUSTOMIZE),
                Window((351, 398)),
                Status,
                Warning,
                Press(BACKGROUND_WELL),
            ],
        ),
        // Held through Done, a chrome yields to a rung (bar room or not) showing Customize (R2).
        (
            background,
            at((173, 251), 2),
            vec![
                Press(BACKGROUND_WELL),
                Up,
                Erase,
                Again,
                Window((172, 251)),
                Press(PALETTE_SLOT),
                Focus,
                Press(DONE),
            ],
        ),
        // The rung whose told screen shows the most more of a long name is the one seated (R2).
        (latte, at((1_003, 573), 2), vec![Window((299, 536)), Status]),
        // A filter pinned around the list is revealed where the chrome chosen again lists it (R5).
        (
            filter_tokyo,
            Drawn {
                warning: true,
                ..statused((2_193, 260), 2)
            },
            vec![Window((2_192, 260)), Warning, Focus, Type],
        ),
        // Round 8d: a row a gutter narrower, and shorter, tells no rung too short (R3).
        (one_dark, at((294, 1_005), 2), vec![]),
        (one_dark, statused((268, 552), 1), vec![Window((213, 705))]),
        (one_dark, statused((213, 705), 1), vec![]),
        (
            pack,
            Drawn {
                warning: true,
                ..statused((2_179, 575), 2)
            },
            vec![
                Status,
                Text,
                Window((1_939, 1_483)),
                Wheel(-30),
                Down,
                Wheel(2),
                Window((214, 1_028)),
            ],
        ),
        // Round 8e: a give-way's told seat and the judge's seat of one rung disagreed (R3).
        (latte, statused((460, 380), 2), vec![Text]),
        (row, at((275, 572), 1), vec![Status, Text]),
        // Round 8f: a stop fitting its list under the bar can overrun it calm, the page reshaped.
        (first, warned((864, 292), 1), vec![Press(BACKGROUND_WELL)]),
        (
            foreground,
            at((2_083, 294), 1),
            vec![Warning, Window((2_411, 216)), Press(BACKGROUND_WELL)],
        ),
        // Under a bar, a rung keeping its room can show the reveal the chrome giving it cut (R3).
        (off, statused((204, 166), 1), vec![]),
        (off, statused((216, 246), 2), vec![]),
        // A held chrome's choice is judged by the screen it seats, which a give-way reads too (R3).
        (folder, warned_statused((2_094, 249), 2), vec![Text]),
        (folder, warned_statused((1_558, 248), 2), vec![Text]),
        (folder, warned_statused((223, 155), 1), vec![Status]),
        (import_light, warned_statused((2_022, 244), 2), vec![Text]),
        (filter, warned_statused((1_771, 250), 2), vec![Again, Text]),
        (row_after, warned_statused((2_064, 154), 2), vec![Warning]),
        (row_after, warned_statused((2_345, 169), 2), vec![Warning]),
        // Round 8g: a key on a control the landing's chrome pinned keeps it in view, re-laid (R5).
        (customize, warned_statused((312, 688), 2), vec![Up]),
        (
            customize,
            warned_statused((312, 688), 2),
            vec![Status, Status, Down],
        ),
        // Round 8g: a wheel at the list's end holds its screen, not the landing's before it (R4).
        (
            import,
            warned_statused((222, 1_231), 1),
            vec![Wheel(2), Window((993, 597))],
        ),
        (
            folder,
            warned_statused((222, 1_231), 1),
            vec![Wheel(2), Window((993, 597))],
        ),
        (
            import_light,
            statused((698, 671), 1),
            vec![
                Tick,
                Window((2_092, 726)),
                Warning,
                Window((272, 729)),
                Text,
                Wheel(1),
                Text,
                Window((713, 483)),
                Status,
            ],
        ),
        // Walks once known to fault here and on main, which neither breaks now.
        (import_light, warned_statused((802, 868), 2), vec![]),
        (mocha, warned((371, 356), 2), vec![]),
        (mocha, warned_statused((435, 850), 2), vec![]),
        (mocha, warned_statused((490, 844), 2), vec![]),
        (row_after, warned_statused((325, 739), 2), vec![]),
        (following, warned_statused((342, 709), 1), vec![]),
        (
            row,
            warned_statused((1_897, 219), 1),
            vec![Window((2_904, 439)), Window((258, 495))],
        ),
        (
            import_light,
            warned_statused((1_477, 133), 1),
            vec![Window((1_476, 133)), Window((2_813, 285))],
        ),
        (
            folder,
            warned_statused((282, 287), 2),
            vec![Window((1_980, 296))],
        ),
        (
            follow,
            warned_statused((2_438, 221), 2),
            vec![
                Text,
                Window((2_438, 220)),
                Text,
                Window((2_438, 219)),
                Again,
                Press(DONE),
                Again,
                Status,
            ],
        ),
        (
            off,
            statused((1_480, 1_153), 1),
            vec![
                Right,
                Down,
                Wheel(4),
                Again,
                Focus,
                Again,
                Wheel(-4),
                Again,
                Erase,
                Warning,
                Wheel(-2),
                Down,
                Wheel(8),
                Status,
                Window((2_874, 1_046)),
                Window((158, 871)),
                Text,
                Window((158, 870)),
                Press(SAVE_AS),
                Wheel(30),
                Again,
                Wheel(2),
                Down,
                Window((157, 870)),
                Status,
                Warning,
                Press(MOCHA),
            ],
        ),
        (row_after, statused((224, 812), 1), vec![Text]),
        (nord_over_dracula, statused((382, 729), 2), vec![]),
        (
            row_after,
            warned((578, 490), 2),
            vec![Press(MOCHA), Window((365, 417))],
        ),
        (
            foreground,
            warned((231, 621), 2),
            vec![
                Press(DONE),
                Warning,
                Warning,
                Press(MOCHA),
                Window((371, 380)),
            ],
        ),
        (
            off,
            statused((1_776, 633), 1),
            vec![Focus, Warning, Down, Text, Down, Window((369, 421))],
        ),
        (
            side,
            statused((1_320, 1_446), 1),
            vec![Text, Press(MOCHA), Warning, Window((370, 404))],
        ),
        // Once a known fault here and on main; now main (c5ef831d7f) alone breaks it (R2).
        (pack, warned((173, 139), 2), vec![Window((173, 138))]),
        // A sheet not taking the listed toolbar draws the rung it leans on, no list under it (R2).
        (field, statused((930, 148), 1), vec![Text]),
        (
            first,
            statused((1_121, 146), 1),
            vec![Down, Status, Text, Status, Window((1_121, 147))],
        ),
        (
            foreground,
            statused((1_426, 1_026), 2),
            vec![Window((713, 148))],
        ),
        // A frame giving way lays rows out in the whole panel, as R3's shape-only bounds read it.
        (latte, warned_statused((164, 386), 1), vec![]),
        (latte, warned_statused((178, 348), 2), vec![Warning]),
        // R2's search skips a rung for its rooms only where it lays its rows out as the chrome's.
        (solarized, statused((308, 156), 2), vec![]),
        // A key keeping its control, in another chrome, owes what the chrome drawn showed (R5).
        (foreground, warned((718, 125), 2), vec![Up, Up]),
        // A press's status keeps the chrome; a warning or window still gives way for it (R2).
        (
            search,
            at((2_892, 422), 2),
            vec![
                Warning,
                Window((1_438, 142)),
                Text,
                Press(MOCHA),
                Warning,
                Status,
                Press(MOCHA),
                Text,
                Warning,
            ],
        ),
        // The keyboard off the chooser, the filter is pinned under a status's bar as on main (R1).
        (search, at((178, 538), 1), vec![Status, Refresh]),
        (
            search,
            warned((178, 538), 1),
            vec![Warning, Status, Refresh],
        ),
        (search, warned((178, 538), 1), vec![Warning, Status, Tick]),
        // A give-way is remembered only at its window's width; R2's search for more is not.
        (
            follow,
            warned((1_782, 680), 2),
            vec![
                Window((2_466, 680)),
                Window((1_395, 930)),
                Wheel(4),
                Again,
                Focus,
                Press(PALETTE_SLOT),
                Window((778, 460)),
                Press(BACKGROUND_WELL),
                Wheel(8),
                Wheel(3),
                Window((2_633, 737)),
                Again,
                Warning,
                Down,
                Again,
                Text,
                Warning,
                Window((1_188, 478)),
                Window((2_113, 596)),
                Warning,
                Up,
                Wheel(-12),
                Again,
                Status,
                Window((1_320, 390)),
                Wheel(-4),
                Right,
                Wheel(4),
                Right,
                Warning,
            ],
        ),
        (
            import,
            statused((1_367, 544), 2),
            vec![
                Again,
                Window((1_708, 1_197)),
                Up,
                Wheel(-5),
                Again,
                Wheel(20),
                Again,
                Warning,
                Window((1_708, 1_196)),
                Window((1_046, 1_270)),
                Window((1_046, 1_271)),
                Up,
                Warning,
                Warning,
                Warning,
                Window((2_040, 215)),
                Window((2_040, 216)),
                Type,
                Window((2_039, 216)),
                Window((2_040, 216)),
                Warning,
                Type,
                Status,
                Again,
                Status,
                Status,
                Right,
                Focus,
                Wheel(-1),
                Again,
            ],
        ),
        // A rung is tried giving bar room only under bars, or in R2 from a chrome giving it (R4).
        (
            import,
            at((2_652, 574), 2),
            vec![
                Warning,
                Refresh,
                Wheel(-3),
                Again,
                Left,
                Again,
                Wheel(-1),
                Wheel(2),
                Again,
                Status,
                Warning,
                Erase,
                Wheel(1),
                Wheel(-4),
                Status,
                Status,
            ],
        ),
        // A start held for a stop shifted in at the top is let go where no rung shows it so (R2).
        (
            drawn,
            at((172, 142), 2),
            vec![
                Text,
                Focus,
                Warning,
                Focus,
                Wheel(3),
                Wheel(5),
                Again,
                Text,
                Warning,
                Up,
                Left,
                Window((173, 142)),
                Window((173, 141)),
                Erase,
                Press(PALETTE_FIRST),
                Down,
                Type,
                Text,
                Down,
                Window((174, 141)),
                Again,
                Status,
                Press(PALETTE_SLOT),
            ],
        ),
        // Under a bar the listed toolbar is a last resort: main faults R3 asked of each rung.
        (row_after, statused((190, 247), 2), vec![]),
        // A status then two resizes under a bar: main's screens, which keep R3.
        (
            latte,
            at((1_433, 246), 2),
            vec![Status, Window((310, 566)), Window((193, 301))],
        ),
        // A focused card scrolls clear of a status's bar: the filter stays pinned as on main.
        (
            mocha,
            at((190, 868), 1),
            vec![
                Warning,
                Press(MOCHA),
                Warning,
                Again,
                Wheel(-3),
                Again,
                Text,
                Text,
                Status,
                Text,
            ],
        ),
        (mocha, statused((244, 440), 1), vec![]),
        // 8e8c292e8b's review: a start held for a stop shifted in at the top, as on main (R2).
        (
            cursor_color,
            statused(w1095x700, 1),
            vec![
                Window(w444x292),
                Window((1_268, 165)),
                Press(MOCHA),
                Press(BACKGROUND_WELL),
            ],
        ),
        // 8e8c292e8b's review: a new first result cut where drawn gives way, as main does not (R5).
        (
            filter_tokyo,
            statused((760, 330), 1),
            vec![Window((230, 420)), Wheel(-3), Text, Type],
        ),
        // 8e8c292e8b's review: a first result shown whole gives the bar back to the reveal (R5).
        (
            filter_tokyo,
            statused((600, 200), 1),
            vec![
                Warning,
                Warning,
                Wheel(8),
                Wheel(-2),
                Again,
                Right,
                Window((640, 480)),
                Left,
                Type,
                Warning,
                Window((700, 250)),
                Window((600, 200)),
                Wheel(4),
                Again,
                Erase,
                Window(w178x348),
            ],
        ),
        // 8e8c292e8b's review: a focused Close wraps where main cuts it at a side in 28pt (R2).
        (
            foreground,
            statused((2_548, 814), 2),
            vec![
                Press(PALETTE_SLOT),
                Up,
                Down,
                Down,
                Down,
                Window((262, 505)),
            ],
        ),
    ]
}

/// Walks that break a keeping rule on main (c5ef831d7f) as here: walked and printed, failing none.
fn known_faults() -> Vec<(Scenario, Drawn, Vec<Step>)> {
    use Step::{Down, Erase, Press, Status, Text, Type, Up, Warning, Wheel, Window};
    let on = |keyboard, theme| Scenario {
        keyboard,
        theme,
        in_use: theme,
    };
    let nord = |keyboard| on(keyboard, "Nord");
    let mocha = nord(Keyboard::On(MOCHA));
    let following = Scenario {
        keyboard: Keyboard::On(FOLLOW),
        theme: "dark:Dracula,light:GitHub Light",
        in_use: "GitHub Light",
    };
    let palette = on(Keyboard::Sheet(PALETTE_SLOT), "GitHub Light");
    let well = on(
        Keyboard::Sheet("settings/color-open/selection_foreground"),
        "GitHub Light",
    );
    let background = nord(Keyboard::Sheet("settings/control/background"));
    let cursor = nord(Keyboard::Sheet(CURSOR_COLOR));
    let import_light = on(Keyboard::On(IMPORT), "GitHub Light");
    let row_default = nord(Keyboard::Row("Default"));
    let one_dark = on(Keyboard::Row("One Dark"), "GitHub Light");
    let foreground = on(Keyboard::Sheet(FOREGROUND), "Dracula");
    let latte = on(Keyboard::Row("Catppuccin Latte"), "GitHub Light");
    [
        // R3 before the first event: the give-way finds no rung where the judge finds one.
        (mocha, "470x487@1+warning+status", vec![]),
        (mocha, "1847x363@2+warning+status", vec![Window((376, 901))]),
        (row_default, "229x509@1+warning+status", vec![]),
        (row_default, "381x486@1+warning", vec![Status]),
        (row_default, "514x702@1+status", vec![Text]),
        (import_light, "385x485@1+warning+status", vec![]),
        (import_light, "537x917@2+warning+status", vec![]),
        (
            import_light,
            "1719x860@2+warning+status",
            vec![Warning, Warning, Window((850, 893))],
        ),
        (
            one_dark,
            "2601x543@2+warning+status",
            vec![Window((462, 877))],
        ),
        (
            one_dark,
            "2422x423@1+warning",
            vec![Warning, Status, Warning, Window((504, 881)), Text],
        ),
        (
            nord(Keyboard::Row("Solarized Dark")),
            "1120x749@2+warning+status",
            vec![Window((416, 875))],
        ),
        // R3 on round 8k's rules seeds, shortest walks; main c5ef831d7f breaks it on each too.
        (row_default, "2565x201@2", vec![Window((398, 720)), Status]),
        (latte, "349x758@2+status", vec![]),
        (following, "502x724@2", vec![Status]),
        (mocha, "472x702@2+status", vec![]),
        // R5 after typing.
        (background, "1552x137@2+warning+status", vec![Warning, Type]),
        (foreground, "2763x175@2+status", vec![Type]),
        (foreground, "1842x156@2+status", vec![Type]),
        (
            on(
                Keyboard::Sheet("settings/control/selection_foreground"),
                "GitHub Light",
            ),
            "2845x189@2+status",
            vec![Type],
        ),
        (
            nord(Keyboard::Sheet("settings/control/background")),
            "2606x142@1+status",
            vec![Type],
        ),
        (
            nord(Keyboard::Pack),
            "1002x180@1",
            vec![Press(CUSTOMIZE), Window((2_860, 155)), Down, Status, Type],
        ),
        // R2: main's chrome cuts the keyboard's control on these walks too (round 8k seeds).
        (
            on(Keyboard::SheetFirst, "Dracula"),
            "1268x165@2+warning",
            vec![Window((444, 240)), Wheel(8), Press(PALETTE_SLOT)],
        ),
        (palette, "417x156@2", vec![Text, Press(PALETTE_SLOT)]),
        (palette, "435x209@2+status", vec![Press(PALETTE_SLOT)]),
        (well, "204x770@2+status", vec![Press(SAVE_AS), Down, Down]),
        (
            well,
            "476x975@1+status",
            vec![Text, Press(SAVE_AS), Window((210, 818)), Erase, Down],
        ),
        (
            background,
            "2770x1310@2+warning+status",
            vec![
                Window((1_480, 209)),
                Press(SAVE_AS),
                Wheel(-2),
                Press(PALETTE_SLOT),
            ],
        ),
        (
            cursor,
            "1855x1182@2+warning",
            vec![Press(SAVE_AS), Erase, Window((179, 156)), Down],
        ),
        // Rules seeds 997001-3, 997777, 998001-3 and 998777 at 400, shrunk; main breaks each too.
        // R2 across: Cancel cut at a side by Save beside it in a narrow sheet.
        (
            on(
                Keyboard::Sheet("settings/control/selection_foreground"),
                "GitHub Light",
            ),
            "1499x1376@1+warning+status",
            vec![Press(SAVE_AS), Text, Erase, Window((211, 593)), Down],
        ),
        // R2: the theme's name field owed what a frame left, through 1pt resizes and bars.
        (
            on(Keyboard::SheetDrawn, "GitHub Light"),
            "2995x139@2+status",
            vec![
                Press(SAVE_AS),
                Window((2_996, 140)),
                Status,
                Warning,
                Warning,
                Window((2_995, 140)),
                Window((2_996, 140)),
            ],
        ),
        // R3 before the first event, the keyboard on Nord over Dracula.
        (
            on(Keyboard::Row("Nord"), "Dracula"),
            "1197x238@1+status",
            vec![Window((428, 154)), Text, Status],
        ),
        // R4: a wheel's place moved by a warning leaving and a resize.
        (
            latte,
            "2273x902@2+warning",
            vec![
                Status,
                Status,
                Press(MOCHA),
                Text,
                Text,
                Text,
                Status,
                Text,
                Text,
                Press(MOCHA),
                Text,
                Wheel(-3),
                Warning,
                Window((417, 746)),
            ],
        ),
        // R5: the dark and light segment's filter change cuts its first result.
        (
            Scenario {
                keyboard: Keyboard::Side,
                theme: "dark:Dracula,light:GitHub Light",
                in_use: "GitHub Light",
            },
            "215x401@2+status",
            vec![Text, Up, Type],
        ),
        // Panics: a chrome told to cut the control cuts it short; a seat leaves over what was told.
        (
            nord(Keyboard::Row("Solarized Dark")),
            "252x152@1+status",
            vec![Warning, Text],
        ),
        (latte, "2568x529@1+warning+status", vec![Window((184, 406))]),
    ]
    .into_iter()
    .map(|(scenario, start, steps)| (scenario, drawn_of(start), steps))
    .collect()
}

/// The workers each scenario's walks share, so Pack's longest list is not the run's long pole.
const KEEPING_WORKERS: usize = 12;

/// `scenario`'s frames held to the rules ([`walk_from`], [`reviewed`]); [`known_faults`] printed.
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
    let walked = walks.len();
    walks.extend(
        reviewed()
            .into_iter()
            .filter(|(seen, _, _)| *seen == scenario)
            .map(|(_, start, steps)| (start, steps)),
    );
    let known = known_faults()
        .into_iter()
        .filter(|(seen, _, _)| *seen == scenario)
        .map(|(_, start, steps)| (start, steps))
        .collect::<Vec<_>>();
    walks.extend(known.iter().cloned());
    let reviewed_from = &walks[walked..walks.len() - known.len()];
    let found = Mutex::new(Findings::default());
    let still = Mutex::new(Vec::new());
    on_n_workers(
        KEEPING_WORKERS,
        &walks,
        "keeping walks",
        |(start, steps)| {
            // The walks reviews found also seat a bar's told steps one by one.
            let reviewed = reviewed_from
                .iter()
                .any(|(was, seen)| was == start && seen == steps);
            BAR_STEPS_SEATED.with(|seated| seated.set(reviewed));
            if known
                .iter()
                .any(|(was, seen)| was == start && seen == steps)
            {
                // A known walk runs cold on a thread of its own: its panic, main's too, is printed.
                let preferences = crate::native_appearance::current_preferences();
                let walked = std::thread::scope(|scope| {
                    std::thread::Builder::new()
                        .stack_size(8 << 20)
                        .spawn_scoped(scope, || {
                            crate::native_appearance::install_preferences(preferences);
                            walk(&sweep, *start, steps)
                        })
                        .expect("a known walk's thread")
                        .join()
                });
                let said = match walked {
                    // Its reaches that draw another list are main's too: printed with its fault.
                    Ok(walked) => std::iter::once(walked.faults.first().map_or_else(
                        || format!("{start} {steps:?} no longer faults"),
                        |(rule, said)| format!("{rule:?}: {said}"),
                    ))
                    .chain(
                        walked
                            .unlike
                            .into_iter()
                            .map(|said| format!("UNLIKE: {said}")),
                    )
                    .collect::<Vec<_>>()
                    .join("\n  "),
                    Err(panic) => format!(
                        "PANIC: {start} {steps:?}: {}",
                        panic
                            .downcast_ref::<String>()
                            .map(String::as_str)
                            .or_else(|| panic.downcast_ref::<&str>().copied())
                            .unwrap_or("a non-text panic")
                    ),
                };
                still
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(said);
                return;
            }
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
    let mut still = still.into_inner().unwrap_or_else(PoisonError::into_inner);
    still.sort_unstable();
    if !still.is_empty() {
        eprintln!(
            "{scenario}: known faults, main's too ({}):\n  {}",
            still.len(),
            still.join("\n  ")
        );
    }
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

/// Whether Import… can hold the keyboard here: only where a file picker is wired
/// ([`crate::menu::local_file_picker_available`]). Elsewhere (Linux) the button is drawn
/// disabled, so the keyboard never lands on it and no screen owes it a place; the footer's other
/// link's sweeps keep the footer's rules there.
fn import_takes_the_keyboard() -> bool {
    let available = crate::menu::local_file_picker_available();
    if !available {
        let mut lab = super::super::modified::Lab::new("");
        lab.state_mut().navigate(SettingsRoute::Themes);
        let compiled = lab.compile(876.0, 803.0);
        let import = compiled
            .semantic(&UiKey::new(IMPORT))
            .expect("premise: Import… is drawn");
        assert!(
            import.state.is_some_and(|state| !state.enabled),
            "with no file picker, Import… is drawn disabled"
        );
    }
    available
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_one_dark_over_github_light() {
    sweep(
        Scenario {
            keyboard: Keyboard::Row("One Dark"),
            theme: "GitHub Light",
            in_use: "GitHub Light",
        },
        Walks::Reviewed,
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_import_over_github_light() {
    if !import_takes_the_keyboard() {
        return;
    }
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
    if !import_takes_the_keyboard() {
        return;
    }
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

/// A redraw seats its bar where the last frame settled it; a test build settles again, uncounted.
#[test]
fn a_redraw_seats_its_bar_where_the_frame_before_settled_it() {
    let taken = || BAR_SEATED.with(std::cell::Cell::get);
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
        "each redraw of the settled page seated its bar where the frame before settled it"
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_a_tall_row_under_a_docked_status() {
    sweep(
        Scenario {
            keyboard: Keyboard::On(MOCHA),
            theme: "Nord",
            in_use: "Nord",
        },
        Walks::Reviewed,
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_nord_over_dracula() {
    sweep(
        Scenario {
            keyboard: Keyboard::Row("Nord"),
            theme: "Dracula",
            in_use: "Dracula",
        },
        Walks::Reviewed,
    );
}

#[test]
fn the_keeping_rules_hold_with_the_keyboard_on_solarized_dark() {
    sweep(
        Scenario {
            keyboard: Keyboard::Row("Solarized Dark"),
            theme: "Nord",
            in_use: "Nord",
        },
        Walks::Reviewed,
    );
}

/// A revealed row is measured at the width its list lays it out: under a terminal face whose
/// check is wider ([`WIDER_CHECK`]), the chooser's frame gives way at 291x177 and Solarized Dark's
/// row is taller there than at the framed width (the sweep's faces draw it alike at both).
#[cfg(target_os = "macos")]
#[test]
fn the_keeping_rules_hold_on_solarized_dark_in_a_wider_checked_terminal_face() {
    use Step::{Again, Down, Erase, Focus, Text, Tick, Up, Warning, Wheel, Window};
    crate::tray_raster::set_chrome_fonts(Some((WIDER_CHECK.into(), 0)), None, None);
    let sweep = Sweep::new(Scenario {
        keyboard: Keyboard::Row("Solarized Dark"),
        theme: "Nord",
        in_use: "Nord",
    });
    let found = walk(
        &sweep,
        Drawn::bare((1_431, 1_503), 1),
        &[
            Window((1_430, 1_503)),
            Focus,
            Window((2_038, 638)),
            Again,
            Tick,
            Again,
            Down,
            Window((2_038, 637)),
            Window((1_294, 1_478)),
            Window((1_137, 300)),
            Text,
            Wheel(-2),
            Text,
            Wheel(-40),
            Again,
            Text,
            Window((1_137, 301)),
            Wheel(3),
            Window((1_137, 302)),
            Focus,
            Window((290, 178)),
            Window((290, 177)),
            Text,
            Warning,
            Erase,
            Tick,
            Wheel(-40),
            Text,
            Up,
            Window((291, 177)),
        ],
    );
    assert_eq!(found.frames, 31, "every step drawn");
    assert!(
        found.faults.is_empty() && found.unlike.is_empty(),
        "{:?} {:?}",
        found.faults,
        found.unlike
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

/// A frame of a walk whose page work is counted: after a step, unchanged, or untouched at a window.
#[derive(Clone, Copy)]
enum Then {
    Step(Step),
    Redraw,
    Untouched(Size),
}

/// Page work of each frame of a walk from `start`, its bar's told steps checked seated, uncounted.
fn page_work(
    scenario: Scenario,
    start: Drawn,
    frames: &[Then],
) -> Vec<(String, crate::work_counts::SettingsPageWork)> {
    BAR_STEPS_SEATED.with(|seated| seated.set(true));
    let sweep = Sweep::new(scenario);
    let (mut runtime, instance, view) = sweep.opened(start);
    let _ = crate::work_counts::take_settings_page_work();
    let mut drawn = start;
    let mut last = draw(&mut runtime, instance, view, drawn);
    let mut work = vec![(
        format!("opened at {drawn}"),
        crate::work_counts::take_settings_page_work(),
    )];
    for then in frames {
        let frame = match *then {
            Then::Step(step) => {
                let event = match step {
                    Step::Wheel(lines) => Some(AppEvent::ScrollAt {
                        lines,
                        key: UiKey::new(LIST),
                    }),
                    Step::Up => Some(AppEvent::StepFocus(-1)),
                    Step::Down => Some(AppEvent::StepFocus(1)),
                    Step::Type => {
                        Some(AppEvent::TextInput(TextInputEvent::Commit("o".to_string())))
                    }
                    Step::Erase => Some(AppEvent::TextInput(TextInputEvent::Backspace)),
                    Step::Left => Some(AppEvent::TextInput(TextInputEvent::Left { extend: false })),
                    Step::Right => {
                        Some(AppEvent::TextInput(TextInputEvent::Right { extend: false }))
                    }
                    _ => None,
                };
                if let Some(event) = event {
                    let _ = dispatch(&mut runtime, instance, view, event);
                }
                // A press lands only on a control the last frame laid out, as the sweep's does.
                if let Step::Press(key) = step
                    && last.screen.semantic(&UiKey::new(key)).is_some()
                {
                    let _ = act(&mut runtime, instance, view, key, None);
                }
                drawn = drawn.after(step);
                last = draw(&mut runtime, instance, view, drawn);
                format!("{step} to {drawn}")
            }
            Then::Redraw => {
                let _ = compile_settings_view(&runtime, instance, view, &cx_of(drawn));
                format!("redrawn at {drawn}")
            }
            Then::Untouched(window) => {
                drawn = drawn.after(Step::Window(window));
                let _ = compile_settings_view(&runtime, instance, view, &cx_of(drawn));
                format!("drawn untouched at {drawn}")
            }
        };
        work.push((frame, crate::work_counts::take_settings_page_work()));
    }
    work
}

/// A walk, each frame held to at most `[pages, lists measured, docks, bars, lists seated]`.
type WorkWalk = (Scenario, Drawn, Vec<(Option<Then>, [usize; 5])>);

/// The walks [`these_frames_build_the_themes_page_and_its_bars_no_more_than_c5ef831d7f`] holds.
fn work_walks() -> Vec<WorkWalk> {
    use Step::{
        Again, Down, Erase, Left, Press, Right, Status, Text, Type, Up, Warning, Wheel, Window,
    };
    let on = |keyboard, theme| Scenario {
        keyboard,
        theme,
        in_use: theme,
    };
    let nord = |keyboard| on(keyboard, "Nord");
    let at = |window, scale, warning, status| Drawn {
        warning,
        status,
        ..Drawn::bare(window, scale)
    };
    let status = |window, scale| at(window, scale, false, true);
    let step = |step| Some(Then::Step(step));
    let redraw = Some(Then::Redraw);
    let untouched = |window| Some(Then::Untouched(window));
    let mocha = || nord(Keyboard::On(MOCHA));
    let latte = || on(Keyboard::Row("Catppuccin Latte"), "GitHub Light");
    vec![
        (
            nord(Keyboard::Filter),
            at((300, 360), 2, true, true),
            vec![
                (None, [1, 3, 3, 1, 2]),
                (redraw, [1, 0, 3, 1, 1]),
                (redraw, [1, 0, 3, 1, 1]),
                (redraw, [1, 0, 3, 1, 1]),
                (step(Again), [1, 2, 3, 1, 2]),
                (redraw, [1, 0, 3, 1, 1]),
            ],
        ),
        (
            nord(Keyboard::Filter),
            at((300, 360), 2, true, true),
            vec![
                (None, [1, 3, 3, 1, 2]),
                (redraw, [1, 0, 3, 1, 1]),
                (untouched((299, 360)), [1, 3, 3, 1, 2]),
                (untouched((300, 360)), [1, 2, 3, 1, 2]),
                (redraw, [1, 0, 3, 1, 1]),
            ],
        ),
        (
            nord(Keyboard::Filter),
            at((300, 360), 2, true, true),
            vec![
                (None, [1, 3, 3, 1, 2]),
                (step(Window((299, 360))), [1, 3, 3, 1, 2]),
                (step(Window((298, 360))), [1, 3, 3, 1, 2]),
                (step(Window((297, 360))), [1, 3, 3, 1, 2]),
                (redraw, [1, 0, 3, 1, 1]),
                (redraw, [1, 0, 3, 1, 1]),
            ],
        ),
        (
            nord(Keyboard::On(IMPORT)),
            at((1_411, 151), 1, true, false),
            vec![
                (None, [1, 7, 0, 0, 3]),
                (step(Window((1_410, 151))), [1, 7, 0, 0, 3]),
                (step(Window((1_409, 151))), [1, 7, 0, 0, 3]),
                (step(Window((1_408, 151))), [1, 7, 0, 0, 3]),
                (redraw, [1, 0, 0, 0, 1]),
                (redraw, [1, 0, 0, 0, 1]),
            ],
        ),
        (
            mocha(),
            status((230, 420), 2),
            vec![
                (None, [1, 2, 2, 1, 2]),
                (step(Window((229, 420))), [1, 2, 2, 1, 2]),
                (step(Window((228, 420))), [1, 2, 2, 1, 2]),
                (step(Window((227, 420))), [1, 2, 2, 1, 2]),
                (redraw, [1, 0, 2, 1, 1]),
                (redraw, [1, 0, 2, 1, 1]),
            ],
        ),
        (
            nord(Keyboard::Off),
            Drawn::bare((864, 500), 1),
            vec![(None, [1, 0, 0, 0, 1]), (redraw, [1, 0, 0, 0, 1])],
        ),
        (
            nord(Keyboard::Pack),
            status((864, 500), 1),
            vec![
                (None, [1, 1, 0, 1, 2]),
                (step(Again), [1, 1, 0, 1, 2]),
                (redraw, [1, 0, 0, 1, 1]),
                (redraw, [1, 0, 0, 1, 1]),
            ],
        ),
        (
            nord(Keyboard::Pack),
            status((444, 420), 1),
            vec![
                (None, [1, 4, 0, 1, 2]),
                (step(Again), [1, 2, 0, 1, 2]),
                (redraw, [1, 0, 0, 1, 1]),
                (redraw, [1, 0, 0, 1, 1]),
            ],
        ),
        (
            nord(Keyboard::Sheet("settings/control/background")),
            status((444, 420), 2),
            vec![
                (None, [1, 1, 2, 1, 2]),
                (step(Window((443, 420))), [1, 1, 2, 1, 2]),
                (step(Window((442, 420))), [1, 1, 2, 1, 2]),
                (redraw, [1, 0, 2, 1, 1]),
                (redraw, [1, 0, 2, 1, 1]),
            ],
        ),
        (
            nord(Keyboard::Sheet("settings/control/background")),
            status((1_284, 714), 1),
            vec![(None, [1, 1, 0, 1, 2])],
        ),
        (
            nord(Keyboard::Row("Dracula")),
            Drawn::bare((300, 360), 1),
            vec![
                (None, [1, 0, 1, 0, 1]),
                (step(Status), [1, 1, 2, 1, 2]),
                (step(Again), [1, 1, 2, 1, 2]),
                (redraw, [1, 0, 2, 1, 1]),
                (redraw, [1, 0, 2, 1, 1]),
                (step(Status), [1, 0, 1, 0, 1]),
            ],
        ),
        (
            nord(Keyboard::Row("Dracula")),
            at((244, 440), 2, true, true),
            // The warning's words fill half the window, so the status's bar by it shortens more.
            vec![
                (None, [1, 5, 3, 1, 2]),
                (step(Window((243, 440))), [1, 5, 3, 1, 2]),
            ],
        ),
        (
            on(Keyboard::Row("Catppuccin Latte"), "GitHub Light"),
            at((300, 360), 2, true, true),
            vec![
                (None, [1, 2, 3, 1, 2]),
                (step(Window((299, 360))), [2, 4, 3, 1, 5]),
            ],
        ),
        (
            nord(Keyboard::On(IMPORT)),
            Drawn::bare((244, 440), 1),
            vec![(None, [1, 0, 0, 0, 1]), (step(Status), [1, 2, 0, 1, 2])],
        ),
        // Frames a 1pt resize or a bar arriving leaves showing what they showed.
        (
            nord(Keyboard::Row("Dracula")),
            at((864, 292), 1, true, true),
            vec![
                (None, [1, 2, 0, 1, 2]),
                (untouched((863, 292)), [1, 2, 0, 1, 2]),
                (untouched((864, 292)), [1, 1, 0, 1, 2]),
            ],
        ),
        (
            mocha(),
            at((864, 292), 1, true, false),
            vec![(None, [1, 2, 0, 0, 2])],
        ),
        (
            nord(Keyboard::Filter),
            status((380, 318), 1),
            vec![(None, [1, 2, 2, 1, 2])],
        ),
        (
            nord(Keyboard::Row("Dracula")),
            at((380, 318), 1, true, false),
            vec![(None, [1, 2, 3, 0, 2])],
        ),
        (
            nord(Keyboard::Filter),
            at((1_268, 165), 1, true, false),
            vec![(None, [1, 4, 0, 0, 2]), (step(Status), [1, 1, 0, 1, 2])],
        ),
        (
            nord(Keyboard::Filter),
            at((300, 160), 2, true, false),
            vec![
                (None, [1, 2, 3, 0, 3]),
                (untouched((299, 160)), [1, 2, 3, 0, 3]),
                (untouched((300, 160)), [1, 1, 1, 0, 2]),
                (step(Window((299, 160))), [1, 1, 1, 0, 2]),
            ],
        ),
        (
            mocha(),
            at((1_240, 151), 1, true, false),
            vec![
                (None, [1, 7, 0, 0, 3]),
                (step(Window((1_239, 151))), [1, 7, 0, 0, 3]),
                (step(Window((1_240, 151))), [1, 1, 0, 0, 2]),
            ],
        ),
        // An opening whose give-way found no rung is not told again untouched.
        (
            nord(Keyboard::Pack),
            at((520, 691), 1, false, false),
            vec![
                (None, [1, 0, 0, 0, 1]),
                (untouched((519, 691)), [1, 0, 0, 0, 1]),
                (untouched((520, 691)), [1, 0, 0, 0, 1]),
            ],
        ),
        (
            nord(Keyboard::On(IMPORT)),
            at((1_094, 193), 1, false, false),
            vec![
                (None, [1, 1, 0, 0, 1]),
                (step(Window((1_093, 193))), [1, 0, 0, 0, 1]),
                (step(Window((1_094, 193))), [1, 0, 0, 0, 1]),
            ],
        ),
        (
            nord(Keyboard::On(FOLLOW)),
            at((1_094, 193), 1, true, false),
            vec![(None, [1, 3, 0, 0, 2]), (step(Warning), [1, 0, 0, 0, 1])],
        ),
        (
            on(Keyboard::SheetClosed, "Dracula"),
            at((1_094, 193), 1, false, false),
            vec![
                (None, [1, 0, 0, 0, 1]),
                (untouched((1_093, 193)), [1, 0, 0, 0, 1]),
                (untouched((1_094, 193)), [1, 0, 0, 0, 1]),
            ],
        ),
        // A trial keeping no room for a status's bar its bars cover lays out no dock to say so.
        (
            nord(Keyboard::On(SEARCH)),
            at((189, 200), 2, true, false),
            vec![
                (None, [1, 3, 2, 0, 2]),
                (untouched((189, 199)), [1, 3, 2, 0, 2]),
                (untouched((189, 200)), [1, 1, 1, 0, 2]),
            ],
        ),
        // A give-way no rung's room satisfies is untried on opening, a height step or bar leaving.
        (
            nord(Keyboard::Pack),
            at((520, 691), 1, false, false),
            vec![
                (None, [1, 0, 0, 0, 1]),
                (step(Window((520, 690))), [1, 0, 0, 0, 1]),
                (step(Window((520, 689))), [1, 0, 0, 0, 1]),
                (untouched((520, 688)), [1, 0, 0, 0, 1]),
                (untouched((520, 689)), [1, 0, 0, 0, 1]),
            ],
        ),
        (
            nord(Keyboard::On(IMPORT)),
            at((1_094, 193), 1, false, false),
            vec![
                (None, [1, 1, 0, 0, 1]),
                (step(Window((1_094, 192))), [1, 0, 0, 0, 1]),
                (step(Window((1_094, 191))), [1, 0, 0, 0, 1]),
                (untouched((1_094, 190)), [1, 0, 0, 0, 1]),
                (untouched((1_094, 191)), [1, 0, 0, 0, 1]),
            ],
        ),
        (
            nord(Keyboard::Row("Default")),
            at((1_240, 151), 1, true, false),
            vec![(None, [1, 7, 0, 0, 3]), (step(Warning), [1, 0, 0, 0, 1])],
        ),
        (
            nord(Keyboard::On(IMPORT)),
            at((1_411, 151), 1, true, false),
            vec![
                (None, [1, 7, 0, 0, 3]),
                (step(Warning), [1, 0, 0, 0, 1]),
                (step(Warning), [1, 1, 0, 0, 2]),
            ],
        ),
        // A give-way too short once the warning goes is left to the chrome it left, seated once.
        (
            nord(Keyboard::Row("Dracula")),
            at((244, 440), 1, true, false),
            vec![(None, [1, 2, 0, 0, 2]), (step(Warning), [1, 0, 0, 0, 1])],
        ),
        (
            on(Keyboard::Row("Catppuccin Latte"), "GitHub Light"),
            at((244, 440), 1, true, false),
            vec![(None, [1, 2, 0, 0, 2]), (step(Warning), [1, 0, 0, 0, 1])],
        ),
        (
            nord(Keyboard::Row("Tokyo Night")),
            at((244, 440), 1, true, false),
            vec![(None, [1, 2, 0, 0, 2]), (step(Warning), [1, 0, 0, 0, 1])],
        ),
        // Held under a row's caption, a screen tells no rung unable to show landing and control.
        (
            nord(Keyboard::Row("Solarized Dark")),
            Drawn::bare((864, 292), 1),
            vec![
                (None, [1, 0, 0, 0, 1]),
                (untouched((863, 292)), [1, 0, 0, 0, 1]),
                (untouched((864, 292)), [1, 0, 0, 0, 1]),
            ],
        ),
        // A bar leaving a compact window lays out no dock to tell a trial's room.
        (
            nord(Keyboard::Filter),
            at((189, 200), 2, true, false),
            vec![(None, [1, 3, 3, 0, 2]), (step(Warning), [1, 0, 0, 0, 1])],
        ),
        (
            nord(Keyboard::On(FOLLOW)),
            at((300, 160), 2, true, false),
            vec![(None, [1, 2, 3, 0, 3]), (step(Warning), [1, 0, 0, 0, 1])],
        ),
        // A warning leaving once a status came and went keeps the chrome these inputs drew.
        (
            nord(Keyboard::Row("Dracula")),
            at((244, 440), 1, true, false),
            vec![
                (None, [1, 2, 0, 0, 2]),
                (step(Status), [1, 1, 1, 1, 2]),
                (redraw, [1, 0, 1, 1, 1]),
                (step(Status), [1, 1, 0, 0, 2]),
                (step(Warning), [1, 0, 0, 0, 1]),
            ],
        ),
        // A warning coming and going over Pack, and a status leaving, tell no rung main does not.
        (
            nord(Keyboard::Pack),
            at((1_284, 714), 1, true, true),
            vec![(None, [1, 2, 0, 1, 2]), (step(Status), [1, 1, 0, 0, 2])],
        ),
        // A revealed stop fitting the list under a status's bar is not told again calm.
        (
            nord(Keyboard::Filter),
            status((1_284, 714), 2),
            vec![
                (None, [1, 1, 0, 1, 2]),
                (step(Type), [1, 1, 0, 1, 1]),
                (step(Erase), [1, 1, 0, 1, 1]),
                (step(Status), [1, 0, 0, 0, 1]),
                (step(Warning), [1, 1, 0, 0, 1]),
            ],
        ),
        // A sheet's screen seated after the first event cuts no landing: no rung is told ahead.
        (
            on(Keyboard::SheetClosed, "Dracula"),
            Drawn::bare((1_240, 151), 1),
            vec![
                (None, [1, 0, 0, 0, 1]),
                (untouched((1_239, 151)), [2, 0, 0, 0, 2]),
                (untouched((1_240, 151)), [1, 0, 0, 0, 1]),
                (untouched((1_240, 150)), [2, 0, 0, 0, 2]),
                (untouched((1_240, 151)), [1, 0, 0, 0, 1]),
            ],
        ),
        (
            on(Keyboard::SheetFirst, "Dracula"),
            Drawn::bare((1_240, 151), 1),
            vec![
                (None, [4, 0, 0, 0, 4]),
                (untouched((1_239, 151)), [4, 0, 0, 0, 4]),
                (untouched((1_240, 151)), [1, 0, 0, 0, 1]),
                (untouched((1_240, 150)), [4, 0, 0, 0, 4]),
                (untouched((1_240, 151)), [1, 0, 0, 0, 1]),
            ],
        ),
        // A field the warning cuts after typing tells no rung whose list paints none of it.
        (
            nord(Keyboard::Sheet("settings/control/background")),
            at((1_268, 165), 1, true, false),
            vec![
                (None, [1, 1, 0, 0, 2]),
                (step(Type), [1, 1, 0, 0, 2]),
                (step(Erase), [1, 1, 0, 0, 2]),
            ],
        ),
        // A rung keeping a status's bar room the chrome gives is not laid out, its dock unlaid.
        (
            nord(Keyboard::Filter),
            status((189, 200), 2),
            vec![
                (None, [1, 6, 2, 1, 2]),
                (step(Type), [1, 5, 2, 1, 2]),
                (step(Erase), [1, 5, 2, 1, 2]),
                (step(Status), [1, 0, 0, 0, 1]),
            ],
        ),
        // A give-way none of whose rungs was told to show it is not told again untouched.
        (
            nord(Keyboard::On(CUSTOMIZE)),
            status((178, 348), 1),
            vec![
                (None, [1, 3, 2, 1, 2]),
                (untouched((177, 348)), [1, 3, 2, 1, 2]),
                (untouched((178, 348)), [1, 2, 2, 1, 2]),
                (untouched((178, 347)), [1, 3, 2, 1, 2]),
                (untouched((178, 348)), [1, 2, 2, 1, 2]),
            ],
        ),
        // The memo's control: unread, each step back to 178x348 lays out a dock again.
        (
            nord(Keyboard::On(CUSTOMIZE)),
            status((178, 348), 2),
            vec![
                (None, [1, 4, 2, 1, 2]),
                (untouched((177, 348)), [1, 4, 2, 1, 2]),
                (untouched((178, 348)), [1, 3, 2, 1, 2]),
                (untouched((178, 347)), [1, 4, 2, 1, 2]),
                (untouched((178, 348)), [1, 3, 2, 1, 2]),
            ],
        ),
        // A row two from the theme in use, opened or a bar leaving, measures no stop main does not.
        (
            latte(),
            Drawn::bare((230, 420), 1),
            vec![
                (None, [1, 0, 0, 0, 1]),
                (untouched((229, 420)), [1, 0, 0, 0, 1]),
                (untouched((230, 420)), [1, 0, 0, 0, 1]),
                (untouched((230, 419)), [1, 0, 0, 0, 1]),
                (untouched((230, 420)), [1, 0, 0, 0, 1]),
            ],
        ),
        (
            latte(),
            at((230, 420), 1, true, false),
            vec![(None, [1, 2, 0, 0, 2]), (step(Warning), [1, 0, 0, 0, 1])],
        ),
        (
            nord(Keyboard::Pack),
            Drawn::bare((189, 200), 1),
            vec![
                (None, [1, 0, 1, 0, 1]),
                (step(Text), [4, 0, 1, 0, 4]),
                (untouched((188, 200)), [3, 0, 1, 0, 3]),
                (untouched((189, 200)), [1, 0, 0, 0, 1]),
            ],
        ),
        // A status leaving, a text size, a key typed: one walk per trade round 8f timed slower.
        (
            latte(),
            status((275, 572), 2),
            vec![(None, [1, 1, 2, 1, 2]), (step(Status), [1, 0, 1, 0, 1])],
        ),
        (
            nord(Keyboard::Row("Solarized Dark")),
            status((178, 348), 1),
            vec![(None, [1, 3, 2, 1, 2]), (step(Status), [1, 0, 1, 0, 1])],
        ),
        (
            latte(),
            Drawn::bare((275, 572), 1),
            vec![(None, [1, 0, 0, 0, 1]), (step(Text), [1, 0, 1, 0, 1])],
        ),
        (
            on(Keyboard::SheetClosed, "Dracula"),
            Drawn::bare((215, 318), 2),
            vec![(None, [1, 0, 1, 0, 1]), (step(Type), [1, 0, 1, 0, 1])],
        ),
        // A give-way whose told rungs show no more of the control keeps its chrome; main breaks R2.
        (
            on(Keyboard::Row("One Dark"), "GitHub Light"),
            at((2_852, 267), 2, true, true),
            vec![
                (None, [1, 5, 0, 1, 3]),
                (step(Warning), [1, 1, 0, 1, 2]),
                (step(Status), [1, 0, 0, 0, 1]),
                (step(Wheel(-1)), [1, 0, 0, 0, 1]),
                (step(Window((529, 726))), [1, 0, 0, 0, 1]),
                (step(Window((2_749, 184))), [2, 0, 0, 0, 2]),
                (step(Up), [1, 0, 0, 0, 1]),
                (step(Warning), [1, 9, 0, 0, 6]),
            ],
        ),
        // A calm rung that would hold is seated untold, no rung passed over before it.
        (
            on(
                Keyboard::Sheet("settings/control/selection_foreground"),
                "GitHub Light",
            ),
            Drawn::bare((1_707, 238), 2),
            vec![
                (None, [1, 0, 0, 0, 1]),
                (step(Status), [1, 1, 0, 1, 1]),
                (step(Status), [1, 0, 0, 0, 1]),
                (step(Left), [1, 0, 0, 0, 1]),
                (step(Text), [2, 0, 0, 0, 2]),
            ],
        ),
        // A stale filter reveal, a press off a landing, a key on a pinned control reveal no more.
        (
            on(Keyboard::Filter, "Tokyo Night"),
            at((1_211, 985), 1, true, true),
            vec![
                (None, [1, 2, 0, 1, 2]),
                (step(Window((1_210, 985))), [1, 2, 0, 1, 2]),
                (step(Up), [1, 1, 0, 1, 2]),
                (step(Window((1_209, 985))), [1, 2, 0, 1, 2]),
                (step(Warning), [1, 1, 0, 1, 2]),
                (step(Status), [1, 0, 0, 0, 1]),
                (step(Again), [1, 0, 0, 0, 1]),
                (step(Type), [1, 0, 0, 0, 1]),
                (step(Type), [1, 0, 0, 0, 1]),
                (step(Right), [1, 0, 0, 0, 1]),
                (step(Window((457, 749))), [1, 0, 0, 0, 1]),
                (step(Warning), [1, 1, 0, 0, 1]),
            ],
        ),
        (
            on(Keyboard::SheetFirst, "Dracula"),
            at((1_606, 717), 2, true, false),
            vec![
                (None, [1, 2, 0, 0, 2]),
                (step(Window((2_255, 799))), [1, 2, 0, 0, 2]),
                (step(Warning), [1, 0, 0, 0, 1]),
                (step(Again), [1, 0, 0, 0, 1]),
                (step(Status), [1, 1, 0, 1, 2]),
                (step(Press(SAVE_AS)), [1, 1, 0, 1, 2]),
                (step(Warning), [1, 1, 0, 1, 1]),
                (step(Again), [1, 0, 0, 1, 1]),
            ],
        ),
        (
            nord(Keyboard::On(FOLLOW)),
            status((1_208, 718), 1),
            vec![
                (None, [1, 1, 0, 1, 2]),
                (step(Status), [1, 0, 0, 0, 1]),
                (step(Warning), [1, 1, 0, 0, 2]),
                (step(Down), [1, 0, 0, 0, 1]),
            ],
        ),
        // Openings timed against main: Pack telling no rung, a field under a status, Follow System.
        (
            nord(Keyboard::Pack),
            Drawn::bare((947, 649), 1),
            vec![
                (None, [1, 0, 0, 0, 1]),
                (untouched((946, 649)), [1, 0, 0, 0, 1]),
                (untouched((947, 649)), [1, 0, 0, 0, 1]),
                (untouched((947, 648)), [1, 0, 0, 0, 1]),
                (untouched((947, 649)), [1, 0, 0, 0, 1]),
            ],
        ),
        (
            on(
                Keyboard::Sheet("settings/control/selection_foreground"),
                "GitHub Light",
            ),
            status((189, 200), 2),
            vec![(None, [1, 2, 1, 1, 2]), (step(Status), [1, 0, 0, 0, 1])],
        ),
        // A status leaving the follow switch's page measures no list again.
        (
            Scenario {
                keyboard: Keyboard::On(FOLLOW),
                theme: "dark:Dracula,light:GitHub Light",
                in_use: "GitHub Light",
            },
            status((184, 662), 2),
            vec![(None, [1, 3, 1, 1, 3]), (step(Status), [1, 0, 0, 0, 1])],
        ),
        // A footer's pinned control told by what a status's bar covers of its panel's foot (R3).
        (
            nord(Keyboard::On(OPEN_FOLDER)),
            status((1_499, 413), 1),
            vec![
                (None, [1, 1, 0, 1, 2]),
                (step(Window((2_923, 358))), [1, 1, 0, 1, 2]),
            ],
        ),
        // Walks whose screens parted from 2dbd55ec43's and keep c5ef831d7f's.
        (
            mocha(),
            Drawn::bare((244, 440), 1),
            vec![
                (None, [1, 0, 0, 0, 1]),
                (step(Warning), [1, 1, 0, 0, 2]),
                (step(Status), [1, 2, 1, 1, 2]),
                (redraw, [1, 0, 1, 1, 1]),
                (redraw, [1, 0, 1, 1, 1]),
                (step(Status), [1, 1, 0, 0, 2]),
            ],
        ),
        (
            mocha(),
            at((244, 440), 1, true, false),
            vec![
                (None, [1, 2, 0, 0, 2]),
                (step(Window((243, 440))), [1, 2, 0, 0, 2]),
            ],
        ),
        (
            on(Keyboard::SheetClosed, "Dracula"),
            status((189, 200), 2),
            vec![
                (None, [1, 3, 1, 1, 2]),
                (untouched((189, 199)), [1, 5, 2, 1, 2]),
                (untouched((189, 200)), [1, 3, 1, 1, 2]),
                (step(Window((189, 199))), [1, 3, 1, 1, 2]),
            ],
        ),
        (
            nord(Keyboard::On(FOLLOW)),
            status((189, 200), 2),
            vec![(None, [1, 6, 2, 1, 2]), (step(Status), [1, 0, 0, 0, 1])],
        ),
        // A key or a bar after an opening at 189x200@2, where 2dbd55ec43 broke R2.
        (
            nord(Keyboard::Row("Tokyo Night")),
            Drawn::bare((189, 200), 2),
            vec![(None, [2, 0, 1, 0, 2]), (step(Down), [1, 0, 0, 0, 1])],
        ),
        (
            nord(Keyboard::On(CUSTOMIZE)),
            Drawn::bare((189, 200), 2),
            vec![(None, [2, 0, 1, 0, 2]), (step(Warning), [1, 1, 2, 0, 2])],
        ),
        // Openings under a status, and the keys and resizes after, that draw main's screens.
        (
            mocha(),
            status((244, 440), 1),
            vec![
                (None, [1, 4, 0, 1, 2]),
                (redraw, [1, 0, 0, 1, 1]),
                (untouched((243, 440)), [1, 4, 0, 1, 2]),
                (untouched((244, 440)), [1, 3, 0, 1, 2]),
            ],
        ),
        (
            nord(Keyboard::On("settings/search")),
            status((444, 292), 1),
            vec![
                (None, [1, 4, 2, 1, 2]),
                (redraw, [1, 0, 2, 1, 1]),
                (redraw, [1, 0, 2, 1, 1]),
                (redraw, [1, 0, 2, 1, 1]),
                (step(Again), [1, 2, 2, 1, 2]),
                (redraw, [1, 0, 2, 1, 1]),
            ],
        ),
        (
            mocha(),
            status((244, 440), 1),
            vec![
                (None, [1, 4, 0, 1, 2]),
                (step(Wheel(3)), [1, 2, 0, 1, 1]),
                (redraw, [1, 0, 0, 1, 1]),
                (step(Down), [1, 1, 0, 1, 2]),
                (redraw, [1, 0, 0, 1, 1]),
                (step(Type), [1, 1, 0, 1, 1]),
                (redraw, [1, 0, 0, 1, 1]),
                (step(Again), [1, 1, 0, 1, 1]),
                (step(Window((243, 440))), [1, 2, 0, 1, 2]),
                (step(Window((242, 440))), [1, 2, 0, 1, 2]),
            ],
        ),
        // A warning back over a held give-way: the chrome these inputs drew, keeping the control.
        (
            nord(Keyboard::On(CUSTOMIZE)),
            at((1_411, 151), 1, true, false),
            vec![
                (None, [1, 7, 0, 0, 3]),
                (step(Warning), [1, 0, 0, 0, 1]),
                (untouched((1_411, 150)), [2, 0, 0, 0, 2]),
                (untouched((1_411, 151)), [1, 0, 0, 0, 1]),
                (step(Warning), [1, 1, 0, 0, 2]),
            ],
        ),
        // Held under a row's caption, a screen tells no rung unable to show landing and control.
        (
            on(Keyboard::On(IMPORT), "GitHub Light"),
            Drawn::bare((864, 292), 1),
            vec![
                (None, [1, 0, 0, 0, 1]),
                (untouched((863, 292)), [1, 0, 0, 0, 1]),
                (untouched((864, 292)), [1, 0, 0, 0, 1]),
                (untouched((864, 291)), [1, 0, 0, 0, 1]),
                (untouched((864, 292)), [1, 0, 0, 0, 1]),
            ],
        ),
        // A rung whose told rooms twin those of one told to show no more is passed over untold.
        (
            nord(Keyboard::Pack),
            at((2_881, 214), 2, true, true),
            vec![(None, [1, 7, 0, 1, 3])],
        ),
        // A chrome whose rows built unlike under another bar is measured with its own rows after.
        (
            mocha(),
            at((226, 857), 2, true, true),
            vec![(None, [2, 10, 1, 1, 6]), (step(Again), [1, 4, 1, 1, 2])],
        ),
        // A rung told in the chrome's own rooms, the wheel no more shown, is passed over.
        (
            on(Keyboard::Sheet(PALETTE_SLOT), "GitHub Light"),
            at((2_290, 195), 2, true, false),
            vec![
                (None, [1, 5, 0, 0, 3]),
                (step(Press(BACKGROUND_WELL)), [1, 2, 0, 0, 2]),
            ],
        ),
        (
            on(
                Keyboard::Sheet("settings/control/selection_foreground"),
                "GitHub Light",
            ),
            at((1_752, 1_051), 2, true, true),
            vec![
                (None, [1, 1, 0, 1, 2]),
                (step(Type), [1, 1, 0, 0, 1]),
                (step(Down), [1, 2, 0, 0, 1]),
                (step(Window((2_458, 243))), [1, 8, 0, 0, 3]),
                (step(Type), [1, 0, 0, 0, 1]),
                (step(Press(BACKGROUND_WELL)), [1, 2, 0, 0, 2]),
            ],
        ),
        // A first result shown whole is owed no more: the reveal it gave back tells no rung (R5).
        (
            nord(Keyboard::Filter),
            Drawn::bare((1_095, 700), 1),
            vec![
                (None, [1, 0, 0, 0, 1]),
                (step(Status), [1, 1, 0, 1, 2]),
                (step(Text), [1, 1, 0, 1, 2]),
                (step(Type), [1, 1, 0, 1, 1]),
                (step(Status), [1, 0, 0, 0, 1]),
                (step(Right), [1, 0, 0, 0, 1]),
                (step(Window((230, 420))), [1, 0, 1, 0, 1]),
            ],
        ),
    ]
}

/// A walk whose screens part from c5ef831d7f's, its frames' work in total at most `[pages, …]`.
type DivergedWalk = (Scenario, Drawn, Vec<Then>, [usize; 5]);

/// The walks [`these_frames_build_the_themes_page_and_its_bars_no_more_than_c5ef831d7f`] totals.
fn diverged_walks() -> Vec<DivergedWalk> {
    use Step::{Status, Warning};
    let on = |keyboard, theme| Scenario {
        keyboard,
        theme,
        in_use: theme,
    };
    let nord = |keyboard| on(keyboard, "Nord");
    let at = |window, scale, warning, status| Drawn {
        warning,
        status,
        ..Drawn::bare(window, scale)
    };
    let status = |window, scale| at(window, scale, false, true);
    let step = Then::Step;
    vec![
        // A status leaving a held give-way chrome not too short keeps it, seating no chrome first.
        (
            nord(Keyboard::Row("Default")),
            status((864, 292), 1),
            vec![step(Status)],
            [3, 2, 0, 1, 6],
        ),
        // Pack under a status, a warning coming and going, opened on the landing main cuts (R3).
        (
            nord(Keyboard::Pack),
            Drawn::bare((1_284, 714), 1),
            vec![step(Status), step(Status), step(Warning)],
            [4, 6, 0, 1, 10],
        ),
        (
            nord(Keyboard::Pack),
            at((1_284, 714), 1, true, false),
            vec![step(Status), step(Status), step(Warning), step(Warning)],
            [5, 8, 0, 1, 12],
        ),
        // A warning leaving a landing's held rung under a bar seats no page before giving way.
        (
            on(Keyboard::Row("One Dark"), "GitHub Light"),
            at((640, 480), 1, true, true),
            vec![step(Status), step(Status), step(Warning)],
            [5, 7, 0, 3, 13],
        ),
    ]
}

/// Each frame at most its pinned page work; each walk parting from c5ef831d7f's at most its total.
#[test]
fn these_frames_build_the_themes_page_and_its_bars_no_more_than_c5ef831d7f() {
    let counts = |work: crate::work_counts::SettingsPageWork| {
        [work.pages, work.measures, work.docks, work.bars, work.seats]
    };
    let mut over = Vec::new();
    for (scenario, start, frames) in work_walks() {
        let thens = frames
            .iter()
            .filter_map(|(then, _)| *then)
            .collect::<Vec<_>>();
        let work = page_work(scenario, start, &thens);
        for ((frame, work), (_, at_most)) in work.into_iter().zip(&frames) {
            if counts(work)
                .iter()
                .zip(at_most)
                .any(|(did, most)| did > most)
            {
                over.push(format!(
                    "{scenario}, {frame}: {:?}, at most {at_most:?}",
                    counts(work)
                ));
            }
        }
    }
    for (scenario, start, frames, at_most) in diverged_walks() {
        let total =
            page_work(scenario, start, &frames)
                .into_iter()
                .fold([0; 5], |total, (_, work)| {
                    let mut sum = total;
                    for (into, did) in sum.iter_mut().zip(counts(work)) {
                        *into += did;
                    }
                    sum
                });
        if total.iter().zip(at_most).any(|(did, most)| *did > most) {
            over.push(format!(
                "{scenario} from {start}, in total: {total:?}, at most {at_most:?}"
            ));
        }
    }
    assert!(
        over.is_empty(),
        "frames over their page work:\n{}",
        over.join("\n")
    );
}

/// A colour well pressed on the sheet lays its least list once a column width, not once a rung.
#[test]
fn a_colour_well_pressed_on_the_sheet_lays_its_least_list_once_a_width() {
    use Step::{Down, Press, Type, Window};
    let light = |keyboard| Scenario {
        keyboard,
        theme: "GitHub Light",
        in_use: "GitHub Light",
    };
    let mut over = Vec::new();
    for (scenario, start, steps, at_most) in [
        (
            light(Keyboard::Sheet(PALETTE_SLOT)),
            "2290x195@2+warning",
            vec![Press(BACKGROUND_WELL)],
            2,
        ),
        (
            light(Keyboard::Sheet("settings/control/selection_foreground")),
            "1752x1051@2+warning+status",
            vec![
                Type,
                Down,
                Window((2_458, 243)),
                Type,
                Press(BACKGROUND_WELL),
            ],
            2,
        ),
    ] {
        let frames = steps.into_iter().map(Then::Step).collect::<Vec<_>>();
        let work = page_work(scenario, drawn_of(start), &frames);
        let (frame, pressed) = work.last().expect("a walk draws its press");
        if pressed.sheet_leasts > at_most {
            over.push(format!(
                "{scenario}, {frame}: {}, at most {at_most}",
                pressed.sheet_leasts
            ));
        }
    }
    assert!(
        over.is_empty(),
        "sheets laid over their pins:\n{}",
        over.join("\n")
    );
}

/// First frames drawn whole by eb53a9b243 and by 2dbd55ec43 or here: control (`true`) or landing.
const FIRST_WHOLE: [(Keyboard, &str, bool, &str); 13] = [
    (Keyboard::Filter, "Nord", false, "230x420@1+status"),
    (Keyboard::Filter, "Tokyo Night", false, "230x420@1+status"),
    (
        Keyboard::Row("Dracula"),
        "Nord",
        true,
        "215x342@1 244x440@1 210x350@1+status 215x318@1+status 220x350@1+status \
         368x534@2+status 368x558@2+status 368x582@2+status 368x606@2+status",
    ),
    (Keyboard::Filter, "Nord", true, "215x318@1+status"),
    (
        Keyboard::On(IMPORT),
        "Nord",
        true,
        "215x318@1+status 368x534@2+status 368x558@2+status 368x582@2+status \
         368x606@2+status",
    ),
    (
        Keyboard::On(IMPORT),
        "GitHub Light",
        true,
        "215x318@1+status 368x534@2+status 368x558@2+status 368x582@2+status \
         368x606@2+status",
    ),
    (Keyboard::On(MOCHA), "Nord", true, "215x318@1+status"),
    (
        Keyboard::Row("Tokyo Night"),
        "Nord",
        true,
        "215x318@1+status 179x678@2 368x534@2+status 368x558@2+status 368x582@2+status \
         368x606@2+status",
    ),
    (
        Keyboard::Row("Dracula"),
        "Nord",
        false,
        "215x342@1 215x390@1 215x414@1 215x438@1 215x462@1 244x440@1 215x390@1+status \
         215x414@1+status 215x438@1+status 215x462@1+status 215x462@1+warning \
         215x510@1+warning 215x318@1+warning+status 215x414@1+warning+status \
         215x438@1+warning+status 215x534@1+warning+status 215x558@1+warning+status",
    ),
    (Keyboard::On(IMPORT), "GitHub Light", false, "215x318@1"),
    (Keyboard::On(MOCHA), "Nord", false, "215x318@1"),
    (
        Keyboard::Off,
        "Nord",
        false,
        "215x318@1+status 368x534@2+status 368x558@2+status 368x582@2+status \
         368x606@2+status",
    ),
    (
        Keyboard::Row("Tokyo Night"),
        "Nord",
        false,
        "215x342@1 215x390@1 215x414@1 215x438@1 215x462@1 215x390@1+status \
         215x414@1+status 215x438@1+status 215x462@1+status 215x462@1+warning \
         215x510@1+warning 215x318@1+warning+status 215x414@1+warning+status \
         215x438@1+warning+status 215x534@1+warning+status 215x558@1+warning+status \
         179x726@2+status 179x750@2+status 188x726@2+status 188x750@2+status",
    ),
];

/// A start as [`Drawn`]'s display writes it.
fn drawn_of(spec: &str) -> Drawn {
    let mut parts = spec.split('+');
    let (size, scale) = parts
        .next()
        .and_then(|head| head.split_once('@'))
        .expect("a start names its window and scale");
    let (width, height) = size.split_once('x').expect("a window is wide x tall");
    let mut drawn = Drawn::bare(
        (
            width.parse().expect("a width"),
            height.parse().expect("a height"),
        ),
        scale.parse().expect("a scale"),
    );
    for bar in parts {
        match bar {
            "warning" => drawn.warning = true,
            "status" => drawn.status = true,
            other => panic!("no bar {other}"),
        }
    }
    drawn
}

#[test]
fn first_frames_show_whole_what_eb53a9b243_showed_whole() {
    let frames: Vec<(Scenario, Drawn, bool)> = FIRST_WHOLE
        .iter()
        .flat_map(|&(keyboard, theme, control, starts)| {
            let scenario = Scenario {
                keyboard,
                theme,
                in_use: theme,
            };
            starts
                .split_whitespace()
                .map(move |start| (scenario, drawn_of(start), control))
        })
        .collect();
    let cut = Mutex::new(Vec::new());
    on_n_workers(
        KEEPING_WORKERS,
        &frames,
        "first frames",
        |&(scenario, drawn, control)| {
            let sweep = Sweep::new(scenario);
            let (mut runtime, instance, view) = sweep.opened(drawn);
            let frame = draw(&mut runtime, instance, view, drawn);
            let focus = state(&runtime, view).common.last_focus.clone();
            let whole = if control {
                focus.is_some_and(|key| frame.kept(key.as_str()) == Kept::Whole)
            } else {
                frame.in_list(&sweep.landing)
            };
            if !whole {
                let what = if control {
                    "the keyboard's control"
                } else {
                    "the theme in use"
                };
                cut.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(format!("{scenario} at {drawn}: {what}"));
            }
        },
    );
    let cut = cut.into_inner().unwrap_or_else(PoisonError::into_inner);
    assert!(
        cut.is_empty(),
        "{} of {} first frames cut what eb53a9b243 showed whole:\n{}",
        cut.len(),
        frames.len(),
        cut.join("\n")
    );
}
