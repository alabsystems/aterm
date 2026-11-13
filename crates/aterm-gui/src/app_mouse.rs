// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Window mouse input handling, hit-testing, selection, and URL/file activation.
use std::cell::Cell;
use std::time::Instant;

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_types::mouse::WheelDir;
use aterm_window::event::{
    ElementState, MouseButton as WinitMouseButton, MouseScrollDelta, TouchPhase,
};
use aterm_window::window::CursorIcon;

use aterm_core::terminal::{CustodyTransition, Terminal};

use crate::app_render::{pixel_to_term_cell, strip_col_for_pixel};
use crate::input::{InputEvent, Source};
use crate::{
    App, GestureOrigin, WindowId, control, is_safe_url, multi_click_ms, pane, plain_url_at,
    term_lock,
};

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "PressCustody",
        action = "UserSelect",
        project = "aterm_gui::press_custody_conformance::project_press_custody"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "PressCustody",
        action = "UserClear",
        project = "aterm_gui::press_custody_conformance::project_press_custody"
    )
)]
pub(crate) fn note_selection_custody(t: &mut Terminal, kept: bool) {
    t.note_custody(if kept {
        CustodyTransition::UserSelect
    } else {
        CustodyTransition::UserClear
    });
}

/// The grid column a selection gesture anchors at for the pointer over pane-local viewport cell `(row, col)`.
pub(crate) fn pointer_char(term: &Terminal, row: u16, col: u16) -> (u16, Option<SelectionSide>) {
    use aterm_core::grid::LineSize;
    match term.grid().row(row).map(aterm_core::grid::Row::line_size) {
        Some(LineSize::DoubleWidth | LineSize::DoubleHeightTop | LineSize::DoubleHeightBottom) => {
            let side = if col.is_multiple_of(2) {
                SelectionSide::Left
            } else {
                SelectionSide::Right
            };
            (col / 2, Some(side))
        }
        _ => (col, None),
    }
}

/// The selection anchor a pointer on viewport cell (`row`, `col`) makes, with its half-cell `side`.
pub(crate) fn pointer_anchor(
    t: &Terminal,
    row: u16,
    col: u16,
    side: SelectionSide,
) -> (u16, SelectionSide) {
    let (logical, rtl) = t.viewport_cell_logical(row, col);
    let side = match (rtl, side) {
        (true, SelectionSide::Left) => SelectionSide::Right,
        (true, SelectionSide::Right) => SelectionSide::Left,
        (_, side) => side,
    };
    (logical, side)
}

/// The logical grid column under the pointer on pane-local viewport cell (`row`, `col`).
pub(crate) fn pointer_cell(t: &Terminal, row: u16, col: u16) -> (u16, SelectionSide) {
    match t.painted_cell_logical(row, col) {
        (logical, true) => (logical, SelectionSide::Right),
        (logical, false) => (logical, SelectionSide::Left),
    }
}

fn pointer_anchor_at_sel_row(
    t: &Terminal,
    sel_row: i32,
    col: u16,
    side: SelectionSide,
) -> (u16, SelectionSide) {
    let viewport_row = i64::from(sel_row) + i64::try_from(t.grid().display_offset()).unwrap_or(0);
    match u16::try_from(viewport_row) {
        Ok(row) => pointer_anchor(t, row, col, side),
        Err(_) => (col, side),
    }
}

/// Map a winit mouse button to the engine's [`aterm_types::mouse::MouseButton`] for an [`InputEvent::MouseButton`].
pub(crate) const fn winit_mouse_button(
    b: WinitMouseButton,
) -> Option<aterm_types::mouse::MouseButton> {
    use aterm_types::mouse::MouseButton;
    match b {
        WinitMouseButton::Left => Some(MouseButton::Left),
        WinitMouseButton::Middle => Some(MouseButton::Middle),
        WinitMouseButton::Right => Some(MouseButton::Right),
        WinitMouseButton::Back => Some(MouseButton::Back),
        WinitMouseButton::Forward => Some(MouseButton::Forward),
        WinitMouseButton::Other(_) => None,
    }
}

const BUTTONS_BY_SLOT: [aterm_types::mouse::MouseButton; 5] = [
    aterm_types::mouse::MouseButton::Left,
    aterm_types::mouse::MouseButton::Middle,
    aterm_types::mouse::MouseButton::Right,
    aterm_types::mouse::MouseButton::Back,
    aterm_types::mouse::MouseButton::Forward,
];

/// The button a motion report names while the buttons in `reported` (a `1 << slot()` mask) are held.
pub(crate) fn held_button(reported: u8) -> Option<aterm_types::mouse::MouseButton> {
    BUTTONS_BY_SLOT
        .into_iter()
        .find(|button| reported & (1 << button.slot()) != 0)
}

/// Forget `button`'s press in window state `ws`.
pub(crate) fn forget_reported_button(
    ws: &mut crate::WindowState,
    button: aterm_types::mouse::MouseButton,
) {
    ws.reported_buttons &= !(1 << button.slot());
    ws.button_owners[usize::from(button.slot())] = None;
    ws.held_mouse_button = held_button(ws.reported_buttons);
}

/// The session a held button's press went to, and the cell (in that session's own grid) and sub-cell pixel last sent to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ButtonOwner {
    pub(crate) session: u64,
    pub(crate) cell: (u16, u16),
    pub(crate) px_off: crate::input::PixelOffset,
}

/// Where one pointer event goes in a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PointerRoute {
    Front,
    Pane { session: u64, cell: (u16, u16) },
    Nowhere,
}

fn leaf_cell_rect(rect: crate::tab_model::LogicalRect) -> (u16, u16, u16, u16) {
    (
        rect.origin.y.round().max(0.0) as u16,
        rect.origin.x.round().max(0.0) as u16,
        (rect.size.height.round() as u16).max(1),
        (rect.size.width.round() as u16).max(1),
    )
}

fn rect_contains((ro, co, rows, cols): (u16, u16, u16, u16), (row, col): (u16, u16)) -> bool {
    (ro..ro.saturating_add(rows)).contains(&row) && (co..co.saturating_add(cols)).contains(&col)
}

fn cell_in_rect((row, col): (u16, u16), (ro, co, rows, cols): (u16, u16, u16, u16)) -> (u16, u16) {
    (
        row.saturating_sub(ro).min(rows.saturating_sub(1)),
        col.saturating_sub(co).min(cols.saturating_sub(1)),
    )
}

fn note_owner_cell(
    ws: &mut crate::WindowState,
    session: u64,
    cell: (u16, u16),
    px_off: crate::input::PixelOffset,
) {
    for owner in ws.button_owners.iter_mut().flatten() {
        if owner.session == session {
            owner.cell = cell;
            owner.px_off = px_off;
        }
    }
}

/// An in-flight drag-to-reorder on the in-grid tab strip.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct StripDrag {
    pub(crate) tab: crate::tab_model::TabId,
    pub(crate) origin_col: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoverProbe {
    Resolved(Option<crate::link_target::LinkHover>),
    Deferred,
}

/// The grid arm of one hover resolution, memoised so the terminal mutex is touched once per question rather than per pixel.
#[derive(Clone, Debug)]
pub(crate) struct HoverMemo {
    session: u64,
    window_cell: (u16, u16),
    cell: (u16, u16),
    mod_held: bool,
    under_pointer: bool,
    frame: HoverFrame,
    disclosed: Option<std::sync::Arc<str>>,
    plain_url: bool,
    path_link: bool,
    citation: Option<crate::path_link::PathTarget>,
    settled: bool,
    named: Option<(i32, Option<String>)>,
}

/// The engine fill a window's frame stands on: the single pane's, the composed focus pane's, and
/// a mixed tab's focused terminal leaf's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HoverFrame {
    single: (u64, u64, i32),
    composed: (u64, u64, i32),
    mixed: (u64, u64, i32),
}

impl HoverMemo {
    /// The engine-fill epoch of `ws`'s frame — see [`HoverFrame`].
    pub(crate) fn frame_of(ws: &crate::WindowState) -> HoverFrame {
        let of = |s: &aterm_core::render::RenderInput| {
            (s.terminal_id, s.engine_fill_seq, s.display_offset)
        };
        let mixed = match ws.front_content {
            Some(crate::front_content::FrontContent::Terminal { view, .. }) => ws
                .leaf_render_cache
                .get(&view)
                .map_or((0, 0, 0), |cache| cache.hover_key),
            _ => (0, 0, 0),
        };
        HoverFrame {
            single: of(&ws.input_scratch),
            composed: of(&ws.composed_focus_scratch),
            mixed,
        }
    }

    fn answers(
        &self,
        session: u64,
        window_cell: (u16, u16),
        mod_held: bool,
        under_pointer: bool,
        frame: HoverFrame,
    ) -> bool {
        self.settled
            && self.session == session
            && self.window_cell == window_cell
            && self.mod_held == mod_held
            && self.under_pointer == under_pointer
            && self.frame == frame
    }

    /// The cached destination for `hover`, if this memo is the resolution that produced it (same session, same cell).
    pub(crate) fn url_for(
        &self,
        hover: &crate::link_target::LinkHover,
    ) -> Option<&std::sync::Arc<str>> {
        (self.session == hover.session && self.cell == hover.cell)
            .then_some(self.disclosed.as_ref())
            .flatten()
    }

    /// Whether the destination this memo discloses for `hover` is a file citation rather than a URL.
    pub(crate) fn path_link_for(&self, hover: &crate::link_target::LinkHover) -> bool {
        self.path_link && self.session == hover.session && self.cell == hover.cell
    }

    /// What the caption this memo draws for `hover` names, or `None` when it draws none for `hover`.
    pub(crate) fn caption_names(
        &self,
        hover: &crate::link_target::LinkHover,
    ) -> Option<CaptionNames> {
        let url = self.url_for(hover)?;
        Some(match self.citation.as_ref().filter(|_| self.path_link) {
            Some(target) => CaptionNames::Citation {
                path: target.path.clone(),
                line: target.line,
                col: target.col,
            },
            None => CaptionNames::Url(url.clone()),
        })
    }

    /// The canonical file the caption names, when it names a citation.
    #[cfg(test)]
    pub(crate) fn caption_citation(&self) -> Option<&std::path::Path> {
        self.citation
            .as_ref()
            .filter(|_| self.path_link)
            .map(|target| target.path.as_path())
    }

    /// Whether this memo answered a citation for `session` under a naming of the group in front other than `named` (`program_pgid`, `program`).
    pub(crate) fn renamed(&self, session: u64, named: (i32, Option<&str>)) -> bool {
        self.answered_citation_of(session)
            && self
                .named
                .as_ref()
                .is_some_and(|(group, program)| (*group, program.as_deref()) != named)
    }

    /// Whether this memo answered a citation for `session`.
    pub(crate) const fn answered_citation_of(&self, session: u64) -> bool {
        self.session == session && self.named.is_some()
    }

    /// Whether this memo answers for the pointer as the frame finds it.
    pub(crate) fn stands_for(&self, ws: &crate::WindowState, session: u64) -> bool {
        self.session == session
            && self.window_cell == ws.last_mouse_window_cell
            && self.mod_held == link_modifier_held(ws.mods)
            && self.frame == Self::frame_of(ws)
    }
}

/// What a drawn link caption names: the one target a press under it may open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CaptionNames {
    Url(std::sync::Arc<str>),
    Citation {
        path: std::path::PathBuf,
        line: Option<u32>,
        col: Option<u32>,
    },
}

/// The link caption carried by the last accepted application present (`WindowState::shown_caption`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShownCaption {
    pub(crate) session: u64,
    pub(crate) cell: (u16, u16),
    pub(crate) names: CaptionNames,
    /// The row as painted, for a client.
    pub(crate) words: String,
}

/// Publish only the caption carried by an accepted application present.
pub(crate) fn commit_presented_caption(
    shown: &mut Option<ShownCaption>,
    composed: Option<ShownCaption>,
    presented: &Result<Option<u64>, crate::metrics::PresentDropReason>,
) {
    if matches!(presented, Ok(Some(_))) {
        *shown = composed;
    }
}

/// A URL a press reads under the pointer, by where it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PressedUrl<'a> {
    Osc8(&'a str),
    Plain(&'a str),
}

impl<'a> PressedUrl<'a> {
    const fn url(self) -> &'a str {
        match self {
            Self::Osc8(url) | Self::Plain(url) => url,
        }
    }
}

/// Whether a press that reads `url` (a URL under the pointer) or `citation` (a resolved file citation, with no URL there) may open it.
pub(crate) fn caption_admits(
    shown: Option<&CaptionNames>,
    url: Option<PressedUrl<'_>>,
    citation: Option<&crate::path_link::PathTarget>,
) -> bool {
    match (shown, url, citation) {
        (Some(CaptionNames::Url(named)), Some(url), _) => **named == *url.url(),
        (Some(CaptionNames::Citation { .. }), Some(_), _) => false,
        (None, Some(PressedUrl::Plain(_)), _) => true,
        (None, Some(PressedUrl::Osc8(_)), _) => false,
        (Some(CaptionNames::Citation { path, line, col }), None, Some(target)) => {
            target.path == *path && target.line == *line && target.col == *col
        }
        (Some(CaptionNames::Url(_)) | None, None, Some(_)) => false,
        (_, None, None) => true,
    }
}

fn probe_hover_cell(
    t: &Terminal,
    row: u16,
    col: u16,
    cell_is_under_pointer: bool,
    mod_held: bool,
    min_contrast: f32,
) -> (Option<std::sync::Arc<str>>, bool, Option<CitationProbe>) {
    if !cell_is_under_pointer {
        return (None, false, None);
    }
    if !mod_held && !t.grid().visible_row_may_have_hyperlinks(row) {
        return (None, false, None);
    }
    let col = pointer_cell(t, row, col).0;
    let link = t.hyperlink_at_visible(row, col);
    if link.is_some() || !mod_held {
        return (link, false, None);
    }
    let cells = t.render_row(usize::from(row));
    match plain_url_at(&cells, usize::from(col), min_contrast) {
        Some(plain) => (
            plain.faint.then(|| plain.to_url_string().into()),
            true,
            None,
        ),
        None => (
            None,
            false,
            crate::plain_path_at(&cells, usize::from(col)).map(|span| CitationProbe::new(t, span)),
        ),
    }
}

/// A file citation read off the grid, with what the engine says of the session's directory read under the same hold.
#[derive(Clone, Debug)]
pub(crate) struct CitationProbe {
    span: crate::path_link::PathSpan,
    osc7: Option<std::path::PathBuf>,
    osc7_host: Option<String>,
    osc7_seq: Option<u64>,
    running: Option<String>,
    executing: bool,
    shell_reads_local: bool,
}

impl CitationProbe {
    fn new(t: &Terminal, span: crate::path_link::PathSpan) -> Self {
        use crate::cwd_native::ReportedCwd as _;
        let running = crate::session_status::executing_candidate(t);
        let shell_reads_local = shell_reads_local(
            t.current_block().is_some() && crate::quit_safety::shell_reports_commands(t),
            running.as_deref(),
        );
        Self {
            span,
            osc7: t
                .native_working_directory()
                .map(|cwd| std::path::PathBuf::from(cwd.as_ref())),
            osc7_host: t.reported_cwd_host().map(String::from),
            osc7_seq: t.reported_cwd_seq(),
            running,
            executing: crate::quit_safety::shell_executing(t),
            shell_reads_local,
        }
    }
}

/// Where no foreground group can be read (a ConPTY has no `tcgetpgrp`), the shell's own integration is the only reading of what runs.
pub(crate) fn shell_reads_local(integrated: bool, running: Option<&str>) -> bool {
    match running {
        None => integrated,
        Some(command) => {
            let mut words = command.split_whitespace();
            let word = words.next().unwrap_or("");
            let word = word.rsplit(['/', '\\']).next().unwrap_or(word);
            let word = word
                .strip_suffix(".exe")
                .or_else(|| word.strip_suffix(".EXE"))
                .unwrap_or(word);
            (crate::quit_safety::is_shell_name(word) && words.next().is_none())
                || aterm_phase::program_of(word).is_some()
        }
    }
}

/// Whether the "open link" modifier is held.
pub(crate) const fn link_modifier_held(mods: aterm_window::keyboard::ModifiersState) -> bool {
    #[cfg(target_os = "macos")]
    {
        mods.super_key()
    }
    #[cfg(not(target_os = "macos"))]
    {
        mods.control_key()
    }
}

/// Audit M6: whether a left press starts a LOCAL text selection instead of being reported to a tracking app.
pub(crate) const fn press_starts_selection(tracking: bool, bypass_held: bool) -> bool {
    !tracking || bypass_held
}

/// Whether the held modifiers take a left press back from a mouse-tracking app ([`press_starts_selection`]).
pub(crate) const fn tracking_bypass_held(mods: aterm_window::keyboard::ModifiersState) -> bool {
    mods.alt_key() || (cfg!(not(target_os = "macos")) && mods.shift_key())
}

/// What a right press does at its pre-dispatch point in `on_mouse_input`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RightPressPlan {
    Chrome,
    Copy,
    Paste,
    Seam,
}

/// Resolve a right press against the conhost/Windows Terminal convention (`right_click = "copy_paste"`).
pub(crate) const fn right_press_plan(
    gesture_on: bool,
    over_strip: bool,
    tracking: bool,
    has_selection: bool,
) -> RightPressPlan {
    if over_strip {
        return RightPressPlan::Chrome;
    }
    if !gesture_on || tracking {
        return RightPressPlan::Seam;
    }
    if has_selection {
        RightPressPlan::Copy
    } else {
        RightPressPlan::Paste
    }
}

/// Selection kind for a single left press.
pub(crate) const fn press_selection_kind(option_held: bool, tracking: bool) -> SelectionType {
    if option_held && !tracking {
        SelectionType::Block
    } else {
        SelectionType::Simple
    }
}

thread_local! {
    static PANE_DRAG_APPLIED_AT: Cell<Option<(WindowId, Instant)>> = const { Cell::new(None) };
    static PANE_DRAG_PENDING: Cell<Option<WindowId>> = const { Cell::new(None) };
    static PANE_DRAG_CELL: Cell<Option<(WindowId, (u16, u16))>> = const { Cell::new(None) };
}

/// Forget window `wid`'s divider-drag throttle and hysteresis state.
pub(crate) fn forget_divider_drag(wid: WindowId) {
    PANE_DRAG_APPLIED_AT.with(|c| {
        if matches!(c.get(), Some((w, _)) if w == wid) {
            c.set(None);
        }
    });
    PANE_DRAG_PENDING.with(|c| {
        if c.get() == Some(wid) {
            c.set(None);
        }
    });
    PANE_DRAG_CELL.with(|c| {
        if matches!(c.get(), Some((w, _)) if w == wid) {
            c.set(None);
        }
    });
}

const DIVIDER_DRAG_HYSTERESIS_DIV: usize = 4;

fn divider_drag_axis(held: u16, raw: u16, off: u16, cell: usize) -> u16 {
    if raw == held || cell == 0 {
        return raw;
    }
    let off = usize::from(off).min(cell - 1);
    let past = if raw > held {
        usize::from(raw - held - 1) * cell + off + 1
    } else {
        usize::from(held - raw - 1) * cell + (cell - off)
    };
    if past > cell / DIVIDER_DRAG_HYSTERESIS_DIV {
        raw
    } else {
        held
    }
}

fn divider_drag_cell(
    held: (u16, u16),
    raw: (u16, u16),
    off: crate::input::PixelOffset,
    cell: (usize, usize),
) -> (u16, u16) {
    (
        divider_drag_axis(held.0, raw.0, off.y, cell.1),
        divider_drag_axis(held.1, raw.1, off.x, cell.0),
    )
}

fn divider_drag_off_axis(g: usize, raw: u16, cell: usize) -> u16 {
    let cell = cell.max(1);
    let at = g / cell;
    let off = match at.cmp(&usize::from(raw)) {
        std::cmp::Ordering::Greater => cell - 1,
        std::cmp::Ordering::Less => 0,
        std::cmp::Ordering::Equal => g % cell,
    };
    u16::try_from(off).unwrap_or(u16::MAX)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PointerGeometry {
    cell: (usize, usize),
    pad: usize,
    pad_top: usize,
    head: usize,
}

/// WHERE THE POINTER CAME TO REST over a window's native views, and what it saw there.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NativePointerRest {
    px: (f64, f64),
    seen: Option<(crate::tab_model::ViewId, crate::native_ui::UiKey)>,
    asks: bool,
    /// The presses made at this rest ([`PressStreak`]).
    streak: Option<PressStreak>,
}

/// A RUN OF PRESSES AT A STILL POINTER, each within a double click's interval of the one before
/// ([`crate::multi_click_ms`]), and the control the run's first press landed on. A later press of
/// the run that lands on another control lands where the first press's action put that control
/// under the pointer: one double-click, not a press of what came up (a double-click on the Themes
/// strip's Keep pressed the Customize that took Keep's place).
#[derive(Clone, Debug, PartialEq)]
struct PressStreak {
    at: std::time::Instant,
    view: crate::tab_model::ViewId,
    key: crate::native_ui::UiKey,
}

impl NativePointerRest {
    fn after_motion(
        rest: Option<Self>,
        px: (f64, f64),
        seen: Option<(crate::tab_model::ViewId, &crate::native_ui::UiKey, bool)>,
    ) -> Option<Self> {
        match rest {
            Some(rest) if rest.px == px => Some(rest),
            _ => Some(Self {
                px,
                asks: seen.is_some_and(|(_, _, asks)| asks),
                seen: seen.map(|(view, key, _)| (view, key.clone())),
                streak: None,
            }),
        }
    }

    /// Count a press at `px` on `(view, key)` at `now` into the run of presses at this rest, and
    /// say whether it continues a run whose first press landed on another control
    /// ([`PressStreak`]), which the press then must not take.
    fn press(
        rest: &mut Option<Self>,
        px: (f64, f64),
        (view, key): (crate::tab_model::ViewId, &crate::native_ui::UiKey),
        now: std::time::Instant,
    ) -> bool {
        let here = rest.get_or_insert_with(|| Self {
            px,
            seen: Some((view, key.clone())),
            asks: false,
            streak: None,
        });
        if here.px != px {
            *here = Self {
                px,
                seen: Some((view, key.clone())),
                asks: false,
                streak: None,
            };
        }
        let continues = here.streak.as_ref().filter(|streak| {
            now.saturating_duration_since(streak.at).as_millis() <= crate::multi_click_ms()
        });
        let onto_another =
            continues.is_some_and(|streak| streak.view != view || streak.key != *key);
        here.streak = Some(match continues {
            Some(streak) => PressStreak {
                at: now,
                ..streak.clone()
            },
            None => PressStreak {
                at: now,
                view,
                key: key.clone(),
            },
        });
        onto_another
    }

    fn looked_at(
        rest: Option<&Self>,
        px: (f64, f64),
        (view, key, asks): (crate::tab_model::ViewId, &crate::native_ui::UiKey, bool),
    ) -> bool {
        rest.is_none_or(|rest| {
            rest.px != px
                || !(asks || rest.asks)
                || rest
                    .seen
                    .as_ref()
                    .is_some_and(|(seen_view, seen)| *seen_view == view && seen == key)
        })
    }

    fn on(
        px: (f64, f64),
        (view, key, asks): (crate::tab_model::ViewId, &crate::native_ui::UiKey, bool),
        streak: Option<PressStreak>,
    ) -> Self {
        Self {
            px,
            seen: Some((view, key.clone())),
            asks,
            streak,
        }
    }
}

/// Every input of the pixel → pane-local cell mapping other than the pixel, plus the session the cell is reported to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HoverMapping {
    front: Option<crate::front_content::FrontContent>,
    win_px: Option<(u32, u32)>,
    dims: (u16, u16),
    chrome_rows: u16,
    geom: PointerGeometry,
    pane_rect: (u16, u16, u16, u16),
}

/// THE GRID A FRAME'S GLASS WAS LAID ON.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GlassGrid {
    geom: PointerGeometry,
    origin: (i64, i64),
    dims: (u16, u16),
    chrome_rows: u16,
    split: u64,
}

impl GlassGrid {
    /// The window's cell in device pixels, `(w, h)`.
    pub(crate) const fn cell(&self) -> (usize, usize) {
        self.geom.cell
    }

    /// Window pixel `(x, y)` as a frame cell `(row, col)` under this grid.
    pub(crate) fn cell_at(&self, x: f64, y: f64) -> Option<(usize, usize)> {
        frame_cell_of(self.geom, self.origin, self.dims.1, x, y)
    }
}

fn frame_cell_of(
    geom: PointerGeometry,
    (ox, oy): (i64, i64),
    cols: u16,
    x: f64,
    y: f64,
) -> Option<(usize, usize)> {
    let (cw, ch) = (geom.cell.0.max(1), geom.cell.1.max(1));
    let top = geom.pad_top + geom.head;
    let (fx, fy) = (x - ox as f64, y - oy as f64);
    if fy < top as f64 || fx < geom.pad as f64 {
        return None;
    }
    let row = ((fy as usize).saturating_sub(top)) / ch;
    let col = ((fx as usize).saturating_sub(geom.pad)) / cw;
    (col < usize::from(cols)).then_some((row, col))
}

fn split_word(tab: Option<&crate::tab_model::Tab>) -> u64 {
    use std::hash::{Hash, Hasher};
    fn fold(tree: &crate::tab_model::SplitTree<crate::tab_model::ViewId>, h: &mut impl Hasher) {
        match tree {
            crate::tab_model::SplitTree::Leaf(view) => {
                0u8.hash(h);
                view.hash(h);
            }
            crate::tab_model::SplitTree::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                1u8.hash(h);
                matches!(axis, crate::tab_model::SplitAxis::Horizontal).hash(h);
                ratio.to_bits().hash(h);
                fold(first, h);
                fold(second, h);
            }
        }
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Some(tab) = tab {
        tab.id.hash(&mut h);
        tab.zoomed.hash(&mut h);
        if tab.zoomed {
            tab.focus.hash(&mut h);
        }
        fold(&tab.root, &mut h);
    }
    h.finish()
}

#[derive(Clone, Copy)]
struct MouseCellMapping {
    pane_cell: (u16, u16),
    pane_rect: (u16, u16, u16, u16),
}

/// The `pointer` verb's answer.
pub(crate) type PointerReply = Result<PointerAt, String>;

/// The pointer's presented location or acknowledgement of a dispatched action (cs-25).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PointerAt {
    Dispatched {
        readback_error: String,
    },
    Absent,
    Grid {
        cell: (i32, u16),
        px: Option<(i32, i32)>,
    },
    Native {
        view: u64,
        x: f32,
        y: f32,
        key: Option<String>,
        px: Option<(i32, i32)>,
    },
}

/// What a `pointer move` on a native view aims at.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NativePointerTarget {
    Key(String),
    Point(f32, f32),
}

/// The unit of a `pointer wheel`'s deltas, and so the device it stands in for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WheelUnit {
    Lines,
    Points,
}

/// What the `pointer` verb asks of the front window's pointer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PointerAction {
    Move {
        row: u16,
        col: u16,
    },
    MoveNative(NativePointerTarget),
    /// The centre of the open overlay card's row for item `n` ([`App::overlay_card_item_px`]).
    MoveItem(usize),
    MoveChrome {
        up: u16,
        col: u16,
    },
    Click,
    Wheel {
        dx: f64,
        dy: f64,
        unit: WheelUnit,
        aim: Option<NativePointerTarget>,
    },
    Leave,
    Status,
    Mods(aterm_window::keyboard::ModifiersState),
}

impl App {
    /// Put `overlay` in `wid`'s one overlay slot (structural mutual exclusion) and arm the pointer as every overlay card opens.
    pub(crate) fn install_overlay_card(
        &mut self,
        wid: WindowId,
        overlay: crate::overlay::Overlay,
        raise: bool,
        claim: fn(&mut Self, WindowId) -> bool,
        motion: fn(&mut Self, WindowId, f64, f64),
    ) -> bool {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        ws.overlay = Some(overlay);
        ws.scroll_residual = 0.0;
        if let Some(w) = &ws.os_window {
            if raise {
                w.focus_window();
            }
            w.request_redraw();
        }
        self.retire_native_input_in(wid);
        self.settle_pointer_drags(wid);
        let _ = claim(self, wid);
        if let Some((x, y)) = self.windows.get(&wid).map(|ws| ws.last_cursor_px) {
            motion(self, wid, x, y);
        }
        self.overlay_a11y_update();
        true
    }

    /// Close `wid`'s overlay card when `is_open` says the slot holds it (a no-op otherwise).
    pub(crate) fn close_overlay_card(
        &mut self,
        wid: WindowId,
        is_open: fn(&crate::WindowState) -> bool,
    ) {
        let mut closed = false;
        if let Some(ws) = self.windows.get_mut(&wid)
            && is_open(ws)
        {
            ws.overlay = None;
            ws.scroll_residual = 0.0;
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
            closed = true;
        }
        self.overlay_a11y_update();
        if closed {
            self.replay_held_pointer(wid);
        }
    }

    /// Re-resolve the cursor where the pointer last moved.
    pub(crate) fn replay_held_pointer(&mut self, wid: WindowId) {
        if let Some((x, y)) = self
            .windows
            .get(&wid)
            .filter(|ws| ws.pointer_position_known)
            .map(|ws| ws.last_cursor_px)
        {
            self.on_cursor_moved(wid, x, y);
        }
    }

    /// Modal pointer boundary of an overlay card.
    pub(crate) fn overlay_card_claims_pointer(
        &mut self,
        wid: WindowId,
        is_open: fn(&crate::WindowState) -> bool,
        sync: fn(&mut Self, WindowId),
    ) -> bool {
        if self.windows.get(&wid).is_none_or(|ws| !is_open(ws)) {
            return false;
        }

        let mut changed = None;
        if let Some((_, view)) = self.active_native_view(wid)
            && self.native_runtime.change_view_state(view, |state| {
                let common = state.common_mut();
                let hovered = common.hovered.take().is_some();
                let pressed = common.pressed.take().is_some();
                hovered || pressed
            })
        {
            changed = Some(view);
        }
        if let Some(view) = changed {
            self.invalidate_native_pointer_style(wid, view);
            self.request_redraw_all_windows();
        }
        sync(self, wid);
        true
    }

    /// A window-space point in the open card's local geometry, with the transform whose `geom` the card's own hit test takes.
    pub(crate) fn overlay_card_point(
        &self,
        wid: WindowId,
        x: f64,
        y: f64,
    ) -> Option<(crate::OverlayCoordinateTransform, f32, f32)> {
        let transform = self.overlay_coordinate_transform(wid)?;
        let (frame_x, frame_y) = self.window_to_frame(wid, x, y);
        let local_x = (frame_x - transform.origin_x) / f64::from(transform.scale);
        let local_y = (frame_y - transform.origin_y) / f64::from(transform.scale);
        Some((transform, local_x as f32, local_y as f32))
    }

    /// The window pixel at the centre of the open card's row for `item` — the palette's filtered
    /// row, as `menu` counts `selected=`, or the Overview's item, as `controls overview` counts
    /// it — where a person's pointer selects that row, or why there is none.
    pub(crate) fn overlay_card_item_px(
        &self,
        wid: WindowId,
        item: usize,
    ) -> Result<(f64, f64), String> {
        let window = self.windows.get(&wid).ok_or("no window")?;
        if window.palette().is_none() && window.overview().is_none() {
            return Err("no palette or Overview is open".into());
        }
        let transform = self
            .overlay_coordinate_transform(wid)
            .ok_or("the open card has no room in the window")?;
        let local = match (window.palette(), window.overview()) {
            (Some(palette), _) => {
                crate::palette::palette_row_centre(palette, &transform.geom, item)
            }
            (None, Some(card)) => {
                crate::overview::overview_item_centre(card, &transform.geom, item)
            }
            (None, None) => None,
        };
        let (x, y) = local.ok_or_else(|| format!("item {item} is not a row the card shows"))?;
        let (ox, oy) = self.frame_origin(wid);
        let scale = f64::from(transform.scale);
        #[allow(
            clippy::cast_precision_loss,
            reason = "window pixel coordinates; the f64 mantissa covers every representable window size"
        )]
        Ok((
            ox as f64 + transform.origin_x + f64::from(x) * scale,
            oy as f64 + transform.origin_y + f64::from(y) * scale,
        ))
    }

    /// Keep the OS cursor aligned with an overlay card's hover.
    pub(crate) fn set_overlay_card_cursor(&mut self, wid: WindowId, pointer: bool) {
        let icon = if pointer {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        };
        self.set_hover_cursor(wid, icon, pointer, false);
    }

    /// Repaint `wid`'s overlay card and republish its accessibility tree when `changed`.
    pub(crate) fn redraw_overlay_card(&mut self, wid: WindowId, changed: bool) {
        if !changed {
            return;
        }
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
        self.overlay_a11y_update();
    }

    /// The palette's modal pointer boundary ([`Self::overlay_card_claims_pointer`]).
    pub(crate) fn palette_claims_pointer(&mut self, wid: WindowId) -> bool {
        self.overlay_card_claims_pointer(
            wid,
            |ws| ws.palette().is_some(),
            Self::sync_palette_pointer_cursor,
        )
    }

    fn palette_row_at_pointer(&self, wid: WindowId, x: f64, y: f64) -> Option<usize> {
        let palette = self.windows.get(&wid)?.palette()?;
        let (transform, x, y) = self.overlay_card_point(wid, x, y)?;
        crate::palette::palette_row_hit(palette, &transform.geom, x, y)
    }

    /// Keep the OS cursor aligned with the palette's current row hover.
    pub(crate) fn sync_palette_pointer_cursor(&mut self, wid: WindowId) {
        let pointer = self
            .windows
            .get(&wid)
            .and_then(|window| window.palette())
            .is_some_and(crate::palette::PaletteState::pointer_over_row);
        self.set_overlay_card_cursor(wid, pointer);
    }

    fn repaint_palette_pointer(&mut self, wid: WindowId, changed: bool) {
        self.sync_palette_pointer_cursor(wid);
        self.redraw_overlay_card(wid, changed);
    }

    /// Hover-select the row under a pointer motion.
    pub(crate) fn palette_pointer_motion(&mut self, wid: WindowId, x: f64, y: f64) {
        let hit = self.palette_row_at_pointer(wid, x, y);
        let changed = self
            .windows
            .get_mut(&wid)
            .and_then(|window| window.palette_mut())
            .is_some_and(|palette| palette.pointer_hover(hit));
        self.repaint_palette_pointer(wid, changed);
    }

    fn palette_pointer_press(&mut self, wid: WindowId, x: f64, y: f64) {
        let hit = self.palette_row_at_pointer(wid, x, y);
        let changed = self
            .windows
            .get_mut(&wid)
            .and_then(|window| window.palette_mut())
            .is_some_and(|palette| palette.pointer_press(hit));
        self.repaint_palette_pointer(wid, changed);
    }

    fn palette_pointer_release(&mut self, wid: WindowId, x: f64, y: f64) {
        let hit = self.palette_row_at_pointer(wid, x, y);
        let (changed, activate) = self
            .windows
            .get_mut(&wid)
            .and_then(|window| window.palette_mut())
            .map_or((false, false), |palette| palette.pointer_release(hit));
        self.repaint_palette_pointer(wid, changed);
        if activate {
            self.palette_activate_in(wid);
        }
    }

    fn palette_pointer_wheel(&mut self, wid: WindowId, delta: isize) {
        let mut changed = self
            .windows
            .get_mut(&wid)
            .and_then(|window| window.palette_mut())
            .is_some_and(|palette| palette.scroll_by(delta));
        let (x, y) = self
            .windows
            .get(&wid)
            .map_or((0.0, 0.0), |window| window.last_cursor_px);
        let hit = self.palette_row_at_pointer(wid, x, y);
        changed |= self
            .windows
            .get_mut(&wid)
            .and_then(|window| window.palette_mut())
            .is_some_and(|palette| palette.pointer_hover(hit));
        self.repaint_palette_pointer(wid, changed);
    }

    pub(crate) fn active_tab_is_single_focused_pane(&self, wid: WindowId) -> bool {
        self.windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active())
            .is_some_and(|tab| {
                (tab.zoomed && tab.root.len() > 1)
                    || (tab.root.len() == 1 && tab.root.contains(tab.focus))
            })
    }

    pub(crate) fn active_tab_has_multiple_visible_panes(&self, wid: WindowId) -> bool {
        self.windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active())
            .is_some_and(|tab| tab.root.len() > 1 && !tab.zoomed)
    }

    /// The focused pane's rect `(row_off, col_off, rows, cols)` in window `wid`'s grid.
    pub(crate) fn focused_pane_rect(&self, wid: WindowId) -> (u16, u16, u16, u16) {
        let Some(ws) = self.windows.get(&wid) else {
            return (0, 0, 0, 0);
        };
        if self.active_tab_is_single_focused_pane(wid) {
            return (0, 0, ws.rows.max(1), ws.cols.max(1));
        }
        self.active_visible_leaf_plan(wid)
            .and_then(|plan| plan.leaf(plan.focused).map(|leaf| leaf.rect))
            .map_or((0, 0, ws.rows, ws.cols), leaf_cell_rect)
    }

    /// The focused pane's rect in a plan already owned by this frame.
    pub(crate) fn focused_pane_rect_from_plan(
        &self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> (u16, u16, u16, u16) {
        let Some(ws) = self.windows.get(&wid) else {
            return (0, 0, 0, 0);
        };
        if self.active_tab_is_single_focused_pane(wid) {
            return (0, 0, ws.rows.max(1), ws.cols.max(1));
        }
        plan.leaf(plan.focused)
            .map_or((0, 0, ws.rows, ws.cols), |leaf| leaf_cell_rect(leaf.rect))
    }

    pub(crate) fn focused_pane_rect_with_plan(
        &self,
        wid: WindowId,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> (u16, u16, u16, u16) {
        match frame_plan {
            Some(plan) => self.focused_pane_rect_from_plan(wid, plan),
            None => self.focused_pane_rect(wid),
        }
    }

    /// Whether terminal `session` is window `wid`'s front (focused) terminal.
    pub(crate) fn session_is_front(&self, wid: WindowId, session: u64) -> bool {
        self.front_terminal(wid)
            .is_some_and(|front| front.session == session)
    }

    /// The content rect `(row_off, col_off, rows, cols)`.
    pub(crate) fn visible_session_rect(
        &self,
        wid: WindowId,
        session: u64,
    ) -> Option<(u16, u16, u16, u16)> {
        self.visible_session_rect_with_plan(wid, session, None)
    }

    pub(crate) fn visible_session_rect_with_plan(
        &self,
        wid: WindowId,
        session: u64,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> Option<(u16, u16, u16, u16)> {
        if self.active_tab_is_single_focused_pane(wid) {
            let ws = self.windows.get(&wid)?;
            let tab = ws.tab_set.active()?;
            let focus_session = self
                .view_store
                .get(tab.focus)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)?;
            return (focus_session == session).then(|| (0, 0, ws.rows.max(1), ws.cols.max(1)));
        }
        let owned_plan;
        let plan = match frame_plan {
            Some(plan) => plan,
            None => {
                owned_plan = self.active_visible_leaf_plan(wid)?;
                &owned_plan
            }
        };
        plan.leaves
            .iter()
            .find(|leaf| {
                self.view_store
                    .get(leaf.view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                    == Some(session)
            })
            .map(|leaf| leaf_cell_rect(leaf.rect))
    }

    /// Where a pointer event other than a release goes in window `wid` (a hover or drag step, a wheel notch, a non-left press).
    #[cfg(all(test, unix))]
    pub(crate) fn pointer_route(
        &self,
        wid: WindowId,
        focused: Option<(u16, u16, u16, u16)>,
    ) -> PointerRoute {
        self.pointer_route_with_plan(wid, focused, None)
    }

    pub(crate) fn pointer_route_with_plan(
        &self,
        wid: WindowId,
        focused: Option<(u16, u16, u16, u16)>,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> PointerRoute {
        self.pointer_capture_route_with_plan(wid, frame_plan)
            .unwrap_or_else(|| self.pane_under_pointer_with_plan(wid, focused, frame_plan))
    }

    /// A held gesture keeps its owner even when the pointer crosses a pane.
    /// With no capture, pointer routing can resolve the leaf under it.
    fn pointer_capture_route_with_plan(
        &self,
        wid: WindowId,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> Option<PointerRoute> {
        let ws = self.windows.get(&wid)?;
        if ws.overlay_open() || ws.tab_menu.is_some() || ws.selecting || ws.divider_drag.is_some() {
            return Some(PointerRoute::Front);
        }
        let page_buttons = if self.active_native_view(wid).is_some() {
            ws.page_buttons
        } else {
            0
        };
        if held_button(ws.reported_buttons | page_buttons)
            .is_some_and(|lowest| page_buttons & (1 << lowest.slot()) != 0)
        {
            return Some(PointerRoute::Front);
        }
        if ws.reported_buttons != 0 {
            let owner = ws
                .held_mouse_button
                .and_then(|button| ws.button_owners[usize::from(button.slot())]);
            return Some(self.owner_route_with_plan(wid, owner, frame_plan));
        }
        None
    }

    fn pane_under_pointer_with_plan(
        &self,
        wid: WindowId,
        focused: Option<(u16, u16, u16, u16)>,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> PointerRoute {
        let Some(ws) = self.windows.get(&wid) else {
            return PointerRoute::Front;
        };
        if !self.active_tab_has_multiple_visible_panes(wid) {
            return PointerRoute::Front;
        }
        let window_cell = ws.last_mouse_window_cell;
        let focused = focused.unwrap_or_else(|| match frame_plan {
            Some(plan) => self.focused_pane_rect_from_plan(wid, plan),
            None => self.focused_pane_rect(wid),
        });
        if rect_contains(focused, window_cell) {
            return PointerRoute::Front;
        }
        let owned_plan;
        let plan = match frame_plan {
            Some(plan) => plan,
            None => {
                let Some(plan) = self.active_visible_leaf_plan(wid) else {
                    return PointerRoute::Front;
                };
                owned_plan = plan;
                &owned_plan
            }
        };
        let Some(leaf) = plan.leaf_at(crate::tab_model::LogicalPoint {
            x: f32::from(window_cell.1),
            y: f32::from(window_cell.0),
        }) else {
            return PointerRoute::Front;
        };
        if leaf.view == plan.focused {
            return PointerRoute::Front;
        }
        match self
            .view_store
            .get(leaf.view)
            .copied()
            .and_then(crate::tab_model::View::terminal_session)
        {
            Some(session) if self.pool.get(session).is_some() => PointerRoute::Pane {
                session,
                cell: cell_in_rect(window_cell, leaf_cell_rect(leaf.rect)),
            },
            _ => PointerRoute::Nowhere,
        }
    }

    fn owner_route_with_plan(
        &self,
        wid: WindowId,
        owner: Option<ButtonOwner>,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> PointerRoute {
        let Some(owner) = owner else {
            return PointerRoute::Front;
        };
        if self.session_is_front(wid, owner.session) {
            return PointerRoute::Front;
        }
        let Some(window_cell) = self.windows.get(&wid).map(|ws| ws.last_mouse_window_cell) else {
            return PointerRoute::Nowhere;
        };
        self.visible_session_rect_with_plan(wid, owner.session, frame_plan)
            .map_or(PointerRoute::Nowhere, |rect| PointerRoute::Pane {
                session: owner.session,
                cell: cell_in_rect(window_cell, rect),
            })
    }

    fn send_pointer(&mut self, wid: WindowId, route: PointerRoute, ev: InputEvent) {
        match route {
            PointerRoute::Front => {
                self.input(wid, ev, Source::Human);
            }
            PointerRoute::Pane { session, .. } => {
                self.input_to_session(
                    wid,
                    ev,
                    Source::Human,
                    Some(session),
                    crate::app_input::PressPhase::Initial,
                );
            }
            PointerRoute::Nowhere => {}
        }
    }

    /// Release every held button whose press went to a session the window no longer shows, then forget it.
    pub(crate) fn settle_hidden_button_owners(&mut self, wid: WindowId) {
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.pane_overlay.drag)
            .is_some_and(|d| !self.active_tab_shows_session(wid, d.session))
        {
            self.scrollbar_release(wid);
        }
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.pane_overlay.hover)
            .is_some_and(|h| !self.active_tab_shows_session(wid, h))
            && let Some(ws) = self.windows.get_mut(&wid)
        {
            ws.pane_overlay.hover = None;
        }
        let Some((reported, owners)) = self
            .windows
            .get(&wid)
            .filter(|ws| ws.reported_buttons != 0)
            .map(|ws| (ws.reported_buttons, ws.button_owners))
        else {
            return;
        };
        let mut released = Vec::new();
        for button in BUTTONS_BY_SLOT {
            let slot = usize::from(button.slot());
            let Some(owner) = owners[slot] else {
                continue;
            };
            if reported & (1 << slot) == 0 || self.active_tab_shows_session(wid, owner.session) {
                continue;
            }
            self.write_owner_release(button, owner);
            released.push(button);
        }
        if let Some(ws) = self.windows.get_mut(&wid) {
            for button in released {
                forget_reported_button(ws, button);
            }
        }
    }

    /// Re-derive window `wid`'s pane-local pointer cell (`last_mouse_cell`) from its window cell and the focused pane's rect.
    pub(crate) fn rederive_pointer_cell(&mut self, wid: WindowId) {
        let rect = self.focused_pane_rect(wid);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_mouse_cell = cell_in_rect(ws.last_mouse_window_cell, rect);
        }
    }

    /// `true` when the pointer's last window cell lies inside the focused pane's rect, i.e.
    /// `last_mouse_cell` translates where the pointer is rather than clamping where it is
    /// not.
    #[cfg(test)]
    pub(crate) fn pointer_is_inside_focused_pane(&self, wid: WindowId) -> bool {
        self.pointer_is_inside_focused_pane_with(wid, None)
    }

    fn pointer_is_inside_focused_pane_with(
        &self,
        wid: WindowId,
        focused_rect: Option<(u16, u16, u16, u16)>,
    ) -> bool {
        let Some((wr, wc)) = self.windows.get(&wid).map(|ws| ws.last_mouse_window_cell) else {
            return false;
        };
        let rect = focused_rect.unwrap_or_else(|| self.focused_pane_rect(wid));
        rect_contains(rect, (wr, wc))
    }

    #[cfg(test)]
    fn pane_header_at_with(
        &self,
        wid: WindowId,
        geom: PointerGeometry,
        x: f64,
        y: f64,
    ) -> Option<crate::tab_model::ViewId> {
        self.pane_header_at_with_plan(wid, geom, x, y, None)
    }

    fn pane_header_at_with_plan(
        &self,
        wid: WindowId,
        geom: PointerGeometry,
        x: f64,
        y: f64,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> Option<crate::tab_model::ViewId> {
        if !self.active_tab_has_multiple_visible_panes(wid)
            || self.windows.get(&wid).is_none_or(|ws| ws.overlay_open())
            || self.band_banner_at(wid, x, y).is_some()
            || self.find_bar_cell_at(wid, x, y).is_some()
            || self.stats_lens_at(wid, x, y)
        {
            return None;
        }
        let (cw, ch) = geom.cell;
        if cw == 0 || ch == 0 {
            return None;
        }
        let (ox, oy) = self.frame_origin_with(wid, geom);
        let (fx, fy) = (x - ox as f64, y - oy as f64);
        let point = crate::tab_model::LogicalPoint {
            x: ((fx - geom.pad as f64) / cw as f64) as f32,
            y: ((fy - (geom.pad_top + geom.head) as f64) / ch as f64
                - f64::from(self.chrome_rows(wid))) as f32,
        };
        if point.y >= 0.0
            && point.x >= 0.0
            && self.chrome_owns_terminal_cell(wid, point.y as usize, point.x as usize)
        {
            return None;
        }
        let owned_plan;
        let plan = match frame_plan {
            Some(plan) => plan,
            None => {
                owned_plan = self.active_visible_leaf_plan(wid)?;
                &owned_plan
            }
        };
        plan.header_at(point).map(|leaf| leaf.view)
    }

    #[cfg(all(test, unix))]
    fn pointer_is_over_pane_header(&self, wid: WindowId) -> bool {
        self.pointer_is_over_pane_header_with_plan(wid, None)
    }

    fn pointer_is_over_pane_header_with_plan(
        &self,
        wid: WindowId,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> bool {
        self.windows.get(&wid).is_some_and(|ws| {
            let (x, y) = ws.last_cursor_px;
            self.pane_header_at_with_plan(wid, self.pointer_geometry(wid), x, y, frame_plan)
                .is_some()
        })
    }

    /// Click-to-focus in window `wid`.
    #[cfg(test)]
    pub(crate) fn focus_pane_under_pointer(&mut self, wid: WindowId) -> bool {
        self.focus_pane_under_pointer_with_plan(wid, None)
    }

    fn focus_pane_under_pointer_with_plan(
        &mut self,
        wid: WindowId,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> bool {
        if !self.active_tab_has_multiple_visible_panes(wid) {
            return false;
        }
        if let Some(plan) = frame_plan {
            return self.focus_pane_under_pointer_from_plan(wid, plan);
        }
        let Some(plan) = self.active_visible_leaf_plan(wid) else {
            return false;
        };
        self.focus_pane_under_pointer_from_plan(wid, &plan)
    }

    fn focus_pane_under_pointer_from_plan(
        &mut self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let (wr, wc) = ws.last_mouse_window_cell;
        if plan.leaves.len() == 1 {
            return false;
        }
        let Some(hit_leaf) = plan.leaf_at(crate::tab_model::LogicalPoint {
            x: f32::from(wc),
            y: f32::from(wr),
        }) else {
            return false; // divider / outside grid: nothing to focus
        };
        let hit = hit_leaf.view;
        if hit == plan.focused {
            return false; // already focused: proceed with the normal press
        }
        let hit_origin = (
            hit_leaf.rect.origin.y.round().max(0.0) as u16,
            hit_leaf.rect.origin.x.round().max(0.0) as u16,
        );
        let terminal_projection = self.active_tree(wid).is_some();
        let moved = if terminal_projection {
            let Some(session) = self
                .view_store
                .get(hit)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
            else {
                return false;
            };
            self.active_tree_mut(wid)
                .is_some_and(|tree| tree.set_focus(session))
        } else {
            self.windows
                .get_mut(&wid)
                .and_then(|window| window.tab_set.active_mut())
                .is_some_and(|tab| tab.set_focus(hit))
        };
        if !moved {
            return false;
        }
        if terminal_projection {
            let active = self.windows.get(&wid).map_or(0, |ws| ws.tabs.active);
            let synced = self.sync_tab_model_from_layout(wid, active);
            debug_assert!(synced);
        }
        let (ro, co) = hit_origin;
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_mouse_cell = (wr.saturating_sub(ro), wc.saturating_sub(co));
        }
        self.resync_active_or_window(wid);
        true
    }

    /// If window `wid`'s last pointer position lands on a pane divider.
    #[cfg(test)]
    pub(crate) fn begin_divider_drag(&mut self, wid: WindowId) -> bool {
        self.begin_divider_drag_with_plan(wid, None)
    }

    fn begin_divider_drag_with_plan(
        &mut self,
        wid: WindowId,
        canonical_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> bool {
        if !self.active_tab_has_multiple_visible_panes(wid) {
            return false;
        }
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let (px, py) = ws.last_cursor_px;
        if self.band_banner_at(wid, px, py).is_some()
            || self.find_bar_cell_at(wid, px, py).is_some()
            || self.stats_lens_at(wid, px, py)
        {
            return false;
        }
        let (wr, wc, rows, cols) = (
            ws.last_mouse_window_cell.0,
            ws.last_mouse_window_cell.1,
            ws.rows,
            ws.cols,
        );
        let hit = if let Some(tree) = self.active_tree(wid) {
            if tree.len() == 1 {
                return false;
            }
            let Some(hit) = tree.divider_at(wr, wc, rows, cols) else {
                return false;
            };
            crate::DividerDrag::Terminal(hit)
        } else {
            let owned_plan;
            let plan = match canonical_plan {
                Some(plan) => plan,
                None => {
                    let Some(plan) = self.active_visible_leaf_plan(wid) else {
                        return false;
                    };
                    owned_plan = plan;
                    &owned_plan
                }
            };
            let Some(divider) = plan.divider_at(crate::tab_model::LogicalPoint {
                x: f32::from(wc),
                y: f32::from(wr),
            }) else {
                return false;
            };
            crate::DividerDrag::Canonical {
                path: divider.path.clone(),
                axis: divider.axis,
            }
        };
        let icon = match &hit {
            crate::DividerDrag::Terminal(hit) => match hit.dir {
                pane::SplitDir::Vertical => CursorIcon::ColResize,
                pane::SplitDir::Horizontal => CursorIcon::RowResize,
            },
            crate::DividerDrag::Canonical { axis, .. } => match axis {
                crate::tab_model::SplitAxis::Horizontal => CursorIcon::ColResize,
                crate::tab_model::SplitAxis::Vertical => CursorIcon::RowResize,
            },
        };
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.divider_drag = Some(hit);
            if let Some(w) = &ws.os_window {
                w.set_cursor(icon);
            }
        }
        PANE_DRAG_APPLIED_AT.with(|c| c.set(None));
        PANE_DRAG_PENDING.with(|c| c.set(None));
        PANE_DRAG_CELL.with(|c| c.set(Some((wid, (wr, wc)))));
        true
    }

    /// Mid-drag of a pane divider.
    pub(crate) fn drag_divider(&mut self, wid: WindowId) {
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        let Some(hit) = ws.divider_drag.clone() else {
            return;
        };
        let raw = ws.last_mouse_window_cell;
        let off = ws.last_mouse_px_off;
        let held = PANE_DRAG_CELL.with(|c| match c.get() {
            Some((w, cell)) if w == wid => cell,
            _ => raw,
        });
        let cell = divider_drag_cell(held, raw, off, self.pointer_geometry(wid).cell);
        let pending = PANE_DRAG_PENDING.with(|c| c.get()) == Some(wid);
        if cell == held && !pending {
            return;
        }
        PANE_DRAG_CELL.with(|c| c.set(Some((wid, cell))));
        let now = Instant::now();
        let gated = PANE_DRAG_APPLIED_AT.with(|c| {
            matches!(c.get(), Some((w, t)) if w == wid
                && now.saturating_duration_since(t) < crate::RESIZE_THROTTLE)
        });
        if gated {
            PANE_DRAG_PENDING.with(|c| c.set(Some(wid)));
            return;
        }
        if self.apply_divider_drag(wid, &hit) {
            PANE_DRAG_APPLIED_AT.with(|c| c.set(Some((wid, now))));
            PANE_DRAG_PENDING.with(|c| c.set(None));
        }
    }

    fn apply_divider_drag(&mut self, wid: WindowId, hit: &crate::DividerDrag) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let (wr, wc) = PANE_DRAG_CELL.with(|c| match c.get() {
            Some((w, cell)) if w == wid => cell,
            _ => ws.last_mouse_window_cell,
        });
        let (applied, terminal_projection) = match hit {
            crate::DividerDrag::Terminal(hit) => {
                let Some(tree) = self.active_tree(wid) else {
                    return false;
                };
                let Some(ratio) = tree.ratio_for_pointer(hit, wr, wc) else {
                    return false;
                };
                (
                    self.active_tree_mut(wid)
                        .is_some_and(|tree| tree.set_divider_ratio(hit, ratio)),
                    true,
                )
            }
            crate::DividerDrag::Canonical { path, .. } => {
                let Some(plan) = self.active_visible_leaf_plan(wid) else {
                    return false;
                };
                let Some(divider) = plan.dividers.iter().find(|divider| divider.path == *path)
                else {
                    return false;
                };
                let Some(ratio) = plan.ratio_for_pointer(
                    divider,
                    crate::tab_model::LogicalPoint {
                        x: f32::from(wc),
                        y: f32::from(wr),
                    },
                ) else {
                    return false;
                };
                (
                    self.windows
                        .get_mut(&wid)
                        .and_then(|window| window.tab_set.active_mut())
                        .is_some_and(|tab| tab.set_divider_ratio(path, ratio)),
                    false,
                )
            }
        };
        if !applied {
            return false;
        }
        if terminal_projection {
            let active = self.windows.get(&wid).map_or(0, |ws| ws.tabs.active);
            let synced = self.sync_tab_model_from_layout(wid, active);
            debug_assert!(synced);
        }
        self.resize_panes(wid);
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
        true
    }

    /// End any in-flight divider drag (left release).
    pub(crate) fn finish_divider_drag(&mut self, wid: WindowId) -> bool {
        let Some(hit) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.divider_drag.clone())
        else {
            return false;
        };
        if PANE_DRAG_PENDING.with(|c| c.get()) == Some(wid) {
            PANE_DRAG_PENDING.with(|c| c.set(None));
            self.apply_divider_drag(wid, &hit);
        }
        PANE_DRAG_APPLIED_AT.with(|c| c.set(None));
        PANE_DRAG_CELL.with(|c| c.set(None));
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.divider_drag = None;
            ws.hover_pointer = false;
            ws.native_text_cursor = false;
            if let Some(w) = &ws.os_window {
                w.set_cursor(CursorIcon::Default);
            }
        }
        true
    }

    /// Settle an in-flight text-selection drag or native control press, for the paths about to drop `selecting`/`gesture` or front custody (window blur, a tab or pane switch).
    pub(crate) fn settle_selection_gesture(&mut self, wid: WindowId) {
        self.settle_native_pointer_press(wid);
        if self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
            self.finish_selection(wid, true);
        }
    }

    pub(crate) fn settle_native_pointer_press(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.page_buttons = 0;
        }
        if let Some((_, view)) = self.active_native_view(wid)
            && self
                .native_runtime
                .view_state(view)
                .is_some_and(|state| state.common().pressed.is_some())
        {
            if let Some(state) = self.native_runtime.view_state_mut(view) {
                state.common_mut().pressed = None;
            }
            self.invalidate_native_pointer_style(wid, view);
            self.request_redraw_all_windows();
        }
    }

    /// Settle any in-flight pointer drag on `wid` (divider resize, text selection, native control press) as their left-release paths in [`Self::on_mouse_input`] would.
    pub(crate) fn settle_pointer_drags(&mut self, wid: WindowId) {
        self.scrollbar_release(wid);
        self.settle_native_pointer_press(wid);
        if self.finish_divider_drag(wid)
            && let Some(ws) = self.windows.get_mut(&wid)
        {
            ws.held_mouse_button = held_button(ws.reported_buttons);
        }
        if self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
            self.finish_selection(wid, false);
            if let Some(ws) = self.windows.get_mut(&wid) {
                forget_reported_button(ws, aterm_types::mouse::MouseButton::Left);
            }
        }
    }

    /// Cmd-C: copy the selected text to the system clipboard (`pbcopy`), leaving the selection so repeated copies work. A control client's gesture copies nothing (copy-on-select included), and the refusal is logged ([`crate::app_take_over::note_clipboard_refused`]).
    pub(crate) fn copy_selection_in(&self, wid: WindowId) -> bool {
        if self
            .windows
            .get(&wid)
            .is_none_or(|ws| ws.overlay_open() || ws.rename_edit.is_some())
        {
            return false;
        }
        if self.clipboard_gesture_refused() {
            crate::app_take_over::note_clipboard_refused("copy");
            return false;
        }
        // A private document's text reaches the clipboard on the person's own gesture only: an
        // accessibility client's request or another process's posted input copies none of it,
        // its text face and an open cell included (one-mesh design §4.8).
        if self.gesture_unvouched().is_some() && self.front_view_is_private(wid) {
            crate::app_take_over::note_clipboard_refused("copy");
            return false;
        }
        // A pinned or keyboard lens's ⌘C copies its instrument's sentence.
        if self.stats_lens_copy_ready(wid).is_some() {
            return self.copy_stats_sentence(wid);
        }
        if self.active_native_view(wid).is_some() {
            return self
                .native_selection_text(wid)
                .is_some_and(|text| control::pbcopy(&text));
        }
        let Some(text) = self.window_selection_text(wid) else {
            return false;
        };
        !text.is_empty() && control::pbcopy(&text)
    }

    /// ⌘C, a Copy binding and Edit ▸ Copy: a grid with no cell open copies its cell through the
    /// editor's `kill-ring-save`, so the value reaches the kill ring (`C-y`) as well as the
    /// clipboard; anything else is [`Self::copy_selection_in`]. A control client's gesture has no
    /// clipboard authority (`selection.copy` asks for it), so the editor refuses its
    /// `kill-ring-save` with a status, anything else (a terminal, a text view, an open grid cell)
    /// copies nothing for it and answers its `invoke` with why
    /// ([`Self::refuse_clipboard_gesture`]), and the clipboard keeps what it held. Whether
    /// anything was copied.
    pub(crate) fn copy_in(&mut self, wid: WindowId) -> bool {
        if !self.editor_grid_copies(wid) {
            if self.refuse_clipboard_gesture("copy") {
                return false;
            }
            return self.copy_selection_in(wid);
        }
        let copied = self.native_has_selection(wid) && !self.clipboard_gesture_refused();
        self.dispatch_native_event(
            wid,
            crate::native_app::AppEvent::EditorCommand(
                crate::native_editor::EditorCommand::KillRingSave,
            ),
        )
        .is_ok()
            && copied
    }

    /// Whether a copy in window `wid` is a grid's, through its editor: the active view draws a
    /// grid with no cell open and no prompt, and nothing in front of it owns Copy.
    fn editor_grid_copies(&self, wid: WindowId) -> bool {
        if self
            .windows
            .get(&wid)
            .is_none_or(|ws| ws.overlay_open() || ws.rename_edit.is_some())
            || self.stats_lens_copy_ready(wid).is_some()
        {
            return false;
        }
        let Some((_, view)) = self.active_native_view(wid) else {
            return false;
        };
        matches!(
            self.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Editor(state)) if state.buffer.as_ref().is_some_and(|buffer| {
                buffer.grid().is_some() && buffer.grid_draft().is_none() && !buffer.minibuffer_active()
            })
        )
    }

    /// The text of window `wid`'s live selection, resolved across panes.
    pub(crate) fn window_selection_text(&self, wid: WindowId) -> Option<String> {
        // Ownership is decided by `has_selection()`, never by whether the selection resolves
        // to text: rows are trailing-trimmed, so a painted selection over blank cells
        // resolves to `None`, and falling through would copy a sibling pane the user never
        // touched (on every drag, with `copy_on_select` on).
        if let Some((term, _)) = self.front_shown(wid) {
            let term = term_lock(&term);
            if term.text_selection().has_selection() {
                return term.selection_to_string();
            }
        }
        if !self.active_tab_has_multiple_visible_panes(wid) {
            return None;
        }
        let plan = self.active_visible_leaf_plan(wid)?;
        for leaf in &plan.leaves {
            let Some(crate::tab_model::View::Terminal(view)) =
                self.view_store.get(leaf.view).copied()
            else {
                continue;
            };
            let Some(term) = self.pool.shown_term(view.session) else {
                continue;
            };
            let term = term_lock(&term);
            if term.text_selection().has_selection() {
                return term.selection_to_string();
            }
        }
        None
    }

    /// Whether Edit ▸ Copy and the palette's Copy row are live for window `wid`'s native view: it
    /// has a selection, and it is no private document's. A private document's Copy is the
    /// person's keystroke alone (⌘C, M-w), which its window classifies as it arrives; a menu item
    /// is pressed through the Accessibility API with no event of the person's behind it, so it
    /// stays greyed there, and ⌘C, no menu's key equivalent then, reaches the view as a key
    /// (one-mesh design §4.8).
    pub(crate) fn menu_copies(&self, wid: WindowId) -> bool {
        self.native_has_selection(wid) && !self.front_view_is_private(wid)
    }

    /// Edit ▸ Copy: [`Self::copy_in`] against the frontmost window.
    pub(crate) fn copy_selection(&mut self) -> bool {
        self.frontmost_window.is_some_and(|wid| self.copy_in(wid))
    }

    /// Map a pixel position to a 0-based `(row, col)` terminal grid cell of window `wid`, clamped to the grid.
    #[cfg(test)]
    pub(crate) fn pixel_to_cell(&self, wid: WindowId, x: f64, y: f64) -> (u16, u16) {
        self.pixel_to_cell_with(wid, self.pointer_geometry(wid), x, y)
    }

    fn pointer_geometry(&self, wid: WindowId) -> PointerGeometry {
        PointerGeometry {
            cell: self.win_cell_size(wid),
            pad: self.win_pad(wid),
            pad_top: self.win_pad_top(wid),
            head: self.win_head(wid),
        }
    }

    fn pixel_to_cell_with(
        &self,
        wid: WindowId,
        geom: PointerGeometry,
        x: f64,
        y: f64,
    ) -> (u16, u16) {
        let (row, col) = self.pixel_to_glass_cell_with(wid, geom, x, y);
        let Some(shifted) = self
            .windows
            .get(&wid)
            .map(|ws| ws.claude_footer_undo.grid_col(row, col))
            .filter(|&c| c != col)
        else {
            return (row, col);
        };
        if self.laid_terminal_cell_at(wid, x, y) == Some((row, col)) {
            (row, shifted)
        } else {
            (row, col)
        }
    }

    /// The terminal cell `(row, col)` on the laid glass grid under window pixel `(x, y)`, when
    /// the grid stands as [`Self::splice_claude_footer`] laid it and no chrome owns that cell.
    fn laid_terminal_cell_at(&self, wid: WindowId, x: f64, y: f64) -> Option<(u16, u16)> {
        if self.band_banner_at(wid, x, y).is_some() || self.find_bar_cell_at(wid, x, y).is_some() {
            return None;
        }
        let (frame_row, col) = self.glass_cell_at(wid, x, y)?;
        let term_row = frame_row.checked_sub(usize::from(self.chrome_rows(wid)))?;
        if self.chrome_owns_terminal_cell(wid, term_row, col) {
            return None;
        }
        let row = u16::try_from(term_row).ok()?;
        let col = u16::try_from(col).ok()?;
        (row < self.windows.get(&wid)?.rows).then_some((row, col))
    }

    /// Whether a press at window pixel `(x, y)` lands on a cell the glass's rewrite of a row
    /// vacated (`claude_footer::FooterUndo::swallows`): it lands on nothing.
    pub(crate) fn glass_swallows_at(&self, wid: WindowId, x: f64, y: f64) -> bool {
        if !self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.claude_footer_undo.has_glass_shifts())
        {
            return false;
        }
        let Some((row, col)) = self.laid_terminal_cell_at(wid, x, y) else {
            return false;
        };
        self.windows
            .get(&wid)
            .is_some_and(|ws| ws.claude_footer_undo.swallows(row, col))
    }

    /// The window grid cell under `(x, y)` as the glass shows it, before a rewritten row's
    /// columns are mapped back to Claude's.
    fn pixel_to_glass_cell_with(
        &self,
        wid: WindowId,
        geom: PointerGeometry,
        x: f64,
        y: f64,
    ) -> (u16, u16) {
        let (cw, ch) = geom.cell;
        let (rows, cols) = self
            .windows
            .get(&wid)
            .map_or((0, 0), |ws| (ws.rows, ws.cols));
        let (x, y) = self.window_to_frame_with(wid, geom, x, y);
        let (row, col) = pixel_to_term_cell(
            x,
            y,
            cw,
            ch,
            rows,
            cols,
            self.chrome_rows(wid),
            geom.pad,
            geom.pad_top,
            geom.head,
        );
        (row, col)
    }

    fn refresh_mouse_cell(
        &mut self,
        wid: WindowId,
        geom: PointerGeometry,
        x: f64,
        y: f64,
    ) -> MouseCellMapping {
        self.refresh_mouse_cell_with_plan(wid, geom, x, y, None)
    }

    fn refresh_mouse_cell_with_plan(
        &mut self,
        wid: WindowId,
        geom: PointerGeometry,
        x: f64,
        y: f64,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> MouseCellMapping {
        let (row, col) = self.pixel_to_cell_with(wid, geom, x, y);
        let pane_rect = match frame_plan {
            Some(plan) => self.focused_pane_rect_from_plan(wid, plan),
            None => self.focused_pane_rect(wid),
        };
        let (lr, lc) = cell_in_rect((row, col), pane_rect);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_mouse_window_cell = (row, col);
            ws.last_mouse_cell = (lr, lc);
        }
        MouseCellMapping {
            pane_cell: (lr, lc),
            pane_rect,
        }
    }

    fn pointer_subcell_px(
        &self,
        wid: WindowId,
        geom: PointerGeometry,
        x: f64,
        y: f64,
    ) -> crate::input::PixelOffset {
        let (cw, ch) = geom.cell;
        let (gx, gy) = self.pointer_grid_px(wid, geom, x, y);
        crate::input::PixelOffset {
            x: (gx % cw.max(1)) as u16,
            y: (gy % ch.max(1)) as u16,
        }
    }

    fn pointer_grid_px(
        &self,
        wid: WindowId,
        geom: PointerGeometry,
        x: f64,
        y: f64,
    ) -> (usize, usize) {
        let ch = geom.cell.1.max(1);
        let (fx, fy) = self.window_to_frame_with(wid, geom, x, y);
        let gx = (fx - geom.pad as f64).max(0.0) as usize;
        let strip_px = usize::from(self.chrome_rows(wid)) * ch;
        let gy = (fy - (geom.pad_top + geom.head) as f64).max(0.0) as usize;
        (gx, gy.saturating_sub(strip_px))
    }

    fn hover_mapping(&self, wid: WindowId) -> HoverMapping {
        self.hover_mapping_with(wid, self.pointer_geometry(wid), self.focused_pane_rect(wid))
    }

    fn hover_mapping_with(
        &self,
        wid: WindowId,
        geom: PointerGeometry,
        pane_rect: (u16, u16, u16, u16),
    ) -> HoverMapping {
        let ws = self.windows.get(&wid);
        HoverMapping {
            front: ws.and_then(|ws| ws.front_content),
            win_px: ws.and_then(|ws| ws.win_px.map(|px| (px.width, px.height))),
            dims: ws.map_or((0, 0), |ws| (ws.rows, ws.cols)),
            chrome_rows: self.chrome_rows(wid),
            geom,
            pane_rect,
        }
    }

    /// The window pixel at the centre of terminal cell `(row, col)`.
    pub(crate) fn cell_centre_px(&self, wid: WindowId, row: u16, col: u16) -> Option<(f64, f64)> {
        let (rows, cols) = self.windows.get(&wid).map(|ws| (ws.rows, ws.cols))?;
        if row >= rows || col >= cols {
            return None;
        }
        let geom = self.pointer_geometry(wid);
        let (cw, ch) = geom.cell;
        if cw == 0 || ch == 0 {
            return None;
        }
        let (ox, oy) = self.frame_origin_with(wid, geom);
        let strip_px = usize::from(self.chrome_rows(wid)) * ch;
        let fx = usize::from(col) * cw + cw / 2 + geom.pad;
        let fy = usize::from(row) * ch + ch / 2 + strip_px + geom.pad_top + geom.head;
        #[allow(
            clippy::cast_precision_loss,
            reason = "window pixel coordinates; the f64 mantissa covers every representable window size"
        )]
        Some((ox as f64 + fx as f64, oy as f64 + fy as f64))
    }

    /// A mouse button in `wid` exactly as the event loop's `MouseInput` arm delivers it, shared with `pointer click` so the two cannot drift.
    pub(crate) fn mouse_input_then_fold(
        &mut self,
        wid: WindowId,
        state: ElementState,
        button: WinitMouseButton,
    ) {
        let pressed = state == ElementState::Pressed;
        let seen = pressed.then(|| self.focused_session_id(wid)).flatten();
        if let Some(view) = self.mouse_input_target(wid, state, button, Instant::now()) {
            self.interact_system_state_view(wid, view);
        }
        if let Some(session) = seen {
            self.note_human_acted_on(wid, session);
        }
    }

    /// Dispatch one pointer request and read the frame its event left.
    pub(crate) fn pointer_request(&mut self, action: PointerAction) -> PointerReply {
        let status = matches!(&action, PointerAction::Status);
        let cell = self.pointer_cmd(action)?;
        match self.pointer_reply(cell) {
            Err(readback_error) if !status => Ok(PointerAt::Dispatched { readback_error }),
            reply => reply,
        }
    }

    /// Drive the front window's pointer and report where it ended up.
    pub(crate) fn pointer_cmd(
        &mut self,
        action: PointerAction,
    ) -> Result<Option<(i32, u16)>, String> {
        let Some(wid) = self.frontmost_window else {
            return Err("no window".into());
        };
        if self.window_shows_private(wid) {
            return Err(crate::app_control::PRIVATE_VIEW_REFUSED.into());
        }
        // The pointer aims at, and hits, the frame the window shows: a page changed since its last
        // present (a press's save landing, a recompile) is presented first, committed where the
        // window has no glass and drawn on glass where it has, as the reply reads it after.
        if matches!(
            action,
            PointerAction::Move { .. }
                | PointerAction::MoveNative(_)
                | PointerAction::MoveItem(_)
                | PointerAction::Click
                | PointerAction::Wheel { .. }
        ) && !self.commit_glassless_native_frame(wid)
        {
            self.present_native_frame_on_glass(wid);
        }
        if matches!(
            action,
            PointerAction::Move { .. }
                | PointerAction::MoveItem(_)
                | PointerAction::MoveChrome { .. }
                | PointerAction::Click
        ) {
            let _ = self.lay_glassless_terminal_chrome(wid);
        }
        match action {
            PointerAction::MoveNative(target) => {
                let (x, y) = self.native_pointer_target_px(wid, &target)?;
                self.on_cursor_moved(wid, x, y);
            }
            PointerAction::Wheel { dx, dy, unit, aim } => {
                let aimed = aim.is_some();
                let (x, y) = match aim {
                    Some(target) => self.native_pointer_target_px(wid, &target)?,
                    None => self
                        .windows
                        .get(&wid)
                        .filter(|ws| ws.pointer_position_known)
                        .map(|ws| ws.last_cursor_px)
                        .ok_or("the pointer holds no position (move it first)")?,
                };
                if aimed || cfg!(target_os = "macos") {
                    self.on_cursor_moved(wid, x, y);
                }
                let (delta, lifts) = match unit {
                    WheelUnit::Lines => {
                        (MouseScrollDelta::LineDelta(-dx as f32, -dy as f32), false)
                    }
                    WheelUnit::Points => {
                        let scale = self.windows.get(&wid).map_or(1.0, |ws| ws.scale);
                        (
                            MouseScrollDelta::PixelDelta(aterm_window::dpi::PhysicalPosition::new(
                                -dx * scale,
                                -dy * scale,
                            )),
                            true,
                        )
                    }
                };
                self.on_mouse_wheel_phased(wid, delta, TouchPhase::Moved);
                if lifts {
                    self.on_mouse_wheel_phased(
                        wid,
                        MouseScrollDelta::PixelDelta(aterm_window::dpi::PhysicalPosition::new(
                            0.0, 0.0,
                        )),
                        TouchPhase::Ended,
                    );
                }
            }
            PointerAction::Move { row, col } => {
                let (x, y) = self
                    .cell_centre_px(wid, row, col)
                    .ok_or("cell is outside the grid")?;
                self.on_cursor_moved(wid, x, y);
            }
            PointerAction::MoveItem(item) => {
                let (x, y) = self.overlay_card_item_px(wid, item)?;
                self.on_cursor_moved(wid, x, y);
            }
            PointerAction::MoveChrome { up, col } => {
                let (x, y) = self
                    .chrome_cell_centre_px(wid, up, col)
                    .ok_or("cell is outside the chrome rows")?;
                self.on_cursor_moved(wid, x, y);
            }
            PointerAction::Click => {
                let Some((x, y)) = self
                    .windows
                    .get(&wid)
                    .filter(|ws| ws.pointer_position_known)
                    .map(|ws| ws.last_cursor_px)
                else {
                    return Err("the pointer holds no position (move it first)".into());
                };
                for state in [ElementState::Pressed, ElementState::Released] {
                    if cfg!(target_os = "macos") {
                        self.on_cursor_moved(wid, x, y);
                    }
                    self.mouse_input_then_fold(wid, state, WinitMouseButton::Left);
                }
            }
            PointerAction::Leave => self.on_cursor_left(wid),
            PointerAction::Status => {}
            PointerAction::Mods(mods) => {
                // Keep the person's own modifiers aside, once per script, for
                // their next input to put back (`App::release_scripted_mods`).
                if let Some(ws) = self.windows.get_mut(&wid)
                    && ws.scripted_mods_saved.is_none()
                {
                    ws.scripted_mods_saved = Some(ws.mods);
                }
                self.on_modifiers_changed(wid, mods);
            }
        }
        if !self.commit_glassless_native_frame(wid) {
            self.present_native_frame_on_glass(wid);
        }
        Ok(self.pointer_readback(wid))
    }

    fn present_native_frame_on_glass(&mut self, wid: WindowId) {
        if !self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.present.is_some())
        {
            return;
        }
        let Some((_, view)) = self.active_native_view(wid) else {
            return;
        };
        let Some(plan) = self.active_visible_leaf_plan(wid) else {
            return;
        };
        if self
            .retained_native_leaf_artifact_from_plan(wid, view, true, &plan)
            .is_none()
        {
            self.redraw_capture_frame(wid, None);
        }
    }

    /// The `pointer` verb's answer after [`Self::pointer_cmd`] ran.
    pub(crate) fn pointer_reply(&self, cell: Option<(i32, u16)>) -> PointerReply {
        let px = self.pointer_px();
        let Some(cell) = cell else {
            return Ok(PointerAt::Absent);
        };
        let Some(wid) = self.frontmost_window else {
            return Err("no window".into());
        };
        if let Some((x, y)) = self
            .windows
            .get(&wid)
            .filter(|ws| ws.pointer_position_known)
            .map(|ws| ws.last_cursor_px)
            && let Some((artifact, local_x, local_y)) =
                self.retained_native_leaf_at_pointer(wid, x, y)
        {
            let key = artifact
                .compiled
                .hit_test(local_x, local_y)
                .map(|hit| hit.key.as_str().to_string());
            return Ok(PointerAt::Native {
                view: artifact.view.get(),
                x: local_x,
                y: local_y,
                key,
                px,
            });
        }
        if cell.0 >= 0
            && let Some((_, view)) = self.active_native_view(wid)
            && !self.active_tab_has_multiple_visible_panes(wid)
        {
            let framed = self
                .windows
                .get(&wid)
                .and_then(|ws| ws.leaf_render_cache.get(&view))
                .is_some_and(|cache| cache.native.is_some());
            return Err(if framed {
                "the page under the pointer changed and its next frame is not presented yet \
                 (pointer status reads it)"
            } else {
                "no presented native frame under the pointer"
            }
            .into());
        }
        Ok(PointerAt::Grid { cell, px })
    }

    /// Commit the frame a capture of `wid` shows as the presented one, where the window has no glass to present on.
    pub(crate) fn commit_glassless_native_frame(&mut self, wid: WindowId) -> bool {
        if !self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.os_window.is_none() && ws.present.is_none())
        {
            return false;
        }
        let Some((_, view)) = self.active_native_view(wid) else {
            return false;
        };
        let Some((plan, route)) = self.active_visible_frame_layout(wid) else {
            return false;
        };
        // A mixed tab's focused native leaf is committed with the mixed frame it sits in.
        if route == crate::VisibleContentRoute::Heterogeneous {
            if !self.commit_glassless_heterogeneous_frame_from_plan(wid, &plan) {
                return false;
            }
            self.refresh_native_hover_for_frame_from_plan(wid, &plan);
            return true;
        }
        if !self.prepare_native_input_scratch_from_capture_plan(wid, &plan) {
            return false;
        }
        let Some(raster) = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.leaf_render_cache.get_mut(&view))
            .and_then(|cache| cache.native.as_mut())
        else {
            return false;
        };
        raster.presented = true;
        self.refresh_native_hover_for_frame_from_plan(wid, &plan);
        true
    }

    fn native_pointer_target_px(
        &self,
        wid: WindowId,
        target: &NativePointerTarget,
    ) -> Result<(f64, f64), String> {
        let (_, view) = self
            .active_native_view(wid)
            .ok_or("the front tab is not a native view")?;
        let artifact = self
            .retained_native_pointer_artifact(wid, view)
            .ok_or("no presented native frame to aim at")?;
        let (x, y) = match target {
            NativePointerTarget::Key(key) => {
                let compiled = &artifact.compiled;
                let shown = compiled
                    .paint
                    .iter()
                    .find(|node| node.key.as_str() == key)
                    .map(|node| node.clip)
                    .filter(|clip| clip.width > 0.0 && clip.height > 0.0)
                    .ok_or_else(|| format!("{key} is not on the presented page"))?;
                let rect = compiled
                    .hits
                    .iter()
                    .find(|hit| hit.key.as_str() == key)
                    .map_or(shown, |hit| hit.rect);
                (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
            }
            NativePointerTarget::Point(x, y) => (*x, *y),
        };
        let px = artifact.device_x as f64 + f64::from(x) * artifact.scale;
        let py = artifact.device_y as f64 + f64::from(y) * artifact.scale;
        if !(0.0..f64::from(artifact.width)).contains(&(px - artifact.device_x as f64))
            || !(0.0..f64::from(artifact.height)).contains(&(py - artifact.device_y as f64))
        {
            return Err("the point is outside the native view".into());
        }
        Ok((px, py))
    }

    /// The front window's pointer as a window pixel (rounded), or `None` when it holds no position.
    pub(crate) fn pointer_px(&self) -> Option<(i32, i32)> {
        let ws = self
            .frontmost_window
            .and_then(|wid| self.windows.get(&wid))
            .filter(|ws| ws.pointer_position_known)?;
        let (x, y) = ws.last_cursor_px;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a window pixel, rounded; the saturating cast clamps any nonsense"
        )]
        Some((x.round() as i32, y.round() as i32))
    }

    fn apply_native_hover(
        &mut self,
        wid: WindowId,
        hit: Option<(crate::tab_model::ViewId, crate::native_ui::UiKey)>,
    ) {
        let mut hover_changed = Vec::new();
        if let Some(tab) = self.windows.get(&wid).and_then(|ws| ws.tab_set.active()) {
            let view_store = &self.view_store;
            let native_runtime = &mut self.native_runtime;
            let mut update_if_native = |view: &crate::tab_model::ViewId| {
                let target = *view;
                if !matches!(
                    view_store.get(target),
                    Some(crate::tab_model::View::Native(_))
                ) {
                    return;
                }
                let desired = hit
                    .as_ref()
                    .filter(|(hit_view, _)| *hit_view == target)
                    .map(|(_, key)| key.clone());
                if native_runtime.change_view_state(target, |state| {
                    let common = state.common_mut();
                    let moved = common.hovered != desired;
                    if moved {
                        common.hovered = desired;
                    }
                    moved
                }) {
                    hover_changed.push(target);
                }
            };
            if (tab.zoomed && tab.root.contains(tab.focus)) || tab.root.len() == 1 {
                update_if_native(&tab.focus);
            } else {
                tab.root.visit(&mut update_if_native);
            }
        }
        for target in hover_changed {
            self.invalidate_native_pointer_style(wid, target);
            self.request_redraw_all_windows();
        }
    }

    fn native_hover_hit(
        &self,
        wid: WindowId,
        x: f64,
        y: f64,
        over_chrome: bool,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> Option<(
        crate::tab_model::ViewId,
        u64,
        crate::native_ui::UiKey,
        bool,
        bool,
    )> {
        if over_chrome {
            return None;
        }
        self.retained_native_leaf_at_pointer_from_plan(wid, x, y, frame_plan)
            .and_then(|(artifact, local_x, local_y)| {
                let hit = artifact.compiled.hit_test(local_x, local_y)?.clone();
                let is_text = artifact
                    .compiled
                    .semantic(&hit.key)
                    .is_some_and(|node| node.role == crate::native_ui::SemanticRole::TextField);
                Some((
                    artifact.view,
                    artifact.generation,
                    hit.key,
                    is_text,
                    hit.look_first,
                ))
            })
            .filter(|(target, generation, ..)| {
                self.native_runtime.view_generation(*target) == Some(*generation)
            })
    }

    /// A PAGE A WHEEL OR A PRESS MOVED IS HOVERED WHERE THE POINTER IS ON THE FRAME IT LEFT.
    #[cfg(test)]
    pub(crate) fn refresh_native_hover_for_frame(&mut self, wid: WindowId) {
        if self.active_native_view(wid).is_none()
            || !self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.native_rehover && ws.pointer_position_known)
        {
            return;
        }
        let Some(plan) = self.active_visible_leaf_plan(wid) else {
            return;
        };
        self.refresh_native_hover_for_frame_from_plan(wid, &plan);
    }

    pub(crate) fn refresh_native_hover_for_frame_from_plan(
        &mut self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) {
        let Some((_, front)) = self.active_native_view(wid) else {
            return;
        };
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        if !ws.native_rehover || !ws.pointer_position_known {
            return;
        }
        let (x, y) = ws.last_cursor_px;
        if self
            .retained_native_pointer_artifact_from_plan(wid, front, plan)
            .is_none()
        {
            return;
        }
        let geom = self.pointer_geometry(wid);
        let over_chrome = self
            .pane_header_at_with_plan(wid, geom, x, y, Some(plan))
            .is_some()
            || self.strip_col_at_with(wid, geom, x, y).is_some()
            || self.band_banner_at(wid, x, y).is_some()
            || self.find_bar_cell_at(wid, x, y).is_some()
            || self.stats_owns_pointer(wid);
        let hit = self.native_hover_hit(wid, x, y, over_chrome, Some(plan));
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.native_rehover = false;
        }
        self.apply_native_hover(wid, hit.map(|(view, _, key, ..)| (view, key)));
    }

    pub(crate) fn evict_native_hover_under_stats_lens(&mut self, wid: WindowId) {
        let Some((x, y)) = self
            .windows
            .get(&wid)
            .filter(|ws| ws.pointer_position_known)
            .map(|ws| ws.last_cursor_px)
        else {
            return;
        };
        if !self.stats_lens_at(wid, x, y) {
            return;
        }
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.native_rest = None;
            if ws.hover_pointer || ws.native_text_cursor {
                ws.hover_pointer = false;
                ws.native_text_cursor = false;
                if let Some(w) = &ws.os_window {
                    w.set_cursor(CursorIcon::Default);
                }
            }
        }
        self.apply_native_hover(wid, None);
    }

    fn pointer_readback(&self, wid: WindowId) -> Option<(i32, u16)> {
        let ws = self
            .windows
            .get(&wid)
            .filter(|ws| ws.pointer_position_known)?;
        let geom = self.pointer_geometry(wid);
        let (cw, ch) = geom.cell;
        let chrome = usize::from(self.chrome_rows(wid));
        let chrome_height = geom.head.saturating_add(chrome.saturating_mul(ch));
        if cw > 0 && ch > 0 && chrome_height > 0 {
            let (x, y) = ws.last_cursor_px;
            #[allow(
                clippy::cast_precision_loss,
                reason = "window pixel coordinates; the f64 mantissa covers every representable window size"
            )]
            let (top, band_h, pad) = (geom.pad_top as f64, chrome_height as f64, geom.pad as f64);
            let fy = self.window_to_frame_y_signed(wid, geom, y) - top;
            if (0.0..band_h).contains(&fy) {
                let (fx, _) = self.window_to_frame_with(wid, geom, x, y);
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "non-negative, and bounded by the chrome rows and the columns"
                )]
                let (up, col) = (
                    ((band_h - fy) / ch as f64).ceil() as usize,
                    ((fx - pad).max(0.0) / cw as f64) as usize,
                );
                let col = u16::try_from(col)
                    .unwrap_or(u16::MAX)
                    .min(ws.cols.saturating_sub(1));
                let up = i32::try_from(up).unwrap_or(i32::MAX);
                return Some((-up, col));
            }
        }
        let (row, col) = ws.last_mouse_window_cell;
        Some((i32::from(row), col))
    }

    fn chrome_cell_centre_px(&self, wid: WindowId, up: u16, col: u16) -> Option<(f64, f64)> {
        let cols = self.windows.get(&wid).map(|ws| ws.cols)?;
        if up == 0 || col >= cols {
            return None;
        }
        let geom = self.pointer_geometry(wid);
        let (cw, ch) = geom.cell;
        if cw == 0 || ch == 0 {
            return None;
        }
        let height = geom
            .head
            .saturating_add(usize::from(self.chrome_rows(wid)).saturating_mul(ch));
        let up = usize::from(up);
        if up > height.div_ceil(ch) {
            return None;
        }
        let (ox, oy) = self.frame_origin_with(wid, geom);
        let fx = usize::from(col) * cw + cw / 2 + geom.pad;
        // Bands count upward from the grid, `up = 1` the row just above it.
        // The top band may contain only part of a cell's height.
        let bottom = height.saturating_sub((up - 1).saturating_mul(ch));
        let top = height.saturating_sub(up.saturating_mul(ch));
        let fy = top + (bottom - top) / 2 + geom.pad_top;
        #[allow(
            clippy::cast_precision_loss,
            reason = "window pixel coordinates; the f64 mantissa covers every representable window size"
        )]
        Some((ox as f64 + fx as f64, oy as f64 + fy as f64))
    }

    /// Where the padded frame's top-left sits inside the raw window.
    pub(crate) fn frame_origin(&self, wid: WindowId) -> (i64, i64) {
        // The bail-outs are geometry-free, so they come before `pointer_geometry`.
        if !self.windows.get(&wid).is_some_and(|ws| ws.win_px.is_some()) {
            return (0, 0);
        }
        self.frame_origin_with(wid, self.pointer_geometry(wid))
    }

    fn frame_origin_with(&self, wid: WindowId, geom: PointerGeometry) -> (i64, i64) {
        let Some(ws) = self.windows.get(&wid) else {
            return (0, 0);
        };
        let Some(size) = ws.win_px else {
            return (0, 0);
        };
        let (cw, ch) = geom.cell;
        let (pad, pad_top, head) = (geom.pad, geom.pad_top, geom.head);
        let composed_rows = usize::from(ws.rows).saturating_add(usize::from(self.chrome_rows(wid)));
        let frame_w = usize::from(ws.cols)
            .saturating_mul(cw)
            .saturating_add(pad.saturating_mul(2));
        let frame_h = composed_rows
            .saturating_mul(ch)
            .saturating_add(head)
            .saturating_add(pad_top)
            .saturating_add(pad);
        (
            aterm_render::band_offset(size.width as usize, frame_w),
            aterm_render::band_offset_y(size.height as usize, frame_h),
        )
    }

    /// Translate a window-space pixel to frame space (strip the leading bands).
    pub(crate) fn window_to_frame(&self, wid: WindowId, x: f64, y: f64) -> (f64, f64) {
        let (ox, oy) = self.frame_origin(wid);
        ((x - ox as f64).max(0.0), (y - oy as f64).max(0.0))
    }

    fn window_to_frame_with(
        &self,
        wid: WindowId,
        geom: PointerGeometry,
        x: f64,
        y: f64,
    ) -> (f64, f64) {
        let (ox, oy) = self.frame_origin_with(wid, geom);
        ((x - ox as f64).max(0.0), (y - oy as f64).max(0.0))
    }

    fn window_to_frame_y_signed(&self, wid: WindowId, geom: PointerGeometry, y: f64) -> f64 {
        let (_, oy) = self.frame_origin_with(wid, geom);
        y - oy as f64
    }

    /// If pixel position `(x, y)` lands in window `wid`'s tab-strip region (the top `tab_strip_rows` pixel rows), return its strip column.
    pub(crate) fn strip_col_at(&self, wid: WindowId, x: f64, y: f64) -> Option<u16> {
        // The disabled-strip bail (the macOS default) comes before the geometry derivation.
        if !self.tab_strip_enabled() {
            return None;
        }
        self.strip_col_at_with(wid, self.pointer_geometry(wid), x, y)
    }

    /// A left press inside a paste or close question's rows answers it (cancel), judged against
    /// the banner's own floor (`paste_banner_tray_floor_y`, `0` = none); `false` elsewhere.
    fn press_banner_at(&mut self, wid: WindowId, px: f64, py: f64) -> bool {
        let Some(paste_up) = self.band_banner_at(wid, px, py) else {
            return false;
        };
        if self.inline_rename_edit(wid).is_some() {
            self.clear_strip_press(wid);
            self.settle_rename_edit(wid);
        }
        if paste_up {
            self.answer_paste_banner(false);
        } else {
            self.answer_close_banner(false);
        }
        self.request_redraw_all_windows();
        true
    }

    fn handle_tab_strip_right_press(&mut self, wid: WindowId, col: u16, px: f64, py: f64) {
        if self.inline_rename_edit(wid).is_some() {
            self.clear_strip_press(wid);
            self.settle_rename_edit(wid);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = (px, py);
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = col;
        #[cfg(any(windows, target_os = "linux"))]
        {
            let segs = self
                .windows
                .get(&wid)
                .map(|ws| ws.tab_segments.clone())
                .unwrap_or_default();
            match crate::tab_bar::hit_test(&segs, col) {
                Some(
                    crate::tab_bar::TabHit::Select(index) | crate::tab_bar::TabHit::Close(index),
                ) => {
                    let _ = self.open_tab_context_menu(wid, index, col, false);
                }
                _ => {
                    #[cfg(windows)]
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        ws.band_menu_release_pending = true;
                    }
                    #[cfg(target_os = "linux")]
                    if let Some(ws) = self.windows.get(&wid)
                        && ws
                            .client_frame
                            .is_some_and(|frame| frame.header && frame.window_menu)
                        && let Some(w) = &ws.os_window
                    {
                        w.show_window_menu(aterm_window::dpi::PhysicalPosition::new(px, py));
                    }
                }
            }
        }
    }

    pub(crate) fn band_banner_at(&self, wid: WindowId, px: f64, py: f64) -> Option<bool> {
        let paste_up = self.paste_banner.as_ref().is_some_and(|p| p.wid == wid);
        let close_up = self.close_band(wid).is_some();
        if !(paste_up || close_up) {
            return None;
        }
        let floor = self.paste_banner_tray_floor_y(wid);
        let (_, fy) = self.window_to_frame(wid, px, py);
        let top = (self.win_pad_top(wid) + self.win_head(wid)) as f64;
        (floor > 0 && fy >= top && fy < f64::from(floor)).then_some(paste_up)
    }

    fn strip_col_at_with(
        &self,
        wid: WindowId,
        geom: PointerGeometry,
        x: f64,
        y: f64,
    ) -> Option<u16> {
        if !self.tab_strip_enabled() {
            return None;
        }
        let (cw, ch) = geom.cell;
        let cols = self.windows.get(&wid).map_or(0, |ws| ws.cols);
        let (x, y) = self.window_to_frame_with(wid, geom, x, y);
        if y >= 0.0
            && self
                .deck_rows_px(wid)
                .is_some_and(|rows| rows.contains(&(y as usize)))
        {
            return None;
        }
        strip_col_for_pixel(
            x,
            y,
            cw,
            ch,
            cols,
            self.tab_strip_rows,
            geom.pad,
            geom.pad_top,
            geom.head,
        )
    }

    /// Window pixel `(x, y)` as a frame cell `(row, col)`.
    pub(crate) fn frame_cell_at(&self, wid: WindowId, x: f64, y: f64) -> Option<(usize, usize)> {
        let geom = self.pointer_geometry(wid);
        let cols = self.windows.get(&wid).map_or(0, |ws| ws.cols);
        frame_cell_of(geom, self.frame_origin_with(wid, geom), cols, x, y)
    }

    /// The grid window `wid`'s frame is laid on now ([`GlassGrid`]).
    pub(crate) fn glass_grid(&self, wid: WindowId) -> Option<GlassGrid> {
        let ws = self.windows.get(&wid)?;
        let geom = self.pointer_geometry(wid);
        Some(GlassGrid {
            geom,
            origin: self.frame_origin_with(wid, geom),
            dims: (ws.rows, ws.cols),
            chrome_rows: self.chrome_rows(wid),
            split: split_word(ws.tab_set.active()),
        })
    }

    /// THE ONE GATE for a target a frame laid in frame cells.
    pub(crate) fn laid_cell_at(
        &self,
        wid: WindowId,
        laid: Option<GlassGrid>,
        x: f64,
        y: f64,
    ) -> Option<(usize, usize)> {
        self.laid_grid(wid, laid)?.cell_at(x, y)
    }

    /// [`Self::laid_cell_at`]'s test alone, for a target hit-tested in pixels rather than cells.
    pub(crate) fn laid_grid(&self, wid: WindowId, laid: Option<GlassGrid>) -> Option<GlassGrid> {
        let laid = laid?;
        (self.glass_grid(wid)? == laid).then_some(laid)
    }

    fn tab_menu_probe(
        &self,
        wid: WindowId,
        x: f64,
        y: f64,
    ) -> Option<(crate::tab_menu::MenuRect, usize, usize)> {
        let ws = self.windows.get(&wid)?;
        let rect = ws.tab_menu_rect?;
        let (row, col) = self.laid_cell_at(wid, ws.tab_menu_laid, x, y)?;
        Some((rect, row, col))
    }

    fn tab_menu_action_at(&self, wid: WindowId, x: f64, y: f64) -> Option<usize> {
        let (rect, row, col) = self.tab_menu_probe(wid, x, y)?;
        let ws = self.windows.get(&wid)?;
        let menu = ws.tab_menu.as_ref()?;
        crate::tab_menu::action_at(rect, &menu.entries, row, col)
    }

    fn tab_menu_contains(&self, wid: WindowId, x: f64, y: f64) -> bool {
        self.tab_menu_probe(wid, x, y)
            .is_some_and(|(rect, row, col)| rect.contains(row, col))
    }

    fn track_tab_menu_hover(&mut self, wid: WindowId, x: f64, y: f64) {
        let hovered = self.tab_menu_action_at(wid, x, y);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let Some(menu) = ws.tab_menu.as_mut() else {
            return;
        };
        if hovered.is_none() && menu.keyboard {
            return;
        }
        if hovered.is_some() {
            menu.keyboard = false;
        }
        if menu.highlight == hovered {
            return;
        }
        menu.highlight = hovered;
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
    }

    /// A left press at the pointer pets the cat when it lands on it: the stroke is stamped `at`,
    /// the instant the button went down, which the pet's latch measures against its own clock.
    fn pet_press_at(&mut self, wid: WindowId, at: Instant) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let (px, py) = ws.last_cursor_px;
        if self.band_banner_at(wid, px, py).is_some()
            || self.find_bar_cell_at(wid, px, py).is_some()
            || self.stats_lens_at(wid, px, py)
        {
            return false;
        }
        let (fx, fy) = self.window_to_frame(wid, px, py);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        if ws.fx.companion_mut().press(at, (fx as f32, fy as f32))
            != aterm_effects::host::PressOutcome::Pet
        {
            return false;
        }
        if let Some(w) = ws.os_window.as_ref() {
            w.request_redraw();
        }
        true
    }

    fn track_strip_hover(&mut self, wid: WindowId, geom: PointerGeometry, x: f64, y: f64) {
        if !self.tab_strip_enabled() {
            return;
        }
        self.advance_strip_drag(wid, geom, x, y);
        let hit = self.strip_col_at_with(wid, geom, x, y).and_then(|col| {
            let segs = self
                .windows
                .get(&wid)
                .map(|ws| ws.tab_segments.as_slice())?;
            crate::tab_bar::hit_test(segs, col)
        });
        let hovered = match hit {
            Some(crate::tab_bar::TabHit::Select(index)) => Some(index),
            _ => None,
        };
        let on_new_tab = matches!(hit, Some(crate::tab_bar::TabHit::NewTab));
        let on_version = matches!(hit, Some(crate::tab_bar::TabHit::Version { .. }));
        let on_caption = match hit {
            Some(crate::tab_bar::TabHit::Caption(button)) => Some(button),
            _ => None,
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if ws.strip_hover == hovered
            && ws.strip_hover_new_tab == on_new_tab
            && ws.strip_hover_version == on_version
            && ws.strip_hover_caption == on_caption
        {
            return;
        }
        ws.strip_hover = hovered;
        ws.strip_hover_new_tab = on_new_tab;
        ws.strip_hover_version = on_version;
        ws.strip_hover_caption = on_caption;
        if let Some(window) = &ws.os_window {
            window.request_redraw();
        }
    }

    fn route_frame_motion(&mut self, wid: WindowId, x: f64, y: f64) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let Some(frame) = ws.client_frame else {
            return false;
        };
        if let Some((px, py)) = ws.caption_move {
            let threshold = crate::client_frame::MOVE_THRESHOLD_LOGICAL_PX * ws.scale.max(1.0);
            if (x - px).abs() <= threshold && (y - py).abs() <= threshold {
                return false;
            }
            let window = ws.os_window.clone();
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.caption_move = None;
                ws.strip_band_press = None;
            }
            if let Some(w) = window {
                let _ = w.drag_window();
            }
            return true;
        }
        let gesture = ws.selecting
            || ws.held_mouse_button.is_some()
            || ws.divider_drag.is_some()
            || ws.strip_drag.is_some();
        let edge = if frame.edges && !gesture {
            ws.win_px.and_then(|size| {
                crate::client_frame::edge_at(
                    x,
                    y,
                    f64::from(size.width),
                    f64::from(size.height),
                    ws.scale,
                    self.frame_margins(wid, size),
                )
            })
        } else {
            None
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        let Some(dir) = edge else {
            if ws.frame_edge.take().is_some() {
                ws.hover_pointer = true;
                ws.native_text_cursor = true;
                if let Some(w) = &ws.os_window {
                    w.set_cursor(CursorIcon::Default);
                }
            }
            return false;
        };
        if ws.frame_edge != Some(dir) {
            ws.frame_edge = Some(dir);
            if let Some(w) = &ws.os_window {
                w.set_cursor(dir.cursor());
            }
        }
        ws.hover_pointer = true;
        ws.native_text_cursor = true;
        let lit = ws.strip_hover.take().is_some()
            | std::mem::take(&mut ws.strip_hover_new_tab)
            | std::mem::take(&mut ws.strip_hover_version)
            | ws.strip_hover_caption.take().is_some();
        if lit && let Some(w) = &ws.os_window {
            w.request_redraw();
        }
        true
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "window pixel coordinates; the f64 mantissa covers every representable window size"
    )]
    fn frame_margins(
        &self,
        wid: WindowId,
        size: aterm_window::dpi::PhysicalSize<u32>,
    ) -> crate::client_frame::FrameMargins {
        let geom = self.pointer_geometry(wid);
        let (ox, oy) = self.frame_origin_with(wid, geom);
        let (cw, ch) = geom.cell;
        let (rows, cols) = self
            .windows
            .get(&wid)
            .map_or((0, 0), |ws| (usize::from(ws.rows), usize::from(ws.cols)));
        let chrome = usize::from(self.chrome_rows(wid));
        let px = |n: usize| n as f64;
        let x0 = ox as f64 + px(geom.pad);
        let y0 = oy as f64 + px(geom.pad_top + geom.head + chrome * ch);
        crate::client_frame::FrameMargins::around_grid(
            f64::from(size.width),
            f64::from(size.height),
            x0,
            y0,
            x0 + px(cols * cw),
            y0 + px(rows * ch),
        )
    }

    /// The pointer left the window.
    pub(crate) fn on_cursor_left(&mut self, wid: WindowId) {
        self.apply_native_hover(wid, None);
        let native_front = self.active_native_view(wid).is_some();
        if native_front && let Some(ws) = self.windows.get_mut(&wid) {
            ws.hover_pointer = false;
            ws.native_text_cursor = false;
        }
        self.retire_link_target(wid);
        self.clear_claude_light_hover(wid);
        self.scrollbar_pointer_left(wid);
        self.stats_pointer_left(wid, crate::metrics::host_now());
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        ws.pointer_position_known = false;
        ws.hover_grid_owned = false;
        ws.native_rehover = false;
        ws.frame_edge = None;
        if ws.strip_hover.is_none()
            && !ws.strip_hover_new_tab
            && !ws.strip_hover_version
            && ws.strip_hover_caption.is_none()
        {
            return;
        }
        ws.strip_hover = None;
        ws.strip_hover_new_tab = false;
        ws.strip_hover_version = false;
        ws.strip_hover_caption = None;
        if let Some(window) = &ws.os_window {
            window.request_redraw();
        }
    }

    fn advance_strip_drag(&mut self, wid: WindowId, geom: PointerGeometry, x: f64, y: f64) {
        let Some(drag) = self.windows.get(&wid).and_then(|ws| ws.strip_drag) else {
            return;
        };
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.palette().is_some())
        {
            return;
        }
        let Some(col) = self.strip_col_at_with(wid, geom, x, y) else {
            return;
        };
        if col == drag.origin_col {
            return;
        }
        let mut stepped = false;
        for _ in 0..self
            .windows
            .get(&wid)
            .map_or(0, |ws| ws.tab_set.shown_len())
        {
            let Some((from, to)) = self.strip_drag_step(wid, drag.tab, col) else {
                break;
            };
            self.move_tab(wid, from, to);
            stepped = true;
        }
        if !stepped {
            return;
        }
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.strip_drag = Some(StripDrag {
                origin_col: col,
                ..drag
            });
            if let Some(window) = &ws.os_window {
                window.request_redraw();
            }
        }
    }

    fn strip_drag_step(
        &self,
        wid: WindowId,
        tab: crate::tab_model::TabId,
        col: u16,
    ) -> Option<(usize, usize)> {
        let ws = self.windows.get(&wid)?;
        let from = ws.tab_set.tabs().iter().position(|t| t.id == tab)?;
        let segment = |index: usize| {
            ws.tab_segments
                .iter()
                .find(|seg| seg.kind == crate::tab_bar::TabHit::Select(index))
        };
        let mid = |seg: &crate::tab_bar::TabSegment| {
            seg.start_col + seg.end_col.saturating_sub(seg.start_col) / 2
        };
        if let Some(next) = segment(from + 1)
            && col >= mid(next)
        {
            return Some((from, from + 1));
        }
        if let Some(prev) = from.checked_sub(1).and_then(segment)
            && col < mid(prev)
        {
            return Some((from, from - 1));
        }
        let held = segment(from)?;
        let tabs = ws.tab_set.shown_len();
        if from + 1 < tabs && segment(from + 1).is_none() && col >= held.end_col {
            return Some((from, from + 1));
        }
        if from > 0 && segment(from - 1).is_none() && col < held.start_col {
            return Some((from, from - 1));
        }
        None
    }

    /// [`Self::find_bar_click_at`] by cell alone: the well maps cell to character.
    #[cfg(test)]
    pub(crate) fn find_bar_click(&mut self, wid: WindowId, row: u16, col: u16) -> bool {
        self.find_bar_click_at(wid, row, col, None)
    }

    /// A left click on the Cmd-F find panel.
    pub(crate) fn find_bar_click_at(
        &mut self,
        wid: WindowId,
        row: u16,
        col: u16,
        x: Option<f64>,
    ) -> bool {
        let Some(hit) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.find_bar_hit.clone())
        else {
            return false;
        };
        let (row, col) = (row as usize, col as usize);
        if !hit.band.contains(&row) || !hit.cols.contains(&col) {
            return false;
        }
        if row != hit.row {
            return true; // the panel's pad / hints rows: inert chrome, but consumed
        }
        let on =
            |span: &Option<std::ops::Range<usize>>| span.as_ref().is_some_and(|r| r.contains(&col));
        if on(&hit.case_cols) {
            self.search_toggle_case_in(wid);
        } else if on(&hit.regex_cols) {
            self.search_toggle_regex_in(wid);
        } else if on(&hit.prev_cols) || on(&hit.next_cols) {
            self.search_repeat_in(wid, on(&hit.next_cols));
        } else if on(&hit.close_cols) {
            self.search_cancel_in(wid);
        } else if let Some(slot) = x.and_then(|x| self.find_box_slot(wid, &hit, x)) {
            self.search_click_caret_in(wid, slot);
        } else if hit.field_cols.contains(&col) {
            self.search_click_caret_in(wid, hit.field_scroll + (col - hit.field_cols.start));
        }
        true
    }

    fn find_box_slot(&self, wid: WindowId, hit: &crate::FindBarHit, x: f64) -> Option<usize> {
        let ws = self.windows.get(&wid)?;
        let query = ws.search.as_ref()?.query.as_str();
        let (cell_w, cell_h) = self.win_cell_size(wid);
        let (ox, _) = self.frame_origin(wid);
        let col0 = hit.cols.start;
        let panel_x = x - ox as f64 - (self.win_pad(wid) + col0 * cell_w) as f64;
        let field =
            hit.field_cols.start.saturating_sub(col0)..hit.field_cols.end.saturating_sub(col0);
        let geom = crate::find_bar::FluentGeom {
            cell_w,
            cell_h,
            scale: ws.scale as f32,
        };
        crate::find_bar::well_click(query, hit.field_scroll, &field, geom, panel_x as f32)
    }

    /// Pixel gate for [`Self::find_bar_click_at`].
    pub(crate) fn find_bar_cell_at(&self, wid: WindowId, x: f64, y: f64) -> Option<(u16, u16)> {
        let hit = self.windows.get(&wid)?.find_bar_hit.as_ref()?;
        let (frame_row, col) = self.laid_cell_at(wid, hit.laid, x, y)?;
        let row = frame_row.checked_sub(usize::from(self.chrome_rows(wid)))?;
        if !hit.band.contains(&row) || !hit.cols.contains(&col) {
            return None;
        }
        Some((u16::try_from(row).ok()?, u16::try_from(col).ok()?))
    }

    /// Whether window pixel `(x, y)` is on the find bar ([`Self::find_bar_cell_at`]).
    #[cfg(test)]
    pub(crate) fn find_bar_pixel_hit(&self, wid: WindowId, x: f64, y: f64) -> bool {
        self.find_bar_cell_at(wid, x, y).is_some()
    }

    /// Resolve the OS cursor for the pointer's current location.
    pub(crate) fn update_hover_cursor(&mut self, wid: WindowId) {
        self.update_hover_cursor_from_plan(wid, None);
    }

    pub(crate) fn update_hover_cursor_from_plan(
        &mut self,
        wid: WindowId,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) {
        let parked = self.park_link_target(wid);
        self.update_hover_cursor_with(
            wid,
            self.pointer_geometry(wid),
            parked,
            None,
            None,
            frame_plan,
        );
        self.settle_link_target(wid, parked);
    }

    /// Re-resolve the hover when the frame under a still pointer has changed.
    pub(crate) fn refresh_hover_for_frame_locked(&mut self, wid: WindowId, t: &Terminal) {
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        if !ws.pointer_position_known || !ws.hover_grid_owned {
            return;
        }
        let Some(session) = ws.front_terminal().map(|front| front.session) else {
            return;
        };
        if ws
            .hover_memo
            .as_ref()
            .is_some_and(|memo| memo.stands_for(ws, session))
        {
            return;
        }
        let parked = self.park_link_target(wid);
        self.update_hover_cursor_with(wid, self.pointer_geometry(wid), parked, Some(t), None, None);
    }

    fn park_link_target(&mut self, wid: WindowId) -> Option<crate::link_target::LinkHover> {
        self.windows
            .get_mut(&wid)
            .and_then(|ws| ws.link_hover.take())
    }

    fn settle_link_target(&mut self, wid: WindowId, parked: Option<crate::link_target::LinkHover>) {
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        if ws.link_hover == parked {
            return;
        }
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
    }

    /// Retire any standing caption, repainting it away.
    pub(crate) fn retire_link_target(&mut self, wid: WindowId) {
        let parked = self.park_link_target(wid);
        self.settle_link_target(wid, parked);
    }

    fn update_hover_cursor_with(
        &mut self,
        wid: WindowId,
        geom: PointerGeometry,
        parked: Option<crate::link_target::LinkHover>,
        probe: Option<&Terminal>,
        precomputed: Option<(bool, (u16, u16, u16, u16))>,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) {
        let resolved = match self.resolve_hover_cursor(wid, geom, probe, precomputed, frame_plan) {
            HoverProbe::Resolved(hover) => hover,
            HoverProbe::Deferred => parked,
        };
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.link_hover = resolved;
        }
    }

    fn resolve_hover_cursor(
        &mut self,
        wid: WindowId,
        geom: PointerGeometry,
        probe: Option<&Terminal>,
        precomputed: Option<(bool, (u16, u16, u16, u16))>,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> HoverProbe {
        use HoverProbe::Resolved;
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.overlay.is_some())
        {
            return Resolved(None);
        }
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.tab_menu.is_some())
        {
            return Resolved(None);
        }
        if self.autopilot_menu.as_ref().is_some_and(|m| m.wid == wid) {
            return Resolved(None);
        }
        if self.active_native_view(wid).is_some() {
            return Resolved(None);
        }
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.divider_drag.is_some())
        {
            return Resolved(None);
        }
        let Some((px, py)) = self.windows.get(&wid).map(|ws| ws.last_cursor_px) else {
            return Resolved(None);
        };
        if self.stats_lens_at(wid, px, py) {
            self.set_hover_cursor(wid, CursorIcon::Default, false, false);
            return Resolved(None);
        }
        if self.strip_col_at_with(wid, geom, px, py).is_some() {
            self.set_hover_cursor(wid, CursorIcon::Default, false, false);
            return Resolved(None);
        }
        match crate::claude_lights::OverChips::of_glass(self.claude_glass_at(wid, px, py).as_ref())
        {
            crate::claude_lights::OverChips::Chip => {
                self.set_hover_cursor(wid, CursorIcon::Pointer, true, false);
                return Resolved(None);
            }
            crate::claude_lights::OverChips::Gap => {
                self.set_hover_cursor(wid, CursorIcon::Default, false, false);
                return Resolved(None);
            }
            crate::claude_lights::OverChips::No => {}
        }
        if let Some((session, url)) = self.claude_glass_hit_at(wid, px, py) {
            let mods = self.windows.get(&wid).map(|ws| ws.mods).unwrap_or_default();
            let held = link_modifier_held(mods);
            let tracking = self
                .pool
                .get(session)
                .is_some_and(|owner| owner.ctx.modes.mouse_tracking_enabled());
            // The hand where a press opens the link (`press_claude_glass_hit`): with the link
            // modifier, or with no modifier in the focused pane.
            if url.is_some() && (held || (mods.is_empty() && self.session_is_front(wid, session))) {
                self.set_hover_cursor(wid, CursorIcon::Pointer, true, false);
            } else if tracking {
                self.set_hover_cursor(wid, CursorIcon::Default, false, false);
            } else {
                self.set_hover_cursor(wid, CursorIcon::Text, false, true);
            }
            return Resolved(None);
        }
        let over_pane_header = match precomputed {
            Some((over_header, _)) => over_header,
            None => self
                .pane_header_at_with_plan(wid, geom, px, py, frame_plan)
                .is_some(),
        };
        if over_pane_header {
            self.set_hover_cursor(wid, CursorIcon::Pointer, true, false);
            return Resolved(None);
        }
        if let Some(icon) = self.divider_cursor_under_pointer_with_plan(wid, frame_plan) {
            self.set_hover_cursor(wid, icon, true, true);
            return Resolved(None);
        }
        let mod_held = self
            .windows
            .get(&wid)
            .is_some_and(|ws| link_modifier_held(ws.mods));
        let (row, col) = self
            .windows
            .get(&wid)
            .map_or((0, 0), |ws| ws.last_mouse_cell);
        let window_cell = self
            .windows
            .get(&wid)
            .map_or((0, 0), |ws| ws.last_mouse_window_cell);
        let (window_row, window_col) = window_cell;
        let focused_rect = precomputed
            .map(|(_, rect)| rect)
            .or_else(|| frame_plan.map(|plan| self.focused_pane_rect_from_plan(wid, plan)));
        let cell_is_under_pointer = self.pointer_is_inside_focused_pane_with(wid, focused_rect)
            && !self.chrome_owns_terminal_cell(
                wid,
                usize::from(window_row),
                usize::from(window_col),
            )
            && self.band_banner_at(wid, px, py).is_none()
            && self.find_bar_cell_at(wid, px, py).is_none()
            && !self.glass_swallows_at(wid, px, py);
        if self.rewound_front(wid).is_some() {
            self.set_hover_cursor(wid, CursorIcon::Text, false, true);
            return Resolved(None);
        }
        let Some((term, session)) = self
            .front_terminal(wid)
            .map(|terminal| (terminal.term.clone(), terminal.session))
        else {
            self.set_hover_cursor(wid, CursorIcon::Text, false, true);
            return Resolved(None);
        };
        let tracking = self
            .pool
            .get(session)
            .is_some_and(|owner| owner.ctx.modes.mouse_tracking_enabled());
        let Some(ws) = self.windows.get_mut(&wid) else {
            return Resolved(None);
        };
        let frame = HoverMemo::frame_of(ws);
        let min_contrast = self.render_knobs.minimum_contrast;
        let answered = ws.hover_memo.as_ref().is_some_and(|memo| {
            memo.answers(session, window_cell, mod_held, cell_is_under_pointer, frame)
        });
        if !answered {
            let (disclosed, plain_url, citation) = match probe {
                Some(t) => {
                    probe_hover_cell(t, row, col, cell_is_under_pointer, mod_held, min_contrast)
                }
                None => {
                    let Some(t) = crate::term_try_lock(&term) else {
                        return HoverProbe::Deferred;
                    };
                    probe_hover_cell(&t, row, col, cell_is_under_pointer, mod_held, min_contrast)
                }
            };
            let named = citation.as_ref().and_then(|_| self.front_naming(session));
            let looked = citation.map(|probe| self.resolve_citation(session, probe));
            let settled = !matches!(looked, Some(None));
            let target = looked.flatten().flatten();
            let path_link = target.is_some();
            let disclosed = match &target {
                Some(target) => Some(target.shown.as_str().into()),
                None => disclosed,
            };
            let citation = target;
            let Some(ws) = self.windows.get_mut(&wid) else {
                return Resolved(None);
            };
            ws.hover_memo = Some(HoverMemo {
                session,
                window_cell,
                cell: (row, col),
                mod_held,
                under_pointer: cell_is_under_pointer,
                frame,
                disclosed,
                plain_url: plain_url || path_link,
                path_link,
                citation,
                settled,
                named,
            });
        }
        let Some(ws) = self.windows.get_mut(&wid) else {
            return Resolved(None);
        };
        let (disclosed, plain_url) = ws.hover_memo.as_ref().map_or((false, false), |memo| {
            (memo.disclosed.is_some(), memo.plain_url)
        });
        if mod_held && (disclosed || plain_url) {
            self.set_hover_cursor(wid, CursorIcon::Pointer, true, false);
        } else if tracking {
            self.set_hover_cursor(wid, CursorIcon::Default, false, false);
        } else {
            self.set_hover_cursor(wid, CursorIcon::Text, false, true);
        }
        Resolved(disclosed.then_some(crate::link_target::LinkHover {
            cell: (row, col),
            window_row,
            session,
        }))
    }

    fn set_hover_cursor(&mut self, wid: WindowId, icon: CursorIcon, pointer: bool, text: bool) {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let divider = pointer && text;
        if !divider && ws.hover_pointer == pointer && ws.native_text_cursor == text {
            return;
        }
        ws.hover_pointer = pointer;
        ws.native_text_cursor = text;
        if let Some(w) = &ws.os_window {
            w.set_cursor(icon);
        }
    }

    #[cfg(test)]
    fn divider_cursor_under_pointer(&self, wid: WindowId) -> Option<CursorIcon> {
        self.divider_cursor_under_pointer_with_plan(wid, None)
    }

    fn divider_cursor_under_pointer_with_plan(
        &self,
        wid: WindowId,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> Option<CursorIcon> {
        if !self.active_tab_has_multiple_visible_panes(wid) {
            return None;
        }
        let ws = self.windows.get(&wid)?;
        let (px, py) = ws.last_cursor_px;
        if self.band_banner_at(wid, px, py).is_some()
            || self.find_bar_cell_at(wid, px, py).is_some()
            || self.stats_lens_at(wid, px, py)
        {
            return None;
        }
        let (wr, wc) = ws.last_mouse_window_cell;
        if let Some(tree) = self.active_tree(wid) {
            if tree.len() == 1 {
                return None;
            }
            let hit = tree.divider_at(wr, wc, ws.rows, ws.cols)?;
            return Some(match hit.dir {
                pane::SplitDir::Vertical => CursorIcon::ColResize,
                pane::SplitDir::Horizontal => CursorIcon::RowResize,
            });
        }
        let owned_plan;
        let plan = match frame_plan {
            Some(plan) => plan,
            None => {
                owned_plan = self.active_visible_leaf_plan(wid)?;
                &owned_plan
            }
        };
        let divider = plan.divider_at(crate::tab_model::LogicalPoint {
            x: f32::from(wc),
            y: f32::from(wr),
        })?;
        Some(match divider.axis {
            crate::tab_model::SplitAxis::Horizontal => CursorIcon::ColResize,
            crate::tab_model::SplitAxis::Vertical => CursorIcon::RowResize,
        })
    }

    /// A Markdown workspace owns its inner pane focus and divider; neither is
    /// a tab split, and changing either leaves document and edit-ticket ownership intact.
    fn markdown_workspace_pointer(
        &mut self,
        wid: WindowId,
        x: f64,
        y: f64,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
        press: bool,
    ) -> bool {
        use crate::native_app::{AppViewState, MarkdownPane, NativeApp};
        let Some((instance, view)) = self.active_native_view(wid) else {
            return false;
        };
        if !x.is_finite() || !y.is_finite() {
            return false;
        }
        let Ok(viewport) = self.native_ui_viewport_for(wid, view) else {
            return false;
        };
        let (Some(NativeApp::Editor(editor)), Some(AppViewState::Editor(state))) = (
            self.native_runtime.app(instance),
            self.native_runtime.view_state(view),
        ) else {
            return false;
        };
        if state.markdown.is_none() {
            return false;
        }
        let layout = editor.workspace_layout(state, viewport);
        let dragging = state
            .common
            .pressed
            .as_ref()
            .is_some_and(|key| key.as_str() == "editor/markdown/divider");
        // A drag stays owned by its original leaf outside that leaf's bounds.
        // Retain its presented geometry and let resize_per_mille clamp either edge.
        let target = if !press && dragging {
            let artifact = match frame_plan {
                Some(plan) => self.retained_native_pointer_artifact_from_plan(wid, view, plan),
                None => self.retained_native_pointer_artifact(wid, view),
            };
            artifact.map(|artifact| {
                let local_x = ((x - artifact.device_x as f64) / artifact.scale) as f32;
                let local_y = ((y - artifact.device_y as f64) / artifact.scale) as f32;
                (artifact, local_x, local_y)
            })
        } else {
            self.retained_native_leaf_at_pointer_from_plan(wid, x, y, frame_plan)
        };
        let Some((artifact, x, y)) = target else {
            return false;
        };
        if artifact.view != view
            || artifact.instance != instance
            || self.native_runtime.view_generation(view) != Some(artifact.generation)
        {
            return false;
        }
        if !press && dragging {
            if layout.divider.is_some() {
                let ratio = layout.resize_per_mille(x, y);
                let _ = self.dispatch_native_event(
                    wid,
                    crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
                        id: crate::native_ui::ActionId::new("editor/markdown/resize"),
                        value: Some(crate::native_app::SemanticInput::Number(f64::from(ratio))),
                    }),
                );
                self.request_redraw_all_windows();
            }
            return true;
        }
        if !press {
            if layout.divider.is_some_and(|rect| rect.contains(x, y)) {
                self.set_hover_cursor(
                    wid,
                    if layout.stacked {
                        CursorIcon::RowResize
                    } else {
                        CursorIcon::ColResize
                    },
                    false,
                    false,
                );
                return true;
            }
            return false;
        }
        let divider = layout.divider.is_some_and(|rect| rect.contains(x, y));
        let focus = if layout.editor.is_some_and(|rect| rect.contains(x, y)) {
            Some(MarkdownPane::Editor)
        } else if layout.reader.is_some_and(|rect| rect.contains(x, y)) {
            Some(MarkdownPane::Reader)
        } else {
            None
        };
        let focus_changed = focus.is_some_and(|focus| {
            state
                .markdown
                .as_ref()
                .is_some_and(|workspace| workspace.focus != focus)
        });
        if focus_changed
            && focus == Some(MarkdownPane::Reader)
            && (!state.preedit.is_empty()
                || state
                    .buffer
                    .as_ref()
                    .is_some_and(|buffer| buffer.minibuffer_active()))
        {
            // The source field still owns this composition or minibuffer. Consume the
            // press too, so a reader control cannot take its focus later in this event.
            return true;
        }
        let divider_changed = divider && !dragging;
        if (focus_changed || divider_changed)
            && let Some(AppViewState::Editor(state)) = self.native_runtime.view_state_mut(view)
        {
            if focus_changed
                && let (Some(workspace), Some(focus)) = (state.markdown.as_mut(), focus)
            {
                workspace.focus = focus;
                state.common.last_focus = (focus == MarkdownPane::Editor)
                    .then(|| crate::native_ui::UiKey::new("editor/buffer"));
                state.common.retire_input();
            }
            if divider_changed {
                state.common.pressed =
                    Some(crate::native_ui::UiKey::new("editor/markdown/divider"));
            }
        }
        if focus_changed || divider_changed {
            self.invalidate_native_pointer_style(wid, view);
            self.request_redraw_all_windows();
        }
        divider
    }

    fn native_editor_pointer_target(
        &self,
        wid: WindowId,
        x: f64,
        y: f64,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> Option<(crate::tab_model::ViewId, Option<usize>, usize)> {
        let (instance, active_view) = self.active_native_view(wid)?;
        if self.native_runtime.app(instance)?.kind() != crate::native_app::AppKind::Editor {
            return None;
        }
        let (artifact, x, y) =
            self.retained_native_leaf_at_pointer_from_plan(wid, x, y, frame_plan)?;
        if artifact.view != active_view
            || artifact.instance != instance
            || self.native_runtime.view_generation(artifact.view) != Some(artifact.generation)
        {
            return None;
        }
        let node = artifact.compiled.paint.iter().rev().find(|node| {
            node.rect.contains(x, y)
                && matches!(&node.content, crate::native_ui::UiContent::TextViewport(_))
        })?;
        let crate::native_ui::UiContent::TextViewport(spec) = &node.content else {
            return None;
        };
        // A grid places its cursor by cell ([`Self::native_editor_grid_target`]), not by byte.
        if spec.grid.is_some() {
            return None;
        }
        let geometry = spec.geometry(node.rect);
        if x < geometry.text_x
            || x >= node.rect.right()
            || y < geometry.body_y
            || y >= geometry.body_y + geometry.body_h
        {
            return None;
        }
        let visible_lines = (geometry.body_h / geometry.line_h).ceil().max(1.0) as usize;
        let byte = crate::native_ui::text_viewport_byte_at(spec, node.rect, x, y);
        Some((artifact.view, byte, visible_lines))
    }

    /// What the pointer is over in the active Editor view, when that view draws a grid: its view,
    /// and the cell or `◂`/`▸` mark there.
    fn native_editor_grid_target(
        &self,
        wid: WindowId,
        x: f64,
        y: f64,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> Option<(crate::tab_model::ViewId, crate::native_ui::GridHit)> {
        let (instance, active_view) = self.active_native_view(wid)?;
        if self.native_runtime.app(instance)?.kind() != crate::native_app::AppKind::Editor {
            return None;
        }
        let (artifact, x, y) =
            self.retained_native_leaf_at_pointer_from_plan(wid, x, y, frame_plan)?;
        if artifact.view != active_view
            || artifact.instance != instance
            || self.native_runtime.view_generation(artifact.view) != Some(artifact.generation)
        {
            return None;
        }
        let node = artifact.compiled.paint.iter().rev().find(|node| {
            node.rect.contains(x, y)
                && matches!(&node.content, crate::native_ui::UiContent::TextViewport(_))
        })?;
        let crate::native_ui::UiContent::TextViewport(spec) = &node.content else {
            return None;
        };
        let hit = crate::native_ui::editor_grid_hit(spec, node.rect, x, y)?;
        Some((artifact.view, hit))
    }

    /// Scroll the grid of Editor view `view` `columns` whole columns sideways (a sideways wheel
    /// or swipe, a press on a `◂`/`▸` mark); whether its grid took it.
    fn native_editor_grid_scroll(
        &mut self,
        wid: WindowId,
        view: crate::tab_model::ViewId,
        columns: i32,
    ) -> bool {
        let Some(crate::tab_model::View::Native(native)) = self.view_store.get(view).copied()
        else {
            return false;
        };
        let handled = self
            .dispatch_editor_event(
                native.instance,
                view,
                &crate::native_app::AppEvent::EditorInput(amacs::input::EditorInput::GridScroll {
                    columns,
                }),
            )
            .is_ok_and(|result| result.is_some());
        if handled {
            self.invalidate_native_ui_cache(wid);
            self.request_redraw_all_windows();
        }
        handled
    }

    /// A sideways wheel or swipe over native view `view`, when it is an Editor drawing a grid:
    /// a wheel's notch is a column, and a swipe's cells bank toward one
    /// ([`GRID_SWIPE_CELLS`]). Whether the grid took it.
    fn native_editor_grid_sideways(
        &mut self,
        wid: WindowId,
        view: crate::tab_model::ViewId,
        right: bool,
        notches: i32,
        precise: bool,
    ) -> bool {
        let Some(crate::native_app::AppViewState::Editor(state)) =
            self.native_runtime.view_state_mut(view)
        else {
            return false;
        };
        if !state
            .buffer
            .as_ref()
            .is_some_and(|buffer| buffer.grid().is_some())
        {
            return false;
        }
        let signed = if right { notches } else { -notches };
        let columns = if precise {
            // A swipe that turns back starts its bank afresh.
            if state.grid_swipe.signum() * signed.signum() < 0 {
                state.grid_swipe = 0;
            }
            state.grid_swipe = state.grid_swipe.saturating_add(signed);
            let columns = state.grid_swipe / GRID_SWIPE_CELLS;
            state.grid_swipe %= GRID_SWIPE_CELLS;
            columns
        } else {
            signed
        };
        if columns != 0 {
            let _ = self.native_editor_grid_scroll(wid, view, columns);
        }
        true
    }

    /// A press at `at` on cell `record`, `column` of grid view `expected_view`: a click places
    /// the cursor there, and a second click on the same cell within a double click's interval
    /// opens its editor.
    fn native_editor_grid_press(
        &mut self,
        wid: WindowId,
        expected_view: crate::tab_model::ViewId,
        (record, column): (usize, usize),
        at: std::time::Instant,
    ) -> bool {
        let Some((instance, view)) = self.active_native_view(wid) else {
            return false;
        };
        if view != expected_view {
            return false;
        }
        let Some(crate::native_app::AppViewState::Editor(state)) =
            self.native_runtime.view_state_mut(view)
        else {
            return false;
        };
        let open = state
            .grid_press
            .is_some_and(|(then, pressed_record, pressed_column)| {
                (pressed_record, pressed_column) == (record, column)
                    && at.saturating_duration_since(then).as_millis() <= crate::multi_click_ms()
            });
        state.grid_press = (!open).then_some((at, record, column));
        let handled = self
            .dispatch_editor_event(
                instance,
                view,
                &crate::native_app::AppEvent::EditorInput(amacs::input::EditorInput::GridPointer {
                    record,
                    column,
                    open,
                }),
            )
            .is_ok_and(|result| result.is_some());
        if handled {
            self.invalidate_native_ui_cache(wid);
            self.request_redraw_all_windows();
        }
        handled
    }

    /// A press on the cell panel of grid view `expected_view` at byte `byte` of the value it
    /// shows: the caret goes there, the cell's editor opening first when it was closed.
    fn native_editor_grid_panel_press(
        &mut self,
        wid: WindowId,
        expected_view: crate::tab_model::ViewId,
        byte: usize,
    ) -> bool {
        let Some((instance, view)) = self.active_native_view(wid) else {
            return false;
        };
        if view != expected_view {
            return false;
        }
        if let Some(crate::native_app::AppViewState::Editor(state)) =
            self.native_runtime.view_state_mut(view)
        {
            state.grid_press = None;
        }
        let handled = self
            .dispatch_editor_event(
                instance,
                view,
                &crate::native_app::AppEvent::EditorInput(
                    amacs::input::EditorInput::GridPanelPointer { byte },
                ),
            )
            .is_ok_and(|result| result.is_some());
        if handled {
            self.invalidate_native_ui_cache(wid);
            self.request_redraw_all_windows();
        }
        handled
    }

    fn native_editor_pointer_select(
        &mut self,
        wid: WindowId,
        expected_view: crate::tab_model::ViewId,
        byte: usize,
        extend: bool,
        visible_lines: usize,
    ) -> bool {
        let Some((instance, view)) = self.active_native_view(wid) else {
            return false;
        };
        if view != expected_view {
            return false;
        }
        if self.native_runtime.document_id(instance).is_none() {
            return false;
        }
        let Some(crate::native_app::AppViewState::Editor(state)) =
            self.native_runtime.view_state_mut(view)
        else {
            return false;
        };
        let Some(buffer) = state.buffer.as_mut() else {
            return false;
        };
        let changed = self
            .editor_workspace
            .handle_input(
                &mut self.document_store,
                buffer,
                amacs::input::EditorInput::Pointer {
                    byte,
                    extend,
                    visible_lines,
                },
            )
            .is_ok_and(|reduction| reduction.follow != amacs::input::Follow::Ignored);
        if changed {
            state.common.presentation_revision =
                state.common.presentation_revision.saturating_add(1);
            self.invalidate_native_ui_cache(wid);
            self.request_redraw_all_windows();
        }
        changed
    }

    pub(crate) fn on_cursor_moved(&mut self, wid: WindowId, x: f64, y: f64) {
        let parked = self.park_link_target(wid);
        self.route_cursor_moved(wid, x, y, parked);
        self.settle_link_target(wid, parked);
    }

    fn route_cursor_moved(
        &mut self,
        wid: WindowId,
        x: f64,
        y: f64,
        parked: Option<crate::link_target::LinkHover>,
    ) {
        // A drag owns a user gesture, including one consumed by chrome below; plain hover
        // owns no terminal input and keeps the move licence. Real mouse reports retire the
        // licence at the input seam, whose byte receipt tells DEC1003 motion from
        // DEC1000/1002's byte-silent motion.
        if self.windows.get(&wid).is_some_and(|ws| {
            ws.selecting
                || ws.held_mouse_button.is_some()
                || ws.divider_drag.is_some()
                || ws.strip_drag.is_some()
        }) {
            self.fence_pointer_license(wid);
        }
        if self.active_native_view(wid).is_none()
            && self.windows.get(&wid).is_some_and(|ws| {
                ws.pointer_position_known
                    && ws.hover_grid_owned
                    && (x, y) == ws.last_cursor_px
                    && ws.overlay.is_none()
                    && ws.tab_menu.is_none()
                    && ws.held_mouse_button.is_none()
                    && !ws.selecting
                    && ws.divider_drag.is_none()
            })
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.hover_mapping == self.hover_mapping(wid))
        {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.link_hover = parked;
            }
            return;
        }
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_cursor_px = (x, y);
            ws.pointer_position_known = true;
            ws.hover_grid_owned = false;
            ws.native_rehover = false;
        }
        if self.route_frame_motion(wid, x, y) {
            return;
        }
        let geom = self.pointer_geometry(wid);
        self.track_strip_hover(wid, geom, x, y);
        let over_claude_chips = self.track_claude_light_hover(wid, x, y);
        if self.palette_claims_pointer(wid) {
            self.scrollbar_pointer_left(wid);
            self.palette_pointer_motion(wid, x, y);
            return;
        }
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.tab_menu.is_some())
        {
            self.scrollbar_pointer_left(wid);
            self.refresh_mouse_cell(wid, geom, x, y);
            self.track_tab_menu_hover(wid, x, y);
            return;
        }
        if self.autopilot_menu.as_ref().is_some_and(|m| m.wid == wid) {
            self.scrollbar_pointer_left(wid);
            self.refresh_mouse_cell(wid, geom, x, y);
            self.autopilot_menu_motion(wid, x, y);
            return;
        }
        if self.overview_claims_pointer(wid) {
            self.scrollbar_pointer_left(wid);
            self.overview_pointer_motion(wid, x, y);
            return;
        }
        if !self.windows.get(&wid).is_some_and(|ws| {
            ws.selecting
                || ws.held_mouse_button.is_some()
                || ws.divider_drag.is_some()
                || ws.pane_overlay.drag.is_some()
        }) && self.stats_pointer_motion(wid, crate::metrics::host_now())
        {
            self.scrollbar_pointer_left(wid);
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.native_rest = None;
                if ws.hover_pointer || ws.native_text_cursor {
                    ws.hover_pointer = false;
                    ws.native_text_cursor = false;
                    if let Some(w) = &ws.os_window {
                        w.set_cursor(CursorIcon::Default);
                    }
                }
            }
            self.apply_native_hover(wid, None);
            return;
        }
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.divider_drag.is_some())
        {
            self.refresh_mouse_cell(wid, geom, x, y);
            let (gx, gy) = self.pointer_grid_px(wid, geom, x, y);
            let (raw_r, raw_c) = self
                .windows
                .get(&wid)
                .map_or((0, 0), |ws| ws.last_mouse_window_cell);
            let px_off = crate::input::PixelOffset {
                x: divider_drag_off_axis(gx, raw_c, geom.cell.0),
                y: divider_drag_off_axis(gy, raw_r, geom.cell.1),
            };
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.last_mouse_px_off = px_off;
            }
            self.drag_divider(wid);
            return;
        }
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.pane_overlay.drag.is_some())
            && self.scrollbar_pointer_motion(wid, x, y)
        {
            self.set_hover_cursor(wid, CursorIcon::Default, false, false);
            return;
        }
        let motion_plan = (self.active_tab_has_multiple_visible_panes(wid)
            || self.active_native_view(wid).is_some())
        .then(|| self.active_visible_leaf_plan(wid))
        .flatten();
        if self.markdown_workspace_pointer(wid, x, y, motion_plan.as_ref(), false) {
            return;
        }
        let over_pane_header = self
            .pane_header_at_with_plan(wid, geom, x, y, motion_plan.as_ref())
            .is_some();
        if let Some((_, view)) = self.active_native_view(wid) {
            if self.active_tab_has_multiple_visible_panes(wid) {
                self.refresh_mouse_cell_with_plan(wid, geom, x, y, motion_plan.as_ref());
            }
            let dragging_editor = self
                .native_runtime
                .view_state(view)
                .and_then(|state| state.common().pressed.as_ref())
                .is_some_and(|key| key.as_str() == "editor/buffer");
            let editor_target = self.native_editor_pointer_target(wid, x, y, motion_plan.as_ref());
            if dragging_editor && let Some((target_view, Some(byte), visible_lines)) = editor_target
            {
                let _ =
                    self.native_editor_pointer_select(wid, target_view, byte, true, visible_lines);
            }
            let over_strip = self.strip_col_at_with(wid, geom, x, y).is_some();
            let over_banner = self.band_banner_at(wid, x, y).is_some()
                || self.find_bar_cell_at(wid, x, y).is_some();
            let over_chrome = over_pane_header || over_strip || over_banner;
            let native_hover = self.native_hover_hit(wid, x, y, over_chrome, motion_plan.as_ref());
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.native_rest = NativePointerRest::after_motion(
                    ws.native_rest.take(),
                    (x, y),
                    native_hover
                        .as_ref()
                        .map(|(view, _, key, _, asks)| (*view, key, *asks)),
                );
            }
            let text_field_hovered = native_hover
                .as_ref()
                .is_some_and(|(_, _, _, is_text, _)| *is_text);
            self.apply_native_hover(
                wid,
                native_hover
                    .as_ref()
                    .map(|(view, _, key, ..)| (*view, key.clone())),
            );
            if !dragging_editor
                && !over_strip
                && !self
                    .windows
                    .get(&wid)
                    .is_some_and(|ws| ws.selecting || ws.held_mouse_button.is_some())
                && self.scrollbar_pointer_motion(wid, x, y)
            {
                self.set_hover_cursor(wid, CursorIcon::Default, false, false);
                return;
            }
            if !over_chrome
                && !dragging_editor
                && let Some(icon) =
                    self.divider_cursor_under_pointer_with_plan(wid, motion_plan.as_ref())
            {
                self.set_hover_cursor(wid, icon, true, true);
            } else {
                let text_cursor = !over_chrome && (editor_target.is_some() || text_field_hovered);
                let pointer = over_pane_header
                    || (!text_field_hovered
                        && native_hover
                            .as_ref()
                            .is_some_and(|(_, _, key, ..)| key.as_str() != "editor/buffer"));
                let icon = if text_cursor {
                    CursorIcon::Text
                } else if pointer {
                    CursorIcon::Pointer
                } else {
                    CursorIcon::Default
                };
                self.set_hover_cursor(wid, icon, pointer, text_cursor);
            }
            if !self.pointer_routes_to_a_terminal_pane_with_plan(wid, motion_plan.as_ref()) {
                return;
            }
        }
        if self.strip_col_at_with(wid, geom, x, y).is_some() {
            if let Some(ws) = self.windows.get_mut(&wid)
                && (ws.hover_pointer || ws.native_text_cursor)
            {
                ws.hover_pointer = false;
                ws.native_text_cursor = false;
                if let Some(w) = &ws.os_window {
                    w.set_cursor(CursorIcon::Default);
                }
            }
            if !self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
                return;
            }
        }
        let mapped_cell = self.refresh_mouse_cell_with_plan(wid, geom, x, y, motion_plan.as_ref());
        let (lr, lc) = mapped_cell.pane_cell;
        if self.active_native_view(wid).is_none()
            && !self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.selecting || ws.held_mouse_button.is_some())
            && self.scrollbar_pointer_motion(wid, x, y)
        {
            self.set_hover_cursor(wid, CursorIcon::Default, false, false);
            return;
        }
        if over_pane_header
            && !self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.selecting || ws.held_mouse_button.is_some())
        {
            self.set_hover_cursor(wid, CursorIcon::Pointer, true, false);
            return;
        }
        match over_claude_chips {
            crate::claude_lights::OverChips::Chip => {
                self.set_hover_cursor(wid, CursorIcon::Pointer, true, false);
                return;
            }
            crate::claude_lights::OverChips::Gap => {
                self.set_hover_cursor(wid, CursorIcon::Default, false, false);
                return;
            }
            crate::claude_lights::OverChips::No => {}
        }
        if let Some(ws) = self.windows.get_mut(&wid)
            && pet_wake_wanted(
                ws.fx.companion_mut().hit_rect().is_some(),
                ws.fx.companion_mut().needs_frames(),
                ws.pet_pointer_sampled_px,
                (x, y),
                geom.cell,
            )
            && let Some(w) = ws.os_window.as_ref()
        {
            w.request_redraw();
        }
        let mapping = self.hover_mapping_with(wid, geom, mapped_cell.pane_rect);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.hover_grid_owned = true;
            ws.hover_mapping = mapping;
        }
        self.update_hover_cursor_with(
            wid,
            geom,
            parked,
            None,
            Some((over_pane_header, mapped_cell.pane_rect)),
            motion_plan.as_ref(),
        );
        let cw = geom.cell.0.max(1);
        let px_off = self.pointer_subcell_px(wid, geom, x, y);
        let side = if usize::from(px_off.x) * 2 >= cw {
            SelectionSide::Right
        } else {
            SelectionSide::Left
        };
        let mods = self.mouse_modifiers(wid);
        let buttons = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.held_mouse_button)
            .map_or(3u8, |button| button.code());
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_mouse_px_off = px_off;
        }
        self.selection_autoscroll_with(wid, geom, y, mapped_cell.pane_rect);
        let route =
            self.pointer_route_with_plan(wid, Some(mapped_cell.pane_rect), motion_plan.as_ref());
        let (session, (row, col)) = match route {
            PointerRoute::Front => (
                self.front_terminal(wid).map(|front| front.session),
                (lr, lc),
            ),
            PointerRoute::Pane { session, cell } => (Some(session), cell),
            PointerRoute::Nowhere => (None, (lr, lc)),
        };
        let tracking = session
            .and_then(|session| self.pool.get(session))
            .is_some_and(|owner| owner.ctx.modes.mouse_tracking_enabled());
        if let (Some(session), Some(ws)) = (session, self.windows.get_mut(&wid)) {
            note_owner_cell(ws, session, (row, col), px_off);
        }
        if !tracking && !self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.last_mouse_side = side;
            }
            return;
        }
        self.send_pointer(
            wid,
            route,
            InputEvent::MouseMove {
                buttons,
                row,
                col,
                mods,
                side,
                px_off,
            },
        );
    }

    /// Mid-drag: grow the selection to the hovered viewport cell.
    pub(crate) fn drag_selection(&mut self, wid: WindowId, row: u16, col: u16) {
        let Some((term, _)) = self.front_shown(wid) else {
            return;
        };
        let controller = self.person_gesture_refused();
        let Some(fws) = self.windows.get_mut(&wid) else {
            return;
        };
        // A control client's move steers the selection the button's release would copy.
        fws.sel_by_controller |= controller;
        let (sel_row, col) = {
            let mut term = term_lock(&term);
            let (col, doubled_side) = pointer_char(&term, row, col);
            let side = doubled_side.unwrap_or(fws.last_mouse_side);
            let selection_before = term.text_selection().has_selection();
            let sel_row = i32::from(row) - term.grid().display_offset() as i32;
            let (col, side) = pointer_anchor(&term, row, col, side);
            let origin_cell = match term.text_selection().state() {
                aterm_core::selection::SelectionState::None => None,
                _ => {
                    let start = term.text_selection().start();
                    Some((start.row, start.col))
                }
            };
            match fws.gesture {
                None => {
                    term.text_selection_mut()
                        .update_selection(sel_row, col, side);
                }
                Some(g) if g.kind == SelectionType::Lines => {
                    let (g_first, g_last) = match origin_cell {
                        Some((r, _)) => term.logical_line_span(r),
                        None => (g.row, g.end_row),
                    };
                    let (m_first, m_last) = term.logical_line_span(sel_row);
                    let max_col = term.cols().saturating_sub(1);
                    let sel = term.text_selection_mut();
                    if sel_row < g_first {
                        sel.start_selection(
                            g_last,
                            max_col,
                            SelectionSide::Right,
                            SelectionType::Lines,
                        );
                        sel.update_selection(m_first, 0, SelectionSide::Left);
                    } else {
                        sel.start_selection(g_first, 0, SelectionSide::Left, SelectionType::Lines);
                        sel.update_selection(m_last, max_col, SelectionSide::Right);
                    }
                }
                Some(g) => {
                    let (g_start, g_end) = match origin_cell {
                        Some((r, c)) => control::word_span(&term, r, c).unwrap_or(((r, c), (r, c))),
                        None => ((g.row, g.start_col), (g.end_row, g.end_col)),
                    };
                    let (ws, we) = control::word_span(&term, sel_row, col)
                        .unwrap_or(((sel_row, col), (sel_row, col)));
                    let sel = term.text_selection_mut();
                    if (sel_row, col) < g_start {
                        sel.start_selection(
                            g_end.0,
                            g_end.1,
                            SelectionSide::Right,
                            SelectionType::Semantic,
                        );
                        sel.update_selection(ws.0, ws.1, SelectionSide::Left);
                    } else {
                        sel.start_selection(
                            g_start.0,
                            g_start.1,
                            SelectionSide::Left,
                            SelectionType::Semantic,
                        );
                        sel.update_selection(we.0, we.1, SelectionSide::Right);
                    }
                }
            }
            if !selection_before && term.text_selection().has_selection() {
                note_selection_custody(&mut term, true);
            }
            (sel_row, col)
        };
        if (sel_row, col) != fws.sel_press_cell {
            fws.sel_dragged = true;
        }
        if let Some(w) = &fws.os_window {
            w.request_redraw();
        }
    }

    /// While a left-drag selection is in flight.
    pub(crate) fn selection_autoscroll(&mut self, wid: WindowId, y: f64) -> bool {
        let geom = self.pointer_geometry(wid);
        let pane_rect = self.focused_pane_rect(wid);
        self.selection_autoscroll_with(wid, geom, y, pane_rect)
    }

    fn selection_autoscroll_with(
        &mut self,
        wid: WindowId,
        geom: PointerGeometry,
        y: f64,
        pane_rect: (u16, u16, u16, u16),
    ) -> bool {
        let selecting = match self.windows.get(&wid) {
            Some(ws) => ws.selecting,
            None => return false,
        };
        if !selecting {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.next_autoscroll = None;
            }
            return false;
        }
        let ch = geom.cell.1.max(1);
        let (pane_row, _, pane_rows, _) = pane_rect;
        let pane_top = geom
            .pad_top
            .saturating_add(geom.head)
            .saturating_add(usize::from(pane_row).saturating_mul(ch));
        let strip_px = usize::from(self.chrome_rows(wid)) * ch;
        let y = self.window_to_frame_y_signed(wid, geom, y);
        let lines =
            crate::app_render::selection_autoscroll_lines(y, pane_top, strip_px, ch, pane_rows);
        if lines == 0 {
            // Pointer is back inside the grid → stop auto-scrolling.
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.next_autoscroll = None;
            }
            return false;
        }
        let moved = if let Some(session) = self.rewound_front(wid) {
            self.rewind_scroll(session, lines)
        } else {
            let term = match self.front_terminal(wid) {
                Some(terminal) => terminal.term.clone(),
                None => {
                    // A tab close/switch can retire the front terminal between
                    // repeat ticks. Drop the old level-triggered deadline here or
                    // the event loop keeps re-observing a time in the past.
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        ws.next_autoscroll = None;
                    }
                    return false;
                }
            };
            let mut term = term_lock(&term);
            let before = term.grid().display_offset();
            term.scroll_display(lines);
            term.grid().display_offset() != before
        };
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.next_autoscroll = Some(Instant::now() + crate::SELECTION_AUTOSCROLL_INTERVAL);
        }
        if moved && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.clone()) {
            w.request_redraw();
        }
        moved
    }

    /// Repeat-timer tick (`new_events`).
    pub(crate) fn tick_selection_autoscroll(&mut self, wid: WindowId) {
        let Some((last_y, (lr, lc))) = self
            .windows
            .get(&wid)
            .map(|ws| (ws.last_cursor_px.1, ws.last_mouse_cell))
        else {
            return;
        };
        if self.selection_autoscroll(wid, last_y) {
            self.drag_selection(wid, lr, lc);
            if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.clone()) {
                w.request_redraw();
            }
        }
    }

    /// A shift-click pressed at `at`, which starts the grid's click streak there.
    pub(crate) fn shift_extend_press(&mut self, wid: WindowId, at: Instant) {
        let Some((row, col)) = self.windows.get(&wid).map(|ws| ws.last_mouse_cell) else {
            return;
        };
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return;
        };
        let (sel_row, col, doubled_side) = {
            let term = term_lock(&term);
            let sel_row = i32::from(row) - term.grid().display_offset() as i32;
            let (col, doubled_side) = pointer_char(&term, row, col);
            (sel_row, col, doubled_side)
        };
        let side = doubled_side.unwrap_or_else(|| {
            self.windows
                .get(&wid)
                .map_or(SelectionSide::Left, |ws| ws.last_mouse_side)
        });
        self.extend_selection_to(wid, sel_row, col, side);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_press = Some((at, (sel_row, col)));
            ws.click_count = 1;
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
        }
    }

    /// Advance the `multi_click_ms` streak by a press at `at` and return the resulting click count (1, 2, 3; a fourth rapid click wraps to 1).
    pub(crate) fn advance_click_streak(&mut self, wid: WindowId, at: Instant) -> u8 {
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return 1;
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return 1;
        };
        let (row, col) = ws.last_mouse_cell;
        let (sel_row, col) = {
            let term = term_lock(&term);
            let sel_row = i32::from(row) - term.grid().display_offset() as i32;
            (sel_row, pointer_char(&term, row, col).0)
        };
        ws.click_count = match ws.last_press {
            Some((t, cell))
                if cell == (sel_row, col)
                    && at.saturating_duration_since(t).as_millis() <= multi_click_ms() =>
            {
                ws.click_count % 3 + 1
            }
            _ => 1,
        };
        ws.last_press = Some((at, (sel_row, col)));
        ws.click_count
    }

    /// Shift-click.
    pub(crate) fn extend_selection_to(
        &mut self,
        wid: WindowId,
        sel_row: i32,
        col: u16,
        side: SelectionSide,
    ) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let Some(terminal) = ws.front_terminal() else {
            return false;
        };
        let mut term = term_lock(&terminal.term);
        let (col, side) = pointer_anchor_at_sel_row(&term, sel_row, col, side);
        let sel = term.text_selection_mut();
        if !sel.has_selection() || sel.is_empty() {
            return false;
        }
        sel.extend_selection(sel_row, col, side);
        sel.complete_selection();
        true
    }

    /// Double-click.
    pub(crate) fn select_word_click(&mut self, wid: WindowId, sel_row: i32, col: u16) {
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return;
        };
        let (start, end, col) = {
            let mut term = term_lock(&term);
            let (col, _) = pointer_anchor_at_sel_row(&term, sel_row, col, SelectionSide::Left);
            let (start, end) = control::select_word(&mut term, sel_row, col);
            note_selection_custody(&mut term, true);
            (start, end, col)
        };
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.gesture = Some(GestureOrigin {
                row: start.0,
                end_row: end.0,
                start_col: start.1,
                end_col: end.1,
                kind: SelectionType::Semantic,
            });
        }
        self.arm_gesture_drag(wid, sel_row, col);
    }

    /// Triple-click.
    pub(crate) fn select_line_click(&mut self, wid: WindowId, sel_row: i32, col: u16) {
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return;
        };
        let (first_row, last_row, end_col, col) = {
            let mut term = term_lock(&term);
            let (first_row, last_row) = control::select_line(&mut term, sel_row);
            note_selection_custody(&mut term, true);
            let (col, _) = pointer_anchor_at_sel_row(&term, sel_row, col, SelectionSide::Left);
            (first_row, last_row, term.cols().saturating_sub(1), col)
        };
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.gesture = Some(GestureOrigin {
                row: first_row,
                end_row: last_row,
                start_col: 0,
                end_col,
                kind: SelectionType::Lines,
            });
        }
        self.arm_gesture_drag(wid, sel_row, col);
    }

    /// Keep a completed double/triple-click selection draggable while the button stays down.
    pub(crate) fn arm_gesture_drag(&mut self, wid: WindowId, sel_row: i32, col: u16) {
        let pane = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.staged_drag_pane.take())
            .unwrap_or_else(|| self.focused_pane_rect(wid));
        let controller = self.person_gesture_refused();
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        ws.selecting = true;
        ws.sel_by_controller = controller;
        ws.drag_pane = pane;
        ws.sel_dragged = true;
        ws.sel_press_cell = (sel_row, col);
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
    }

    /// Single press with mouse tracking off.
    pub(crate) fn begin_selection(&mut self, wid: WindowId, kind: SelectionType) {
        let Some((term, _)) = self.front_shown(wid) else {
            return;
        };
        let pane = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.staged_drag_pane.take())
            .unwrap_or_else(|| self.focused_pane_rect(wid));
        let controller = self.person_gesture_refused();
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let (row, col) = ws.last_mouse_cell;
        let (sel_row, col) = {
            let mut term = term_lock(&term);
            let sel_row = i32::from(row) - term.grid().display_offset() as i32;
            let (col, side) = pointer_cell(&term, row, col);
            term.text_selection_mut()
                .start_selection(sel_row, col, side, kind);
            note_selection_custody(&mut term, true);
            (sel_row, col)
        };
        ws.selecting = true;
        ws.sel_by_controller = controller;
        ws.drag_pane = pane;
        ws.sel_dragged = false;
        ws.sel_press_cell = (sel_row, col);
        ws.gesture = None;
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
    }

    /// End the text-selection gesture in flight: a drag or a multi-click completes its selection,
    /// a bare click clears it. With `copy_on_select` on, a completed selection is copied unless
    /// `suppress_copy_on_select`, and a control client's gesture (one it started or dragged,
    /// `sel_by_controller`, or its release) copies nothing ([`Self::copy_selection_in`] refuses
    /// it under the client's mark) and sets no Linux PRIMARY. Whether the copy was due.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelectionCustody",
            action = "SelectLow",
            project = "aterm_gui::selection_custody_conformance::project_selection_custody"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelectionCustody",
            action = "SelectOldest",
            project = "aterm_gui::selection_custody_conformance::project_selection_custody"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelectionCustody",
            action = "SelectHigh",
            project = "aterm_gui::selection_custody_conformance::project_selection_custody"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelectionCustody",
            action = "UserClear",
            project = "aterm_gui::selection_custody_conformance::project_selection_custody"
        )
    )]
    pub(crate) fn finish_selection(
        &mut self,
        wid: WindowId,
        suppress_copy_on_select: bool,
    ) -> bool {
        let Some((term, _)) = self.front_shown(wid) else {
            return false;
        };
        let released_by_controller = self.person_gesture_refused();
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        // A selection a control client started, dragged or let go of is the client's: its end
        // copies nothing on select, even at the person's release.
        let by_controller = std::mem::take(&mut ws.sel_by_controller) || released_by_controller;
        let completed = ws.sel_dragged;
        {
            let mut term = term_lock(&term);
            let sel = term.text_selection_mut();
            if completed {
                sel.complete_selection();
            } else {
                sel.clear();
            }
            note_selection_custody(&mut term, completed);
        }
        ws.selecting = false;
        ws.gesture = None;
        ws.next_autoscroll = None;
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
        let fired = completed && self.copy_on_select && !suppress_copy_on_select;
        if fired {
            // Under the client's mark, `copy_selection_in` refuses it and logs why.
            self.with_controller_mark(by_controller, |app| app.copy_selection_in(wid));
        }
        #[cfg(target_os = "linux")]
        if completed
            && !suppress_copy_on_select
            && !by_controller
            && let Some(text) = term_lock(&term).selection_to_string()
            && !text.is_empty()
        {
            crate::control::primary_set(&text);
        }
        fired
    }

    /// The URL a press at the pointer would open, if any.
    #[cfg(test)]
    pub(crate) fn link_under_pointer(&self, wid: WindowId) -> Option<String> {
        self.sourced_link_under_pointer(wid).map(|(url, _)| url)
    }

    #[cfg(test)]
    fn sourced_link_under_pointer(&self, wid: WindowId) -> Option<(String, bool)> {
        self.sourced_link_under_pointer_with(wid, None)
    }

    fn sourced_link_under_pointer_with(
        &self,
        wid: WindowId,
        focused_rect: Option<(u16, u16, u16, u16)>,
    ) -> Option<(String, bool)> {
        if self.rewound_front(wid).is_some() {
            return None;
        }
        let ws = self.windows.get(&wid)?;
        let (row, col) = ws.last_mouse_cell;
        let (window_row, window_col) = (
            usize::from(ws.last_mouse_window_cell.0),
            usize::from(ws.last_mouse_window_cell.1),
        );
        if self.chrome_owns_terminal_cell(wid, window_row, window_col) {
            return None;
        }
        let (px, py) = ws.last_cursor_px;
        if self.chrome_overlay_at(wid, px, py)
            || crate::claude_lights::OverChips::of_glass(self.claude_glass_at(wid, px, py).as_ref())
                != crate::claude_lights::OverChips::No
        {
            return None;
        }
        if let Some((_, url)) = self.claude_glass_hit_at(wid, px, py) {
            return url.map(|url| (url.to_owned(), false));
        }
        if !self.pointer_is_inside_focused_pane_with(wid, focused_rect)
            || self.glass_swallows_at(wid, px, py)
        {
            return None;
        }
        let term = term_lock(&ws.front_terminal()?.term);
        let col = pointer_cell(&term, row, col).0;
        term.hyperlink_at_visible(row, col)
            .map(|url| (url.to_string(), true))
            .or_else(|| {
                let cells = term.render_row(row as usize);
                plain_url_at(&cells, col as usize, self.render_knobs.minimum_contrast)
                    .map(|plain| (plain.to_url_string(), false))
            })
    }

    /// The file citation a press at the pointer would open (`crate::path_link`).
    #[cfg(test)]
    pub(crate) fn path_link_under_pointer(
        &self,
        wid: WindowId,
    ) -> Option<crate::path_link::PathTarget> {
        self.path_link_under_pointer_with(wid, None)
    }

    fn path_link_under_pointer_with(
        &self,
        wid: WindowId,
        focused_rect: Option<(u16, u16, u16, u16)>,
    ) -> Option<crate::path_link::PathTarget> {
        if self.rewound_front(wid).is_some() {
            return None;
        }
        let ws = self.windows.get(&wid)?;
        let (row, col) = ws.last_mouse_cell;
        let (window_row, window_col) = (
            usize::from(ws.last_mouse_window_cell.0),
            usize::from(ws.last_mouse_window_cell.1),
        );
        if self.chrome_owns_terminal_cell(wid, window_row, window_col) {
            return None;
        }
        let (px, py) = ws.last_cursor_px;
        if self.chrome_overlay_at(wid, px, py)
            || crate::claude_lights::OverChips::of_glass(self.claude_glass_at(wid, px, py).as_ref())
                != crate::claude_lights::OverChips::No
            || self.claude_glass_hit_at(wid, px, py).is_some()
            || !self.pointer_is_inside_focused_pane_with(wid, focused_rect)
            || self.glass_swallows_at(wid, px, py)
        {
            return None;
        }
        let front = ws.front_terminal()?;
        let session = front.session;
        let probe = {
            let term = term_lock(&front.term);
            let col = pointer_cell(&term, row, col).0;
            let cells = term.render_row(row as usize);
            crate::plain_path_at(&cells, col as usize).map(|span| CitationProbe::new(&term, span))
        }?;
        self.resolve_citation(session, probe).flatten()
    }

    fn front_naming(&self, session: u64) -> Option<(i32, Option<String>)> {
        let pooled = self.pool.get(session)?;
        let timeline = pooled
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let agent = timeline.agent();
        Some((agent.program_pgid, agent.program.clone()))
    }

    /// Re-ask every standing hover over `session` whose citation was answered under another naming of the group in front ([`HoverMemo::renamed`]).
    pub(crate) fn reask_renamed_hovers(&mut self, session: u64) {
        let Some(named) = self.front_naming(session) else {
            return;
        };
        let stale: Vec<WindowId> =
            self.windows
                .iter()
                .filter(|(_, ws)| {
                    ws.pointer_position_known
                        && ws.hover_grid_owned
                        && ws.hover_memo.as_ref().is_some_and(|memo| {
                            memo.renamed(session, (named.0, named.1.as_deref()))
                        })
                })
                .map(|(wid, _)| *wid)
                .collect();
        for wid in stale {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.hover_memo = None;
            }
            self.update_hover_cursor(wid);
        }
    }

    /// Resolve a citation read off `session`'s grid against that session's own directories, in order.
    pub(crate) fn resolve_citation(
        &self,
        session: u64,
        probe: CitationProbe,
    ) -> Option<Option<crate::path_link::PathTarget>> {
        let Some(pooled) = self.pool.get(session) else {
            return Some(None);
        };
        let front = if pooled.master >= 0 {
            (self.session_factory.fg_probe)(pooled.master)
        } else {
            -1
        };
        let (named, witness) = {
            let timeline = pooled
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let agent = timeline.agent();
            let named = (front > 0 && agent.program_pgid == front).then(|| {
                (
                    agent.program.clone(),
                    crate::harness_host::identified_agent(agent),
                )
            });
            (named, timeline.cwd_witness())
        };
        let here = if front > 0 {
            named.is_some_and(|(program, agent)| {
                crate::path_link::runs_here(
                    &crate::path_link::Front {
                        program: program.as_deref(),
                        agent,
                        root: front == pooled.pid,
                        running: probe.executing || probe.running.is_some(),
                    },
                    || (self.session_factory.lone_leader)(front),
                )
            })
        } else {
            cfg!(windows) && probe.shell_reads_local
        };
        if !here
            || probe
                .running
                .as_deref()
                .is_some_and(crate::path_link::is_remote_shell)
        {
            return Some(None);
        }
        let stands = crate::path_link::report_stands(probe.osc7_seq, witness, front);
        if stands
            && probe
                .osc7_host
                .as_deref()
                .is_some_and(|host| !crate::tab_label::names_this_machine(host))
        {
            return Some(None);
        }
        let agent_cwd = u32::try_from(front)
            .ok()
            .filter(|p| *p > 0)
            .and_then(aterm_sysprobe::process_cwd);
        let bases: Vec<std::path::PathBuf> = agent_cwd
            .into_iter()
            .chain(probe.osc7.filter(|_| stands))
            .collect();
        let home = aterm_types::dirs::home_dir();
        self.citation_looks
            .target(&probe.span, &bases, home.as_deref())
            .map(Result::ok)
    }

    /// Cmd-click.
    #[cfg(test)]
    pub(crate) fn open_link_under_pointer(&mut self, wid: WindowId) -> bool {
        self.open_link_under_pointer_with_plan(wid, None)
    }

    fn open_link_under_pointer_with_plan(
        &mut self,
        wid: WindowId,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> bool {
        let focused_rect = Some(self.focused_pane_rect_with_plan(wid, frame_plan));
        let sourced = self.sourced_link_under_pointer_with(wid, focused_rect);
        let pressed = sourced.as_ref().map(|(url, osc8)| {
            if *osc8 {
                PressedUrl::Osc8(url)
            } else {
                PressedUrl::Plain(url)
            }
        });
        let url = sourced.as_ref().map(|(url, _)| url.clone());
        let path = if url.is_none() {
            self.path_link_under_pointer_with(wid, focused_rect)
        } else {
            None
        };
        let shown = self.windows.get(&wid).and_then(|ws| {
            let front = ws.front_terminal()?.session;
            ws.shown_caption
                .as_ref()
                .filter(|shown| shown.session == front && shown.cell == ws.last_mouse_cell)
                .map(|shown| shown.names.clone())
        });
        if !caption_admits(shown.as_ref(), pressed, path.as_ref()) {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.hover_memo = None;
                if let Some(w) = &ws.os_window {
                    w.request_redraw();
                }
            }
            self.update_hover_cursor_from_plan(wid, frame_plan);
            return false;
        }
        match crate::path_link::decide(url, path) {
            crate::path_link::LinkAction::External(url) => {
                debug_assert!(is_safe_url(&url));
                open_url_external(&url);
                true
            }
            crate::path_link::LinkAction::Document(target) => {
                self.open_citation(wid, &target);
                true
            }
            crate::path_link::LinkAction::Nothing => false,
        }
    }

    /// Open a resolved citation in `wid`.
    pub(crate) fn open_citation(&mut self, wid: WindowId, target: &crate::path_link::PathTarget) {
        let opened = crate::native_document_host::path_to_file_uri(&target.path)
            .map_err(|error| error.to_string())
            .and_then(|uri| {
                self.open_document_tab_at(
                    wid,
                    crate::native_app::AppKind::Editor,
                    &uri,
                    target.line,
                    target.col,
                )
            });
        if let Err(error) = opened {
            crate::logging::stderr_line!(
                "aterm-gui: could not open {} in the editor: {error}",
                target.shown
            );
        }
    }

    /// `MouseInput` -> when no app is tracking the mouse.
    #[cfg(test)]
    pub(crate) fn on_mouse_input(
        &mut self,
        wid: WindowId,
        state: ElementState,
        button: WinitMouseButton,
    ) {
        self.on_mouse_input_at(wid, state, button, Instant::now());
    }

    /// [`Self::on_mouse_input`] with the button gone down or up at `at`.
    #[cfg(any(test, all(unix, feature = "bench-support", debug_assertions)))]
    pub(crate) fn on_mouse_input_at(
        &mut self,
        wid: WindowId,
        state: ElementState,
        button: WinitMouseButton,
        at: Instant,
    ) {
        let _ = self.mouse_input_target(wid, state, button, at);
    }

    /// Route first, then report the native body a press reached. Chrome and
    /// focus-only navigation do not become page input when they change focus.
    ///
    /// The button went down or up at `at`. Every run of presses (a still pointer's
    /// [`NativePointerRest`], the grid's click streak, a tab chip or the bare band, a menu's
    /// spent card, the harness switch and chip, a banner's link) measures a double click's
    /// interval between its presses by `at`, never by how long the work between them took.
    fn mouse_input_target(
        &mut self,
        wid: WindowId,
        state: ElementState,
        button: WinitMouseButton,
        at: Instant,
    ) -> Option<crate::tab_model::ViewId> {
        let target = self.route_mouse_input(wid, state, button, at);
        if state == ElementState::Released
            && let Some(button) = winit_mouse_button(button)
        {
            self.end_unpaired_release(wid, button);
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.page_buttons &= !(1 << button.slot());
            }
        }
        target
    }

    fn end_unpaired_release(&mut self, wid: WindowId, button: aterm_types::mouse::MouseButton) {
        let Some(owner) = self.windows.get(&wid).and_then(|ws| {
            (ws.reported_buttons & (1 << button.slot()) != 0)
                .then(|| ws.button_owners[usize::from(button.slot())])
        }) else {
            return;
        };
        if let Some(owner) = owner {
            self.write_owner_release(button, owner);
        }
        if let Some(ws) = self.windows.get_mut(&wid) {
            forget_reported_button(ws, button);
        }
    }

    fn write_owner_release(&self, button: aterm_types::mouse::MouseButton, owner: ButtonOwner) {
        let Some(session) = self.pool.get(owner.session) else {
            return;
        };
        crate::app_input::write_ui_pointer_report(
            &session.term,
            &session.ctx.modes,
            &session.ctx.sink,
            &InputEvent::MouseButton {
                button,
                pressed: false,
                row: owner.cell.0,
                col: owner.cell.1,
                mods: 0,
                click_count: 1,
                side: SelectionSide::Left,
                block: false,
                suppress_copy_on_select: false,
                px_off: owner.px_off,
            },
            &session.ctx.ui_waiting,
        );
    }

    /// Resolve a native pointer without moving keyboard focus. Captured gestures
    /// keep their owner; free gestures use the current visible layout, even
    /// when an earlier gesture has invalidated its retained paint.
    fn native_pointer_view(
        &self,
        wid: WindowId,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> Option<crate::tab_model::ViewId> {
        let focused = || self.active_native_view(wid).map(|(_, view)| view);
        if let Some(route) = self.pointer_capture_route_with_plan(wid, frame_plan) {
            return match route {
                PointerRoute::Front => focused(),
                PointerRoute::Pane { .. } | PointerRoute::Nowhere => None,
            };
        }
        self.native_view_under_pointer(wid, frame_plan)
    }

    /// The free body hit, used when a new press overrides an older hold.
    fn native_view_under_pointer(
        &self,
        wid: WindowId,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> Option<crate::tab_model::ViewId> {
        if !self.active_tab_contains_native(wid) {
            return None;
        }
        let window = self.windows.get(&wid)?;
        let (x, y) = window.last_cursor_px;
        if !window.pointer_position_known || !x.is_finite() || !y.is_finite() {
            return self.active_native_view(wid).map(|(_, view)| view);
        }
        if self.band_banner_at(wid, x, y).is_some() || self.find_bar_cell_at(wid, x, y).is_some() {
            return None;
        }
        let owned_plan;
        let plan = match frame_plan {
            Some(plan) => plan,
            None => {
                owned_plan = self.active_visible_leaf_plan(wid)?;
                &owned_plan
            }
        };
        let geom = self.pointer_geometry(wid);
        let (cw, ch) = geom.cell;
        if cw == 0 || ch == 0 {
            return None;
        }
        let (ox, oy) = self.frame_origin_with(wid, geom);
        // Keep outside pixels signed: clamping a title row or padding into
        // the first grid cell would turn a focus-only press into page input.
        let point = crate::tab_model::LogicalPoint {
            x: ((x - ox as f64 - geom.pad as f64) / cw as f64) as f32,
            y: ((y - oy as f64 - (geom.pad_top + geom.head) as f64) / ch as f64
                - f64::from(self.chrome_rows(wid))) as f32,
        };
        let leaf = plan.leaf_at(point)?;
        matches!(
            self.view_store.get(leaf.view),
            Some(crate::tab_model::View::Native(_))
        )
        .then_some(leaf.view)
    }

    fn native_scroll_target(
        &self,
        wid: WindowId,
        view: crate::tab_model::ViewId,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> Option<crate::native_ui::UiKey> {
        let ws = self
            .windows
            .get(&wid)
            .filter(|ws| ws.pointer_position_known)?;
        let (x, y) = ws.last_cursor_px;
        let (artifact, local_x, local_y) =
            self.retained_native_leaf_at_pointer_from_plan(wid, x, y, frame_plan)?;
        if artifact.view != view {
            return None;
        }
        artifact
            .compiled
            .semantics
            .iter()
            .filter(|node| {
                !node.offscreen
                    && local_x >= node.rect.x
                    && local_y >= node.rect.y
                    && local_x < node.rect.right()
                    && local_y < node.rect.bottom()
            })
            .min_by(|a, b| {
                (a.rect.width * a.rect.height).total_cmp(&(b.rect.width * b.rect.height))
            })
            .map(|node| node.key.clone())
    }

    fn pointer_routes_to_a_terminal_pane_with_plan(
        &self,
        wid: WindowId,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> bool {
        matches!(
            self.pointer_route_with_plan(wid, None, frame_plan),
            PointerRoute::Pane { .. }
        )
    }

    fn press_lands_on_a_terminal_pane(
        &self,
        wid: WindowId,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> bool {
        matches!(
            self.pane_under_pointer_with_plan(wid, None, frame_plan),
            PointerRoute::Pane { .. }
        )
    }

    fn route_mouse_input(
        &mut self,
        wid: WindowId,
        state: ElementState,
        button: WinitMouseButton,
        at: Instant,
    ) -> Option<crate::tab_model::ViewId> {
        self.fence_pointer_license(wid);
        let pressed = state == ElementState::Pressed;
        if button == WinitMouseButton::Left
            && let Some(ws) = self.windows.get_mut(&wid)
        {
            #[cfg(target_os = "linux")]
            if pressed && let Some(edge) = ws.frame_edge {
                if let Some(w) = &ws.os_window {
                    let _ = w.drag_resize_window(edge.direction());
                }
                return None;
            }
            if !pressed {
                ws.caption_move = None;
            }
        }
        if pressed {
            self.clear_claude_light_selection(wid);
        }
        if pressed {
            crate::input_hint::arm();
        }
        if !pressed
            && button == WinitMouseButton::Left
            && let Some(ws) = self.windows.get_mut(&wid)
        {
            ws.strip_drag = None;
        }
        if !pressed && button == WinitMouseButton::Left && self.scrollbar_release(wid) {
            return None;
        }
        if self.palette_claims_pointer(wid) {
            let (x, y) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |window| window.last_cursor_px);
            if button == WinitMouseButton::Left {
                if pressed {
                    self.palette_pointer_press(wid, x, y);
                } else {
                    self.palette_pointer_release(wid, x, y);
                }
            }
            if !pressed {
                self.settle_pointer_drags(wid);
            }
            return None;
        }
        if self.autopilot_menu.as_ref().is_some_and(|m| m.wid == wid) {
            if pressed {
                let (px, py) = self
                    .windows
                    .get(&wid)
                    .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
                self.autopilot_menu_press(wid, button == WinitMouseButton::Left, px, py, at);
            } else {
                self.settle_pointer_drags(wid);
            }
            return None;
        }
        if pressed && self.spends_menu_follow_up(wid, at) {
            return None;
        }
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.tab_menu.is_some())
        {
            if pressed {
                let (px, py) = self
                    .windows
                    .get(&wid)
                    .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
                if button == WinitMouseButton::Left
                    && let Some(i) = self.tab_menu_action_at(wid, px, py)
                {
                    self.activate_tab_menu_entry(wid, i);
                } else if !self.tab_menu_contains(wid, px, py) {
                    self.close_tab_menu(wid);
                }
            } else {
                self.settle_pointer_drags(wid);
            }
            return None;
        }
        if self.overview_claims_pointer(wid) {
            let (x, y) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |window| window.last_cursor_px);
            if button == WinitMouseButton::Left {
                if pressed {
                    self.overview_pointer_press(wid, x, y);
                } else {
                    self.overview_pointer_release(wid, x, y);
                }
            }
            if !pressed {
                self.settle_pointer_drags(wid);
            }
            return None;
        }
        let mods_state = self.windows.get(&wid).map(|ws| ws.mods)?;
        let input_plan = (self.active_tab_has_multiple_visible_panes(wid)
            || self.active_tab_contains_native(wid))
        .then(|| self.active_visible_leaf_plan(wid))
        .flatten();
        if self.windows.get(&wid).is_some_and(|ws| !ws.overlay_open())
            && self.pointer_is_over_pane_header_with_plan(wid, input_plan.as_ref())
        {
            let captured = self.windows.get(&wid).is_some_and(|ws| {
                (button == WinitMouseButton::Left && (ws.selecting || ws.divider_drag.is_some()))
                    || winit_mouse_button(button)
                        .is_some_and(|button| ws.reported_buttons & (1 << button.slot()) != 0)
            }) || (button == WinitMouseButton::Left
                && self
                    .active_native_view(wid)
                    .and_then(|(_, view)| self.native_runtime.view_state(view))
                    .is_some_and(|state| state.common().pressed.is_some()));
            if pressed || !captured {
                if pressed && !captured {
                    self.close_stats_lens(wid);
                    if self.inline_rename_edit(wid).is_some() {
                        self.clear_strip_press(wid);
                        self.settle_rename_edit(wid);
                    }
                }
                if pressed && button == WinitMouseButton::Left && !captured {
                    let (x, y) = self.windows[&wid].last_cursor_px;
                    self.refresh_mouse_cell_with_plan(
                        wid,
                        self.pointer_geometry(wid),
                        x,
                        y,
                        input_plan.as_ref(),
                    );
                    self.focus_pane_under_pointer_with_plan(wid, input_plan.as_ref());
                }
                return None;
            }
        }
        if self.windows.get(&wid).is_some_and(|ws| !ws.overlay_open()) {
            if pressed {
                let left = button == WinitMouseButton::Left;
                let taken = if left {
                    self.stats_press(wid)
                } else {
                    self.stats_owns_pointer(wid)
                };
                if taken {
                    if self.inline_rename_edit(wid).is_some() {
                        self.clear_strip_press(wid);
                        self.settle_rename_edit(wid);
                    }
                    return None;
                }
            } else if self.windows.get(&wid).is_some_and(|ws| {
                !ws.selecting && ws.held_mouse_button.is_none() && ws.divider_drag.is_none()
            }) && !(button == WinitMouseButton::Left
                && self
                    .active_native_view(wid)
                    .and_then(|(_, view)| self.native_runtime.view_state(view))
                    .is_some_and(|state| state.common().pressed.is_some()))
                && self.stats_owns_pointer(wid)
            {
                return None;
            }
        }
        let page_press = pressed
            && self
                .windows
                .get(&wid)
                .is_some_and(|window| !window.selecting && window.divider_drag.is_none());
        let native_body_press = page_press
            .then(|| self.native_view_under_pointer(wid, input_plan.as_ref()))
            .flatten();
        let mut native_press = if !page_press {
            None
        } else if let Some(route) = self.pointer_capture_route_with_plan(wid, input_plan.as_ref()) {
            match route {
                PointerRoute::Front => self.active_native_view(wid).map(|(_, view)| view),
                PointerRoute::Pane { .. } | PointerRoute::Nowhere => None,
            }
        } else {
            native_body_press
        };
        let mut canonical_press_routed = false;
        if pressed
            && button == WinitMouseButton::Left
            && let Some(plan) = input_plan.as_ref()
            && self.visible_content_route_from_plan(plan)
                == Some(crate::VisibleContentRoute::Heterogeneous)
        {
            canonical_press_routed = true;
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |window| window.last_cursor_px);
            let on_banner = self.band_banner_at(wid, px, py).is_some()
                || self.find_bar_cell_at(wid, px, py).is_some();
            let in_content = !on_banner && self.strip_col_at(wid, px, py).is_none();
            if in_content && self.press_claude_light(wid, at) {
                return None;
            }
            if in_content && self.scrollbar_press(wid) {
                if self.inline_rename_edit(wid).is_some() {
                    self.clear_strip_press(wid);
                    self.settle_rename_edit(wid);
                }
                return None;
            }
            if in_content && self.begin_divider_drag_with_plan(wid, Some(plan)) {
                if self.inline_rename_edit(wid).is_some() {
                    self.clear_strip_press(wid);
                    self.settle_rename_edit(wid);
                }
                return None;
            }
            if in_content && self.focus_pane_under_pointer_from_plan(wid, plan) {
                if self.inline_rename_edit(wid).is_some() {
                    self.clear_strip_press(wid);
                    self.settle_rename_edit(wid);
                }
                return native_body_press;
            }
        }
        let terminal_beside = self.active_native_view(wid).is_some()
            && if pressed {
                self.press_lands_on_a_terminal_pane(wid, input_plan.as_ref())
            } else {
                winit_mouse_button(button).is_some_and(|button| {
                    self.windows.get(&wid).is_some_and(|ws| {
                        ws.reported_buttons & (1 << button.slot()) != 0
                            && matches!(
                                self.owner_route_with_plan(
                                    wid,
                                    ws.button_owners[usize::from(button.slot())],
                                    input_plan.as_ref()
                                ),
                                PointerRoute::Pane { .. }
                            )
                    })
                })
            };
        if self.active_native_view(wid).is_some() && !terminal_beside {
            // The native handler takes this body press directly, including a
            // new left press while an older terminal button remains held.
            native_press = native_body_press;
            // A press on this page, including a body miss, owns its drag and
            // wheel until release. An ignored press on a native sibling must
            // not grant capture to the focused page.
            if pressed
                && self
                    .active_native_view(wid)
                    .is_some_and(|(_, view)| native_press == Some(view))
                && let Some(button) = winit_mouse_button(button)
                && let Some(ws) = self.windows.get_mut(&wid)
            {
                ws.page_buttons |= 1 << button.slot();
            }
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
            if !pressed && button == WinitMouseButton::Left {
                if self.finish_divider_drag(wid) {
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        ws.held_mouse_button = held_button(ws.reported_buttons);
                    }
                    return native_press;
                }
                let Some((_, view)) = self.active_native_view(wid) else {
                    return native_press;
                };
                let armed = self
                    .native_runtime
                    .view_state(view)
                    .and_then(|state| state.common().pressed.clone());
                let release = (self.band_banner_at(wid, px, py).is_none()
                    && self.find_bar_cell_at(wid, px, py).is_none()
                    && !self.stats_owns_pointer(wid))
                .then(|| {
                    self.retained_native_leaf_at_pointer_from_plan(wid, px, py, input_plan.as_ref())
                })
                .flatten()
                .and_then(|(artifact, x, y)| {
                    if artifact.view != view
                        || self.native_runtime.view_generation(artifact.view)
                            != Some(artifact.generation)
                    {
                        return None;
                    }
                    let hit = artifact.compiled.hit_test(x, y)?.clone();
                    let value = artifact
                        .compiled
                        .text_field_byte_at(&hit.key, x)
                        .map(|byte| crate::native_app::SemanticInput::TextPosition {
                            byte,
                            extend: true,
                        })
                        .or_else(|| {
                            // A click on a slider's number opens it for an exact value.
                            (!artifact.compiled.slider_readout_at(&hit.key, x, y))
                                .then(|| artifact.compiled.slider_value_at(&hit.key, x))
                                .flatten()
                                .map(crate::native_app::SemanticInput::Number)
                        })
                        .or_else(|| {
                            artifact.compiled.color_wheel_pick_at(&hit.key, x, y).map(
                                |(hue, saturation)| crate::native_app::SemanticInput::ColorPick {
                                    hue,
                                    saturation,
                                },
                            )
                        });
                    Some((artifact.view, artifact.generation, hit, value))
                });
                if let Some(state) = self.native_runtime.view_state_mut(view) {
                    state.common_mut().pressed = None;
                }
                self.invalidate_native_pointer_style(wid, view);
                self.request_redraw_all_windows();
                if let (Some(armed), Some((release_view, generation, release, value))) =
                    (armed, release)
                    && armed == release.key
                    && self
                        .active_native_view(wid)
                        .is_some_and(|(_, live)| live == release_view)
                    && self.native_runtime.view_generation(release_view) == Some(generation)
                {
                    // WHAT IS LIT IS WHAT A PRESS HITS: the action may move the page under the
                    // still pointer (a page that replaces it, a row it removes), so its next
                    // frame on glass is hovered where the pointer then is, as a wheel's is.
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        ws.native_rehover = true;
                    }
                    let _ = self.dispatch_native_pointer_action(
                        wid,
                        release_view,
                        release.key,
                        crate::native_app::ActionInvocation {
                            id: release.action,
                            value,
                        },
                    );
                }
                return native_press;
            }
            if pressed && button == WinitMouseButton::Left {
                if let Some(col) = self.strip_col_at(wid, px, py) {
                    self.handle_tab_strip_click(wid, col, at);
                    return None;
                }
                if self.press_banner_at(wid, px, py) {
                    return None;
                }
                if self.inline_rename_edit(wid).is_some() {
                    self.clear_strip_press(wid);
                    self.settle_rename_edit(wid);
                }
                if let Some((row, col)) = self.find_bar_cell_at(wid, px, py)
                    && self.find_bar_click_at(wid, row, col, Some(px))
                {
                    return None;
                }
                let press_plan = input_plan.as_ref();
                if self.markdown_workspace_pointer(wid, px, py, press_plan, true) {
                    return native_press;
                }
                let editor_target = self.native_editor_pointer_target(wid, px, py, press_plan);
                let grid_target = self.native_editor_grid_target(wid, px, py, press_plan);
                let press = self
                    .retained_native_leaf_at_pointer_from_plan(wid, px, py, press_plan)
                    .and_then(|(artifact, x, y)| {
                        let (_, active_view) = self.active_native_view(wid)?;
                        if artifact.view != active_view
                            || self.native_runtime.view_generation(artifact.view)
                                != Some(artifact.generation)
                        {
                            return None;
                        }
                        let hit = artifact.compiled.hit_test(x, y)?;
                        Some((
                            artifact.view,
                            artifact.generation,
                            hit.key.clone(),
                            hit.action.clone(),
                            artifact.compiled.text_field_byte_at(&hit.key, x),
                            hit.look_first,
                        ))
                    });
                let picture = if press.is_none() {
                    self.retained_native_leaf_at_pointer_from_plan(wid, px, py, press_plan)
                        .and_then(|(artifact, x, y)| {
                            let (_, active_view) = self.active_native_view(wid)?;
                            (artifact.view == active_view
                                && self.native_runtime.view_generation(artifact.view)
                                    == Some(artifact.generation))
                            .then(|| artifact.compiled.list_picture_at(x, y).cloned())
                            .flatten()
                            .map(|key| (artifact.view, key))
                        })
                } else {
                    None
                };
                let onto_another = press.as_ref().is_some_and(|(target_view, _, key, ..)| {
                    self.windows.get_mut(&wid).is_some_and(|ws| {
                        NativePointerRest::press(
                            &mut ws.native_rest,
                            (px, py),
                            (*target_view, key),
                            at,
                        )
                    })
                });
                if let Some((target_view, generation, key, _, _, asks)) = &press
                    && (onto_another
                        || !NativePointerRest::looked_at(
                            self.windows
                                .get(&wid)
                                .and_then(|ws| ws.native_rest.as_ref()),
                            (px, py),
                            (*target_view, key, *asks),
                        ))
                {
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        let streak = ws.native_rest.take().and_then(|rest| rest.streak);
                        ws.native_rest = Some(NativePointerRest::on(
                            (px, py),
                            (*target_view, key, *asks),
                            streak,
                        ));
                    }
                    if self.native_runtime.view_generation(*target_view) == Some(*generation)
                        && let Some(state) = self.native_runtime.view_state_mut(*target_view)
                    {
                        let common = state.common_mut();
                        common.hovered = Some(key.clone());
                        common.pressed = None;
                    }
                    self.invalidate_native_pointer_style(wid, *target_view);
                    self.request_redraw_all_windows();
                    return native_press;
                }
                if let Some((target_view, generation, key, action, text_position, _)) = press {
                    if self.native_runtime.view_generation(target_view) == Some(generation)
                        && let Some(state) = self.native_runtime.view_state_mut(target_view)
                    {
                        let common = state.common_mut();
                        common.hovered = Some(key.clone());
                        common.pressed = Some(key.clone());
                        common.last_focus = Some(key.clone());
                        common.focus_visible = false;
                    }
                    self.invalidate_native_pointer_style(wid, target_view);
                    self.request_redraw_all_windows();
                    if let Some(byte) = text_position
                        && self.native_runtime.view_generation(target_view) == Some(generation)
                    {
                        let _ = self.dispatch_native_view_event(
                            wid,
                            target_view,
                            crate::native_app::AppEvent::Action(
                                crate::native_app::ActionInvocation {
                                    id: action,
                                    value: Some(crate::native_app::SemanticInput::TextPosition {
                                        byte,
                                        extend: mods_state.shift_key(),
                                    }),
                                },
                            ),
                        );
                    }
                    if let Some((target_view, Some(byte), visible_lines)) = editor_target {
                        let _ = self.native_editor_pointer_select(
                            wid,
                            target_view,
                            byte,
                            mods_state.shift_key(),
                            visible_lines,
                        );
                    }
                    match grid_target {
                        Some((target_view, crate::native_ui::GridHit::Cell(record, column))) => {
                            let _ = self.native_editor_grid_press(
                                wid,
                                target_view,
                                (record, column),
                                at,
                            );
                        }
                        Some((target_view, crate::native_ui::GridHit::Scroll(columns))) => {
                            let _ = self.native_editor_grid_scroll(wid, target_view, columns);
                        }
                        Some((target_view, crate::native_ui::GridHit::Panel(byte))) => {
                            let _ = self.native_editor_grid_panel_press(wid, target_view, byte);
                        }
                        None => {}
                    }
                } else if let Some((_, view)) = self.active_native_view(wid)
                    && let Some(state) = self.native_runtime.view_state_mut(view)
                {
                    state.common_mut().pressed = None;
                    self.invalidate_native_pointer_style(wid, view);
                    if let Some((target_view, key)) = picture {
                        let _ = self.dispatch_native_view_event(
                            wid,
                            target_view,
                            crate::native_app::AppEvent::PressListPicture(key),
                        );
                        self.request_redraw_all_windows();
                    }
                }
            }
            #[cfg(windows)]
            if button == WinitMouseButton::Right
                && self
                    .windows
                    .get(&wid)
                    .is_some_and(|ws| ws.band_menu_release_pending)
            {
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.band_menu_release_pending = false;
                }
                if !pressed {
                    if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                        let _ = crate::platform_win::popup_system_menu(w);
                    }
                    return native_press;
                }
            }
            if pressed && button != WinitMouseButton::Left {
                if self.inline_rename_edit(wid).is_some() {
                    self.clear_strip_press(wid);
                    self.settle_rename_edit(wid);
                }
                if self.band_banner_at(wid, px, py).is_some()
                    || self.find_bar_cell_at(wid, px, py).is_some()
                {
                    return None;
                }
                if let Some(col) = self.strip_col_at(wid, px, py) {
                    if button == WinitMouseButton::Middle {
                        self.handle_tab_strip_middle_press(wid, col);
                    } else if button == WinitMouseButton::Right {
                        self.handle_tab_strip_right_press(wid, col, px, py);
                    }
                    return None;
                }
            }
            return native_press;
        }
        if pressed && button == WinitMouseButton::Left {
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
            if self.press_banner_at(wid, px, py) {
                return None;
            }
            if !canonical_press_routed && self.press_claude_light(wid, at) {
                return None;
            }
        }
        if pressed && button != WinitMouseButton::Left {
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
            if self.glass_swallows_at(wid, px, py) {
                if self.inline_rename_edit(wid).is_some() {
                    self.clear_strip_press(wid);
                    self.settle_rename_edit(wid);
                }
                return None;
            }
            // Every button on the glass is the glass's (the harness label's included, which
            // opens nothing); the lockup is not glass a pointer hits, so its presses are the pane's.
            if self.claude_glass_at(wid, px, py).is_some() {
                if self.inline_rename_edit(wid).is_some() {
                    self.clear_strip_press(wid);
                    self.settle_rename_edit(wid);
                }
                #[cfg(windows)]
                if button == WinitMouseButton::Right
                    && let Some(ws) = self.windows.get_mut(&wid)
                {
                    ws.band_menu_release_pending = false;
                }
                // A right press opens a fact's menu, and on the permission chip its box, with
                // no click made; on another chip, the switch or the label it opens nothing and
                // is the glass's all the same.
                if button == WinitMouseButton::Right && !self.open_fact_menu_at(wid, px, py) {
                    self.open_permission_box_at(wid, px, py);
                }
                return None;
            }
            if self.band_banner_at(wid, px, py).is_some()
                || self.find_bar_cell_at(wid, px, py).is_some()
            {
                if self.inline_rename_edit(wid).is_some() {
                    self.clear_strip_press(wid);
                    self.settle_rename_edit(wid);
                }
                #[cfg(windows)]
                if button == WinitMouseButton::Right
                    && let Some(ws) = self.windows.get_mut(&wid)
                {
                    ws.band_menu_release_pending = false;
                }
                return None;
            }
            if button != WinitMouseButton::Right
                && let Some(col) = self.strip_col_at(wid, px, py)
            {
                if self.inline_rename_edit(wid).is_some() {
                    self.clear_strip_press(wid);
                    self.settle_rename_edit(wid);
                }
                if button == WinitMouseButton::Middle {
                    self.handle_tab_strip_middle_press(wid, col);
                }
                return None;
            }
        }
        let press_route = if pressed && button != WinitMouseButton::Left {
            self.pointer_route_with_plan(wid, None, input_plan.as_ref())
        } else {
            PointerRoute::Front
        };
        #[cfg(target_os = "linux")]
        if pressed && button == WinitMouseButton::Middle {
            let tracking = self.route_term(wid, press_route).is_some_and(|t| {
                let tl = match self.pool.ui_waiting_of(&t) {
                    Some(waiting) => {
                        crate::term_lock_ui(&t, &waiting, crate::metrics::TermWaitSite::Mouse)
                    }
                    None => term_lock(&t),
                };
                tl.mouse_tracking_enabled()
            });
            if !tracking {
                if self.front_pane_for_local_press(wid, press_route, input_plan.as_ref()) {
                    self.paste_primary_into(wid);
                }
                return native_press;
            }
        }
        #[cfg(windows)]
        if button == WinitMouseButton::Right
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.band_menu_release_pending)
        {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.band_menu_release_pending = false;
            }
            if !pressed {
                if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    let _ = crate::platform_win::popup_system_menu(w);
                }
                return native_press;
            }
        }
        if pressed && button == WinitMouseButton::Right {
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
            let over_strip = self.strip_col_at(wid, px, py).is_some();
            let gesture_on = self.config.right_click_or_default()
                == crate::app_config::RightClickGesture::CopyPaste;
            let rewound = match press_route {
                PointerRoute::Front => self.rewound_front(wid),
                PointerRoute::Pane { session, .. } => {
                    self.pool.is_rewound(session).then_some(session)
                }
                PointerRoute::Nowhere => None,
            };
            let term = match rewound {
                Some(session) => self.pool.shown_term(session),
                None => self.route_term(wid, press_route),
            };
            let ui_waiting = term.as_ref().and_then(|t| self.pool.ui_waiting_of(t));
            let (tracking, selection) = term.as_ref().map_or((false, None), |t| {
                let site = crate::metrics::TermWaitSite::Mouse;
                let tl = match &ui_waiting {
                    Some(waiting) => crate::term_lock_ui(t, waiting, site),
                    None => term_lock(t),
                };
                (
                    tl.mouse_tracking_enabled() && rewound.is_none(),
                    tl.selection_to_string().filter(|s| !s.is_empty()),
                )
            });
            match right_press_plan(gesture_on, over_strip, tracking, selection.is_some()) {
                RightPressPlan::Chrome => {
                    if self.inline_rename_edit(wid).is_some() {
                        self.clear_strip_press(wid);
                        self.settle_rename_edit(wid);
                    }
                    if let Some(col) = self.strip_col_at(wid, px, py) {
                        self.handle_tab_strip_right_press(wid, col, px, py);
                    }
                    return None;
                }
                RightPressPlan::Copy => {
                    if !self.front_pane_for_local_press(wid, press_route, input_plan.as_ref()) {
                        return native_press;
                    }
                    if let (Some(text), Some(t)) = (selection, term.as_ref()) {
                        let _ = control::pbcopy(&text);
                        let site = crate::metrics::TermWaitSite::Mouse;
                        let mut tl = match &ui_waiting {
                            Some(waiting) => crate::term_lock_ui(t, waiting, site),
                            None => term_lock(t),
                        };
                        tl.text_selection_mut().clear();
                        drop(tl);
                        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
                        {
                            w.request_redraw();
                        }
                    }
                    return native_press;
                }
                RightPressPlan::Paste => {
                    if self.front_pane_for_local_press(wid, press_route, input_plan.as_ref()) {
                        self.paste_clipboard_into(wid);
                    }
                    return native_press;
                }
                RightPressPlan::Seam => {}
            }
        }
        if pressed && button == WinitMouseButton::Left {
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
            if let Some(col) = self.strip_col_at(wid, px, py) {
                self.handle_tab_strip_click(wid, col, at);
                return None;
            }
            if self.inline_rename_edit(wid).is_some() {
                self.clear_strip_press(wid);
                self.settle_rename_edit(wid);
            }
        }
        if pressed && button == WinitMouseButton::Left {
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
            if let Some((row, col)) = self.find_bar_cell_at(wid, px, py)
                && self.find_bar_click_at(wid, row, col, Some(px))
            {
                return None;
            }
        }
        if pressed && button == WinitMouseButton::Left && self.pet_press_at(wid, at) {
            return None;
        }
        if pressed
            && button == WinitMouseButton::Left
            && !canonical_press_routed
            && self.scrollbar_press(wid)
        {
            return None;
        }
        if button == WinitMouseButton::Left {
            if pressed {
                if !canonical_press_routed
                    && self.begin_divider_drag_with_plan(wid, input_plan.as_ref())
                {
                    return None;
                }
            } else if self.finish_divider_drag(wid) {
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.held_mouse_button = held_button(ws.reported_buttons);
                }
                return native_press;
            }
        }
        if pressed
            && button == WinitMouseButton::Left
            && !canonical_press_routed
            && self.focus_pane_under_pointer_with_plan(wid, input_plan.as_ref())
        {
            return native_press;
        }
        if let Some(session) = self.rewound_front(wid) {
            if button == WinitMouseButton::Left {
                self.rewind_press(wid, session, pressed, input_plan.as_ref());
            } else if pressed {
                self.note_rewind_withheld(session);
            }
            return native_press;
        }
        let mut click_count: u8 = 1;
        if button == WinitMouseButton::Left {
            let Some((term, ui_waiting)) = self
                .front_terminal(wid)
                .map(|terminal| (terminal.term.clone(), terminal.ui_waiting.clone()))
            else {
                return native_press;
            };
            let tracking =
                crate::term_lock_ui(&term, &ui_waiting, crate::metrics::TermWaitSite::Mouse)
                    .mouse_tracking_enabled();
            let option_held = mods_state.alt_key();
            if pressed && press_starts_selection(tracking, tracking_bypass_held(mods_state)) {
                // Cmd-click (macOS) / Ctrl-click (elsewhere) on a link opens it (safe
                // schemes only) instead of starting a selection; GUI-only, never the seam.
                if link_modifier_held(mods_state)
                    && self.open_link_under_pointer_with_plan(wid, input_plan.as_ref())
                {
                    return native_press;
                }
                if mods_state.shift_key() && !(tracking && !option_held) {
                    self.shift_extend_press(wid, at);
                    return native_press;
                }
                let drag_pane = self.focused_pane_rect_with_plan(wid, input_plan.as_ref());
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.staged_drag_pane = Some(drag_pane);
                }
                click_count = self.advance_click_streak(wid, at);
                if tracking {
                    let (row, col) = self
                        .windows
                        .get(&wid)
                        .map_or((0, 0), |ws| ws.last_mouse_cell);
                    let block = press_selection_kind(option_held, tracking) == SelectionType::Block;
                    self.seam_left_press(wid, row, col, click_count, block);
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        ws.staged_drag_pane = None;
                    }
                    return native_press;
                }
            }
            if !pressed && self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
                self.finish_selection(wid, false);
                if let Some(ws) = self.windows.get_mut(&wid) {
                    forget_reported_button(ws, aterm_types::mouse::MouseButton::Left);
                }
                return native_press;
            }
        }
        let Some(button) = winit_mouse_button(button) else {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.staged_drag_pane = None;
            }
            return native_press;
        };
        let slot = usize::from(button.slot());
        let bit = 1u8 << (button.slot() & 7);
        let (was_reported, owner) = self.windows.get(&wid).map_or((false, None), |ws| {
            (ws.reported_buttons & bit != 0, ws.button_owners[slot])
        });
        let route = if pressed {
            press_route
        } else {
            self.owner_route_with_plan(wid, owner, input_plan.as_ref())
        };
        if pressed && route == PointerRoute::Nowhere {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.staged_drag_pane = None;
            }
            return native_press;
        }
        let (row, col) = match route {
            PointerRoute::Pane { cell, .. } => cell,
            _ => self
                .windows
                .get(&wid)
                .map_or((0, 0), |ws| ws.last_mouse_cell),
        };
        let mods = self.mouse_modifiers(wid);
        let side = self
            .windows
            .get(&wid)
            .map_or(SelectionSide::Left, |ws| ws.last_mouse_side);
        let px_off = self
            .windows
            .get(&wid)
            .map_or(crate::input::PixelOffset::CELL_ORIGIN, |ws| {
                ws.last_mouse_px_off
            });
        let pressed_into = match route {
            PointerRoute::Pane { session, .. } => Some(session),
            _ => self.front_terminal(wid).map(|front| front.session),
        };
        if let Some(ws) = self.windows.get_mut(&wid) {
            if pressed {
                ws.reported_buttons |= bit;
                ws.button_owners[slot] = pressed_into.map(|session| ButtonOwner {
                    session,
                    cell: (row, col),
                    px_off,
                });
                ws.held_mouse_button = held_button(ws.reported_buttons);
            } else {
                forget_reported_button(ws, button);
            }
        }
        if !pressed && !was_reported {
            return native_press;
        }
        let block = mods_state.alt_key();
        self.send_pointer(
            wid,
            route,
            InputEvent::MouseButton {
                button,
                pressed,
                row,
                col,
                mods,
                click_count,
                side,
                block,
                suppress_copy_on_select: false,
                px_off,
            },
        );
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.staged_drag_pane = None;
        }
        native_press
    }

    fn front_pane_for_local_press(
        &mut self,
        wid: WindowId,
        route: PointerRoute,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> bool {
        match route {
            PointerRoute::Front => true,
            PointerRoute::Pane { session, .. } => {
                self.focus_pane_under_pointer_with_plan(wid, frame_plan)
                    && self.session_is_front(wid, session)
            }
            PointerRoute::Nowhere => false,
        }
    }

    fn route_term(
        &self,
        wid: WindowId,
        route: PointerRoute,
    ) -> Option<std::sync::Arc<std::sync::Mutex<Terminal>>> {
        match route {
            PointerRoute::Front => self
                .front_terminal(wid)
                .map(|terminal| terminal.term.clone()),
            PointerRoute::Pane { session, .. } => {
                self.pool.get(session).map(|owner| owner.term.clone())
            }
            PointerRoute::Nowhere => None,
        }
    }

    /// Normalize one winit wheel delta into whole notches.
    pub(crate) fn wheel_notches(
        &mut self,
        wid: WindowId,
        delta: MouseScrollDelta,
    ) -> Option<(WheelDir, i32)> {
        let cell = self.win_cell_size(wid);
        let ws = self.windows.get_mut(&wid)?;
        wheel_notches_in(
            delta,
            cell,
            &mut ws.scroll_residual_x,
            &mut ws.scroll_residual,
        )
    }

    /// `MouseWheel` -> when an app is tracking the mouse, report wheel up/down at the cell under the pointer.
    #[cfg(test)]
    pub(crate) fn on_mouse_wheel(&mut self, wid: WindowId, delta: MouseScrollDelta) {
        self.on_mouse_wheel_phased(wid, delta, TouchPhase::Moved);
    }

    /// `Self::on_mouse_wheel` with winit's gesture phase, which changes nothing about routing or bytes.
    pub(crate) fn on_mouse_wheel_phased(
        &mut self,
        wid: WindowId,
        delta: MouseScrollDelta,
        phase: TouchPhase,
    ) {
        if self
            .autopilot_menu
            .as_ref()
            .is_some_and(|menu| menu.wid == wid)
        {
            crate::input_hint::arm();
            self.fence_pointer_license(wid);
            let cell = self.win_cell_size(wid);
            let menu = self
                .autopilot_menu
                .as_mut()
                .expect("the menu owns this wheel event");
            let notches = wheel_notches_in(
                delta,
                cell,
                &mut menu.wheel_residual_x,
                &mut menu.wheel_residual,
            );
            if let Some((dir, lines)) = notches {
                self.autopilot_menu_wheel(wid, dir, lines);
            }
            return;
        }
        self.on_mouse_wheel_inner(wid, delta);
        if matches!(delta, MouseScrollDelta::PixelDelta(_))
            && matches!(phase, TouchPhase::Ended | TouchPhase::Cancelled)
        {
            self.release_scroll_track(wid);
        }
    }

    fn on_mouse_wheel_inner(&mut self, wid: WindowId, delta: MouseScrollDelta) {
        crate::input_hint::arm();
        self.fence_pointer_license(wid);
        let palette = self.palette_claims_pointer(wid);
        let overview = self.overview_claims_pointer(wid);
        let wheel_plan = (!(palette || overview)
            && (self.active_tab_has_multiple_visible_panes(wid)
                || self.active_tab_contains_native(wid)))
        .then(|| self.active_visible_leaf_plan(wid))
        .flatten();
        if !(palette || overview)
            && self.pointer_is_over_pane_header_with_plan(wid, wheel_plan.as_ref())
        {
            return;
        }
        if !(palette || overview) && self.stats_owns_pointer(wid) {
            return;
        }
        let seam_owned = self
            .windows
            .get(&wid)
            .is_none_or(|ws| ws.tab_menu.is_some() || ws.overlay_open());
        let native_view = (!seam_owned)
            .then(|| self.native_pointer_view(wid, wheel_plan.as_ref()))
            .flatten();
        let native_page = native_view.is_some();
        if !(seam_owned || palette || overview || native_page)
            && let Some(session) = self.rewound_under_pointer(wid, wheel_plan.as_ref())
        {
            let (dy, px) = match delta {
                MouseScrollDelta::LineDelta(_, y) => (f64::from(y), false),
                MouseScrollDelta::PixelDelta(p) => (p.y, true),
            };
            let cell_h = self.win_cell_size(wid).1 as f64;
            self.rewind_wheel(session, dy, px, cell_h);
            return;
        }
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (f64::from(x), f64::from(y)),
            MouseScrollDelta::PixelDelta(p) => (p.x, p.y),
        };
        if let Some(view) = native_view
            && !(seam_owned || palette || overview)
            && dx.is_finite()
            && dy.is_finite()
            && (dx != 0.0 || dy != 0.0)
        {
            self.interact_system_state_view(wid, view);
        }
        let track_px = match delta {
            MouseScrollDelta::PixelDelta(p)
                if wheel_axis(p.x, p.y, f64::EPSILON) == Some(WheelAxis::Vertical) =>
            {
                Some(p.y)
            }
            _ => None,
        };
        let Some((dir, lines)) = self.wheel_notches(wid, delta) else {
            // Otherwise the band would move under a card whose rows move nothing.
            if let Some(dy_px) = track_px
                && !(seam_owned || palette || overview || native_page)
            {
                match self.pointer_route_with_plan(wid, None, wheel_plan.as_ref()) {
                    PointerRoute::Front => self.track_precise_scroll(wid, dy_px, None),
                    PointerRoute::Pane { session, .. } => {
                        self.track_precise_scroll(wid, dy_px, Some(session));
                    }
                    PointerRoute::Nowhere => {}
                }
            }
            return;
        };
        let vertical_up = dir.vertical_up();
        // A sideways wheel or swipe over a grid shows the columns off to that side.
        if vertical_up.is_none()
            && !(palette || overview || seam_owned)
            && let Some(view) = native_view
        {
            let precise = matches!(delta, MouseScrollDelta::PixelDelta(_));
            let right = matches!(dir, WheelDir::Right);
            if self.native_editor_grid_sideways(wid, view, right, lines, precise) {
                return;
            }
        }
        if vertical_up.is_none() && (palette || overview || native_page) {
            return;
        }
        if let Some(up) = vertical_up {
            let signed = if up {
                -(lines as isize)
            } else {
                lines as isize
            };
            if palette {
                self.palette_pointer_wheel(wid, signed);
                return;
            }
            if overview {
                self.overview_pointer_wheel(wid, signed);
                return;
            }
            if let Some(view) = native_view {
                let signed = if up { -lines } else { lines };
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.native_rehover = true;
                }
                let key = self.native_scroll_target(wid, view, wheel_plan.as_ref());
                let _ = self.scroll_native_view(wid, view, signed, key);
                return;
            }
        }
        let route = self.pointer_route_with_plan(wid, None, wheel_plan.as_ref());
        let (row, col) = match route {
            PointerRoute::Pane { cell, .. } => cell,
            PointerRoute::Front => self
                .windows
                .get(&wid)
                .map_or((0, 0), |ws| ws.last_mouse_cell),
            PointerRoute::Nowhere => return,
        };
        let mods = self.mouse_modifiers(wid);
        let px_off = self
            .windows
            .get(&wid)
            .map_or(crate::input::PixelOffset::CELL_ORIGIN, |ws| {
                ws.last_mouse_px_off
            });
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.wheel_precise_px = track_px;
        }
        self.send_pointer(
            wid,
            route,
            InputEvent::Wheel {
                dir,
                lines,
                detent: matches!(delta, MouseScrollDelta::LineDelta(..)),
                row,
                col,
                mods,
                px_off,
            },
        );
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.wheel_precise_px = None;
        }
    }

    fn track_precise_scroll(&mut self, wid: WindowId, dy_px: f64, pane: Option<u64>) {
        use aterm_types::mouse::{ALT_MASK, SHIFT_MASK};
        if !self.smooth_scroll_animates(wid) {
            return;
        }
        let Some(session) = pane.or_else(|| self.front_terminal(wid).map(|front| front.session))
        else {
            return;
        };
        let Some((term, modes, ui_waiting)) = self.pool.get(session).map(|owner| {
            (
                owner.term.clone(),
                owner.ctx.modes.clone(),
                owner.ctx.ui_waiting.clone(),
            )
        }) else {
            return;
        };
        if self.rewrap_in_flight(session) {
            self.release_scroll_track(wid);
            return;
        }
        let mods = self.mouse_modifiers(wid);
        if modes.mouse_tracking_enabled() && mods & (SHIFT_MASK | ALT_MASK) == 0 {
            self.release_scroll_track(wid);
            return;
        }
        let cell_h = self.win_cell_size(wid).1.max(1) as i64;
        let same = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.scroll_glide.as_ref())
            .is_some_and(|st| std::sync::Arc::ptr_eq(&st.term, &term) && st.cell_h == cell_h);
        let viewport = if same {
            None
        } else {
            let site = crate::metrics::TermWaitSite::Mouse;
            let t = crate::term_lock_ui(&term, &ui_waiting, site);
            if crate::input::wheel_route_for(&t, mods) != crate::input::WheelRoute::Viewport {
                return;
            }
            Some(crate::input::WheelViewport {
                display_offset: t.grid().display_offset(),
                scrollback_lines: t.grid().scrollback_lines(),
                rows: t.rows(),
            })
        };
        self.scroll_wheel_animated_with(wid, &term, 0, viewport, Some(dy_px));
    }

    /// Start a tracked band's rest-settle now (`Glide::release`).
    pub(crate) fn release_scroll_track(&mut self, wid: WindowId) {
        self.release_scroll_track_at(wid, Instant::now());
    }

    /// [`Self::release_scroll_track`] at an explicit `now`.
    pub(crate) fn release_scroll_track_at(&mut self, wid: WindowId, now: Instant) {
        let app_interval = self.frame_interval;
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let tick_interval = ws.frame_interval.unwrap_or(app_interval);
        if let Some(st) = ws.scroll_glide.as_mut() {
            let parked = now < st.glide.settle_start();
            st.glide.release(now);
            if parked {
                st.next_tick = st.next_tick.min(now + tick_interval);
            }
        }
    }

    /// Snap the viewport back to the live bottom.
    pub(crate) fn snap_to_bottom(&mut self, wid: WindowId) {
        if self.rewound_front(wid).is_some() {
            return;
        }
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return;
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if ws
            .scroll_glide
            .as_ref()
            .is_none_or(|glide| std::sync::Arc::ptr_eq(&glide.term, &term))
        {
            ws.scroll_glide = None;
            ws.overscroll = None;
            ws.scroll_frac_px = 0;
        }
        let scrolled = {
            let mut term = term_lock(&term);
            let away = term.grid().display_offset() != 0 || term.grid().reader_aim_held();
            term.return_to_live();
            away
        };
        if scrolled && let Some(w) = &ws.os_window {
            w.request_redraw();
        }
    }

    /// Edit ▸ Select All.
    pub(crate) fn select_all(&mut self) {
        if let Some(wid) = self.frontmost_window {
            self.select_all_in(wid);
        }
    }

    /// Select only the document or terminal that owns this window's input.
    pub(crate) fn select_all_in(&mut self, wid: WindowId) {
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        if ws.overlay_open() || ws.rename_edit.is_some() {
            return;
        }
        if self.active_native_view(wid).is_some() {
            let _ = self.dispatch_native_event(
                wid,
                crate::native_app::AppEvent::TextInput(
                    crate::native_app::TextInputEvent::SelectAll,
                ),
            );
            return;
        }
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        if ws.search.is_some() {
            return;
        }
        let Some((shown, _)) = self.front_shown(wid) else {
            return;
        };
        {
            let mut term = term_lock(&shown);
            let last = i32::from(term.grid().rows().saturating_sub(1));
            let max_col = term.grid().cols().saturating_sub(1);
            let off = term.grid().display_offset() as i32;
            let sel = term.text_selection_mut();
            sel.start_selection(-off, 0, SelectionSide::Left, SelectionType::Lines);
            sel.update_selection(last - off, max_col, SelectionSide::Right);
            sel.expand_lines(max_col);
            sel.complete_selection();
            note_selection_custody(&mut term, true);
        }
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
    }
}

/// Open an already-allowlisted URL with the OS default handler.
pub(crate) fn open_url_external(url: &str) {
    #[cfg(test)]
    crate::platform::record_opened_url(url);
    #[cfg(all(not(test), target_os = "macos"))]
    drop(
        aterm_uds::spawnfd::command("/usr/bin/open")
            .arg(url)
            .spawn(),
    );
    #[cfg(all(not(test), target_os = "linux"))]
    drop(
        crate::startup_token::for_a_press(aterm_uds::spawnfd::command("xdg-open"))
            .arg(url)
            .spawn(),
    );
    #[cfg(all(not(test), windows))]
    open_url_windows(url);
}

#[cfg(all(not(test), windows))]
fn open_url_windows(url: &str) {
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: isize,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show_cmd: i32,
        ) -> isize;
    }
    const SW_SHOWNORMAL: i32 = 1;
    let verb: [u16; 5] = [
        u16::from(b'o'),
        u16::from(b'p'),
        u16::from(b'e'),
        u16::from(b'n'),
        0,
    ];
    let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: both UTF-16 buffers are NUL-terminated and outlive the call; null arguments are optional.
    let _ = unsafe {
        ShellExecuteW(
            0,
            verb.as_ptr(),
            wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
}

/// Which axis one wheel event belongs to, or `None` when it carries no axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WheelAxis {
    Horizontal,
    Vertical,
}

/// The cells of a sideways swipe over a grid that scroll it one column.
const GRID_SWIPE_CELLS: i32 = 6;

/// Classify a raw `(x, y)` wheel delta.
pub(crate) fn wheel_axis(x: f64, y: f64, floor: f64) -> Option<WheelAxis> {
    let (ax, ay) = (x.abs(), y.abs());
    if ax > ay && ax >= floor && ax > 0.0 {
        return Some(WheelAxis::Horizontal);
    }
    if ay > ax && ay >= floor && ay > 0.0 {
        return Some(WheelAxis::Vertical);
    }
    None
}

const fn horizontal_dir(positive: bool) -> WheelDir {
    if positive {
        WheelDir::Left
    } else {
        WheelDir::Right
    }
}

const fn vertical_dir(up: bool) -> WheelDir {
    if up { WheelDir::Up } else { WheelDir::Down }
}

fn bank_scroll_lines(residual: &mut f64, delta: f64) -> Option<(bool, i32)> {
    if *residual != 0.0 && (*residual > 0.0) != (delta > 0.0) {
        *residual = 0.0;
    }
    *residual += delta;
    let whole = residual.trunc();
    *residual -= whole;
    let n = whole.abs() as i32;
    (n > 0).then_some((whole > 0.0, n))
}

/// Apply the same units and axis policy to independently owned gesture banks.
fn wheel_notches_in(
    delta: MouseScrollDelta,
    (cw, ch): (usize, usize),
    horizontal: &mut f64,
    vertical: &mut f64,
) -> Option<(WheelDir, i32)> {
    let (axis, x, y) = match delta {
        MouseScrollDelta::LineDelta(x, y) => (
            wheel_axis(f64::from(x), f64::from(y), 0.0)?,
            f64::from(x),
            f64::from(y),
        ),
        MouseScrollDelta::PixelDelta(p) => (
            wheel_axis(p.x, p.y, f64::EPSILON)?,
            p.x / cw.max(1) as f64,
            p.y / ch.max(1) as f64,
        ),
    };
    match axis {
        WheelAxis::Horizontal => bank_scroll_lines(horizontal, x)
            .map(|(positive, lines)| (horizontal_dir(positive), lines)),
        WheelAxis::Vertical => {
            bank_scroll_lines(vertical, y).map(|(up, lines)| (vertical_dir(up), lines))
        }
    }
}

fn pet_wake_wanted(
    pet_visible: bool,
    brain_running: bool,
    sampled_px: Option<(f64, f64)>,
    pointer_px: (f64, f64),
    cell: (usize, usize),
) -> bool {
    if !pet_visible || brain_running {
        return false;
    }
    let Some((sx, sy)) = sampled_px else {
        return true;
    };
    let (cw, ch) = (cell.0.max(1) as f64, cell.1.max(1) as f64);
    (pointer_px.0 - sx).abs() >= cw || (pointer_px.1 - sy).abs() >= ch
}

#[cfg(all(test, unix))]
#[path = "app_mouse_pointer_license_tests.rs"]
mod pointer_license_tests;

#[cfg(test)]
#[path = "path_link_app_tests.rs"]
mod path_link_app_tests;

#[cfg(test)]
#[path = "app_mouse_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "app_mouse_pointer_route_tests.rs"]
mod pointer_route_tests;

#[cfg(all(test, windows))]
#[path = "app_mouse_right_click_win_tests.rs"]
mod right_click_win_tests;

#[cfg(test)]
#[path = "app_mouse_bidi_tests.rs"]
mod bidi_selection_tests;

#[cfg(test)]
#[path = "app_mouse_client_frame_tests.rs"]
mod client_frame_tests;

#[cfg(test)]
#[path = "app_mouse_menu_regrid_tests.rs"]
mod menu_regrid_tests;

#[cfg(test)]
#[path = "app_mouse_system_state_split_tests.rs"]
mod system_state_split_tests;
