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

/// The engine fill a window's frame stands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HoverFrame {
    single: (u64, u64, i32),
    composed: (u64, u64, i32),
}

impl HoverMemo {
    /// The engine-fill epoch of `ws`'s frame — see [`HoverFrame`].
    pub(crate) fn frame_of(ws: &crate::WindowState) -> HoverFrame {
        let of = |s: &aterm_core::render::RenderInput| {
            (s.terminal_id, s.engine_fill_seq, s.display_offset)
        };
        HoverFrame {
            single: of(&ws.input_scratch),
            composed: of(&ws.composed_focus_scratch),
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
    let col = pointer_cell(t, row, col).0;
    let link = t.hyperlink_at_visible(row, col);
    if link.is_some() || !mod_held {
        return (link, false, None);
    }
    let cells = t.render_row(usize::from(row));
    match plain_url_at(&cells, usize::from(col), min_contrast) {
        Some(plain) => (plain.faint.then(|| plain.url.into()), true, None),
        None => (
            None,
            false,
            crate::plain_path_at(&cells, usize::from(col), min_contrast)
                .map(|span| CitationProbe::new(t, span)),
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
        let running = crate::session_status::executing_detail(t);
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
            }),
        }
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
    ) -> Self {
        Self {
            px,
            seen: Some((view, key.clone())),
            asks,
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

/// Where the front window's pointer IS, as the `pointer` verb says it (cs-25).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PointerAt {
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
            self.invalidate_native_view_cache(wid, view, crate::native_app::DamageRegion::All);
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

    fn active_tab_is_single_focused_pane(&self, wid: WindowId) -> bool {
        self.windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active())
            .is_some_and(|tab| tab.root.len() == 1 && tab.root.contains(tab.focus))
    }

    fn active_tab_has_multiple_visible_panes(&self, wid: WindowId) -> bool {
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
        let plan = self.active_visible_leaf_plan(wid)?;
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
    pub(crate) fn pointer_route(
        &self,
        wid: WindowId,
        focused: Option<(u16, u16, u16, u16)>,
    ) -> PointerRoute {
        let Some(ws) = self.windows.get(&wid) else {
            return PointerRoute::Front;
        };
        if ws.overlay_open() || ws.tab_menu.is_some() || ws.selecting || ws.divider_drag.is_some() {
            return PointerRoute::Front;
        }
        let page_buttons = if self.active_native_view(wid).is_some() {
            ws.page_buttons
        } else {
            0
        };
        if held_button(ws.reported_buttons | page_buttons)
            .is_some_and(|lowest| page_buttons & (1 << lowest.slot()) != 0)
        {
            return PointerRoute::Front;
        }
        if ws.reported_buttons != 0 {
            let owner = ws
                .held_mouse_button
                .and_then(|button| ws.button_owners[usize::from(button.slot())]);
            return self.owner_route(wid, owner);
        }
        self.pane_under_pointer(wid, focused)
    }

    fn pane_under_pointer(
        &self,
        wid: WindowId,
        focused: Option<(u16, u16, u16, u16)>,
    ) -> PointerRoute {
        let Some(ws) = self.windows.get(&wid) else {
            return PointerRoute::Front;
        };
        if !self.active_tab_has_multiple_visible_panes(wid) {
            return PointerRoute::Front;
        }
        let window_cell = ws.last_mouse_window_cell;
        let focused = focused.unwrap_or_else(|| self.focused_pane_rect(wid));
        if rect_contains(focused, window_cell) {
            return PointerRoute::Front;
        }
        let Some(plan) = self.active_visible_leaf_plan(wid) else {
            return PointerRoute::Front;
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

    fn owner_route(&self, wid: WindowId, owner: Option<ButtonOwner>) -> PointerRoute {
        let Some(owner) = owner else {
            return PointerRoute::Front;
        };
        if self.session_is_front(wid, owner.session) {
            return PointerRoute::Front;
        }
        let Some(window_cell) = self.windows.get(&wid).map(|ws| ws.last_mouse_window_cell) else {
            return PointerRoute::Nowhere;
        };
        self.visible_session_rect(wid, owner.session)
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
            if reported & (1 << slot) == 0
                || self.visible_session_rect(wid, owner.session).is_some()
            {
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
    pub(crate) fn pointer_is_inside_focused_pane(&self, wid: WindowId) -> bool {
        let Some((wr, wc)) = self.windows.get(&wid).map(|ws| ws.last_mouse_window_cell) else {
            return false;
        };
        rect_contains(self.focused_pane_rect(wid), (wr, wc))
    }

    fn pane_header_at_with(
        &self,
        wid: WindowId,
        geom: PointerGeometry,
        x: f64,
        y: f64,
    ) -> Option<crate::tab_model::ViewId> {
        if !self.active_tab_has_multiple_visible_panes(wid)
            || self.windows.get(&wid).is_none_or(|ws| ws.overlay_open())
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
        if point.y >= 0.0 && self.chrome_owns_terminal_row(wid, point.y as usize) {
            return None;
        }
        self.active_visible_leaf_plan(wid)?
            .header_at(point)
            .map(|leaf| leaf.view)
    }

    fn pointer_is_over_pane_header(&self, wid: WindowId) -> bool {
        self.windows.get(&wid).is_some_and(|ws| {
            let (x, y) = ws.last_cursor_px;
            self.pane_header_at_with(wid, self.pointer_geometry(wid), x, y)
                .is_some()
        })
    }

    /// Click-to-focus in window `wid`.
    pub(crate) fn focus_pane_under_pointer(&mut self, wid: WindowId) -> bool {
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
    pub(crate) fn begin_divider_drag(&mut self, wid: WindowId) -> bool {
        self.begin_divider_drag_with_plan(wid, None)
    }

    fn begin_divider_drag_with_plan(
        &mut self,
        wid: WindowId,
        canonical_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
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

    /// Settle an in-flight text-selection drag only, for the paths about to drop `selecting`/`gesture` (window blur, a tab or pane switch).
    pub(crate) fn settle_selection_gesture(&mut self, wid: WindowId) {
        if self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
            self.finish_selection(wid, true);
        }
    }

    /// Settle any in-flight pointer drag on `wid` (divider resize, text selection) as their left-release paths in [`Self::on_mouse_input`] would.
    pub(crate) fn settle_pointer_drags(&mut self, wid: WindowId) {
        self.scrollbar_release(wid);
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

    /// Cmd-C: copy the selected text to the system clipboard (`pbcopy`), leaving the selection so repeated copies work.
    pub(crate) fn copy_selection_in(&self, wid: WindowId) -> bool {
        // A pinned or keyboard lens's ⌘C copies its instrument's sentence.
        if self.windows.get(&wid).is_some_and(|ws| {
            ws.stats_lens
                .open
                .is_some_and(|l| l.how != crate::stats_lens::Opened::Hover)
        }) {
            return self.copy_stats_sentence(wid);
        }
        let Some(text) = self.window_selection_text(wid) else {
            return false;
        };
        !text.is_empty() && control::pbcopy(&text)
    }

    /// The text of window `wid`'s live selection, resolved across panes.
    pub(crate) fn window_selection_text(&self, wid: WindowId) -> Option<String> {
        // Ownership is decided by `has_selection()`, never by whether the selection resolves
        // to text: rows are trailing-trimmed, so a painted selection over blank cells
        // resolves to `None`, and falling through would copy a sibling pane the user never
        // touched (on every drag, with `copy_on_select` on).
        if let Some(terminal) = self.front_terminal(wid) {
            let term = term_lock(&terminal.term);
            if term.text_selection().has_selection() {
                return term.selection_to_string();
            }
        }
        let plan = self.active_visible_leaf_plan(wid)?;
        for leaf in &plan.leaves {
            let Some(crate::tab_model::View::Terminal(view)) =
                self.view_store.get(leaf.view).copied()
            else {
                continue;
            };
            let Some(session) = self.pool.get(view.session) else {
                continue;
            };
            let term = term_lock(&session.term);
            if term.text_selection().has_selection() {
                return term.selection_to_string();
            }
        }
        None
    }

    /// [`Self::copy_selection_in`] against the frontmost window.
    pub(crate) fn copy_selection(&self) -> bool {
        self.frontmost_window
            .is_some_and(|wid| self.copy_selection_in(wid))
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
        let col = self
            .windows
            .get(&wid)
            .map_or(col, |ws| ws.claude_footer_undo.grid_col(row, col));
        (row, col)
    }

    fn refresh_mouse_cell(
        &mut self,
        wid: WindowId,
        geom: PointerGeometry,
        x: f64,
        y: f64,
    ) -> MouseCellMapping {
        let (row, col) = self.pixel_to_cell_with(wid, geom, x, y);
        let pane_rect = self.focused_pane_rect(wid);
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
        self.on_mouse_input(wid, state, button);
        if pressed {
            self.interact_system_state(wid);
        }
        if let Some(session) = seen {
            self.note_human_acted_on(wid, session);
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
        if matches!(
            action,
            PointerAction::Move { .. }
                | PointerAction::MoveNative(_)
                | PointerAction::Click
                | PointerAction::Wheel { .. }
        ) {
            let _ = self.commit_glassless_native_frame(wid);
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
                if self.band_banner_at(wid, x, y).is_none()
                    && self
                        .band_hit_at(wid, x, y)
                        .is_some_and(|target| self.band_press_acts(wid, target))
                {
                    // Not while parked, as `notice` refuses (`take_notice`): the band crossed
                    // to the successor in the carry the park captured, so a press here would
                    // be undone at Commit while its side effects stood. Refused before any
                    // press is spent.
                    if self.update_handoff_parked() {
                        let line = aterm_messages::wire::switching_reply();
                        return Err(line.strip_prefix("ERR ").unwrap_or(&line).to_string());
                    }
                    aterm_messages::wire::spend_press(&mut self.wire_gate, Instant::now())
                        .map_err(|line| line.strip_prefix("ERR ").unwrap_or(&line).to_string())?;
                }
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
        if !self.prepare_native_input_scratch(wid) {
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
        self.refresh_native_hover_for_frame(wid);
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
        let plan = self
            .active_visible_leaf_plan(wid)
            .ok_or("no presented native frame to aim at")?;
        let artifact = self
            .retained_native_leaf_artifact_from_plan(wid, view, true, &plan)
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
        let visible_native_views = self
            .active_visible_leaf_plan(wid)
            .map(|plan| {
                plan.leaves
                    .into_iter()
                    .filter_map(|leaf| {
                        matches!(
                            self.view_store.get(leaf.view),
                            Some(crate::tab_model::View::Native(_))
                        )
                        .then_some(leaf.view)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut hover_changed = Vec::new();
        for target in visible_native_views {
            let desired = hit
                .as_ref()
                .filter(|(hit_view, _)| *hit_view == target)
                .map(|(_, key)| key.clone());
            if self.native_runtime.change_view_state(target, |state| {
                let common = state.common_mut();
                let moved = common.hovered != desired;
                if moved {
                    common.hovered = desired;
                }
                moved
            }) {
                hover_changed.push(target);
            }
        }
        for target in hover_changed {
            self.invalidate_native_view_cache(wid, target, crate::native_app::DamageRegion::All);
            self.request_redraw_all_windows();
        }
    }

    fn native_hover_hit(
        &self,
        wid: WindowId,
        geom: PointerGeometry,
        x: f64,
        y: f64,
        over_pane_header: bool,
    ) -> Option<(
        crate::tab_model::ViewId,
        u64,
        crate::native_ui::UiKey,
        bool,
        bool,
    )> {
        if over_pane_header || self.strip_col_at_with(wid, geom, x, y).is_some() {
            return None;
        }
        self.retained_native_leaf_at_pointer(wid, x, y)
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

    /// A WHEEL'S PAGE IS HOVERED WHERE THE POINTER IS ON THE FRAME IT LEFT.
    pub(crate) fn refresh_native_hover_for_frame(&mut self, wid: WindowId) {
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
        let current = self.active_visible_leaf_plan(wid).is_some_and(|plan| {
            self.retained_native_leaf_artifact_from_plan(wid, front, true, &plan)
                .is_some()
        });
        if !current {
            return;
        }
        let geom = self.pointer_geometry(wid);
        let over_pane_header = self.pane_header_at_with(wid, geom, x, y).is_some();
        let hit = self.native_hover_hit(wid, geom, x, y, over_pane_header);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.native_rehover = false;
        }
        self.apply_native_hover(wid, hit.map(|(view, _, key, ..)| (view, key)));
    }

    fn pointer_readback(&self, wid: WindowId) -> Option<(i32, u16)> {
        let ws = self
            .windows
            .get(&wid)
            .filter(|ws| ws.pointer_position_known)?;
        let geom = self.pointer_geometry(wid);
        let (cw, ch) = geom.cell;
        let chrome = usize::from(self.chrome_rows(wid));
        if cw > 0 && ch > 0 && chrome > 0 {
            let (x, y) = ws.last_cursor_px;
            #[allow(
                clippy::cast_precision_loss,
                reason = "window pixel coordinates; the f64 mantissa covers every representable window size"
            )]
            let (top, band_h, pad) = (
                (geom.pad_top + geom.head) as f64,
                (chrome * ch) as f64,
                geom.pad as f64,
            );
            let fy = self.window_to_frame_y_signed(wid, geom, y) - top;
            if (0.0..band_h).contains(&fy) {
                let (fx, _) = self.window_to_frame_with(wid, geom, x, y);
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "non-negative, and bounded by the chrome rows and the columns"
                )]
                let (k, col) = (
                    (fy / ch as f64) as usize,
                    ((fx - pad).max(0.0) / cw as f64) as usize,
                );
                let col = u16::try_from(col)
                    .unwrap_or(u16::MAX)
                    .min(ws.cols.saturating_sub(1));
                let up = i32::try_from(chrome - k.min(chrome - 1)).unwrap_or(i32::MAX);
                return Some((-up, col));
            }
        }
        let (row, col) = ws.last_mouse_window_cell;
        Some((i32::from(row), col))
    }

    fn chrome_cell_centre_px(&self, wid: WindowId, up: u16, col: u16) -> Option<(f64, f64)> {
        let cols = self.windows.get(&wid).map(|ws| ws.cols)?;
        let chrome = self.chrome_rows(wid);
        if up == 0 || up > chrome || col >= cols {
            return None;
        }
        let geom = self.pointer_geometry(wid);
        let (cw, ch) = geom.cell;
        if cw == 0 || ch == 0 {
            return None;
        }
        let (ox, oy) = self.frame_origin_with(wid, geom);
        let k = usize::from(chrome - up);
        let fx = usize::from(col) * cw + cw / 2 + geom.pad;
        let fy = k * ch + ch / 2 + geom.pad_top + geom.head;
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

    fn press_band_at(&mut self, wid: WindowId, px: f64, py: f64) -> bool {
        // Judged against the banner's own floor (`paste_banner_tray_floor_y`, `0` = none)
        // before the band's hit test, whether or not the band has a row under it.
        if let Some(paste_up) = self.band_banner_at(wid, px, py) {
            if paste_up {
                self.answer_paste_banner(false);
            } else {
                self.answer_close_banner(false);
            }
            self.request_redraw_all_windows();
            return true;
        }
        let Some(target) = self.band_hit_at(wid, px, py) else {
            return false;
        };
        self.press_band(wid, target);
        true
    }

    fn band_banner_at(&self, wid: WindowId, px: f64, py: f64) -> Option<bool> {
        let paste_up = self.paste_banner.as_ref().is_some_and(|p| p.wid == wid);
        let close_up = self.close_banner.as_ref().is_some_and(|p| p.wid == wid);
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

    fn pet_press_at(&mut self, wid: WindowId) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let (px, py) = ws.last_cursor_px;
        let (fx, fy) = self.window_to_frame(wid, px, py);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        if ws
            .fx
            .companion_mut()
            .press(Instant::now(), (fx as f32, fy as f32))
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
            | ws.strip_hover_caption.take().is_some()
            | ws.band_hover.take().is_some();
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
        self.stats_pointer_left(wid, Instant::now());
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        ws.pointer_position_known = false;
        ws.hover_grid_owned = false;
        ws.native_rehover = false;
        let band_lit = ws.band_hover.take().is_some();
        ws.frame_edge = None;
        if ws.strip_hover.is_none()
            && !ws.strip_hover_new_tab
            && !ws.strip_hover_version
            && ws.strip_hover_caption.is_none()
            && !band_lit
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
        for _ in 0..self.windows.get(&wid).map_or(0, |ws| ws.tab_set.len()) {
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
        let tabs = ws.tab_set.len();
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
            self.search_toggle_case();
        } else if on(&hit.regex_cols) {
            self.search_toggle_regex();
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
        let parked = self.park_link_target(wid);
        self.update_hover_cursor_with(wid, self.pointer_geometry(wid), parked, None);
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
        self.update_hover_cursor_with(wid, self.pointer_geometry(wid), parked, Some(t));
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
    ) {
        let resolved = match self.resolve_hover_cursor(wid, geom, probe) {
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
        if let Some((session, url)) = self.claude_notice_at(wid, px, py) {
            let mods = self.windows.get(&wid).map(|ws| ws.mods).unwrap_or_default();
            let held = link_modifier_held(mods);
            let tracking = self
                .pool
                .get(session)
                .is_some_and(|owner| owner.ctx.modes.mouse_tracking_enabled());
            // The hand where a press opens the link (`press_claude_notice`): with the link
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
        if let Some(target) = self.band_hit_at(wid, px, py) {
            if self.band_press_acts(wid, target) {
                self.set_hover_cursor(wid, CursorIcon::Pointer, true, false);
            } else {
                self.set_hover_cursor(wid, CursorIcon::Default, false, false);
            }
            return Resolved(None);
        }
        if self.pane_header_at_with(wid, geom, px, py).is_some() {
            self.set_hover_cursor(wid, CursorIcon::Pointer, true, false);
            return Resolved(None);
        }
        if let Some(icon) = self.divider_cursor_under_pointer(wid) {
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
        let window_row = window_cell.0;
        let cell_is_under_pointer = self.pointer_is_inside_focused_pane(wid)
            && !self.chrome_owns_terminal_row(wid, usize::from(window_row));
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

    fn divider_cursor_under_pointer(&self, wid: WindowId) -> Option<CursorIcon> {
        let ws = self.windows.get(&wid)?;
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
        let plan = self.active_visible_leaf_plan(wid)?;
        let divider = plan.divider_at(crate::tab_model::LogicalPoint {
            x: f32::from(wc),
            y: f32::from(wr),
        })?;
        Some(match divider.axis {
            crate::tab_model::SplitAxis::Horizontal => CursorIcon::ColResize,
            crate::tab_model::SplitAxis::Vertical => CursorIcon::RowResize,
        })
    }

    fn native_editor_pointer_target(
        &self,
        wid: WindowId,
        x: f64,
        y: f64,
    ) -> Option<(crate::tab_model::ViewId, Option<usize>, usize)> {
        let (_, active_view) = self.active_native_view(wid)?;
        let (artifact, x, y) = self.retained_native_leaf_at_pointer(wid, x, y)?;
        if artifact.view != active_view
            || self.native_runtime.view_generation(artifact.view) != Some(artifact.generation)
            || self.native_runtime.app(artifact.instance)?.kind()
                != crate::native_app::AppKind::Editor
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
        let geometry = crate::native_ui::text_viewport_geometry(node.rect);
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
            self.clear_move_license(wid);
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
        self.track_band_hover(wid, x, y);
        let over_claude_chips = self.track_claude_light_hover(wid, x, y);
        if self.palette_claims_pointer(wid) {
            self.palette_pointer_motion(wid, x, y);
            return;
        }
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.tab_menu.is_some())
        {
            self.refresh_mouse_cell(wid, geom, x, y);
            self.track_tab_menu_hover(wid, x, y);
            return;
        }
        if self.autopilot_menu.as_ref().is_some_and(|m| m.wid == wid) {
            self.refresh_mouse_cell(wid, geom, x, y);
            self.autopilot_menu_motion(wid, x, y);
            return;
        }
        if self.overview_claims_pointer(wid) {
            self.overview_pointer_motion(wid, x, y);
            return;
        }
        if !self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.selecting || ws.held_mouse_button.is_some())
            && self.stats_pointer_motion(wid, Instant::now())
        {
            if let Some(ws) = self.windows.get_mut(&wid)
                && (ws.hover_pointer || ws.native_text_cursor)
            {
                ws.hover_pointer = false;
                ws.native_text_cursor = false;
                if let Some(w) = &ws.os_window {
                    w.set_cursor(CursorIcon::Default);
                }
            }
            return;
        }
        let over_pane_header = self.pane_header_at_with(wid, geom, x, y).is_some();
        if let Some((_, view)) = self.active_native_view(wid) {
            if self.active_tab_has_multiple_visible_panes(wid) {
                self.refresh_mouse_cell(wid, geom, x, y);
            }
            let dragging_editor = self
                .native_runtime
                .view_state(view)
                .and_then(|state| state.common().pressed.as_ref())
                .is_some_and(|key| key.as_str() == "editor/buffer");
            let editor_target = self.native_editor_pointer_target(wid, x, y);
            if dragging_editor && let Some((target_view, Some(byte), visible_lines)) = editor_target
            {
                let _ =
                    self.native_editor_pointer_select(wid, target_view, byte, true, visible_lines);
            }
            let native_hover = self.native_hover_hit(wid, geom, x, y, over_pane_header);
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
            let band_acts = self
                .band_hit_at(wid, x, y)
                .is_some_and(|target| self.band_press_acts(wid, target));
            let text_cursor =
                !over_pane_header && !band_acts && (editor_target.is_some() || text_field_hovered);
            let pointer = over_pane_header
                || band_acts
                || (!text_field_hovered
                    && native_hover
                        .as_ref()
                        .is_some_and(|(_, _, key, ..)| key.as_str() != "editor/buffer"));
            if let Some(ws) = self.windows.get_mut(&wid)
                && (ws.hover_pointer != pointer || ws.native_text_cursor != text_cursor)
            {
                ws.hover_pointer = pointer;
                ws.native_text_cursor = text_cursor;
                if let Some(window) = &ws.os_window {
                    window.set_cursor(if text_cursor {
                        CursorIcon::Text
                    } else if pointer {
                        CursorIcon::Pointer
                    } else {
                        CursorIcon::Default
                    });
                }
            }
            if !self.pointer_routes_to_a_terminal_pane(wid) {
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
        let mapped_cell = self.refresh_mouse_cell(wid, geom, x, y);
        let (lr, lc) = mapped_cell.pane_cell;
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.divider_drag.is_some())
        {
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
        if !self
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
        if self.band_hit_at(wid, x, y).is_some()
            && !self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.selecting || ws.held_mouse_button.is_some())
        {
            self.update_hover_cursor_with(wid, geom, parked, None);
            return;
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
        self.update_hover_cursor_with(wid, geom, parked, None);
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
        let route = self.pointer_route(wid, Some(mapped_cell.pane_rect));
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
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return;
        };
        let Some(fws) = self.windows.get_mut(&wid) else {
            return;
        };
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
        let moved = {
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

    /// Shift-click.
    pub(crate) fn shift_extend_press(&mut self, wid: WindowId) {
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
        let now = Instant::now();
        let side = doubled_side.unwrap_or_else(|| {
            self.windows
                .get(&wid)
                .map_or(SelectionSide::Left, |ws| ws.last_mouse_side)
        });
        self.extend_selection_to(wid, sel_row, col, side);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_press = Some((now, (sel_row, col)));
            ws.click_count = 1;
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
        }
    }

    /// Advance the `multi_click_ms` streak and return the resulting click count (1, 2, 3; a fourth rapid click wraps to 1).
    pub(crate) fn advance_click_streak(&mut self, wid: WindowId) -> u8 {
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
        let now = Instant::now();
        ws.click_count = match ws.last_press {
            Some((t, cell))
                if cell == (sel_row, col)
                    && now.duration_since(t).as_millis() <= multi_click_ms() =>
            {
                ws.click_count % 3 + 1
            }
            _ => 1,
        };
        ws.last_press = Some((now, (sel_row, col)));
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
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        ws.selecting = true;
        ws.sel_dragged = true;
        ws.sel_press_cell = (sel_row, col);
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
    }

    /// Single press with mouse tracking off.
    pub(crate) fn begin_selection(&mut self, wid: WindowId, kind: SelectionType) {
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return;
        };
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
        ws.sel_dragged = false;
        ws.sel_press_cell = (sel_row, col);
        ws.gesture = None;
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
    }

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
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return false;
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
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
            self.copy_selection_in(wid);
        }
        #[cfg(target_os = "linux")]
        if completed
            && !suppress_copy_on_select
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

    fn sourced_link_under_pointer(&self, wid: WindowId) -> Option<(String, bool)> {
        let ws = self.windows.get(&wid)?;
        let (row, col) = ws.last_mouse_cell;
        let window_row = usize::from(ws.last_mouse_window_cell.0);
        if self.chrome_owns_terminal_row(wid, window_row) {
            return None;
        }
        let (px, py) = ws.last_cursor_px;
        if let Some((_, url)) = self.claude_notice_at(wid, px, py) {
            return url.map(|url| (url.to_owned(), false));
        }
        let term = term_lock(&ws.front_terminal()?.term);
        let col = pointer_cell(&term, row, col).0;
        term.hyperlink_at_visible(row, col)
            .map(|url| (url.to_string(), true))
            .or_else(|| {
                let cells = term.render_row(row as usize);
                plain_url_at(&cells, col as usize, self.render_knobs.minimum_contrast)
                    .map(|plain| (plain.url, false))
            })
    }

    /// The file citation a press at the pointer would open (`crate::path_link`).
    pub(crate) fn path_link_under_pointer(
        &self,
        wid: WindowId,
    ) -> Option<crate::path_link::PathTarget> {
        let ws = self.windows.get(&wid)?;
        let (row, col) = ws.last_mouse_cell;
        let window_row = usize::from(ws.last_mouse_window_cell.0);
        if self.chrome_owns_terminal_row(wid, window_row) {
            return None;
        }
        let (px, py) = ws.last_cursor_px;
        if self.claude_notice_at(wid, px, py).is_some() {
            return None;
        }
        let front = ws.front_terminal()?;
        let session = front.session;
        let probe = {
            let term = term_lock(&front.term);
            let col = pointer_cell(&term, row, col).0;
            let cells = term.render_row(row as usize);
            crate::plain_path_at(&cells, col as usize, self.render_knobs.minimum_contrast)
                .map(|span| CitationProbe::new(&term, span))
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
    pub(crate) fn open_link_under_pointer(&mut self, wid: WindowId) -> bool {
        let sourced = self.sourced_link_under_pointer(wid);
        let pressed = sourced.as_ref().map(|(url, osc8)| {
            if *osc8 {
                PressedUrl::Osc8(url)
            } else {
                PressedUrl::Plain(url)
            }
        });
        let url = sourced.as_ref().map(|(url, _)| url.clone());
        let path = if url.is_none() {
            self.path_link_under_pointer(wid)
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
            self.update_hover_cursor(wid);
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
    pub(crate) fn on_mouse_input(
        &mut self,
        wid: WindowId,
        state: ElementState,
        button: WinitMouseButton,
    ) {
        self.route_mouse_input(wid, state, button);
        if state == ElementState::Released
            && let Some(button) = winit_mouse_button(button)
        {
            self.end_unpaired_release(wid, button);
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.page_buttons &= !(1 << button.slot());
            }
        }
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
        crate::app_input::write_ui_report(
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
        );
    }

    fn native_scroll_target(&self, wid: WindowId) -> Option<crate::native_ui::UiKey> {
        let ws = self
            .windows
            .get(&wid)
            .filter(|ws| ws.pointer_position_known)?;
        let (x, y) = ws.last_cursor_px;
        let (artifact, local_x, local_y) = self.retained_native_leaf_at_pointer(wid, x, y)?;
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

    fn pointer_routes_to_a_terminal_pane(&self, wid: WindowId) -> bool {
        matches!(self.pointer_route(wid, None), PointerRoute::Pane { .. })
    }

    fn press_lands_on_a_terminal_pane(&self, wid: WindowId) -> bool {
        matches!(
            self.pane_under_pointer(wid, None),
            PointerRoute::Pane { .. }
        )
    }

    fn route_mouse_input(&mut self, wid: WindowId, state: ElementState, button: WinitMouseButton) {
        self.clear_move_license(wid);
        let pressed = state == ElementState::Pressed;
        if button == WinitMouseButton::Left
            && let Some(ws) = self.windows.get_mut(&wid)
        {
            #[cfg(target_os = "linux")]
            if pressed && let Some(edge) = ws.frame_edge {
                if let Some(w) = &ws.os_window {
                    let _ = w.drag_resize_window(edge.direction());
                }
                return;
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
            return;
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
            return;
        }
        if self.autopilot_menu.as_ref().is_some_and(|m| m.wid == wid) {
            if pressed {
                let (px, py) = self
                    .windows
                    .get(&wid)
                    .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
                self.autopilot_menu_press(wid, button == WinitMouseButton::Left, px, py);
            } else {
                self.settle_pointer_drags(wid);
            }
            return;
        }
        if pressed && self.spends_menu_follow_up(wid) {
            return;
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
            return;
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
            return;
        }
        let Some(mods_state) = self.windows.get(&wid).map(|ws| ws.mods) else {
            return;
        };
        if self.windows.get(&wid).is_some_and(|ws| !ws.overlay_open())
            && self.pointer_is_over_pane_header(wid)
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
                if pressed && button == WinitMouseButton::Left && !captured {
                    if self.inline_rename_edit(wid).is_some() {
                        self.clear_strip_press(wid);
                        self.settle_rename_edit(wid);
                    }
                    let (x, y) = self.windows[&wid].last_cursor_px;
                    self.refresh_mouse_cell(wid, self.pointer_geometry(wid), x, y);
                    self.focus_pane_under_pointer(wid);
                }
                return;
            }
        }
        if self.windows.get(&wid).is_some_and(|ws| !ws.overlay_open()) {
            if pressed {
                let taken = if button == WinitMouseButton::Left {
                    self.stats_press(wid)
                } else {
                    self.stats_owns_pointer(wid)
                };
                if taken {
                    return;
                }
            } else if self
                .windows
                .get(&wid)
                .is_some_and(|ws| !ws.selecting && ws.held_mouse_button.is_none())
                && self.stats_owns_pointer(wid)
            {
                return;
            }
        }
        let mut canonical_press_routed = false;
        if pressed
            && button == WinitMouseButton::Left
            && let Some((plan, crate::VisibleContentRoute::Heterogeneous)) =
                self.active_visible_frame_layout(wid)
        {
            canonical_press_routed = true;
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |window| window.last_cursor_px);
            let on_band = self.band_banner_at(wid, px, py).is_some()
                || self.band_hit_at(wid, px, py).is_some();
            let in_content = !on_band && self.strip_col_at(wid, px, py).is_none();
            if in_content && self.press_claude_light(wid) {
                return;
            }
            if in_content
                && (self.begin_divider_drag_with_plan(wid, Some(&plan))
                    || self.focus_pane_under_pointer_from_plan(wid, &plan))
            {
                return;
            }
        }
        let terminal_beside = if pressed {
            self.press_lands_on_a_terminal_pane(wid)
        } else {
            winit_mouse_button(button).is_some_and(|button| {
                self.windows.get(&wid).is_some_and(|ws| {
                    ws.reported_buttons & (1 << button.slot()) != 0
                        && matches!(
                            self.owner_route(wid, ws.button_owners[usize::from(button.slot())]),
                            PointerRoute::Pane { .. }
                        )
                })
            })
        };
        if self.active_native_view(wid).is_some() && !terminal_beside {
            // Every press this boundary takes is the page's until its release, a miss
            // included: its drag and wheel stay the page's (`pointer_route`).
            if pressed
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
                let Some((_, view)) = self.active_native_view(wid) else {
                    return;
                };
                let armed = self
                    .native_runtime
                    .view_state(view)
                    .and_then(|state| state.common().pressed.clone());
                let release = self.retained_native_leaf_at_pointer(wid, px, py).and_then(
                    |(artifact, x, y)| {
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
                                artifact
                                    .compiled
                                    .slider_value_at(&hit.key, x)
                                    .map(crate::native_app::SemanticInput::Number)
                            })
                            .or_else(|| {
                                artifact.compiled.color_wheel_pick_at(&hit.key, x, y).map(
                                    |(hue, saturation)| {
                                        crate::native_app::SemanticInput::ColorPick {
                                            hue,
                                            saturation,
                                        }
                                    },
                                )
                            });
                        Some((artifact.view, artifact.generation, hit, value))
                    },
                );
                if let Some(state) = self.native_runtime.view_state_mut(view) {
                    state.common_mut().pressed = None;
                }
                self.invalidate_native_view_cache(wid, view, crate::native_app::DamageRegion::All);
                self.request_redraw_all_windows();
                if let (Some(armed), Some((release_view, generation, release, value))) =
                    (armed, release)
                    && armed == release.key
                    && self
                        .active_native_view(wid)
                        .is_some_and(|(_, live)| live == release_view)
                    && self.native_runtime.view_generation(release_view) == Some(generation)
                {
                    let _ = self.dispatch_native_event(
                        wid,
                        crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
                            id: release.action,
                            value,
                        }),
                    );
                }
                return;
            }
            if pressed && button == WinitMouseButton::Left {
                if let Some(col) = self.strip_col_at(wid, px, py) {
                    self.handle_tab_strip_click(wid, col);
                    return;
                }
                if self.press_band_at(wid, px, py) {
                    return;
                }
                if self.inline_rename_edit(wid).is_some() {
                    self.clear_strip_press(wid);
                    self.settle_rename_edit(wid);
                }
                let editor_target = self.native_editor_pointer_target(wid, px, py);
                let press = self.retained_native_leaf_at_pointer(wid, px, py).and_then(
                    |(artifact, x, y)| {
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
                    },
                );
                let picture = if press.is_none() {
                    self.retained_native_leaf_at_pointer(wid, px, py).and_then(
                        |(artifact, x, y)| {
                            let (_, active_view) = self.active_native_view(wid)?;
                            (artifact.view == active_view
                                && self.native_runtime.view_generation(artifact.view)
                                    == Some(artifact.generation))
                            .then(|| artifact.compiled.list_picture_at(x, y).cloned())
                            .flatten()
                            .map(|key| (artifact.view, key))
                        },
                    )
                } else {
                    None
                };
                if let Some((target_view, generation, key, _, _, asks)) = &press
                    && !NativePointerRest::looked_at(
                        self.windows
                            .get(&wid)
                            .and_then(|ws| ws.native_rest.as_ref()),
                        (px, py),
                        (*target_view, key, *asks),
                    )
                {
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        ws.native_rest =
                            Some(NativePointerRest::on((px, py), (*target_view, key, *asks)));
                    }
                    if self.native_runtime.view_generation(*target_view) == Some(*generation)
                        && let Some(state) = self.native_runtime.view_state_mut(*target_view)
                    {
                        let common = state.common_mut();
                        common.hovered = Some(key.clone());
                        common.pressed = None;
                    }
                    self.invalidate_native_view_cache(
                        wid,
                        *target_view,
                        crate::native_app::DamageRegion::All,
                    );
                    self.request_redraw_all_windows();
                    return;
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
                    self.invalidate_native_view_cache(
                        wid,
                        target_view,
                        crate::native_app::DamageRegion::All,
                    );
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
                } else if let Some((_, view)) = self.active_native_view(wid)
                    && let Some(state) = self.native_runtime.view_state_mut(view)
                {
                    state.common_mut().pressed = None;
                    self.invalidate_native_view_cache(
                        wid,
                        view,
                        crate::native_app::DamageRegion::All,
                    );
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
            return;
        }
        if pressed && button == WinitMouseButton::Left {
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
            if self.press_band_at(wid, px, py) {
                return;
            }
            if self.press_claude_light(wid) {
                return;
            }
        }
        if pressed && button != WinitMouseButton::Left {
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
            if self
                .claude_glass_at(wid, px, py)
                .is_some_and(|(_, at)| at != crate::claude_lights::GlassAt::Mark)
            {
                #[cfg(windows)]
                if button == WinitMouseButton::Right
                    && let Some(ws) = self.windows.get_mut(&wid)
                {
                    ws.band_menu_release_pending = false;
                }
                if button == WinitMouseButton::Right && !self.open_autopilot_menu_at(wid, px, py) {
                    self.open_fact_menu_at(wid, px, py);
                }
                return;
            }
            if button != WinitMouseButton::Right {
                if let Some(col) = self.strip_col_at(wid, px, py) {
                    if button == WinitMouseButton::Middle {
                        self.handle_tab_strip_middle_press(wid, col);
                    }
                    return;
                }
                if self.band_banner_at(wid, px, py).is_some()
                    || self.band_hit_at(wid, px, py).is_some()
                {
                    return;
                }
            }
        }
        let press_route = if pressed && button != WinitMouseButton::Left {
            self.pointer_route(wid, None)
        } else {
            PointerRoute::Front
        };
        #[cfg(target_os = "linux")]
        if pressed && button == WinitMouseButton::Middle {
            let tracking = self
                .route_term(wid, press_route)
                .is_some_and(|t| term_lock(&t).mouse_tracking_enabled());
            if !tracking {
                if self.front_pane_for_local_press(wid, press_route) {
                    self.paste_primary_into(wid);
                }
                return;
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
                return;
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
            let term = self.route_term(wid, press_route);
            let (tracking, selection) = term.as_ref().map_or((false, None), |t| {
                let tl = term_lock(t);
                (
                    tl.mouse_tracking_enabled(),
                    tl.selection_to_string().filter(|s| !s.is_empty()),
                )
            });
            match right_press_plan(gesture_on, over_strip, tracking, selection.is_some()) {
                RightPressPlan::Chrome => {
                    #[cfg(any(windows, target_os = "linux"))]
                    if let Some(col) = self.strip_col_at(wid, px, py) {
                        let segs = self
                            .windows
                            .get(&wid)
                            .map(|ws| ws.tab_segments.clone())
                            .unwrap_or_default();
                        match crate::tab_bar::hit_test(&segs, col) {
                            Some(
                                crate::tab_bar::TabHit::Select(index)
                                | crate::tab_bar::TabHit::Close(index),
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
                                    w.show_window_menu(aterm_window::dpi::PhysicalPosition::new(
                                        px, py,
                                    ));
                                }
                            }
                        }
                    }
                    return;
                }
                RightPressPlan::Copy => {
                    if !self.front_pane_for_local_press(wid, press_route) {
                        return;
                    }
                    if let (Some(text), Some(t)) = (selection, term.as_ref()) {
                        let _ = control::pbcopy(&text);
                        term_lock(t).text_selection_mut().clear();
                        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
                        {
                            w.request_redraw();
                        }
                    }
                    return;
                }
                RightPressPlan::Paste => {
                    if self.front_pane_for_local_press(wid, press_route) {
                        self.paste_clipboard_into(wid);
                    }
                    return;
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
                self.handle_tab_strip_click(wid, col);
                return;
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
                return;
            }
        }
        if pressed && button == WinitMouseButton::Left && self.pet_press_at(wid) {
            return;
        }
        if pressed && button == WinitMouseButton::Left && self.scrollbar_press(wid) {
            return;
        }
        if button == WinitMouseButton::Left {
            if pressed {
                if !canonical_press_routed && self.begin_divider_drag(wid) {
                    return;
                }
            } else if self.finish_divider_drag(wid) {
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.held_mouse_button = held_button(ws.reported_buttons);
                }
                return;
            }
        }
        if pressed
            && button == WinitMouseButton::Left
            && !canonical_press_routed
            && self.focus_pane_under_pointer(wid)
        {
            return;
        }
        let mut click_count: u8 = 1;
        if button == WinitMouseButton::Left {
            let Some(term) = self
                .front_terminal(wid)
                .map(|terminal| terminal.term.clone())
            else {
                return;
            };
            let tracking = term_lock(&term).mouse_tracking_enabled();
            let option_held = mods_state.alt_key();
            if pressed && press_starts_selection(tracking, tracking_bypass_held(mods_state)) {
                // Cmd-click (macOS) / Ctrl-click (elsewhere) on a link opens it (safe
                // schemes only) instead of starting a selection; GUI-only, never the seam.
                if link_modifier_held(mods_state) && self.open_link_under_pointer(wid) {
                    return;
                }
                if mods_state.shift_key() && !(tracking && !option_held) {
                    self.shift_extend_press(wid);
                    return;
                }
                click_count = self.advance_click_streak(wid);
                if tracking {
                    let (row, col) = self
                        .windows
                        .get(&wid)
                        .map_or((0, 0), |ws| ws.last_mouse_cell);
                    let block = press_selection_kind(option_held, tracking) == SelectionType::Block;
                    self.seam_left_press(wid, row, col, click_count, block);
                    return;
                }
            }
            if !pressed && self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
                self.finish_selection(wid, false);
                if let Some(ws) = self.windows.get_mut(&wid) {
                    forget_reported_button(ws, aterm_types::mouse::MouseButton::Left);
                }
                return;
            }
        }
        let Some(button) = winit_mouse_button(button) else {
            return;
        };
        let slot = usize::from(button.slot());
        let bit = 1u8 << (button.slot() & 7);
        let (was_reported, owner) = self.windows.get(&wid).map_or((false, None), |ws| {
            (ws.reported_buttons & bit != 0, ws.button_owners[slot])
        });
        let route = if pressed {
            press_route
        } else {
            self.owner_route(wid, owner)
        };
        if pressed && route == PointerRoute::Nowhere {
            return;
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
            return;
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
    }

    fn front_pane_for_local_press(&mut self, wid: WindowId, route: PointerRoute) -> bool {
        match route {
            PointerRoute::Front => true,
            PointerRoute::Pane { session, .. } => {
                self.focus_pane_under_pointer(wid) && self.session_is_front(wid, session)
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
        match delta {
            MouseScrollDelta::LineDelta(x, y) => {
                let axis = wheel_axis(f64::from(x), f64::from(y), 0.0)?;
                let ws = self.windows.get_mut(&wid)?;
                match axis {
                    WheelAxis::Horizontal => {
                        let (positive, n) =
                            bank_scroll_lines(&mut ws.scroll_residual_x, f64::from(x))?;
                        Some((horizontal_dir(positive), n))
                    }
                    WheelAxis::Vertical => {
                        let (up, n) = bank_scroll_lines(&mut ws.scroll_residual, f64::from(y))?;
                        Some((vertical_dir(up), n))
                    }
                }
            }
            MouseScrollDelta::PixelDelta(p) => {
                let axis = wheel_axis(p.x, p.y, f64::EPSILON)?;
                let (cw, ch) = self.win_cell_size(wid);
                let (cw, ch) = (cw.max(1) as f64, ch.max(1) as f64);
                let ws = self.windows.get_mut(&wid)?;
                match axis {
                    WheelAxis::Horizontal => {
                        let (positive, n) = bank_scroll_lines(&mut ws.scroll_residual_x, p.x / cw)?;
                        Some((horizontal_dir(positive), n))
                    }
                    WheelAxis::Vertical => {
                        let (up, n) = bank_scroll_lines(&mut ws.scroll_residual, p.y / ch)?;
                        Some((vertical_dir(up), n))
                    }
                }
            }
        }
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
        self.on_mouse_wheel_inner(wid, delta);
        if matches!(delta, MouseScrollDelta::PixelDelta(_))
            && matches!(phase, TouchPhase::Ended | TouchPhase::Cancelled)
        {
            self.release_scroll_track(wid);
        }
    }

    fn on_mouse_wheel_inner(&mut self, wid: WindowId, delta: MouseScrollDelta) {
        crate::input_hint::arm();
        self.clear_move_license(wid);
        let palette = self.palette_claims_pointer(wid);
        let overview = self.overview_claims_pointer(wid);
        let native_page =
            self.active_native_view(wid).is_some() && !self.pointer_routes_to_a_terminal_pane(wid);
        if !(palette || overview) && self.pointer_is_over_pane_header(wid) {
            return;
        }
        if !(palette || overview) && self.stats_owns_pointer(wid) {
            return;
        }
        let seam_owned = self
            .windows
            .get(&wid)
            .is_none_or(|ws| ws.tab_menu.is_some() || ws.overlay_open());
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (f64::from(x), f64::from(y)),
            MouseScrollDelta::PixelDelta(p) => (p.x, p.y),
        };
        if native_page
            && !(seam_owned || palette || overview)
            && dx.is_finite()
            && dy.is_finite()
            && (dx != 0.0 || dy != 0.0)
        {
            self.interact_system_state(wid);
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
                match self.pointer_route(wid, None) {
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
            if native_page {
                let signed = if up { -lines } else { lines };
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.native_rehover = true;
                }
                let event = match self.native_scroll_target(wid) {
                    Some(key) => crate::native_app::AppEvent::ScrollAt { lines: signed, key },
                    None => crate::native_app::AppEvent::ScrollLines(signed),
                };
                let _ = self.dispatch_native_event(wid, event);
                return;
            }
        }
        let route = self.pointer_route(wid, None);
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
        let Some((term, modes)) = self
            .pool
            .get(session)
            .map(|owner| (owner.term.clone(), owner.ctx.modes.clone()))
        else {
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
            let t = term_lock(&term);
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
        let Some(ws) = self.front() else { return };
        let Some(terminal) = ws.front_terminal() else {
            return;
        };
        let last = i32::from(ws.rows.saturating_sub(1));
        let max_col = ws.cols.saturating_sub(1);
        {
            let mut term = term_lock(&terminal.term);
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
mod pointer_license_tests {
    use super::PointerAction;
    use crate::cursor_glow::Geom;
    use crate::input::{InputEvent, InputOutcome, PixelOffset, Source};
    use crate::{App, WindowId, term_lock};
    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_session::sink::SinkWriter;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers};
    use std::io::{ErrorKind, Read};
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use std::time::Instant;

    fn fixture(mode: u16) -> (App, UnixStream, Geom) {
        let (reader, writer) = UnixStream::pair().expect("private input observer");
        reader.set_nonblocking(true).unwrap();
        let mut app =
            App::headless_for_test_with_sink(Arc::new(SinkWriter::new_owned(writer.into())));
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty".to_owned());
        app.recompute_sparkle();
        let wid = WindowId(0);
        let term = app.front_terminal(wid).unwrap().term.clone();
        if mode != 0 {
            term_lock(&term).process(format!("\x1b[?{mode}h\x1b[?1006h").as_bytes());
        }
        app.pointer_cmd(PointerAction::Move { row: 2, col: 3 })
            .unwrap();
        let geom = Geom {
            cw: 8,
            ch: 16,
            rows: 24,
            cols: 80,
            origin_x: 0,
            origin_y: 0,
            win_w: 640,
            win_h: 384,
            head: 0,
        };
        let cfg = app.glow_config();
        app.windows
            .get_mut(&wid)
            .unwrap()
            .fx
            .cursor_mut()
            .glow
            .tick(Some((0, 0)), Instant::now(), &cfg, geom, &mut Vec::new());
        (app, reader, geom)
    }

    fn read_input(reader: &mut UnixStream) -> Vec<u8> {
        let mut bytes = [0; 128];
        match reader.read(&mut bytes) {
            Ok(n) => bytes[..n].to_vec(),
            Err(e) if e.kind() == ErrorKind::WouldBlock => Vec::new(),
            Err(e) => panic!("reading input: {e}"),
        }
    }

    fn press(app: &mut App) {
        assert_eq!(
            app.input(
                WindowId(0),
                InputEvent::Key {
                    key: Key::Character('a'),
                    mods: Modifiers::empty(),
                    base_layout: None,
                    layout_text: None,
                    event_type: KeyEventType::Press,
                },
                Source::Human,
            ),
            InputOutcome::Ok,
        );
    }

    fn echo_at(app: &mut App, geom: Geom, now: Instant) -> (u64, &'static str, bool) {
        let wid = WindowId(0);
        let term = app.front_terminal(wid).unwrap().term.clone();
        term_lock(&term).process(b"a");
        let cursor = term_lock(&term).cursor();
        let cfg = app.glow_config();
        let ws = app.windows.get_mut(&wid).unwrap();
        let mut out = Vec::new();
        ws.fx
            .cursor_mut()
            .glow
            .tick(Some((cursor.row, cursor.col)), now, &cfg, geom, &mut out);
        (
            ws.fx.cursor_mut().glow.spawns(),
            ws.fx
                .cursor_mut()
                .glow
                .admission_log()
                .last()
                .unwrap()
                .licence,
            !out.is_empty() || !ws.fx.cursor_mut().glow.under_quads().is_empty(),
        )
    }

    #[test]
    fn byte_silent_pointer_preserves_the_keys_delayed_echo() {
        let wid = WindowId(0);
        let model = aterm_models_effects::cursor_hint_license_model();
        let (mut control, _reader, geom) = fixture(0);
        press(&mut control);
        let pressed = Instant::now();
        control.clear_move_license(wid);
        let cleared_echo = echo_at(&mut control, geom, pressed);
        assert_eq!(cleared_echo, (1, "inflight", true));
        for controller in [false, true] {
            for mode in [0, 1000, 1002, 1003] {
                for same_pixel in [false, true] {
                    let (mut app, mut reader, geom) = fixture(mode);
                    let _ = read_input(&mut reader);
                    press(&mut app);
                    let pressed = Instant::now();
                    assert_eq!(read_input(&mut reader), b"a");
                    let mut before = model.init_state();
                    assert!(model.fire("PressArmsLicense", &mut before));
                    assert!(app.windows[&wid].fx.cursor().glow.move_licensed(pressed));
                    let col = if same_pixel { 3 } else { 4 };
                    if controller {
                        assert_eq!(
                            app.input(
                                wid,
                                InputEvent::MouseMove {
                                    buttons: 3,
                                    row: 2,
                                    col,
                                    mods: 0,
                                    side: SelectionSide::Left,
                                    px_off: PixelOffset::default(),
                                },
                                Source::Controller,
                            ),
                            InputOutcome::Ok,
                        );
                    } else {
                        app.pointer_cmd(PointerAction::Move { row: 2, col })
                            .unwrap();
                    }
                    let reported = mode == 1003 && !same_pixel;
                    let bytes = read_input(&mut reader);
                    assert_eq!(
                        bytes.is_empty(),
                        !reported,
                        "{mode}/{controller}/{same_pixel}"
                    );
                    if reported {
                        assert!(bytes.starts_with(b"\x1b[<35;"), "{bytes:?}");
                    }
                    let ws = &app.windows[&wid];
                    let live = ws.fx.cursor().glow.move_licensed(pressed);
                    assert_eq!(live, !reported, "{mode}/{controller}/{same_pixel}");
                    assert_eq!(ws.fx.cursor().trail.move_licensed(pressed), !reported);
                    assert_eq!(
                        echo_at(&mut app, geom, pressed),
                        if reported {
                            cleared_echo
                        } else {
                            (1, "key", true)
                        },
                        "echo admission {mode}/{controller}/{same_pixel}",
                    );
                    if mode == 0 && !controller && !same_pixel {
                        let mut after = before.clone();
                        after.insert("hint", i64::from(live));
                        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
                            &model,
                            &[],
                            &before,
                            &after,
                            Some("ByteSilentPointerPreservesLicense"),
                            "plain pointer hover",
                        );
                        assert!(ok, "{why}");
                        let mut erased = after;
                        erased.insert("hint", 0);
                        let (ok, _) = aterm_spec::verify::validate_transition_tiered(
                            &model,
                            &[],
                            &before,
                            &erased,
                            Some("ByteSilentPointerPreservesLicense"),
                            "hover-clear negative control",
                        );
                        assert!(!ok, "the model must reject a hover that loses the key");
                    }
                }
            }
        }
    }

    #[test]
    fn pointer_gestures_still_retire_the_pending_typed_license() {
        for local_selection in [false, true] {
            let (mut app, mut reader, _) = fixture(0);
            let wid = WindowId(0);
            press(&mut app);
            assert_eq!(read_input(&mut reader), b"a");
            if local_selection {
                app.begin_selection(wid, SelectionType::Simple);
                app.pointer_cmd(PointerAction::Move { row: 2, col: 5 })
                    .unwrap();
            } else {
                assert_eq!(
                    app.input(
                        wid,
                        InputEvent::MouseMove {
                            buttons: 0,
                            row: 2,
                            col: 5,
                            mods: 0,
                            side: SelectionSide::Left,
                            px_off: PixelOffset::default(),
                        },
                        Source::Controller
                    ),
                    InputOutcome::Ok
                );
            }
            assert_eq!(app.windows[&wid].selecting, local_selection);
            assert!(
                read_input(&mut reader).is_empty(),
                "tracking-OFF drag stays local"
            );
            assert!(
                !app.windows[&wid]
                    .fx
                    .cursor()
                    .glow
                    .move_licensed(Instant::now())
            );
            assert!(
                !app.windows[&wid]
                    .fx
                    .cursor()
                    .trail
                    .move_licensed(Instant::now())
            );
        }
    }
}

#[cfg(test)]
#[path = "path_link_app_tests.rs"]
mod path_link_app_tests;

#[cfg(test)]
mod tests {
    use super::{PointerAction, WheelDir, press_selection_kind};
    use aterm_core::selection::{SelectionSide, SelectionType};

    #[test]
    fn a_still_pointer_keeps_what_it_saw_and_a_press_goes_where_it_looked() {
        use super::NativePointerRest as Rest;
        use crate::native_ui::UiKey;
        use crate::tab_model::ViewId;
        let (view, other) = (ViewId::from_stored(1), ViewId::from_stored(2));
        let row = UiKey::new("harness/session/9/autopilot/land");
        let next = UiKey::new("harness/session/10/autopilot/land");
        let bulk = UiKey::new("harness/session/all/autopilot/land");
        let at = (10.0, 20.0);
        let rest = Rest::after_motion(None, at, Some((view, &row, false)));
        let still = Rest::after_motion(rest.clone(), at, Some((view, &bulk, true)));
        assert_eq!(still, rest, "the same pixel again is no motion");
        assert!(!Rest::looked_at(still.as_ref(), at, (view, &bulk, true)));
        assert!(Rest::looked_at(still.as_ref(), at, (view, &row, false)));
        assert!(
            Rest::looked_at(still.as_ref(), at, (view, &next, false)),
            "rows as before"
        );
        let lit = Rest::on(at, (view, &bulk, true));
        assert!(
            Rest::looked_at(Some(&lit), at, (view, &bulk, true)),
            "the next press takes it"
        );
        let on_bulk = Rest::after_motion(None, at, Some((view, &bulk, true)));
        assert!(!Rest::looked_at(on_bulk.as_ref(), at, (view, &row, false)));
        assert!(!Rest::looked_at(on_bulk.as_ref(), at, (other, &bulk, true)));
        assert!(Rest::looked_at(on_bulk.as_ref(), at, (view, &bulk, true)));
        let lit = Rest::on(at, (view, &row, false));
        assert!(
            Rest::looked_at(Some(&lit), at, (view, &row, false)),
            "the next press takes it"
        );
        let nothing = Rest::after_motion(None, at, None);
        assert!(!Rest::looked_at(nothing.as_ref(), at, (view, &bulk, true)));
        assert!(Rest::looked_at(nothing.as_ref(), at, (view, &row, false)));
        assert!(
            Rest::looked_at(None, at, (view, &bulk, true)),
            "a scroll forgot it"
        );
        let moved = Rest::after_motion(still, (11.0, 20.0), Some((view, &bulk, true)));
        assert!(Rest::looked_at(
            moved.as_ref(),
            (11.0, 20.0),
            (view, &bulk, true)
        ));
        assert!(Rest::looked_at(
            moved.as_ref(),
            (12.0, 20.0),
            (view, &row, false)
        ));
    }

    #[test]
    fn a_glassless_native_pointer_hovers_says_where_and_leaving_clears_it() {
        use super::{NativePointerTarget, PointerAt};
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let cell = app
            .pointer_cmd(PointerAction::Move { row: 2, col: 3 })
            .expect("a terminal move");
        assert!(matches!(
            app.pointer_reply(cell),
            Ok(PointerAt::Grid { cell: (2, 3), .. })
        ));
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::CursorStyles));
        let (_, view) = app.active_native_view(wid).expect("Settings in front");
        let compiled = app.compiled_native_ui(wid).expect("the page compiles");
        let row = compiled
            .focus_order
            .iter()
            .find(|key| key.as_str().starts_with("settings/cursor-style/"))
            .cloned()
            .expect("a style row");
        let hovered = |app: &crate::App| {
            app.native_runtime
                .view_state(view)
                .and_then(|state| state.common().hovered.clone())
        };
        let cell = app
            .pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Key(
                row.as_str().to_string(),
            )))
            .expect("the move aims at the row");
        assert_eq!(hovered(&app), Some(row.clone()), "the row is hovered");
        match app.pointer_reply(cell) {
            Ok(PointerAt::Native {
                view: at,
                key: Some(key),
                ..
            }) => {
                assert_eq!(at, view.get());
                assert_eq!(key, row.as_str());
            }
            other => panic!("a native answer, never a grid cell: {other:?}"),
        }
        let cell = app
            .pointer_cmd(PointerAction::Leave)
            .expect("the pointer leaves");
        assert_eq!(hovered(&app), None, "leaving clears the hover");
        assert_eq!(app.pointer_reply(cell), Ok(PointerAt::Absent));
    }

    #[test]
    fn a_page_scrolled_under_a_still_pointer_is_hovered_where_it_now_is() {
        use super::{NativePointerTarget, PointerAt, WheelUnit};
        let (mut app, wid, view) =
            settings_at_1000_by_600(crate::native_settings::SettingsRoute::CursorStyles);
        let compiled = app.compiled_native_ui(wid).expect("the page compiles");
        let row = compiled
            .focus_order
            .iter()
            .filter(|key| key.as_str().starts_with("settings/cursor-style/"))
            .nth(1)
            .cloned()
            .expect("a style row");
        let hovered = |app: &crate::App| {
            app.native_runtime
                .view_state(view)
                .and_then(|state| state.common().hovered.clone())
                .map(|key| key.as_str().to_string())
        };
        app.pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Key(
            row.as_str().to_string(),
        )))
        .expect("the move aims at the row");
        assert_eq!(
            hovered(&app).as_deref(),
            Some(row.as_str()),
            "CONTROL: the row is hovered"
        );
        let cell = app
            .pointer_cmd(PointerAction::Wheel {
                dx: 0.0,
                dy: 3.0,
                unit: WheelUnit::Lines,
                aim: None,
            })
            .expect("the wheel scrolls the list");
        let under = match app.pointer_reply(cell) {
            Ok(PointerAt::Native { key, .. }) => key,
            other => panic!("a native answer: {other:?}"),
        };
        assert_ne!(
            under.as_deref(),
            Some(row.as_str()),
            "the wheel moved the list under the pointer"
        );
        assert_eq!(hovered(&app), under, "hovered where the pointer now is");
        let tried = app
            .compiled_native_ui(wid)
            .expect("the page compiles")
            .paint
            .iter()
            .find_map(|node| match &node.content {
                crate::native_ui::UiContent::SettingsPreview(spec) => {
                    Some(spec.cursor.effective_trail_style_raw().to_string())
                }
                _ => None,
            })
            .expect("the preview");
        let shown = under
            .as_deref()
            .and_then(|key| key.strip_prefix("settings/cursor-style/"))
            .map(|style| style.replace('-', " "));
        if let Some(shown) = shown {
            assert!(
                tried.eq_ignore_ascii_case(&shown),
                "the preview tries {tried:?}, the row under the pointer is {shown:?}"
            );
        }
    }

    fn settings_at_1000_by_600(
        route: crate::native_settings::SettingsRoute,
    ) -> (crate::App, crate::WindowId, crate::tab_model::ViewId) {
        settings_at_1000_by_600_in(crate::App::headless_for_test(), route)
    }

    fn settings_at_1000_by_600_in(
        app: crate::App,
        route: crate::native_settings::SettingsRoute,
    ) -> (crate::App, crate::WindowId, crate::tab_model::ViewId) {
        settings_sized_in(app, route, (1_000, 600))
    }

    fn settings_sized_in(
        mut app: crate::App,
        route: crate::native_settings::SettingsRoute,
        (width, height): (usize, usize),
    ) -> (crate::App, crate::WindowId, crate::tab_model::ViewId) {
        let wid = crate::WindowId(0);
        let (cell_w, cell_h) = app.win_cell_size(wid);
        let pad = app.win_pad(wid);
        if let Some(window) = app.windows.get_mut(&wid) {
            window.cols = u16::try_from((width - pad * 2) / cell_w.max(1)).unwrap();
            window.rows = u16::try_from((height - pad * 2) / cell_h.max(1)).unwrap();
        }
        assert!(app.open_settings_tab(route));
        let (_, view) = app.active_native_view(wid).expect("Settings in front");
        (app, wid, view)
    }

    #[test]
    fn a_glassless_pointers_reply_reads_the_frame_its_click_left() {
        use super::{NativePointerTarget, PointerAt};
        let (mut app, wid, view) =
            settings_at_1000_by_600(crate::native_settings::SettingsRoute::KeyboardInput);
        let switch = crate::native_ui::UiKey::new("settings/control/copy_on_select");
        let look = |app: &crate::App| {
            let compiled = app.compiled_native_ui(wid).expect("the page compiles");
            compiled
                .semantic(&switch)
                .map(|node| (node.value.clone(), node.state))
        };
        let native_reply = |app: &crate::App, cell| match app.pointer_reply(cell) {
            Ok(PointerAt::Native { view: at, .. }) => assert_eq!(at, view.get()),
            other => panic!("the reply reads the frame after the event: {other:?}"),
        };
        let before = look(&app);
        assert!(
            app.pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Key(
                switch.as_str().to_string()
            )))
            .is_ok()
        );
        let cell = app.pointer_cmd(PointerAction::Click).expect("a click");
        assert_ne!(look(&app), before, "the click acted");
        native_reply(&app, cell);
        let _ = app.dispatch_native_event(wid, crate::native_app::AppEvent::ScrollLines(1));
        assert_eq!(
            app.pointer_reply(cell),
            Err(
                "the page under the pointer changed and its next frame is not presented yet \
                 (pointer status reads it)"
                    .to_string()
            )
        );
        let cell = app.pointer_cmd(PointerAction::Status).expect("status");
        native_reply(&app, cell);
    }

    #[test]
    fn a_pointer_reply_on_glass_reads_the_frame_its_event_left() {
        use super::{NativePointerTarget, PointerAt, WheelUnit};
        let (mut app, wid, view) = settings_at_1000_by_600_in(
            crate::scripted_redraw_fixture::scripted_app(),
            crate::native_settings::SettingsRoute::KeyboardInput,
        );
        assert!(
            !app.commit_glassless_native_frame(wid),
            "the window has glass"
        );
        app.redraw_window(wid);
        let switch = "settings/control/copy_on_select";
        let reply = |app: &mut crate::App, action| {
            let cell = app.pointer_cmd(action).expect("the pointer acts");
            match app.pointer_reply(cell) {
                Ok(PointerAt::Native { view: at, key, .. }) => {
                    assert_eq!(at, view.get());
                    key
                }
                other => panic!("the reply reads the presented frame: {other:?}"),
            }
        };
        let value = |app: &crate::App| {
            app.compiled_native_ui(wid)
                .expect("the page compiles")
                .semantic(&crate::native_ui::UiKey::new(switch))
                .map(|node| (node.value.clone(), node.state))
        };
        let hovered = reply(
            &mut app,
            PointerAction::MoveNative(NativePointerTarget::Key(switch.into())),
        );
        assert_eq!(hovered.as_deref(), Some(switch), "the hover is presented");
        let before = value(&app);
        let _ = reply(&mut app, PointerAction::Click);
        assert_ne!(value(&app), before, "the click acted");
        let wheel = PointerAction::Wheel {
            dx: 0.0,
            dy: 1.0,
            unit: WheelUnit::Lines,
            aim: None,
        };
        let _ = reply(&mut app, wheel);
        let status = reply(&mut app, PointerAction::Status);
        let compiled = app.compiled_native_ui(wid).expect("the page compiles");
        let (x, y) = app.windows[&wid].last_cursor_px;
        let (artifact, local_x, local_y) = app
            .retained_native_leaf_at_pointer(wid, x, y)
            .expect("the frame under the pointer is presented");
        assert_eq!(artifact.compiled.paint.len(), compiled.paint.len());
        assert_eq!(
            status,
            compiled
                .hit_test(local_x, local_y)
                .map(|hit| hit.key.as_str().to_string()),
            "the reply names what the page now holds there"
        );
        let surface = crate::scripted_redraw_fixture::surface;
        let commits = surface(&mut app).commits();
        let _ = reply(&mut app, PointerAction::Status);
        assert_eq!(surface(&mut app).commits(), commits, "nothing changed");
        surface(&mut app).fail_next(crate::present::scripted::ScriptedOutcome::FailCommit);
        let wheel_up = PointerAction::Wheel {
            dx: 0.0,
            dy: -1.0,
            unit: WheelUnit::Lines,
            aim: None,
        };
        let cell = app.pointer_cmd(wheel_up).expect("the wheel turns");
        assert_eq!(
            app.pointer_reply(cell),
            Err(
                "the page under the pointer changed and its next frame is not presented yet \
                 (pointer status reads it)"
                    .to_string()
            )
        );
    }

    #[test]
    fn a_headless_pointer_wheel_takes_the_wheels_own_path_into_a_native_view() {
        use super::{NativePointerTarget, PointerAt, WheelUnit};
        use crate::native_app::AppViewState;
        let (mut app, wid, view) =
            settings_at_1000_by_600(crate::native_settings::SettingsRoute::Terminal);
        let page_scroll = |app: &crate::App| match app.native_runtime.view_state(view) {
            Some(AppViewState::Settings(settings)) => settings.page_scroll,
            _ => panic!("Settings state"),
        };
        let lines = |dy: f64, aim: Option<&str>| PointerAction::Wheel {
            dx: 0.0,
            dy,
            unit: WheelUnit::Lines,
            aim: aim.map(|key| NativePointerTarget::Key(key.to_string())),
        };
        let native_reply = |app: &crate::App, cell| match app.pointer_reply(cell) {
            Ok(PointerAt::Native { view: at, .. }) => assert_eq!(at, view.get()),
            other => panic!("the reply reads the frame after the event: {other:?}"),
        };
        assert_eq!(
            app.pointer_cmd(lines(1.0, None)),
            Err("the pointer holds no position (move it first)".to_string())
        );
        assert_eq!(page_scroll(&app), 0);
        let compiled = app.compiled_native_ui(wid).expect("the page compiles");
        let on_screen = |prefix: &str| {
            compiled
                .paint
                .iter()
                .find(|node| {
                    node.key.as_str().starts_with(prefix)
                        && node.clip.width > 0.0
                        && node.clip.height > 0.0
                })
                .map(|node| node.key.as_str().to_string())
                .unwrap_or_else(|| panic!("a {prefix} control on screen"))
        };
        let (row, nav) = (on_screen("settings/control/"), on_screen("settings/nav/"));
        let cell = app
            .pointer_cmd(lines(2.0, Some(&row)))
            .expect("a wheel aimed at a row");
        assert_eq!(page_scroll(&app), 2, "one stop a line");
        native_reply(&app, cell);
        let cell = app
            .pointer_cmd(lines(-1.0, None))
            .expect("a wheel where it rests");
        assert_eq!(page_scroll(&app), 1);
        native_reply(&app, cell);
        let sideways = PointerAction::Wheel {
            dx: 3.0,
            dy: 0.0,
            unit: WheelUnit::Lines,
            aim: None,
        };
        assert!(app.pointer_cmd(sideways).is_ok());
        assert_eq!(page_scroll(&app), 1);
        let (_, cell_h) = app.win_cell_size(wid);
        let scale = app.windows[&wid].scale;
        let points = PointerAction::Wheel {
            dx: 0.0,
            dy: 2.0 * cell_h as f64 / scale,
            unit: WheelUnit::Points,
            aim: None,
        };
        assert!(app.pointer_cmd(points).is_ok());
        assert_eq!(page_scroll(&app), 3, "points bank into whole stops");
        assert!(app.pointer_cmd(lines(1.0, Some(&nav))).is_ok());
        assert_eq!(page_scroll(&app), 3, "the sidebar's wheel leaves the list");
    }

    #[test]
    fn a_pointer_wheel_over_a_terminal_scrolls_its_history() {
        use super::WheelUnit;
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let term = app.front_terminal(wid).expect("a terminal").term.clone();
        {
            let mut terminal = crate::term_lock(&term);
            for line in 0..100 {
                terminal.process(format!("history {line}\r\n").as_bytes());
            }
        }
        let wheel = |dy: f64| PointerAction::Wheel {
            dx: 0.0,
            dy,
            unit: WheelUnit::Lines,
            aim: None,
        };
        assert!(
            app.pointer_cmd(PointerAction::Move { row: 2, col: 3 })
                .is_ok()
        );
        assert!(app.pointer_cmd(wheel(1.0)).is_ok());
        assert_eq!(crate::term_lock(&term).grid().display_offset(), 0);
        assert!(
            app.windows[&wid].scroll_glide.is_none(),
            "down at the bottom"
        );
        assert!(app.pointer_cmd(wheel(-3.0)).is_ok());
        assert!(
            app.windows[&wid].scroll_glide.is_some()
                || crate::term_lock(&term).grid().display_offset() > 0,
            "up reaches the history"
        );
    }

    #[test]
    fn a_wheel_over_an_open_picker_moves_the_hover_only_when_the_list_moves() {
        use super::{NativePointerTarget, PointerAt, WheelUnit};
        use crate::native_app::AppViewState;
        let (mut app, wid, view) =
            settings_at_1000_by_600(crate::native_settings::SettingsRoute::Appearance);
        let aim = |key: &str| PointerAction::MoveNative(NativePointerTarget::Key(key.into()));
        let wheel = |dy: f64| PointerAction::Wheel {
            dx: 0.0,
            dy,
            unit: WheelUnit::Lines,
            aim: None,
        };
        let tiles = |app: &crate::App| {
            app.compiled_native_ui(wid)
                .expect("the page compiles")
                .paint
                .iter()
                .filter(|node| {
                    node.key.as_str().starts_with("settings/choice/theme/")
                        && node.clip.height > 0.0
                })
                .map(|node| node.key.as_str().to_string())
                .collect::<Vec<_>>()
        };
        let hovered = |app: &crate::App| match app.native_runtime.view_state(view) {
            Some(AppViewState::Settings(settings)) => settings
                .common
                .hovered
                .as_ref()
                .map(|key| key.as_str().to_string()),
            _ => panic!("Settings state"),
        };
        assert!(app.pointer_cmd(aim("settings/control/theme")).is_ok());
        assert!(app.pointer_cmd(PointerAction::Click).is_ok());
        let top = tiles(&app);
        let tile = top.get(1).cloned().expect("the picker opened on its tiles");
        assert!(app.pointer_cmd(aim(&tile)).is_ok());
        assert_eq!(hovered(&app), Some(tile.clone()));
        assert!(app.pointer_cmd(wheel(-1.0)).is_ok());
        assert_eq!(tiles(&app), top, "the list was at its top");
        assert_eq!(hovered(&app), Some(tile.clone()), "nothing moved under it");
        let cell = app.pointer_cmd(wheel(1.0)).expect("the wheel scrolls");
        assert_ne!(tiles(&app), top, "the list scrolled");
        let under = match app.pointer_reply(cell) {
            Ok(PointerAt::Native { key, .. }) => key,
            other => panic!("a native answer: {other:?}"),
        };
        assert_ne!(under.as_deref(), Some(tile.as_str()), "the tile moved away");
        assert_eq!(hovered(&app), under, "the hover went with the move");
    }

    #[test]
    fn a_pointer_wheel_puts_a_press_nudge_back_on_the_lists_stops() {
        use super::{NativePointerTarget, WheelUnit};
        use crate::native_ui::LogicalRect;
        let wheel = |dy: f64| PointerAction::Wheel {
            dx: 0.0,
            dy,
            unit: WheelUnit::Lines,
            aim: None,
        };
        let place = |app: &crate::App, wid| {
            app.compiled_native_ui(wid)
                .expect("the page compiles")
                .semantics
                .iter()
                .filter(|node| {
                    !node.offscreen && node.key.as_str().starts_with("settings/control/")
                })
                .map(|node| (node.key.as_str().to_string(), node.rect.y, node.rect.height))
                .collect::<Vec<_>>()
        };
        let security = |size| {
            let (app, wid, _) = settings_sized_in(
                crate::App::headless_for_test(),
                crate::native_settings::SettingsRoute::Security,
                size,
            );
            let compiled = app.compiled_native_ui(wid).expect("the page compiles");
            let cut = compiled
                .paint
                .iter()
                .filter(|node| matches!(node.content, crate::native_ui::UiContent::Switch(_)))
                .find_map(|node| {
                    let picture = compiled
                        .list_pictures
                        .iter()
                        .find(|picture| picture.key == node.key)?
                        .rect;
                    (compiled.hits.iter().all(|hit| hit.key != node.key)
                        && picture.y == node.rect.y
                        && picture.height > 4.0
                        && node.fade.is_some_and(|(top, _)| top <= node.rect.y + 0.5))
                    .then(|| (node.key.as_str().to_string(), node.rect, picture))
                })?;
            Some((app, wid, cut))
        };
        let press =
            |app: &mut crate::App,
             wid,
             (cut, rect, picture): &(String, LogicalRect, LogicalRect)| {
                let (x, y) = (picture.x + picture.width / 2.0, picture.y + 2.0);
                assert!(
                    app.pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Point(x, y)))
                        .is_ok()
                );
                let on = app
                    .compiled_native_ui(wid)
                    .expect("the page compiles")
                    .paint
                    .iter()
                    .find_map(|node| match &node.content {
                        crate::native_ui::UiContent::Switch(control)
                            if node.key.as_str() == cut =>
                        {
                            Some(control.value.clone())
                        }
                        _ => None,
                    })
                    .expect("the switch is drawn");
                assert!(app.pointer_cmd(PointerAction::Click).is_ok());
                let nudged = place(app, wid);
                let y1 = nudged
                    .iter()
                    .find(|(key, ..)| key == cut)
                    .map(|(_, y, _)| *y)
                    .expect("the switch is live now");
                assert!(
                    y1 < rect.y - 0.5,
                    "{cut}: nudged up by its part under the room's edge, not by a stop: {rect:?} -> {y1}"
                );
                let compiled = app.compiled_native_ui(wid).expect("the page compiles");
                assert_eq!(
                    compiled
                        .semantic(&crate::native_ui::UiKey::new(cut.as_str()))
                        .map(|node| node.value.clone()),
                    Some(on),
                    "it turned nothing"
                );
                nudged
            };
        let mut checked = 0;
        for size in [(600, 360), (640, 400), (738, 418), (800, 480), (1_000, 600)] {
            let Some((mut app, wid, cut)) = security(size) else {
                continue;
            };
            let top = place(&app, wid);
            let (x, y) = (cut.2.x + cut.2.width / 2.0, cut.2.y + 2.0);
            assert!(
                app.pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Point(x, y)))
                    .is_ok()
            );
            assert!(app.pointer_cmd(wheel(1.0)).is_ok());
            let one_on = place(&app, wid);
            assert_ne!(one_on, top, "{size:?}: the wheel scrolled the list");
            assert!(app.pointer_cmd(wheel(-1.0)).is_ok());
            assert_eq!(place(&app, wid), top, "{size:?}: back at the top");
            let nudged = press(&mut app, wid, &cut);
            assert!(app.pointer_cmd(PointerAction::Status).is_ok());
            assert_eq!(place(&app, wid), nudged, "{size:?}: the nudge stands");
            assert!(app.pointer_cmd(wheel(1.0)).is_ok());
            assert_eq!(
                place(&app, wid),
                one_on,
                "{size:?}: down a line from the nudge is a stop on"
            );
            let (mut app, wid, cut) = security(size).expect("the same page");
            let _ = press(&mut app, wid, &cut);
            assert!(app.pointer_cmd(wheel(-1.0)).is_ok());
            assert_eq!(
                place(&app, wid),
                top,
                "{size:?}: up a line from the nudge is the top"
            );
            checked += 1;
        }
        assert!(checked > 0, "some size cuts a switch at the list's bottom");
    }

    fn file_uri(path: &std::path::Path) -> String {
        crate::native_document_host::path_to_file_uri(path).unwrap()
    }

    #[test]
    fn the_hand_the_press_and_the_caption_agree_on_a_plain_url() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        crate::term_lock(&term).process(
            b"Docs: https://google.com\x1b[8m.evil.example/login\x1b[28m (official)\r\n\
              Docs: \x1b[48;2;17;19;24mhttps://google.com\x1b[38;2;40;43;50m.evil.example/login\x1b[0m\r\n\
              Docs: https://example.com/path/to/page\r\n\
              Docs: \x1b[38;2;33;34;44;48;2;40;42;54mhttps://example.com/path\x1b[0m\r\n",
        );
        app.on_modifiers_changed(wid, link_modifier());
        let (cw, ch) = app.win_cell_size(wid);
        let strip_px = f64::from(app.tab_strip_rows) * ch as f64;
        let hand = |app: &mut crate::App, row: u16, col: u16| {
            let x = app.win_pad(wid) as f64 + cw as f64 * (f64::from(col) + 0.5);
            let y = (app.win_pad_top(wid) + app.win_head(wid)) as f64
                + strip_px
                + ch as f64 * (f64::from(row) + 0.5);
            app.on_cursor_moved(wid, x, y);
            assert_eq!(app.windows[&wid].last_mouse_cell, (row, col));
            app.windows[&wid].hover_pointer
        };
        for row in 0..3 {
            for col in 0..80 {
                let offered = hand(&mut app, row, col);
                assert_eq!(
                    offered,
                    app.link_under_pointer(wid).is_some(),
                    "({row}, {col})"
                );
            }
        }
        let band = |app: &mut crate::App| {
            assert!(
                app.prepare_terminal_capture_grid_with_cursor_fx(
                    wid,
                    crate::app_render::ComposedCursorFxClock::Advance(std::time::Instant::now()),
                )
                .is_some()
            );
            app.splice_link_target(wid);
            let rows = &app.windows[&wid].input_scratch.cells;
            rows.last()
                .expect("rows")
                .iter()
                .map(|cell| cell.ch)
                .collect::<String>()
        };
        assert!(hand(&mut app, 0, 20), "the visible URL takes the hand");
        assert!(
            !band(&mut app).contains("google.com"),
            "and is its own disclosure"
        );
        assert!(!hand(&mut app, 0, 30), "a cell that looks blank never does");
        let whole = "https://google.com.evil.example/login";
        assert!(hand(&mut app, 1, 30), "a faint tail is still the URL's");
        assert_eq!(app.link_under_pointer(wid).as_deref(), Some(whole));
        let caption = band(&mut app);
        assert!(
            caption.contains(whole),
            "the band names it first: {caption:?}"
        );
        assert!(hand(&mut app, 2, 20));
        let disclosed = |app: &crate::App| app.windows[&wid].link_hover;
        assert_eq!(disclosed(&app), None, "an ordinary URL takes no row");
        app.render_knobs.minimum_contrast = 4.5;
        assert!(hand(&mut app, 3, 20));
        assert_eq!(
            disclosed(&app),
            None,
            "a URL the configured floor lifts takes no row"
        );
    }

    #[test]
    fn divider_wobble_across_a_cell_edge_never_regrids_the_siblings() {
        use aterm_window::event::{ElementState, MouseButton};

        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let top = app.focused_session_id(wid).unwrap();
        let bottom = app.split_active_stub_tab_dir(wid, crate::pane::SplitDir::Horizontal);
        let (rows, cols) = (app.windows[&wid].rows, app.windows[&wid].cols);
        let col = cols / 4;
        let divider = (0..rows)
            .find(|&r| {
                app.active_tree(wid)
                    .expect("terminal split tree")
                    .divider_at(r, col, rows, cols)
                    .is_some()
            })
            .expect("the split has a divider row");
        let ch = app.pointer_geometry(wid).cell.1;
        assert!(
            ch / 4 >= 2,
            "a cell tall enough for a 1 px nudge to sit in the band"
        );
        let engine_rows = |app: &crate::App| {
            let rows = |sid| crate::term_lock(&app.pool.get(sid).unwrap().term).rows();
            (rows(top), rows(bottom))
        };
        let before = engine_rows(&app);

        let (x, y) = app.cell_centre_px(wid, divider, col).unwrap();
        let (_, below) = app.cell_centre_px(wid, divider + 1, col).unwrap();
        #[allow(clippy::cast_precision_loss, reason = "a cell height in px")]
        let nudge = below - (ch / 2) as f64 + 1.0;
        assert_eq!(
            app.pixel_to_cell(wid, x, nudge).0,
            divider + 1,
            "the nudge really crosses into the next cell"
        );

        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        assert!(
            app.windows[&wid].divider_drag.is_some(),
            "press arms the drag"
        );
        let mut seen = vec![engine_rows(&app)];
        app.on_cursor_moved(wid, x, nudge);
        seen.push(engine_rows(&app));
        app.on_cursor_moved(wid, x, y);
        seen.push(engine_rows(&app));
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        seen.push(engine_rows(&app));
        assert!(
            app.windows[&wid].divider_drag.is_none(),
            "release ends the drag"
        );
        assert!(
            seen.iter().all(|&s| s == before),
            "a wobble inside the dead band never re-grids either pane: \
             before={before:?} seen={seen:?}"
        );

        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        assert!(
            app.windows[&wid].divider_drag.is_some(),
            "press re-arms the drag"
        );
        app.on_cursor_moved(wid, x, below);
        assert_eq!(
            engine_rows(&app),
            (before.0 + 1, before.1 - 1),
            "a real one-row move applies at once — the band adds no latency"
        );
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        assert_eq!(engine_rows(&app), (before.0 + 1, before.1 - 1));
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn divider_drag_flushes_a_coalesced_step_on_a_move_inside_the_band() {
        use aterm_window::event::{ElementState, MouseButton};

        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let top = app.focused_session_id(wid).unwrap();
        let bottom = app.split_active_stub_tab_dir(wid, crate::pane::SplitDir::Horizontal);
        let (rows, cols) = (app.windows[&wid].rows, app.windows[&wid].cols);
        let col = cols / 4;
        let divider = (0..rows)
            .find(|&r| {
                app.active_tree(wid)
                    .expect("terminal split tree")
                    .divider_at(r, col, rows, cols)
                    .is_some()
            })
            .expect("the split has a divider row");
        let engine_rows = |app: &crate::App| {
            let rows = |sid| crate::term_lock(&app.pool.get(sid).unwrap().term).rows();
            (rows(top), rows(bottom))
        };
        let before = engine_rows(&app);
        let (x, y) = app.cell_centre_px(wid, divider, col).unwrap();
        let (_, one) = app.cell_centre_px(wid, divider + 1, col).unwrap();
        let (_, two) = app.cell_centre_px(wid, divider + 2, col).unwrap();

        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_cursor_moved(wid, x, one);
        assert_eq!(
            engine_rows(&app),
            (before.0 + 1, before.1 - 1),
            "the first step is the leading edge"
        );
        super::PANE_DRAG_APPLIED_AT.with(|c| c.set(Some((wid, std::time::Instant::now()))));
        app.on_cursor_moved(wid, x, two);
        assert_eq!(
            super::PANE_DRAG_PENDING.with(|c| c.get()),
            Some(wid),
            "the in-window step is pending, not applied"
        );
        assert_eq!(engine_rows(&app), (before.0 + 1, before.1 - 1));
        let past = std::time::Instant::now()
            .checked_sub(crate::RESIZE_THROTTLE * 2)
            .expect("a monotonic clock older than two throttle windows");
        super::PANE_DRAG_APPLIED_AT.with(|c| c.set(Some((wid, past))));
        app.on_cursor_moved(wid, x, two + 1.0);
        assert_eq!(
            engine_rows(&app),
            (before.0 + 2, before.1 - 2),
            "a move inside the band flushes the coalesced step"
        );
        assert_eq!(super::PANE_DRAG_PENDING.with(|c| c.get()), None);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        assert_eq!(engine_rows(&app), (before.0 + 2, before.1 - 2));
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn divider_drag_offset_is_read_against_the_clamped_cell() {
        use super::divider_drag_off_axis as off;
        assert_eq!(off(187, 9, 20), 7, "inside the cell: the real offset");
        assert_eq!(off(203, 9, 20), 19, "past the last row: its far pixel");
        assert_eq!(off(455, 9, 20), 19, "far past it: still its far pixel");
        assert_eq!(off(3, 1, 20), 0, "before the cell: its first pixel");
        assert_eq!(off(5, 0, 0), 0, "no geometry: no offset");
        assert_eq!(super::divider_drag_axis(8, 9, off(203, 9, 20), 20), 9);
    }

    fn pane_header_point(app: &crate::App, wid: crate::WindowId, index: usize) -> (f64, f64) {
        let plan = app.active_visible_leaf_plan(wid).expect("split plan");
        let header = plan.leaves[index].header.expect("pane header");
        app.cell_centre_px(wid, header.origin.y as u16, header.origin.x as u16 + 1)
            .expect("header cell")
    }

    #[test]
    fn pane_header_click_focuses_without_selecting_or_reporting_any_button() {
        use aterm_window::event::{ElementState, MouseButton};

        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let left = app.focused_session_id(wid).unwrap();
        app.split_active_stub_tab(wid);
        let header = pane_header_point(&app, wid, 0);
        app.on_cursor_moved(wid, header.0, header.1);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        assert_eq!(app.focused_session_id(wid), Some(left));
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        assert!(!app.windows[&wid].selecting);

        let term = app.front_terminal(wid).unwrap().term.clone();
        for tracking in [false, true] {
            crate::term_lock(&term).process(if tracking {
                b"\x1b[?1003h"
            } else {
                b"\x1b[?1003l"
            });
            for button in [
                MouseButton::Left,
                MouseButton::Middle,
                MouseButton::Right,
                MouseButton::Back,
                MouseButton::Forward,
            ] {
                app.on_mouse_input(wid, ElementState::Pressed, button);
                assert_eq!(app.windows[&wid].reported_buttons, 0);
                assert!(!app.windows[&wid].selecting);
                app.on_mouse_input(wid, ElementState::Released, button);
                assert!(app.windows[&wid].held_mouse_button.is_none());
            }
        }
        crate::term_lock(&term).process(b"\x1b[?1003l");
        let (row, col, _, _) = app.focused_pane_rect(wid);
        let content = app.cell_centre_px(wid, row, col + 1).unwrap();
        app.on_cursor_moved(wid, content.0, content.1);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        assert!(app.windows[&wid].selecting);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    }

    #[test]
    fn pane_header_hover_and_wheel_never_reach_terminal_input() {
        use aterm_window::event::MouseScrollDelta;

        let (mut app, wid, _, _) = stacked_autoscroll_fixture();
        let term = app.front_terminal(wid).unwrap().term.clone();
        let header = pane_header_point(&app, wid, 1);
        for tracking in [false, true] {
            crate::term_lock(&term).process(if tracking {
                b"\x1b[?1003h"
            } else {
                b"\x1b[?1003l"
            });
            let before = crate::term_lock_acquisitions_on_this_thread();
            app.on_cursor_moved(wid, header.0, header.1);
            app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 3.0));
            app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 0.5));
            app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.5, 0.0));
            app.on_mouse_wheel(
                wid,
                MouseScrollDelta::PixelDelta(aterm_window::dpi::PhysicalPosition::new(0.0, 0.5)),
            );
            assert_eq!(
                crate::term_lock_acquisitions_on_this_thread(),
                before,
                "header hover/wheel never enters the terminal seam, including any-motion tracking"
            );
            let window = &app.windows[&wid];
            assert_eq!(
                (window.scroll_residual, window.scroll_residual_x),
                (0.0, 0.0)
            );
            assert!(window.scroll_glide.is_none());
            assert!(!window.hover_grid_owned);
            assert!(window.hover_pointer && !window.native_text_cursor);
            assert_eq!(crate::term_lock(&term).grid().display_offset(), 0);
        }
        assert!(
            app.pane_header_at_with(wid, app.pointer_geometry(wid), -1.0, header.1)
                .is_none()
        );
        assert!(
            app.pane_header_at_with(wid, app.pointer_geometry(wid), header.0, -1.0)
                .is_none()
        );
    }

    #[test]
    fn pane_header_crossing_preserves_selection_and_reported_button_releases() {
        use aterm_window::event::{ElementState, MouseButton};

        let (mut app, wid, _, bottom) = stacked_autoscroll_fixture();
        let (row, col, _, _) = app.focused_pane_rect(wid);
        let content = app.cell_centre_px(wid, row + 1, col + 2).unwrap();
        let header = pane_header_point(&app, wid, 1);
        let term = app.front_terminal(wid).unwrap().term.clone();
        for tracking in [false, true] {
            crate::term_lock(&term).process(if tracking {
                b"\x1b[?1003h"
            } else {
                b"\x1b[?1003l"
            });
            app.on_cursor_moved(wid, content.0, content.1);
            app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
            assert_eq!(app.windows[&wid].selecting, !tracking);
            assert_ne!(app.windows[&wid].reported_buttons, 0);
            app.on_cursor_moved(wid, header.0, header.1);
            app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
            assert!(!app.windows[&wid].selecting);
            assert!(app.windows[&wid].held_mouse_button.is_none());
            assert_eq!(app.windows[&wid].reported_buttons, 0);
            assert_eq!(app.focused_session_id(wid), Some(bottom));
        }
    }

    #[test]
    fn pane_header_focuses_native_leaf_and_does_not_scroll_its_content() {
        use crate::native_app::AppViewState;
        use aterm_window::event::{ElementState, MouseButton, MouseScrollDelta};

        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, native) = app.active_native_view(wid).unwrap();
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        let header = pane_header_point(&app, wid, 0);
        app.on_cursor_moved(wid, header.0, header.1);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        assert_eq!(
            app.active_native_view(wid).map(|(_, view)| view),
            Some(native)
        );
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state_mut(native)
        else {
            panic!("Settings view");
        };
        settings.scroll_page_to(1);
        app.on_cursor_moved(wid, header.0, header.1);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 3.0));
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(native) else {
            panic!("Settings view");
        };
        assert_eq!(settings.page_scroll, 1);
        assert!(settings.common.pressed.is_none());
        assert!(settings.common.hovered.is_none());
        assert!(app.windows[&wid].hover_pointer);
    }

    #[test]
    fn the_pointer_verb_reports_only_where_the_pointer_is() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        assert_eq!(
            app.pointer_cmd(PointerAction::Status),
            Ok(None),
            "a window that has never seen the pointer claims no position"
        );
        assert_eq!(
            app.pointer_cmd(PointerAction::Move { row: 3, col: 5 }),
            Ok(Some((3, 5)))
        );
        let (rows, cols) = (app.windows[&wid].rows, app.windows[&wid].cols);
        for (row, col) in [(rows, 0), (0, cols)] {
            assert_eq!(
                app.pointer_cmd(PointerAction::Move { row, col }),
                Err("cell is outside the grid".to_string())
            );
        }
        assert_eq!(
            app.pointer_cmd(PointerAction::Status),
            Ok(Some((3, 5))),
            "a refused move left the pointer where it was"
        );
        assert_eq!(app.pointer_cmd(PointerAction::Leave), Ok(None));
        assert_eq!(
            app.pointer_cmd(PointerAction::Status),
            Ok(None),
            "the departed pointer's last cell must not be reported as where it is"
        );
        assert_eq!(app.windows[&wid].last_mouse_window_cell, (3, 5));
    }

    #[test]
    fn every_cell_maps_to_a_pixel_that_maps_back_to_that_cell() {
        let app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let (rows, cols) = (app.windows[&wid].rows, app.windows[&wid].cols);
        for row in 0..rows {
            for col in 0..cols {
                let (x, y) = app
                    .cell_centre_px(wid, row, col)
                    .expect("an in-grid cell has a centre");
                assert_eq!(
                    app.pixel_to_cell(wid, x, y),
                    (row, col),
                    "cell ({row}, {col}) round-trips through pixel ({x}, {y})"
                );
            }
        }
    }

    fn native_palette_row_center(
        app: &crate::App,
        wid: crate::WindowId,
        slot: usize,
    ) -> (f64, f64) {
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid);
        let head = app.win_head(wid);
        let window = &app.windows[&wid];
        let scale = window.scale.max(f64::EPSILON);
        let geom = crate::settings::SettingsGeom {
            cw: cw as f32 / scale as f32,
            ch: ch as f32 / scale as f32,
            font_px: 13.0,
            cols: window.cols as usize,
            panel_rows: window.overlay_rows(),
            px_per_pt: 1.0,
        };
        let (rx, ry, rw, rh) =
            crate::palette::palette_row_rect(window.palette().expect("open palette"), &geom, slot)
                .expect("visible palette row");
        let (ox, oy) = app.frame_origin(wid);
        (
            ox as f64 + pad as f64 + f64::from(rx + rw * 0.5) * scale,
            oy as f64
                + (head + usize::from(app.chrome_rows(wid)) * ch) as f64
                + f64::from(ry + rh * 0.5) * scale,
        )
    }

    fn native_content_to_window(
        app: &mut crate::App,
        wid: crate::WindowId,
        x: f32,
        y: f32,
    ) -> (f64, f64) {
        let view = prepare_presented_native(app, wid);
        let artifact = app
            .retained_native_leaf_artifact(wid, view, true)
            .expect("presented native semantic artifact");
        (
            artifact.device_x as f64 + f64::from(x) * artifact.scale,
            artifact.device_y as f64 + f64::from(y) * artifact.scale,
        )
    }

    fn prepare_presented_native(
        app: &mut crate::App,
        wid: crate::WindowId,
    ) -> crate::tab_model::ViewId {
        let (_, view) = app.active_native_view(wid).expect("active native view");
        assert!(app.prepare_native_input_scratch(wid));
        app.windows
            .get_mut(&wid)
            .and_then(|window| window.leaf_render_cache.get_mut(&view))
            .and_then(|cache| cache.native.as_mut())
            .expect("retained native raster")
            .presented = true;
        assert!(
            app.retained_native_leaf_artifact(wid, view, true).is_some(),
            "prepared native artifact must satisfy the shipping glass guard"
        );
        view
    }

    fn presented_native_ui(
        app: &mut crate::App,
        wid: crate::WindowId,
    ) -> crate::native_ui::CompiledUi {
        let view = prepare_presented_native(app, wid);
        app.retained_native_leaf_artifact(wid, view, true)
            .expect("presented native semantic artifact")
            .compiled
            .clone()
    }

    fn native_text_boundary_point(
        app: &mut crate::App,
        wid: crate::WindowId,
        key: &crate::native_ui::UiKey,
        byte: usize,
    ) -> (f64, f64) {
        let compiled = presented_native_ui(app, wid);
        let paint = compiled
            .paint
            .iter()
            .find(|node| &node.key == key)
            .expect("painted text field");
        let crate::native_ui::UiContent::TextField(control) = &paint.content else {
            panic!("target is a text field");
        };
        let text = control.spec.input.as_ref().map_or_else(
            || match &control.value {
                crate::native_ui::SemanticValue::Text(value) => value.as_str(),
                _ => "",
            },
            |input| input.text.as_str(),
        );
        let geometry = crate::native_ui::text_field_geometry(control, paint.rect, 13.0);
        let x = crate::native_ui::text_field_x_for_byte(text, &geometry, byte, 13.0);
        assert_eq!(
            compiled.text_field_byte_at(key, x),
            Some(byte),
            "test point must invert through shipping hit geometry"
        );
        native_content_to_window(app, wid, x, paint.rect.y + paint.rect.height * 0.5)
    }

    #[test]
    fn a_press_on_a_switch_drawn_in_the_lists_strip_scrolls_it_in_and_works_nothing() {
        use crate::native_ui::UiContent;
        use crate::{App, WindowId};
        use aterm_window::event::{ElementState, MouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.on_resize(wid, aterm_window::dpi::PhysicalSize::new(760, 460));
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).expect("Settings active");
        let switch_picture = |compiled: &crate::native_ui::CompiledUi| {
            compiled.list_pictures.iter().find_map(|picture| {
                let paint = compiled.paint.iter().find(|node| node.key == picture.key)?;
                let UiContent::Switch(control) = &paint.content else {
                    return None;
                };
                (picture.rect.height >= 12.0).then(|| (picture.clone(), control.value.clone()))
            })
        };
        let mut found = switch_picture(&presented_native_ui(&mut app, wid));
        for _ in 0..40 {
            if found.is_some() {
                break;
            }
            let _ = app.dispatch_native_view_event(
                wid,
                view,
                crate::native_app::AppEvent::ScrollLines(1),
            );
            found = switch_picture(&presented_native_ui(&mut app, wid));
        }
        let (picture, value) = found.expect("a screen of Top Settings draws a switch in its strip");
        let (x, y) = native_content_to_window(
            &mut app,
            wid,
            picture.rect.x + picture.rect.width * 0.5,
            picture.rect.y + picture.rect.height * 0.5,
        );
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let compiled = presented_native_ui(&mut app, wid);
        let key = &picture.key;
        let paint = compiled
            .paint
            .iter()
            .find(|node| &node.key == key)
            .expect("the switch is drawn");
        let hit = compiled
            .hits
            .iter()
            .find(|hit| &hit.key == key)
            .unwrap_or_else(|| panic!("{key:?} came into the room, live: {:?}", paint.rect));
        assert_eq!(hit.rect, paint.rect, "{key:?} is whole in the room");
        let semantic = compiled.semantic(key).expect("live");
        assert_eq!(semantic.value, value, "the press worked nothing");
        assert!(
            semantic
                .state
                .is_some_and(|state| !state.busy && !state.pressed),
            "and asked for no change: {:?}",
            semantic.state
        );
    }

    #[test]
    fn a_press_on_a_preview_drawn_in_the_lists_strip_scrolls_it_in_and_pauses_nothing() {
        use crate::native_ui::UiContent;
        use crate::{App, WindowId};
        use aterm_window::event::{ElementState, MouseButton};

        let preview_picture = |compiled: &crate::native_ui::CompiledUi| {
            compiled.list_pictures.iter().find_map(|picture| {
                let paint = compiled.paint.iter().find(|node| node.key == picture.key)?;
                let UiContent::SettingsPreview(spec) = &paint.content else {
                    return None;
                };
                (picture.rect.height >= 12.0).then(|| (picture.clone(), spec.paused))
            })
        };
        let wid = WindowId(0);
        let mut found = None;
        for (width, height) in [(760, 520), (1_100, 740), (760, 460)] {
            let mut app = App::headless_for_test();
            // Top Settings as it lists with a trail drawn (Windows draws none by default).
            app.native_config_service = crate::native_config_service::VersionedConfigService::new(
                crate::app_config::trail_drawn(""),
            )
            .unwrap();
            app.on_resize(wid, aterm_window::dpi::PhysicalSize::new(width, height));
            assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
            let (_, view) = app.active_native_view(wid).expect("Settings active");
            for _ in 0..12 {
                if let Some(picture) = preview_picture(&presented_native_ui(&mut app, wid)) {
                    found = Some((app, picture));
                    break;
                }
                let _ = app.dispatch_native_view_event(
                    wid,
                    view,
                    crate::native_app::AppEvent::ScrollLines(1),
                );
            }
            if found.is_some() {
                break;
            }
        }
        let (mut app, (picture, paused)) =
            found.expect("a screen of Top Settings draws a preview in its strip");
        let (x, y) = native_content_to_window(
            &mut app,
            wid,
            picture.rect.x + picture.rect.width * 0.5,
            picture.rect.y + picture.rect.height * 0.5,
        );
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let compiled = presented_native_ui(&mut app, wid);
        let key = &picture.key;
        let paint = compiled
            .paint
            .iter()
            .find(|node| &node.key == key)
            .expect("the preview is drawn");
        let hit = compiled
            .hits
            .iter()
            .find(|hit| &hit.key == key)
            .unwrap_or_else(|| panic!("{key:?} came into the room, live: {:?}", paint.rect));
        assert_eq!(hit.rect, paint.rect, "{key:?} is whole in the room");
        let UiContent::SettingsPreview(spec) = &paint.content else {
            unreachable!()
        };
        assert_eq!(spec.paused, paused, "the press paused nothing");
    }

    #[test]
    fn palette_over_native_tab_swallows_pointer_before_underlying_hit_test() {
        use crate::native_app::AppViewState;
        use crate::{App, WindowId};
        use aterm_window::event::{ElementState, MouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).expect("Settings active");
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state_mut(view) else {
            panic!("Settings state");
        };
        settings.search_input = crate::native_text_input::TextInputState::new("écho".to_string());
        settings.search_input.set_selection(2, 2);
        settings.set_search("écho".to_string());
        assert!(app.prepare_native_input_scratch(wid));
        app.windows
            .get_mut(&wid)
            .unwrap()
            .leaf_render_cache
            .get_mut(&view)
            .unwrap()
            .native
            .as_mut()
            .unwrap()
            .presented = true;
        let artifact = app
            .retained_native_leaf_artifact(wid, view, true)
            .expect("presented Settings UI");
        let hit = artifact
            .compiled
            .hits
            .iter()
            .find(|hit| hit.key.as_str() == "settings/search")
            .expect("actionable Settings search field");
        let scale = artifact.scale;
        let x = artifact.device_x as f64 + f64::from(hit.rect.x + hit.rect.width * 0.5) * scale;
        let y = artifact.device_y as f64 + f64::from(hit.rect.y + hit.rect.height * 0.5) * scale;

        let point = app.native_content_point(wid, x, y).unwrap();
        assert!(artifact.compiled.hit_test(point.0, point.1).is_some());
        app.on_cursor_moved(wid, x, y);
        let before_focus = app
            .native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.clone());
        let before_selection = match app.native_runtime.view_state(view).unwrap() {
            AppViewState::Settings(settings) => settings.search_input.selection().clone(),
            _ => unreachable!(),
        };
        assert!(
            app.native_runtime
                .view_state(view)
                .and_then(|state| state.common().hovered.as_ref())
                .is_some(),
            "underlying hover is live before the modal opens"
        );

        app.palette_enter();
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let common = app.native_runtime.view_state(view).unwrap().common();
        assert!(common.hovered.is_none());
        assert!(common.pressed.is_none());
        assert_eq!(
            common.last_focus, before_focus,
            "click never reached Settings"
        );
        let AppViewState::Settings(settings) = app.native_runtime.view_state(view).unwrap() else {
            unreachable!();
        };
        assert_eq!(settings.search_input.selection(), &before_selection);
        assert!(app.windows[&wid].palette().is_some(), "modal remains open");
        assert!(!app.windows[&wid].native_text_cursor);
        assert!(!app.windows[&wid].hover_pointer);

        app.palette_exit();
        assert!(
            app.native_runtime
                .view_state(view)
                .and_then(|state| state.common().hovered.as_ref())
                .is_some(),
            "closing immediately restores hover/cursor ownership to revealed content"
        );
    }

    #[test]
    fn settings_text_pointer_click_drag_and_shift_click_use_shipping_reducer_path() {
        use crate::native_app::AppViewState;
        use crate::native_text_input::TextInputState;
        use crate::native_ui::UiKey;
        use crate::{App, WindowId};
        use aterm_window::event::{ElementState, MouseButton};
        use aterm_window::keyboard::ModifiersState;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).expect("Settings active");
        let suffix = "Aé👩‍💻末";
        let text = format!("{}{}", "long-prefix-".repeat(400), suffix);
        let suffix_start = text.len() - suffix.len();
        let woman_start = suffix_start + "Aé".len();
        let woman_end = woman_start + "👩‍💻".len();
        let e_start = suffix_start + "A".len();
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state_mut(view) else {
            panic!("Settings state");
        };
        settings.search_input = TextInputState::new(text.clone());
        settings.set_search(text);
        settings.common.last_focus = Some(UiKey::new("settings/search"));

        let key = UiKey::new("settings/search");
        let (press_x, press_y) = native_text_boundary_point(&mut app, wid, &key, woman_start);
        app.on_cursor_moved(wid, press_x, press_y);
        assert!(app.windows[&wid].native_text_cursor);
        assert!(!app.windows[&wid].hover_pointer);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
            panic!("Settings state");
        };
        assert_eq!(
            settings.search_input.selection().range(),
            woman_start..woman_start
        );
        assert_eq!(settings.common.pressed.as_ref(), Some(&key));

        let (release_x, release_y) = native_text_boundary_point(&mut app, wid, &key, woman_end);
        app.on_cursor_moved(wid, release_x, release_y);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
            panic!("Settings state");
        };
        assert_eq!(settings.search_input.selected_text(), "👩‍💻");
        assert!(settings.common.pressed.is_none());

        let (click_x, click_y) = native_text_boundary_point(&mut app, wid, &key, woman_end);
        app.on_cursor_moved(wid, click_x, click_y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        let (click_x, click_y) = native_text_boundary_point(&mut app, wid, &key, woman_end);
        app.on_cursor_moved(wid, click_x, click_y);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
            unreachable!();
        };
        assert_eq!(
            settings.search_input.selection().range(),
            woman_end..woman_end
        );

        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::SHIFT;
        let (shift_x, shift_y) = native_text_boundary_point(&mut app, wid, &key, e_start);
        app.on_cursor_moved(wid, shift_x, shift_y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        let (shift_x, shift_y) = native_text_boundary_point(&mut app, wid, &key, e_start);
        app.on_cursor_moved(wid, shift_x, shift_y);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
            unreachable!();
        };
        assert_eq!(settings.search_input.selection().anchor, woman_end);
        assert_eq!(settings.search_input.selection().head, e_start);
        assert_eq!(settings.search_input.selected_text(), "é👩‍💻");
    }

    #[test]
    fn settings_color_field_padding_and_swatch_clamp_to_committed_edges() {
        use crate::native_app::AppViewState;
        use crate::native_text_input::TextInputState;
        use crate::native_ui::UiKey;
        use crate::{App, WindowId};
        use aterm_window::event::{ElementState, MouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
        let (_, view) = app.active_native_view(wid).expect("Settings active");
        let field_key = crate::prefs::EDIT_FOREGROUND;
        let value = "#123456";
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state_mut(view) else {
            panic!("Settings state");
        };
        settings.search_input = TextInputState::new("foreground".to_string());
        settings.set_search("foreground".to_string());
        settings.scroll_page_to(0);
        settings
            .legacy
            .fields
            .iter_mut()
            .find(|field| field.key == field_key)
            .expect("foreground preference")
            .seed = Some(value.to_string());

        let key = UiKey::new(format!("settings/control/{field_key}"));
        let edge_point = |app: &mut App, trailing: bool| {
            let compiled = presented_native_ui(app, wid);
            let paint = compiled
                .paint
                .iter()
                .find(|node| node.key == key)
                .expect("painted foreground field");
            let x = if trailing {
                paint.rect.right() - 1.0
            } else {
                paint.rect.x + 1.0
            };
            assert_eq!(
                compiled.text_field_byte_at(&key, x),
                Some(if trailing { value.len() } else { 0 })
            );
            native_content_to_window(app, wid, x, paint.rect.y + paint.rect.height * 0.5)
        };

        let (x, y) = edge_point(&mut app, true);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        let (x, y) = edge_point(&mut app, true);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
            unreachable!();
        };
        assert_eq!(settings.editing_field.as_deref(), Some(field_key));
        assert_eq!(
            settings.field_inputs[field_key].selection().range(),
            value.len()..value.len(),
            "swatch click places an end caret"
        );

        let (x, y) = edge_point(&mut app, false);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        let (x, y) = edge_point(&mut app, false);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
            unreachable!();
        };
        assert_eq!(
            settings.field_inputs[field_key].selection().range(),
            0..0,
            "leading padding click places a start caret"
        );

        let (x, y) = edge_point(&mut app, false);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        let (x, y) = edge_point(&mut app, true);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
            unreachable!();
        };
        assert_eq!(settings.field_inputs[field_key].selected_text(), value);
    }

    #[test]
    fn settings_text_release_rejects_a_detached_native_view() {
        use crate::native_app::AppViewState;
        use crate::native_text_input::TextInputState;
        use crate::native_ui::UiKey;
        use crate::{App, WindowId};
        use aterm_window::event::{ElementState, MouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).expect("Settings active");
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state_mut(view) else {
            panic!("Settings state");
        };
        settings.search_input = TextInputState::new("écho".to_string());
        settings.set_search("écho".to_string());
        settings.common.last_focus = Some(UiKey::new("settings/search"));

        let key = UiKey::new("settings/search");
        let (x, y) = native_text_boundary_point(&mut app, wid, &key, 2);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        let (release_x, release_y) = native_text_boundary_point(&mut app, wid, &key, 5);
        app.on_cursor_moved(wid, release_x, release_y);

        let detached = app
            .native_runtime
            .take_view_state(view)
            .expect("detach live Settings view");
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let AppViewState::Settings(settings) = detached else {
            unreachable!();
        };
        assert_eq!(settings.search_input.selection().range(), 2..2);
        assert_eq!(settings.search_input.value(), "écho");
    }

    #[test]
    fn text_field_pointer_payload_does_not_replace_button_slider_or_document_paths() {
        use crate::native_app::{AppKind, AppViewState};
        use crate::native_ui::{UiContent, UiKey};
        use crate::{App, WindowId};
        use aterm_window::event::{ElementState, MouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, settings_view) = app.active_native_view(wid).expect("Settings active");
        app.dispatch_native_event(
            wid,
            crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("settings/compact-navigation"),
                value: None,
            }),
        )
        .unwrap();
        let Some(AppViewState::Settings(settings)) =
            app.native_runtime.view_state_mut(settings_view)
        else {
            panic!("Settings state");
        };
        settings.scroll_page_to(0);

        let compiled = presented_native_ui(&mut app, wid);
        let route_button = compiled
            .hits
            .iter()
            .find(|hit| hit.action.as_str() == "settings/route/modified")
            .expect("visible Modified route button");
        let route_key = route_button.key.clone();
        assert_eq!(
            compiled.text_field_byte_at(&route_key, route_button.rect.x),
            None
        );
        let (x, y) = native_content_to_window(
            &mut app,
            wid,
            route_button.rect.x + route_button.rect.width * 0.5,
            route_button.rect.y + route_button.rect.height * 0.5,
        );
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(settings_view)
        else {
            panic!("Settings state");
        };
        assert_eq!(settings.route, crate::native_settings::SettingsRoute::Home);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(settings_view)
        else {
            unreachable!();
        };
        assert_eq!(
            settings.route,
            crate::native_settings::SettingsRoute::Modified
        );

        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::TextFonts));
        let Some(AppViewState::Settings(settings)) =
            app.native_runtime.view_state_mut(settings_view)
        else {
            panic!("same singleton Settings view");
        };
        settings
            .legacy
            .fields
            .iter_mut()
            .find(|field| field.key == crate::prefs::EDIT_FONT_PX)
            .expect("font size field")
            .seed = Some("14".to_string());
        settings.search_input =
            crate::native_text_input::TextInputState::new("font size".to_string());
        settings.set_search("font size".to_string());
        let slider_key = UiKey::new(format!("settings/control/{}", crate::prefs::EDIT_FONT_PX));
        let mut found = None;
        for scroll in 0..8 {
            let Some(AppViewState::Settings(settings)) =
                app.native_runtime.view_state_mut(settings_view)
            else {
                unreachable!();
            };
            settings.scroll_page_to(scroll);
            let compiled = presented_native_ui(&mut app, wid);
            if let Some(index) = compiled
                .paint
                .iter()
                .position(|node| node.key == slider_key)
            {
                found = Some((compiled, index));
                break;
            }
        }
        let (compiled, index) = found.expect("painted font slider");
        let slider = &compiled.paint[index];
        assert!(matches!(slider.content, UiContent::Slider(_)));
        let geometry = crate::native_ui::slider_geometry(slider.rect);
        let slider_x = geometry.track_x + (geometry.track_right - geometry.track_x) * 0.75;
        assert!(compiled.slider_value_at(&slider_key, slider_x).is_some());
        assert_eq!(compiled.text_field_byte_at(&slider_key, slider_x), None);
        let (x, y) = native_content_to_window(
            &mut app,
            wid,
            slider_x,
            slider.rect.y + slider.rect.height * 0.5,
        );
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(settings_view)
        else {
            unreachable!();
        };
        assert_ne!(
            settings.editing_field.as_deref(),
            Some(crate::prefs::EDIT_FONT_PX),
            "slider remains numeric rather than becoming a text editor"
        );

        let dir = std::env::temp_dir().join(format!(
            "aterm-pointer-negative-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pointer.txt");
        std::fs::write(&path, "alpha βeta\nsecond\n").unwrap();
        let uri = file_uri(&path);
        app.open_document_tab(AppKind::Editor, &uri).unwrap();
        let (_, editor_view) = app.active_native_view(wid).expect("Editor active");
        let compiled = presented_native_ui(&mut app, wid);
        let editor_key = UiKey::new("editor/buffer");
        let editor = compiled
            .paint
            .iter()
            .find(|node| node.key == editor_key)
            .expect("painted editor viewport");
        let UiContent::TextViewport(spec) = &editor.content else {
            panic!("editor body remains a text viewport");
        };
        let geometry = crate::native_ui::text_viewport_geometry(editor.rect);
        let content_x = geometry.text_x + 36.0;
        let content_y = geometry.body_y + geometry.line_h * 0.5;
        let expected =
            crate::native_ui::text_viewport_byte_at(spec, editor.rect, content_x, content_y)
                .expect("materialized source byte");
        assert_eq!(compiled.text_field_byte_at(&editor_key, content_x), None);
        let (x, y) = native_content_to_window(&mut app, wid, content_x, content_y);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Editor(editor)) = app.native_runtime.view_state(editor_view) else {
            panic!("Editor state");
        };
        assert_eq!(
            editor.buffer.as_ref().unwrap().primary_selection().head,
            expected
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wheel_scrolled_native_palette_hit_activates_the_newly_painted_row() {
        use crate::native_app::AppViewState;
        use crate::{App, WindowId};
        use aterm_window::event::{ElementState, MouseButton, MouseScrollDelta};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).expect("Settings active");
        assert!(app.prepare_native_input_scratch(wid));
        app.palette_enter();
        for c in "Settings:".chars() {
            app.palette_filter_push(c);
        }
        let palette = app.windows[&wid].palette().unwrap();
        let count = palette.filtered().len();
        let body = palette.scroll_extent().2;
        assert!(count > body, "Settings commands exceed the visible band");
        let about_index = palette
            .controls_lines()
            .iter()
            .filter(|line| line.starts_with("menu row "))
            .position(|line| line.contains("action=settings/route/about"))
            .expect("About route command");
        let wanted_scroll = about_index.saturating_sub(body - 1);
        assert!(wanted_scroll > 0, "About route exercises wheel scrolling");
        let about_slot = about_index - wanted_scroll;
        let before_page_scroll = match app.native_runtime.view_state(view).unwrap() {
            AppViewState::Settings(settings) => settings.page_scroll,
            _ => panic!("Settings state"),
        };

        let (x, y) = native_palette_row_center(&app, wid, about_slot);
        app.on_cursor_moved(wid, x, y);
        assert!(app.windows[&wid].palette().unwrap().pointer_over_row());
        assert!(
            app.windows[&wid].hover_pointer,
            "row advertises pointer affordance"
        );
        app.on_mouse_wheel(
            wid,
            MouseScrollDelta::LineDelta(0.0, -(wanted_scroll as f32)),
        );
        let palette = app.windows[&wid].palette().unwrap();
        assert_eq!(palette.scroll_extent().0, wanted_scroll);
        assert!(matches!(
            palette.selected_target(),
            Some(crate::palette::PaletteTarget::Native(target))
                if target.action.as_str() == "settings/route/about"
        ));
        assert_eq!(
            match app.native_runtime.view_state(view).unwrap() {
                AppViewState::Settings(settings) => settings.page_scroll,
                _ => unreachable!(),
            },
            before_page_scroll,
            "modal wheel never reaches the native Settings scroller"
        );

        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        assert!(app.windows[&wid].palette().is_some(), "press only arms");
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        assert!(
            app.windows[&wid].palette().is_none(),
            "matched release activates"
        );
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
            panic!("Settings state");
        };
        assert_eq!(settings.route, crate::native_settings::SettingsRoute::About);
    }

    #[test]
    fn pointer_release_rejects_a_native_command_after_its_view_generation_dies() {
        use crate::native_app::AppViewState;
        use crate::{App, WindowId};
        use aterm_window::event::{ElementState, MouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).expect("Settings active");
        assert!(app.prepare_native_input_scratch(wid));
        app.palette_enter();
        for c in "Settings: Go to Themes".chars() {
            app.palette_filter_push(c);
        }
        assert_eq!(app.windows[&wid].palette().unwrap().filtered().len(), 1);
        let (x, y) = native_palette_row_center(&app, wid, 0);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);

        let detached = app
            .native_runtime
            .take_view_state(view)
            .expect("detach live Settings view");
        let before_route = match &detached {
            AppViewState::Settings(settings) => settings.route,
            _ => panic!("Settings state"),
        };
        assert_eq!(app.native_runtime.view_generation(view), None);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);

        assert!(app.windows[&wid].palette().is_none(), "release was settled");
        let AppViewState::Settings(settings) = detached else {
            panic!("Settings state");
        };
        assert_eq!(
            settings.route, before_route,
            "stale generation receives no route action"
        );
    }

    #[test]
    fn terminal_palette_pointer_strips_frame_remainder_and_top_origin_once() {
        use crate::{App, WindowId};
        use aterm_window::dpi::PhysicalSize;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.backend.set_pad(12);
        app.backend.set_pad_top(4);
        app.backend.set_head(3);
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.metrics.pad = 12;
            ws.metrics.pad_top = 4;
            ws.input_scratch.cells = vec![Vec::new(); 24];
        }
        app.palette_enter();
        assert_eq!(
            (app.win_pad(wid), app.win_pad_top(wid), app.win_head(wid)),
            (12, 4, 3),
            "fixture geometry reaches the pointer seams"
        );

        let (cw, ch) = app.win_cell_size(wid);
        let (rows, cols) = {
            let ws = &app.windows[&wid];
            (
                usize::from(ws.rows) + usize::from(app.chrome_rows(wid)),
                usize::from(ws.cols),
            )
        };
        let rw = cw.saturating_sub(1).max(4);
        let rh = ch.saturating_sub(1).max(4);
        app.windows.get_mut(&wid).unwrap().win_px = Some(PhysicalSize::new(
            (cols * cw + 24 + rw) as u32,
            (rows * ch + 4 + 12 + 3 + rh) as u32,
        ));
        let (ox, oy) = app.frame_origin(wid);
        assert!(ox > 0, "fixture needs the horizontal remainder band");
        if cfg!(target_os = "linux") {
            assert_eq!(oy, 0, "Linux pins the frame top");
        } else {
            assert!(oy > 0, "fixture needs the vertical remainder band");
        }

        let window = &app.windows[&wid];
        let palette = window.palette().expect("open palette");
        let geom = crate::settings::SettingsGeom {
            cw: cw as f32,
            ch: ch as f32,
            font_px: app.win_font_px(wid),
            cols,
            panel_rows: window.overlay_rows(),
            px_per_pt: window.scale as f32,
        };
        let visible = 3;
        let to_window = |x: f32, y: f32| {
            (
                ox as f64 + 12.0 + f64::from(x),
                oy as f64 + 4.0 + 3.0 + f64::from(y),
            )
        };
        for slot in 0..visible {
            let (rx, ry, rw, rh) =
                crate::palette::palette_row_rect(palette, &geom, slot).expect("painted row");
            let want = Some(slot);
            for (cx, cy, what) in [
                (rx + 0.5, ry + 0.5, "top-left corner"),
                (rx + rw - 0.5, ry + rh - 0.5, "bottom-right corner"),
                (rx + rw * 0.5, ry + rh * 0.5, "centre"),
            ] {
                let (x, y) = to_window(cx, cy);
                assert_eq!(
                    app.palette_row_at_pointer(wid, x, y),
                    want,
                    "slot {slot} {what}: origin stripped exactly once"
                );
            }
            for (cx, cy, band) in [
                (rx - 0.5, ry + rh * 0.5, "left"),
                (rx + rw + 0.5, ry + rh * 0.5, "right"),
                (rx + rw * 0.5, ry - 0.5, "top"),
                (rx + rw * 0.5, ry + rh + 0.5, "bottom"),
            ] {
                let (x, y) = to_window(cx, cy);
                assert_eq!(
                    app.palette_row_at_pointer(wid, x, y),
                    None,
                    "slot {slot} {band} exterior band misses"
                );
            }
        }
    }

    /// On a Retina window a pointer lands on a palette row as far right as the row is painted: past
    /// where a card capped at 820 device pixels would end, and not one pixel further.
    #[test]
    fn a_retina_pointer_lands_on_a_palette_row_past_a_device_pixel_cap() {
        use crate::{App, WindowId};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.cols = 400;
            ws.scale = 2.0;
            ws.input_scratch.cells = vec![Vec::new(); 50];
        }
        app.palette_enter();
        let (cw, ch) = app.win_cell_size(wid);
        let window = &app.windows[&wid];
        let painted = crate::settings::SettingsGeom {
            cw: cw as f32,
            ch: ch as f32,
            font_px: app.win_font_px(wid),
            cols: usize::from(window.cols),
            panel_rows: window.overlay_rows(),
            px_per_pt: 2.0,
        };
        let t = app
            .overlay_coordinate_transform(wid)
            .expect("the open palette's transform");
        assert_eq!(t.scale, 1.0, "the terminal route: device-pixel geometry");
        let (ox, oy) = app.frame_origin(wid);
        let to_window = |x: f32, y: f32| {
            (
                ox as f64 + t.origin_x + f64::from(x),
                oy as f64 + t.origin_y + f64::from(y),
            )
        };
        let palette = window.palette().expect("open palette");
        for slot in 0..3 {
            let (rx, ry, rw, rh) =
                crate::palette::palette_row_rect(palette, &painted, slot).expect("a painted row");
            assert!(
                rw > 820.0,
                "the 2x row ({rw} px) reaches past a card capped at 820 px"
            );
            let (x, y) = to_window(rx + rw - 0.5, ry + rh * 0.5);
            assert_eq!(
                app.palette_row_at_pointer(wid, x, y),
                Some(slot),
                "slot {slot}'s right end"
            );
            let (x, y) = to_window(rx + rw + 0.5, ry + rh * 0.5);
            assert_eq!(
                app.palette_row_at_pointer(wid, x, y),
                None,
                "slot {slot}: past its painted right end"
            );
        }
    }

    #[test]
    fn click_below_terminal_palette_is_swallowed_without_starting_selection() {
        use crate::{App, WindowId};
        use aterm_window::event::{ElementState, MouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.palette_enter();
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid);
        let head = app.win_head(wid);
        let panel_rows = app.windows[&wid].overlay_rows();
        let (ox, oy) = app.frame_origin(wid);
        let x = ox as f64 + (pad + cw * 3) as f64;
        let y = oy as f64 + (pad + head + ch * (panel_rows + 2)) as f64;
        assert_eq!(app.palette_row_at_pointer(wid, x, y), None);
        let before_cell = app.windows[&wid].last_mouse_cell;

        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);

        let window = &app.windows[&wid];
        assert!(
            window.palette().is_some(),
            "outside click does not dismiss modal"
        );
        assert!(
            !window.selecting,
            "terminal selection gesture never started"
        );
        assert_eq!(
            window.last_mouse_cell, before_cell,
            "terminal pointer seam was bypassed"
        );
        assert!(
            !crate::term_lock(&window.front_terminal().expect("front terminal").term)
                .text_selection()
                .has_selection()
        );
    }

    #[test]
    fn palette_wheel_never_reaches_terminal_scrollback() {
        use crate::{App, WindowId};
        use aterm_window::event::MouseScrollDelta;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        {
            let mut terminal = crate::term_lock(&term);
            for line in 0..100 {
                terminal.process(format!("history {line}\r\n").as_bytes());
            }
            assert!(terminal.grid().scrollback_lines() > 0);
            assert_eq!(terminal.grid().display_offset(), 0);
        }

        app.palette_enter();
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 3.0));
        assert_eq!(crate::term_lock(&term).grid().display_offset(), 0);
        assert!(
            app.windows[&wid].scroll_glide.is_none(),
            "modal wheel cannot arm terminal smooth scrolling"
        );

        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 0.75));
        app.palette_exit();
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 0.5));
        assert!(app.windows[&wid].scroll_glide.is_none());
        assert_eq!(crate::term_lock(&term).grid().display_offset(), 0);

        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 3.0));
        assert!(
            app.windows[&wid].scroll_glide.is_some()
                || crate::term_lock(&term).grid().display_offset() > 0
        );
    }

    #[test]
    fn editor_pointer_selection_updates_only_the_exact_active_native_view() {
        use crate::native_app::{AppKind, AppViewState};
        use crate::{App, WindowId};

        let dir = std::env::temp_dir().join(format!(
            "aterm-editor-pointer-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pointer.md");
        std::fs::write(&path, "alpha\nβeta\n").unwrap();
        let uri = file_uri(&path);

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &uri).unwrap();
        let (_, view) = app.active_native_view(wid).expect("editor active");

        assert!(app.native_editor_pointer_select(wid, view, 5, false, 12));
        assert!(app.native_editor_pointer_select(wid, view, 1, true, 12));
        let selection = match app.native_runtime.view_state(view).unwrap() {
            AppViewState::Editor(state) => {
                state.buffer.as_ref().unwrap().primary_selection().clone()
            }
            _ => panic!("editor view changed kind"),
        };
        assert_eq!(selection.anchor, 5);
        assert_eq!(selection.head, 1);

        app.switch_tab_in(wid, 0);
        assert!(!app.native_editor_pointer_select(wid, view, 0, false, 12));
        let after = match app.native_runtime.view_state(view).unwrap() {
            AppViewState::Editor(state) => {
                state.buffer.as_ref().unwrap().primary_selection().clone()
            }
            _ => panic!("editor view changed kind"),
        };
        assert_eq!(after, selection);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn enum_chip_click_opens_the_choice_picker_and_commits_nothing() {
        use crate::native_ui::UiContent;
        use crate::{App, WindowId};
        use aterm_window::event::{ElementState, MouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Sound));
        let key = crate::native_ui::UiKey::new(format!(
            "settings/control/{}",
            crate::prefs::EDIT_TRAIL_SOUND_STYLE
        ));
        let mut compiled = presented_native_ui(&mut app, wid);
        for _ in 0..40 {
            // On screen, not in the peek of the next stop below.
            if compiled
                .paint
                .iter()
                .any(|node| node.key == key && !node.peek)
            {
                break;
            }
            app.dispatch_native_event(wid, crate::native_app::AppEvent::ScrollLines(1))
                .unwrap();
            compiled = presented_native_ui(&mut app, wid);
        }
        let chip = compiled
            .paint
            .iter()
            .find(|node| node.key == key && !node.peek)
            .expect("enum control painted");
        let UiContent::Button(control) = &chip.content else {
            panic!("popup-chip enum row paints a Button, got another control");
        };
        let committed = control.spec.label.clone();
        let chip_rect = chip.rect;

        let click = |app: &mut App, x: f64, y: f64| {
            app.on_cursor_moved(wid, x, y);
            app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
            app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        };

        let (x, y) = native_content_to_window(
            &mut app,
            wid,
            chip_rect.x + chip_rect.width / 2.0,
            chip_rect.y + chip_rect.height / 2.0,
        );
        click(&mut app, x, y);
        let open = presented_native_ui(&mut app, wid);
        assert!(
            open.paint.iter().any(|node| {
                node.key.as_str()
                    == format!(
                        "settings/choice-picker/{}",
                        crate::prefs::EDIT_TRAIL_SOUND_STYLE
                    )
            }),
            "a click on a popup-chip enum row opens the ChoicePicker"
        );

        let done = open
            .paint
            .iter()
            .find(|node| node.key.as_str() == "settings/choice-close")
            .expect("picker paints its Cancel control");
        let (dx, dy) = native_content_to_window(
            &mut app,
            wid,
            done.rect.x + done.rect.width / 2.0,
            done.rect.y + done.rect.height / 2.0,
        );
        click(&mut app, dx, dy);
        let closed = presented_native_ui(&mut app, wid);
        assert!(
            !closed
                .paint
                .iter()
                .any(|node| node.key.as_str().starts_with("settings/choice-picker/")),
            "Cancel dismisses the pull-down"
        );
        let chip = closed
            .paint
            .iter()
            .find(|node| node.key == key)
            .expect("enum control repaints after dismissal");
        let UiContent::Button(control) = &chip.content else {
            panic!("popup-chip enum row paints a Button, got another control");
        };
        assert_eq!(
            control.spec.label, committed,
            "opening + dismissing the pull-down never cycles the committed value"
        );
    }

    #[test]
    fn markdown_pointer_selects_the_exact_authored_block_through_semantic_hit_testing() {
        use crate::native_app::{AppKind, AppViewState};
        use crate::native_ui::UiContent;
        use crate::{App, WindowId};
        use aterm_window::event::{ElementState, MouseButton};

        let dir = std::env::temp_dir().join(format!(
            "aterm-markdown-pointer-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pointer.md");
        let source = "# **Heading**\n\nBody [link](https://example.com).\n";
        std::fs::write(&path, source).unwrap();
        let uri = file_uri(&path);
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Markdown, &uri).unwrap();
        let (_, view) = app.active_native_view(wid).expect("Markdown active");
        let compiled = presented_native_ui(&mut app, wid);
        let block = compiled
            .paint
            .iter()
            .find(|node| {
                matches!(&node.content, UiContent::MarkdownBlock(_))
                    && node.key.as_str() == "markdown/block/0"
            })
            .expect("first Markdown block painted");
        let expected = match &block.content {
            UiContent::MarkdownBlock(spec) => spec.source.clone(),
            _ => unreachable!(),
        };
        let (x, y) = native_content_to_window(
            &mut app,
            wid,
            block.rect.x + block.rect.width / 2.0,
            block.rect.y + block.rect.height / 2.0,
        );
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Markdown(state)) = app.native_runtime.view_state(view) else {
            panic!("Markdown view changed kind");
        };
        assert_eq!(state.selection, Some(expected.clone()));
        assert_eq!(&source[expected], "# **Heading**\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wheel_axis_matches_the_old_vertical_guard_and_adds_the_horizontal_one() {
        use super::{WheelAxis, wheel_axis};
        assert_eq!(wheel_axis(0.0, 1.0, 0.0), Some(WheelAxis::Vertical));
        assert_eq!(wheel_axis(0.0, -1.0, 0.0), Some(WheelAxis::Vertical));
        assert_eq!(wheel_axis(0.4, -1.0, 0.0), Some(WheelAxis::Vertical));
        assert_eq!(wheel_axis(1.0, 0.0, 0.0), Some(WheelAxis::Horizontal));
        assert_eq!(wheel_axis(-1.0, 0.4, 0.0), Some(WheelAxis::Horizontal));
        assert_eq!(wheel_axis(0.0, 0.0, 0.0), None);
        assert_eq!(wheel_axis(1.0, 1.0, 0.0), None);
        assert_eq!(wheel_axis(-2.0, 2.0, 0.0), None);
        assert_eq!(wheel_axis(0.0, f64::EPSILON / 2.0, f64::EPSILON), None);
        assert_eq!(wheel_axis(f64::EPSILON / 2.0, 0.0, f64::EPSILON), None);
    }

    #[test]
    fn the_two_wheel_axes_bank_into_separate_window_fields() {
        use crate::{App, WindowId};
        use aterm_window::event::MouseScrollDelta;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let near = |got: f64, want: f64, what: &str| {
            assert!((got - want).abs() < 1e-6, "{what}: got {got}, want ~{want}");
        };
        let residuals = |app: &App| {
            let ws = &app.windows[&wid];
            (ws.scroll_residual_x, ws.scroll_residual)
        };
        assert_eq!(residuals(&app), (0.0, 0.0), "both banks start empty");

        assert_eq!(
            app.wheel_notches(wid, MouseScrollDelta::LineDelta(0.6, 0.0)),
            None
        );
        near(residuals(&app).0, 0.6, "x banked in the x field");
        assert_eq!(residuals(&app).1, 0.0, "the y field is untouched by x");
        assert_eq!(
            app.wheel_notches(wid, MouseScrollDelta::LineDelta(0.0, 0.6)),
            None
        );
        near(residuals(&app).0, 0.6, "the x bank survived a y event");
        near(residuals(&app).1, 0.6, "y banked in the y field");

        assert_eq!(
            app.wheel_notches(wid, MouseScrollDelta::LineDelta(0.0, 0.6)),
            Some((WheelDir::Up, 1)),
            "the vertical bank pays off from vertical motion alone"
        );
        near(
            residuals(&app).0,
            0.6,
            "the vertical payoff must not drain the horizontal bank",
        );
        assert_eq!(
            app.wheel_notches(wid, MouseScrollDelta::LineDelta(-0.6, 0.0)),
            None,
            "a horizontal reversal forfeits only the horizontal bank"
        );
        near(
            residuals(&app).0,
            -0.6,
            "the reversal restarted the x bank in its own direction",
        );
    }

    #[test]
    fn horizontal_flicks_report_both_xterm_buttons_in_the_right_direction() {
        use aterm_window::dpi::PhysicalPosition;
        use aterm_window::event::MouseScrollDelta;

        use crate::{App, WindowId};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);

        assert_eq!(
            app.wheel_notches(wid, MouseScrollDelta::LineDelta(1.0, 0.0)),
            Some((WheelDir::Left, 1)),
            "+x (content moves right) = the reader walks LEFT"
        );
        assert_eq!(
            app.wheel_notches(wid, MouseScrollDelta::LineDelta(-1.0, 0.0)),
            Some((WheelDir::Right, 1)),
            "-x = the reader walks RIGHT (winit's Windows backend negates WM_MOUSEHWHEEL)"
        );
        assert_eq!(
            app.wheel_notches(wid, MouseScrollDelta::LineDelta(-3.0, 0.0)),
            Some((WheelDir::Right, 3))
        );

        let cw = app.win_cell_size(wid).0.max(1) as f64;
        assert_eq!(
            app.wheel_notches(
                wid,
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(cw, 0.0))
            ),
            Some((WheelDir::Left, 1)),
            "+x pixels agree with +x lines"
        );
        assert_eq!(
            app.wheel_notches(
                wid,
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(-cw, 0.0))
            ),
            Some((WheelDir::Right, 1))
        );
    }

    #[test]
    fn horizontal_flicks_move_no_local_viewport_anywhere() {
        use aterm_window::event::MouseScrollDelta;

        use crate::native_app::AppViewState;
        use crate::{App, WindowId};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        {
            let mut terminal = crate::term_lock(&term);
            for line in 0..100 {
                terminal.process(format!("history {line}\r\n").as_bytes());
            }
            assert!(terminal.grid().scrollback_lines() > 0);
        }

        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(3.0, 0.0));
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(-3.0, 0.0));
        assert_eq!(crate::term_lock(&term).grid().display_offset(), 0);
        assert!(
            app.windows[&wid].scroll_glide.is_none(),
            "a horizontal flick never arms the vertical smooth scroll"
        );
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 3.0));
        assert!(
            app.windows[&wid].scroll_glide.is_some()
                || crate::term_lock(&term).grid().display_offset() > 0,
            "negative control: the vertical axis still scrolls"
        );

        let mut app = App::headless_for_test();
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        let (_, view) = app.active_native_view(wid).expect("Settings active");
        let page_scroll = |app: &App| match app.native_runtime.view_state(view).unwrap() {
            AppViewState::Settings(settings) => settings.page_scroll,
            _ => panic!("Settings state"),
        };
        let before = page_scroll(&app);
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(5.0, 0.0));
        assert_eq!(
            page_scroll(&app),
            before,
            "a horizontal flick never scrolls a native view"
        );
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, -5.0));
        assert_ne!(
            page_scroll(&app),
            before,
            "negative control: the vertical axis still reaches the native scroller"
        );

        let mut app = App::headless_for_test();
        app.palette_enter();
        let scroll_of = |app: &App| app.windows[&wid].palette().unwrap().scroll_extent().0;
        let before = scroll_of(&app);
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(5.0, 0.0));
        assert_eq!(
            scroll_of(&app),
            before,
            "a horizontal flick never scrolls the palette card"
        );
    }

    #[test]
    fn shift_is_the_tracking_bypass_off_macos() {
        use super::tracking_bypass_held;
        use aterm_window::keyboard::ModifiersState as M;
        assert!(tracking_bypass_held(M::ALT));
        assert!(tracking_bypass_held(M::ALT | M::SHIFT));
        assert_eq!(
            tracking_bypass_held(M::SHIFT),
            cfg!(not(target_os = "macos"))
        );
        assert!(!tracking_bypass_held(M::empty()));
        assert!(!tracking_bypass_held(M::CONTROL));
        assert!(!tracking_bypass_held(M::SUPER));
    }

    #[test]
    fn option_drag_kind_depends_on_who_owns_the_mouse() {
        assert_eq!(
            press_selection_kind(true, false),
            SelectionType::Block,
            "local Option+drag stays the rectangular block selection"
        );
        assert_eq!(
            press_selection_kind(true, true),
            SelectionType::Simple,
            "bypass Option+drag selects normally (the modifier bought the bypass)"
        );
        assert_eq!(press_selection_kind(false, false), SelectionType::Simple);
        assert_eq!(press_selection_kind(false, true), SelectionType::Simple);
    }

    #[test]
    fn right_press_plan_matches_the_windows_convention() {
        use super::{RightPressPlan, right_press_plan};
        for gesture_on in [false, true] {
            for has_selection in [false, true] {
                for tracking in [false, true] {
                    assert_eq!(
                        right_press_plan(gesture_on, true, tracking, has_selection),
                        RightPressPlan::Chrome,
                        "chrome must swallow (gesture_on={gesture_on}, tracking={tracking})"
                    );
                }
                assert_eq!(
                    right_press_plan(gesture_on, false, true, has_selection),
                    RightPressPlan::Seam,
                    "tracking keeps the button (gesture_on={gesture_on})"
                );
            }
        }
        assert_eq!(
            right_press_plan(true, false, false, true),
            RightPressPlan::Copy,
            "selection exists: copy (and clear) it"
        );
        assert_eq!(
            right_press_plan(true, false, false, false),
            RightPressPlan::Paste,
            "no selection: paste"
        );
        assert_eq!(
            right_press_plan(false, false, false, true),
            RightPressPlan::Seam
        );
        assert_eq!(
            right_press_plan(false, false, false, false),
            RightPressPlan::Seam
        );
    }

    #[test]
    fn pointer_geometry_tracks_the_band_offset() {
        use crate::{App, WindowId};
        use aterm_window::dpi::PhysicalSize;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.backend.set_pad(12);
        app.backend.set_pad_top(2);
        app.windows.get_mut(&wid).unwrap().metrics.pad = 12;
        app.windows.get_mut(&wid).unwrap().metrics.pad_top = 2;
        assert_eq!(
            app.frame_origin(wid),
            (0, 0),
            "headless must be the identity"
        );

        let (cw, ch) = app.cell_size();
        let pad = app.backend.pad();
        let pad_top = app.backend.pad_top();
        let (rows, cols) = {
            let ws = app.windows.get(&wid).expect("seeded window");
            (ws.rows as usize, ws.cols as usize)
        };
        let fit = PhysicalSize::new(
            (cols * cw + 2 * pad) as u32,
            (rows * ch + pad_top + pad) as u32,
        );
        app.windows.get_mut(&wid).expect("window").win_px = Some(fit);
        assert_eq!(
            app.frame_origin(wid),
            (0, 0),
            "exact fit must be the identity"
        );
        let (px, py) = ((pad + 5 * cw) as f64 + 1.0, (pad_top + 2 * ch) as f64 + 1.0);
        let at_fit = app.pixel_to_cell(wid, px, py);

        let (rw, rh) = (cw.saturating_sub(1).max(1), ch.saturating_sub(1).max(1));
        app.windows.get_mut(&wid).expect("window").win_px = Some(PhysicalSize::new(
            fit.width + rw as u32,
            fit.height + rh as u32,
        ));
        let (ox, oy) = (rw / 2, if cfg!(target_os = "linux") { 0 } else { rh / 2 });
        assert_eq!(
            app.frame_origin(wid),
            (ox as i64, oy as i64),
            "a sub-cell remainder must land exactly where the presenter's bands do"
        );
        assert_eq!(
            app.pixel_to_cell(wid, px + ox as f64, py + oy as f64),
            at_fit,
            "a click over the same on-glass cell must map to the same cell"
        );
        let (_, col) = app.pixel_to_cell(wid, 1.0, py + oy as f64);
        assert_eq!(col, 0, "a band click clamps to the leading cell");

        app.windows.get_mut(&wid).expect("window").win_px = Some(PhysicalSize::new(
            fit.width.saturating_sub(3).max(1),
            fit.height.saturating_sub(5).max(1),
        ));
        let cropped_origin = app.frame_origin(wid);
        assert!(cropped_origin.0 < 0);
        if cfg!(target_os = "linux") {
            assert_eq!(cropped_origin.1, 0, "a top-pinned crop keeps the frame top");
        } else {
            assert!(cropped_origin.1 < 0);
        }
        assert_eq!(
            app.window_to_frame(wid, 0.0, 0.0),
            (-cropped_origin.0 as f64, -cropped_origin.1 as f64),
            "pointer transform must retain the source-crop origin"
        );
    }

    #[test]
    fn release_settles_local_selection_after_tracking_flips_off_mid_drag() {
        use crate::{App, WindowId, term_lock};
        use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};
        use aterm_window::keyboard::ModifiersState;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        term_lock(&term).process(b"\x1b[?1000h");
        assert!(term_lock(&term).mouse_tracking_enabled());
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::ALT;
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert!(
            app.windows.get(&wid).unwrap().selecting,
            "the Option-bypass press starts a local selection"
        );
        term_lock(&term).process(b"\x1b[?1000l");
        assert!(!term_lock(&term).mouse_tracking_enabled());
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        assert!(
            !app.windows.get(&wid).unwrap().selecting,
            "release settles the selection (selecting cleared) even though tracking flipped off"
        );
        assert_eq!(
            app.windows.get(&wid).unwrap().reported_buttons & 1,
            0,
            "the Left (selection) reported bit is cleared on settle"
        );
    }

    #[test]
    fn a_thumb_press_never_clears_a_live_drags_reported_bit() {
        use aterm_types::mouse::MouseButton;
        use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

        use crate::{App, WindowId, term_lock};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        term_lock(&term).process(b"\x1b[?1000h");
        assert!(term_lock(&term).mouse_tracking_enabled());
        let bits = |app: &App| app.windows[&wid].reported_buttons;
        let bit = |b: MouseButton| 1u8 << (b.slot() & 7);

        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert_ne!(
            bits(&app) & bit(MouseButton::Left),
            0,
            "Left press reported"
        );
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Back);
        assert_ne!(
            bits(&app) & bit(MouseButton::Back),
            0,
            "Back press reported"
        );
        assert_ne!(
            bits(&app) & bit(MouseButton::Left),
            0,
            "the thumb press must not disturb the live Left drag"
        );
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Back);
        assert_eq!(bits(&app) & bit(MouseButton::Back), 0, "Back bit cleared");
        assert_ne!(
            bits(&app) & bit(MouseButton::Left),
            0,
            "the thumb RELEASE must not clear the live drag's press bit — the \
             orphaned-release bug"
        );
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        assert_eq!(bits(&app), 0, "every bit paired off");

        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Forward);
        assert_eq!(bits(&app), bit(MouseButton::Forward));
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Other(9));
        assert_eq!(
            bits(&app),
            bit(MouseButton::Forward),
            "an unmapped device button touches no bit"
        );
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Forward);
        assert_eq!(bits(&app), 0);
    }

    #[test]
    fn pet_wake_is_edge_gated_on_cell_displacement_and_the_brains_own_cadence() {
        use super::pet_wake_wanted;
        let cell = (10, 20);
        let seen = Some((100.0, 200.0));
        assert!(
            !pet_wake_wanted(false, false, None, (0.0, 0.0), cell),
            "no pet drawn: never"
        );
        assert!(
            !pet_wake_wanted(true, true, None, (0.0, 0.0), cell),
            "the brain's own lane is armed: never (it samples on its ticks)"
        );
        assert!(
            pet_wake_wanted(true, false, None, (0.0, 0.0), cell),
            "never sampled: the first event asks"
        );
        for (dx, dy) in [(0.0, 0.0), (9.9, 0.0), (0.0, 19.9), (-9.9, -19.9)] {
            assert!(
                !pet_wake_wanted(true, false, seen, (100.0 + dx, 200.0 + dy), cell),
                "sub-cell motion ({dx}, {dy}) asks for nothing"
            );
        }
        for (dx, dy) in [(10.0, 0.0), (0.0, 20.0), (-10.0, 0.0), (0.0, -20.0)] {
            assert!(
                pet_wake_wanted(true, false, seen, (100.0 + dx, 200.0 + dy), cell),
                "a cell of travel ({dx}, {dy}) on either axis asks once"
            );
        }
        assert!(pet_wake_wanted(true, false, seen, (101.0, 200.0), (0, 0)));
    }

    #[test]
    fn a_click_on_the_pet_pets_and_never_starts_a_selection() {
        use crate::{App, WindowId};
        use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let (x0, x1, y0, y1) = {
            let ws = app.windows.get_mut(&wid).unwrap();
            let pet = crate::app_render::pet_owner_tick_for_test(
                ws,
                aterm_effects::host::CaptureMode::StaticCapture,
                aterm_effects::kitty_pet::PetSense {
                    caret_drawn: true,
                    now: std::time::Instant::now(),
                    caret: Some((4, 12)),
                    rows: 24,
                    cols: 80,
                    cell_w: 8,
                    cell_h: 16,
                    reduced_motion: false,
                    output_burst: false,
                    pointer: None,
                    wrapped: false,
                },
            );
            assert!(pet.on_glass, "fixture: the resident is drawn");
            ws.fx
                .companion_mut()
                .hit_rect()
                .expect("the drawn body is the hit target")
        };
        let (fx, fy) = (f64::from(x0 + x1) / 2.0, f64::from(y0 + y1) / 2.0);
        let (wx, wy) = {
            let (ox, oy) = app.frame_origin(wid);
            (fx + ox as f64, fy + oy as f64)
        };
        app.on_cursor_moved(wid, wx, wy);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        {
            let ws = app.windows.get(&wid).unwrap();
            assert_eq!(
                ws.fx.companion().brain().pending_pets(),
                1,
                "the press latched a pet (note, never act)"
            );
            assert!(!ws.selecting, "and was consumed before the selection layer");
        }
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        assert!(
            !app.windows.get(&wid).unwrap().selecting,
            "the orphan release is dropped (press/release stay paired)"
        );
        app.on_cursor_moved(wid, 420.0, 300.0);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        {
            let ws = app.windows.get(&wid).unwrap();
            assert_eq!(
                ws.fx.companion().brain().pending_pets(),
                1,
                "no second pet off the body"
            );
            assert!(ws.selecting, "a plain terminal press still selects");
        }
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn a_right_press_on_a_chip_pops_the_menu_and_never_reaches_the_grid() {
        use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f64;
        let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
        app.splice_tab_strip_with(wid, 1);
        let chip = app.windows[&wid]
            .tab_segments
            .iter()
            .find_map(|seg| match seg.kind {
                crate::tab_bar::TabHit::Select(0) => Some(seg.start_col + 1),
                _ => None,
            })
            .expect("the lone tab has a chip");
        let chip_pt = (
            pad + f64::from(chip) * cw as f64 + cw as f64 * 0.5,
            top + ch as f64 * 0.5,
        );
        assert!(
            app.strip_col_at(wid, chip_pt.0, chip_pt.1).is_some(),
            "fixture point must be ON the strip"
        );

        app.on_cursor_moved(wid, chip_pt.0, chip_pt.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Right);
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Right);
        let menu = app.windows[&wid]
            .tab_menu
            .as_ref()
            .expect("the right press popped the tab's menu");
        assert!(
            menu.entries
                .iter()
                .any(|e| matches!(e, crate::session_chrome::TabMenuEntry::Action { .. })),
            "…carrying the composed model's action rows"
        );
        assert_eq!(
            app.windows[&wid].reported_buttons, 0,
            "a press on chrome is never reported to the terminal"
        );

        let plus = app.windows[&wid]
            .tab_segments
            .iter()
            .find_map(|seg| (seg.kind == crate::tab_bar::TabHit::NewTab).then_some(seg.start_col))
            .expect("a one-tab strip still offers the + segment");
        let tabs_before = app.windows[&wid].tab_set.len();
        let plus_pt = (
            pad + f64::from(plus) * cw as f64 + cw as f64 * 0.5,
            top + ch as f64 * 0.5,
        );
        app.on_cursor_moved(wid, plus_pt.0, plus_pt.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert!(
            app.windows[&wid].tab_menu.is_none(),
            "click-away dismissed the card"
        );
        assert_eq!(
            app.windows[&wid].tab_set.len(),
            tabs_before,
            "…and the dismissing press did NOT also open a tab"
        );
        assert_eq!(app.windows[&wid].reported_buttons, 0);
    }

    #[cfg(windows)]
    #[test]
    fn the_bare_band_system_menu_arms_on_press_and_fires_on_release() {
        use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f64;
        let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
        app.splice_tab_strip_with(wid, 1);
        let bare = app.windows[&wid]
            .tab_segments
            .iter()
            .find(|seg| seg.kind == crate::tab_bar::TabHit::NewTab)
            .map(|seg| seg.end_col)
            .expect("the strip laid out its +")
            + 2;
        let bare_pt = (
            pad + f64::from(bare) * cw as f64 + cw as f64 * 0.5,
            top + ch as f64 * 0.5,
        );
        assert!(
            app.strip_col_at(wid, bare_pt.0, bare_pt.1).is_some(),
            "fixture point must be ON the strip"
        );
        assert!(
            crate::tab_bar::hit_test(&app.windows[&wid].tab_segments, bare).is_none(),
            "fixture point must be the BARE band, not a chip or control"
        );

        app.on_cursor_moved(wid, bare_pt.0, bare_pt.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Right);
        assert!(
            app.windows[&wid].band_menu_release_pending,
            "the PRESS armed the latch instead of popping — a popup posted \
             here is discarded under winit's mouse capture"
        );
        assert!(
            app.windows[&wid].tab_menu.is_none(),
            "the bare band pops the SYSTEM menu, never the tab card"
        );
        assert_eq!(
            app.windows[&wid].reported_buttons, 0,
            "a press on chrome is never reported to the terminal"
        );

        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Right);
        assert!(
            !app.windows[&wid].band_menu_release_pending,
            "the RELEASE spent the latch — the popup post lives behind this \
             spend, after winit's ReleaseCapture"
        );
        assert_eq!(
            app.windows[&wid].reported_buttons, 0,
            "…and the consumed release leaked nothing to the terminal"
        );

        let chip = app.windows[&wid]
            .tab_segments
            .iter()
            .find_map(|seg| match seg.kind {
                crate::tab_bar::TabHit::Select(0) => Some(seg.start_col + 1),
                _ => None,
            })
            .expect("the lone tab has a chip");
        let chip_pt = (
            pad + f64::from(chip) * cw as f64 + cw as f64 * 0.5,
            top + ch as f64 * 0.5,
        );
        app.on_cursor_moved(wid, chip_pt.0, chip_pt.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Right);
        assert!(
            !app.windows[&wid].band_menu_release_pending,
            "a chip press must not arm the system menu"
        );
        assert!(
            app.windows[&wid].tab_menu.is_some(),
            "…because it pops the tab card"
        );
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Right);
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn hovering_the_new_tab_button_records_it_and_repaints_the_strip() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f64;
        let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
        app.splice_tab_strip_with(wid, 1);
        let seg = |want_plus: bool| {
            app.windows[&wid]
                .tab_segments
                .iter()
                .find(|s| matches!(s.kind, crate::tab_bar::TabHit::NewTab) == want_plus)
                .copied()
                .expect("the strip laid out both a chip and a +")
        };
        let plus = seg(true);
        let point = |col: u16| {
            (
                pad + f64::from(col) * cw as f64 + cw as f64 * 0.5,
                top + ch as f64 * 0.5,
            )
        };
        let button_col = plus.start_col + 1;
        let bg_of =
            |app: &crate::App, col: u16| app.windows[&wid].cached_strip_rows[0][col as usize].bg;
        let resting = bg_of(&app, button_col);

        let p = point(button_col);
        app.on_cursor_moved(wid, p.0, p.1);
        assert!(
            app.windows[&wid].strip_hover_new_tab,
            "the pointer is on the `+`"
        );
        assert_eq!(
            app.windows[&wid].strip_hover, None,
            "…and the `+` is not a tab, so no chip is hovered"
        );

        app.splice_tab_strip_with(wid, 1);
        let hot = bg_of(&app, button_col);
        assert_ne!(
            resting, hot,
            "the repaint must reach the rows — a cache key without the flag \
             would hit and paint the resting button forever"
        );
        assert_eq!(
            hot,
            crate::tab_bar::strip_hover_bg_for_test(app.chrome_palette_theme()),
            "the button takes the band's own hover material"
        );

        app.on_cursor_left(wid);
        assert!(
            !app.windows[&wid].strip_hover_new_tab,
            "leaving the window clears the `+` wash"
        );
        app.splice_tab_strip_with(wid, 1);
        assert_eq!(
            bg_of(&app, button_col),
            resting,
            "…and the clear reaches the painted row"
        );
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn closing_an_overlay_replays_no_pointer_the_window_has_lost() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f64;
        let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
        app.splice_tab_strip_with(wid, 1);
        let chip = app.windows[&wid]
            .tab_segments
            .iter()
            .find_map(|seg| match seg.kind {
                crate::tab_bar::TabHit::Select(0) => Some(seg.start_col + 2),
                _ => None,
            })
            .expect("the lone tab has a chip");
        let on_tab = (
            pad + f64::from(chip) * cw as f64 + cw as f64 * 0.5,
            top + ch as f64 * 0.5,
        );
        app.on_cursor_moved(wid, on_tab.0, on_tab.1);
        assert_eq!(
            app.windows[&wid].strip_hover,
            Some(0),
            "CONTROL: the tab lit"
        );
        app.on_cursor_left(wid);
        assert_eq!(app.windows[&wid].strip_hover, None, "leaving cleared it");

        type Close = fn(&mut crate::App, crate::WindowId);
        let closes: [(&str, Close); 2] = [
            ("the palette", |app, _| app.palette_exit()),
            ("an overlay card", |app, wid| {
                app.close_overlay_card(wid, |ws| ws.palette().is_some());
            }),
        ];
        for (what, close) in closes {
            app.palette_enter();
            assert!(app.windows[&wid].palette().is_some(), "{what}: opened");
            close(&mut app, wid);
            assert!(app.windows[&wid].palette().is_none(), "{what}: closed");
            let ws = &app.windows[&wid];
            assert!(
                !ws.pointer_position_known,
                "{what}: the close claimed a pointer the window does not hold"
            );
            assert_eq!(ws.strip_hover, None, "{what}: a phantom hover lit tab 0");
        }

        app.on_cursor_moved(wid, on_tab.0, on_tab.1);
        app.palette_enter();
        app.palette_exit();
        assert_eq!(
            app.windows[&wid].strip_hover,
            Some(0),
            "a held pointer is still replayed on close"
        );
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn a_strip_hover_change_moves_the_repaint_key() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f64;
        let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
        app.splice_tab_strip_with(wid, 1);
        let seg = |kind: crate::tab_bar::TabHit| {
            app.windows[&wid]
                .tab_segments
                .iter()
                .find(|s| s.kind == kind)
                .copied()
                .expect("the strip laid out a chip and a +")
        };
        let chip = seg(crate::tab_bar::TabHit::Select(0));
        let plus = seg(crate::tab_bar::TabHit::NewTab);
        let point = |col: u16| {
            (
                pad + f64::from(col) * cw as f64 + cw as f64 * 0.5,
                top + ch as f64 * 0.5,
            )
        };
        let rest = app.redraw_tab_strip_state(wid);
        assert_ne!(rest, 0, "a live strip has a fingerprint");

        let p = point(chip.start_col + 1);
        app.on_cursor_moved(wid, p.0, p.1);
        assert_eq!(app.windows[&wid].strip_hover, Some(0), "on the chip");
        let on_chip = app.redraw_tab_strip_state(wid);
        assert_ne!(on_chip, rest, "lighting a chip moves the repaint key");

        let p = point(chip.start_col + 2);
        app.on_cursor_moved(wid, p.0, p.1);
        assert_eq!(
            app.redraw_tab_strip_state(wid),
            on_chip,
            "motion within the chip repaints nothing"
        );

        let p = point(plus.start_col + 1);
        app.on_cursor_moved(wid, p.0, p.1);
        assert!(app.windows[&wid].strip_hover_new_tab, "on the `+`");
        let on_plus = app.redraw_tab_strip_state(wid);
        assert_ne!(
            on_plus, on_chip,
            "the light moving to the `+` moves the key"
        );
        assert_ne!(on_plus, rest);

        app.on_cursor_left(wid);
        assert_eq!(
            app.redraw_tab_strip_state(wid),
            rest,
            "nothing lit: the resting strip's fingerprint, byte for byte"
        );
    }

    #[cfg(windows)]
    #[test]
    fn hovering_the_version_label_records_it_and_repaints_the_strip() {
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        assert!(
            crate::tray_raster::strip_band_ui_ready(),
            "Windows always has a UI face to measure the label in"
        );
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f64;
        let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
        app.splice_tab_strip_with(wid, 1);
        let label = app.windows[&wid]
            .tab_segments
            .iter()
            .find(|s| matches!(s.kind, crate::tab_bar::TabHit::Version { .. }))
            .copied()
            .expect("the strip lays the version label out once the UI face is in");
        let plus = app.windows[&wid]
            .tab_segments
            .iter()
            .find(|s| s.kind == crate::tab_bar::TabHit::NewTab)
            .copied()
            .expect("the strip offers the +");
        let point = |col: u16| {
            (
                pad + f64::from(col) * cw as f64 + cw as f64 * 0.5,
                top + ch as f64 * 0.5,
            )
        };
        let bg_of =
            |app: &crate::App, col: u16| app.windows[&wid].cached_strip_rows[0][col as usize].bg;
        let resting = bg_of(&app, label.start_col);
        let resting_key = app.redraw_tab_strip_state(wid);

        let p = point(label.start_col + 3);
        app.on_cursor_moved(wid, p.0, p.1);
        assert!(
            app.windows[&wid].strip_hover_version,
            "the pointer is on the label"
        );
        assert!(!app.windows[&wid].strip_hover_new_tab, "…not on the `+`");
        assert_eq!(app.windows[&wid].strip_hover, None, "…and on no chip");
        assert_ne!(
            app.redraw_tab_strip_state(wid),
            resting_key,
            "the label's hover reaches the glass on its own"
        );

        app.splice_tab_strip_with(wid, 1);
        let hot = bg_of(&app, label.start_col);
        assert_ne!(resting, hot, "the repaint must reach the rows");
        assert_eq!(
            hot,
            crate::tab_bar::strip_hover_bg_for_test(app.chrome_palette_theme()),
            "the label takes the band's own hover material"
        );

        app.on_cursor_left(wid);
        assert!(
            !app.windows[&wid].strip_hover_version,
            "leaving clears the label"
        );
        app.splice_tab_strip_with(wid, 1);
        assert_eq!(bg_of(&app, label.start_col), resting);
        assert_eq!(
            app.redraw_tab_strip_state(wid),
            resting_key,
            "…and the repaint key is the resting one again"
        );

        app.on_cursor_moved(wid, p.0, p.1);
        assert!(app.windows[&wid].strip_hover_version, "re-entry relights");
        let q = point(plus.start_col + 1);
        app.on_cursor_moved(wid, q.0, q.1);
        assert!(
            !app.windows[&wid].strip_hover_version,
            "leaving the label clears it"
        );
        assert!(app.windows[&wid].strip_hover_new_tab, "…and lights the `+`");
        app.splice_tab_strip_with(wid, 1);
        assert_eq!(
            bg_of(&app, label.start_col),
            resting,
            "the label is back to resting"
        );
        crate::tray_raster::clear_ui_fonts_for_test();
    }

    #[test]
    fn hover_cursor_resolves_grid_tracking_divider_and_strip_by_location() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f64;
        let strip_px = app.tab_strip_rows as f64 * ch as f64;
        let grid = (pad + cw as f64 * 10.5, pad + strip_px + ch as f64 * 5.5);
        assert!(
            app.strip_col_at(wid, grid.0, grid.1).is_none(),
            "fixture point must be off the strip"
        );
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.last_cursor_px = grid;
            ws.last_mouse_window_cell = (5, 10);
        }

        app.update_hover_cursor(wid);
        let state = |app: &crate::App| {
            let ws = &app.windows[&wid];
            (ws.hover_pointer, ws.native_text_cursor)
        };
        assert_eq!(state(&app), (false, true), "grid = I-beam");

        let term = app.front_terminal(wid).expect("terminal").term.clone();
        crate::term_lock(&term).process(b"\x1b[?1000h");
        app.update_hover_cursor(wid);
        assert_eq!(state(&app), (false, false), "tracking = arrow");
        crate::term_lock(&term).process(b"\x1b[?1000l");
        app.update_hover_cursor(wid);
        assert_eq!(state(&app), (false, true), "tracking off = I-beam again");

        let _sid = app.split_active_stub_tab(wid);
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.last_mouse_window_cell = (5, 40);
        }
        app.update_hover_cursor(wid);
        assert_eq!(state(&app), (true, true), "divider = resize encoding");
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.last_mouse_window_cell = (5, 10);
        }
        app.update_hover_cursor(wid);
        assert_eq!(state(&app), (false, true), "off the seam = I-beam");

        {
            app.tab_strip_rows = 1;
            let strip_px = ch as f64;
            let strip_pt = (pad + cw as f64 * 2.5, pad + strip_px * 0.5);
            assert!(
                app.strip_col_at(wid, strip_pt.0, strip_pt.1).is_some(),
                "fixture point must be ON the strip"
            );
            if let Some(ws) = app.windows.get_mut(&wid) {
                ws.last_cursor_px = strip_pt;
                ws.pointer_position_known = true;
            }
            app.on_modifiers_changed(wid, link_modifier());
            assert_eq!(
                state(&app),
                (false, false),
                "a link-modifier tap over the strip paints chrome's arrow, not an I-beam"
            );
            app.on_modifiers_changed(wid, aterm_window::keyboard::ModifiersState::empty());
        }
        app.tab_strip_rows = 0;

        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.last_cursor_px = grid;
            ws.last_mouse_window_cell = (5, 10);
        }
        app.on_modifiers_changed(wid, aterm_window::keyboard::ModifiersState::SHIFT);
        assert_eq!(
            state(&app),
            (false, false),
            "a Shift edge must not re-resolve the hover (no lock, no probe)"
        );
        app.on_modifiers_changed(wid, aterm_window::keyboard::ModifiersState::empty());
        app.on_modifiers_changed(wid, link_modifier());
        assert_eq!(
            state(&app),
            (false, true),
            "a link-modifier edge resolves the grid"
        );
        app.on_modifiers_changed(wid, aterm_window::keyboard::ModifiersState::empty());

        let term = app.front_terminal(wid).expect("terminal").term.clone();
        let acq = crate::term_lock_acquisitions_on_this_thread;
        {
            let mut held = crate::term_lock(&term);
            held.process(b"\x1b[?1000h");
            let before = acq();
            app.update_hover_cursor(wid);
            assert_eq!(
                acq() - before,
                0,
                "the mode is read from the mirror, lock-free"
            );
            assert_eq!(
                state(&app),
                (false, false),
                "tracking resolves to the arrow while the mutex is busy"
            );
            held.process(b"\x1b[?1000l");
            if let Some(ws) = app.windows.get_mut(&wid) {
                ws.last_cursor_px = (grid.0 + cw as f64 * 2.0, grid.1);
                ws.last_mouse_window_cell = (5, 12);
            }
            let before = acq();
            app.update_hover_cursor(wid);
            assert_eq!(acq() - before, 0, "a busy mutex is never waited on");
            assert_eq!(
                state(&app),
                (false, false),
                "…the probe is deferred: the previous cursor stands, nothing blocked"
            );
        }
        app.update_hover_cursor(wid);
        assert_eq!(
            state(&app),
            (false, true),
            "lock free again: the I-beam resolves"
        );
    }

    fn link_modifier() -> aterm_window::keyboard::ModifiersState {
        #[cfg(target_os = "macos")]
        {
            aterm_window::keyboard::ModifiersState::SUPER
        }
        #[cfg(not(target_os = "macos"))]
        {
            aterm_window::keyboard::ModifiersState::CONTROL
        }
    }

    fn tab_order(app: &crate::App, wid: crate::WindowId) -> Vec<crate::tab_model::TabId> {
        app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .map(|tab| tab.id)
            .collect()
    }

    fn chip_span(app: &crate::App, wid: crate::WindowId, index: usize) -> (u16, u16) {
        let seg = app.windows[&wid]
            .tab_segments
            .iter()
            .find(|seg| seg.kind == crate::tab_bar::TabHit::Select(index))
            .expect("the tab has a laid-out chip");
        (seg.start_col, seg.end_col)
    }

    fn strip_point(app: &crate::App, wid: crate::WindowId, col: u16) -> (f64, f64) {
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f64;
        let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
        let point = (
            pad + f64::from(col) * cw as f64 + cw as f64 * 0.5,
            top + ch as f64 * 0.5,
        );
        assert_eq!(
            app.strip_col_at(wid, point.0, point.1),
            Some(col),
            "fixture point must land on strip column {col}"
        );
        point
    }

    fn app_with_three_chips() -> (crate::App, crate::WindowId, Vec<crate::tab_model::TabId>) {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        app.splice_tab_strip(wid);
        let order = tab_order(&app, wid);
        assert_eq!(order.len(), 3, "three chips to shuffle");
        (app, wid, order)
    }

    #[test]
    fn a_sweep_across_two_chips_walks_the_tab_two_places() {
        use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

        let (mut app, wid, before) = app_with_three_chips();
        let grab = strip_point(&app, wid, chip_span(&app, wid, 2).0 + 1);
        app.on_cursor_moved(wid, grab.0, grab.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);

        let over = strip_point(&app, wid, chip_span(&app, wid, 0).0);
        app.on_cursor_moved(wid, over.0, over.1);
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);

        assert_eq!(
            tab_order(&app, wid),
            vec![before[2], before[0], before[1]],
            "the dragged chip walked all the way to the front"
        );
        assert_eq!(app.windows[&wid].tabs.active, 0);
    }

    #[test]
    fn a_press_that_never_crosses_a_midpoint_never_reorders() {
        use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

        let (mut app, wid, before) = app_with_three_chips();
        let (start, end) = chip_span(&app, wid, 0);
        let grab = strip_point(&app, wid, start + 1);
        app.on_cursor_moved(wid, grab.0, grab.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert!(
            app.windows[&wid].strip_drag.is_some(),
            "the chip IS held — this test is about what a held chip does NOT do"
        );

        app.on_cursor_moved(wid, grab.0 + 0.25, grab.1 + 0.25);
        assert_eq!(tab_order(&app, wid), before, "jitter is not a drag");

        let inside = strip_point(&app, wid, end - 1);
        app.on_cursor_moved(wid, inside.0, inside.1);
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        assert_eq!(
            tab_order(&app, wid),
            before,
            "travel within the chip's own share crosses nothing"
        );
    }

    #[test]
    fn an_interruption_mid_drag_stops_the_held_chip() {
        use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

        for interruption in ["focus loss", "context menu", "palette"] {
            let (mut app, wid, before) = app_with_three_chips();
            let (grab_start, _) = chip_span(&app, wid, 0);
            let grab = strip_point(&app, wid, grab_start + 1);
            app.on_cursor_moved(wid, grab.0, grab.1);
            app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
            assert!(
                app.windows[&wid].strip_drag.is_some(),
                "{interruption}: the chip is held"
            );
            match interruption {
                "focus loss" => {
                    app.on_focus(wid, false);
                }
                "context menu" => {
                    assert!(
                        app.open_tab_context_menu(wid, 0, grab_start + 1, false),
                        "the chip has a composed menu"
                    );
                }
                _ => {
                    app.toggle_palette();
                    assert!(
                        app.windows[&wid].palette().is_some(),
                        "fixture guard: the palette really opened"
                    );
                }
            }
            let frozen = interruption == "palette";
            assert_eq!(
                app.windows[&wid].strip_drag.is_some(),
                frozen,
                "{interruption}: the grab is dropped (or, under the palette, frozen)"
            );
            let (start, end) = chip_span(&app, wid, 1);
            let over = strip_point(&app, wid, start + (end - start) / 2);
            app.on_cursor_moved(wid, over.0, over.1);
            assert_eq!(
                tab_order(&app, wid),
                before,
                "{interruption}: motion reorders nothing"
            );
            if frozen {
                app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
                assert!(
                    app.windows[&wid].strip_drag.is_none(),
                    "the release disarmed through the hoisted disarm"
                );
            }
        }
    }

    #[test]
    fn a_press_that_is_no_grab_arms_no_reorder() {
        use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

        for renaming in [false, true] {
            let (mut app, wid, before) = app_with_three_chips();
            let col = if renaming {
                assert!(
                    app.begin_session_rename(wid, before[0]),
                    "fixture guard: the editor opened on chip 0"
                );
                chip_span(&app, wid, 0).0 + 1
            } else {
                let band = app.windows[&wid]
                    .tab_segments
                    .iter()
                    .find(|seg| seg.kind == crate::tab_bar::TabHit::NewTab)
                    .map(|seg| seg.end_col)
                    .expect("a laid-out strip");
                assert!(
                    crate::tab_bar::hit_test(&app.windows[&wid].tab_segments, band).is_none(),
                    "fixture column must be bare band"
                );
                band
            };
            let point = strip_point(&app, wid, col);
            app.on_cursor_moved(wid, point.0, point.1);
            app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
            assert!(
                app.windows[&wid].strip_drag.is_none(),
                "renaming={renaming}: the press never arms a drag"
            );
            if renaming {
                assert!(
                    app.inline_rename_edit(wid).is_some(),
                    "…and the editor is still open"
                );
            }
            let (start, end) = chip_span(&app, wid, 1);
            let over = strip_point(&app, wid, start + (end - start) / 2);
            app.on_cursor_moved(wid, over.0, over.1);
            assert_eq!(
                tab_order(&app, wid),
                before,
                "renaming={renaming}: a sweep past a midpoint moves nothing"
            );
            app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        }
    }

    #[test]
    fn a_lone_chip_has_nowhere_to_go() {
        use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        app.splice_tab_strip(wid);
        let before = tab_order(&app, wid);
        assert_eq!(before.len(), 1);

        let (start, end) = chip_span(&app, wid, 0);
        let grab = strip_point(&app, wid, start);
        app.on_cursor_moved(wid, grab.0, grab.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        for col in start..end {
            let point = strip_point(&app, wid, col);
            app.on_cursor_moved(wid, point.0, point.1);
        }
        assert_eq!(
            app.windows[&wid].strip_drag.map(|drag| drag.origin_col),
            Some(start),
            "a lone chip has no neighbour to cross, so it never took a step"
        );
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        assert_eq!(tab_order(&app, wid), before);
        assert_eq!(app.windows[&wid].tab_set.len(), 1);
    }

    #[test]
    fn under_pressure_the_wide_active_chip_marches_across_the_strip() {
        use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        for _ in 0..7 {
            app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        }
        app.splice_tab_strip(wid);
        let before = tab_order(&app, wid);
        assert_eq!(before.len(), 8, "eight chips to force the pressure layout");

        let seated = |app: &crate::App| -> Vec<usize> {
            app.windows[&wid]
                .tab_segments
                .iter()
                .filter_map(|seg| match seg.kind {
                    crate::tab_bar::TabHit::Select(index) => Some(index),
                    _ => None,
                })
                .collect()
        };
        let held = seated(&app)[0];
        assert!(
            seated(&app).len() < before.len(),
            "eight tabs on an 80-column strip must be more than it can seat"
        );

        let grab = strip_point(&app, wid, chip_span(&app, wid, held).0 + 1);
        app.on_cursor_moved(wid, grab.0, grab.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert_eq!(
            app.windows[&wid].tabs.active, held,
            "the press selected the chip under it"
        );
        app.splice_tab_strip(wid); // the repaint the selection requested

        let width = |index: usize| {
            let (start, end) = chip_span(&app, wid, index);
            end - start
        };
        let widths: Vec<u16> = seated(&app).iter().map(|&i| width(i)).collect();
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "a seated page divides its band equally: {widths:?}"
        );
        assert!(
            widths[0] > 80 / before.len() as u16,
            "eight tabs on this strip must engage the pressure layout: a seated \
             chip ({} cols) has to beat the equal eighth it refused to shrink to",
            widths[0]
        );

        let mut motions = 0;
        let mut last_col = None::<u16>;
        while app.windows[&wid].tabs.active + 1 < before.len() {
            motions += 1;
            assert!(
                motions <= before.len(),
                "the march must terminate: {:?}",
                tab_order(&app, wid)
            );
            let was = app.windows[&wid].tabs.active;
            let col = if seated(&app).contains(&(was + 1)) {
                let (start, end) = chip_span(&app, wid, was + 1);
                start + (end - start) / 2
            } else {
                chip_span(&app, wid, was).1
            }
            .max(last_col.map_or(0, |c| c + 1));
            last_col = Some(col);
            let over = strip_point(&app, wid, col);
            app.on_cursor_moved(wid, over.0, over.1);
            let now = app.windows[&wid].tabs.active;
            assert!(
                now > was,
                "motion {motions}: the held chip advanced (was {was}, now {now})"
            );
            assert_eq!(
                tab_order(&app, wid)[now],
                before[held],
                "motion {motions}: the dragged tab is the one that moved, and it \
                 is still the selected one"
            );
            app.splice_tab_strip(wid);
            app.on_cursor_moved(wid, over.0, over.1);
            assert_eq!(
                app.windows[&wid].tabs.active, now,
                "motion {motions}: a settled pointer must not oscillate after \
                 the repaint"
            );
            assert_eq!(tab_order(&app, wid)[now], before[held]);
        }
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);

        let mut expect: Vec<_> = before.clone();
        let moved = expect.remove(held);
        expect.push(moved);
        assert_eq!(
            tab_order(&app, wid),
            expect,
            "the chip marched from the front of the strip to the back"
        );
        assert_eq!(app.windows[&wid].tabs.active, before.len() - 1);
        assert!(app.windows[&wid].strip_drag.is_none());
    }

    #[test]
    fn a_line_or_word_drag_re_anchors_on_its_origin_after_output_scrolls_the_grid() {
        use crate::{App, WindowId, term_lock};

        for word in [false, true] {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            let term = app
                .front_terminal(wid)
                .expect("front terminal")
                .term
                .clone();
            let tag = if word { "w" } else { "L" };
            term_lock(&term).process(&numbered_screen(tag));

            if word {
                app.select_word_click(wid, 5, 0);
            } else {
                app.select_line_click(wid, 5, 0);
            }
            let origin = format!("{tag}05");
            assert_eq!(
                term_lock(&term).selection_to_string().as_deref(),
                Some(origin.as_str()),
                "precondition: the click selected {origin}"
            );

            term_lock(&term).process(b"\r\nX\r\nY\r\nZ");
            assert_eq!(
                app.windows[&wid].gesture.map(|g| g.row),
                Some(5),
                "precondition: the captured gesture row is NOT compensated — that is the trap"
            );
            assert!(
                term_lock(&term).text_selection().has_selection(),
                "precondition: ordinary output must not clear the selection (Phase 4)"
            );

            app.drag_selection(wid, 8, 0);

            let text = term_lock(&term)
                .selection_to_string()
                .expect("the drag resolves to text");
            assert_eq!(
                text.lines().next(),
                Some(origin.as_str()),
                "word={word}: the drag must stay anchored on its ORIGIN; anchoring on the \
                 stale gesture row would start the copy at row 8 — text the user never \
                 selected"
            );
            if !word {
                assert_eq!(
                    text.lines().last(),
                    Some("L11"),
                    "…and reach the hovered line"
                );
            }
        }
    }

    #[test]
    fn a_drag_with_no_live_selection_falls_back_to_the_captured_gesture_row() {
        use crate::{App, WindowId, term_lock};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        term_lock(&term).process(&numbered_screen("L"));

        app.select_line_click(wid, 5, 0);
        term_lock(&term).text_selection_mut().clear();

        app.drag_selection(wid, 8, 0);
        let text = term_lock(&term)
            .selection_to_string()
            .expect("the fallback still builds a selection");
        assert_eq!(
            text.lines().next(),
            Some("L05"),
            "with nothing live to read, the captured gesture row is the origin"
        );
    }

    fn wrapped_command_screen() -> (Vec<u8>, String) {
        const HEAD: &str =
            "docker run --rm -v $PWD:/w -w /w --network host --name bld-boxy aterm-build rust";
        const TAIL: &str = ":1.85 cargo build --release --features gpu,wayland";
        assert_eq!(HEAD.len(), 80, "the head must fill the 80-column row");
        let cmd = format!("{HEAD}{TAIL}");
        let mut seed = cmd.clone().into_bytes();
        seed.extend_from_slice(b"\r\necho AFTER");
        (seed, cmd)
    }

    #[test]
    fn a_triple_click_copies_the_whole_soft_wrapped_command() {
        use crate::{App, WindowId, term_lock};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        let (seed, cmd) = wrapped_command_screen();
        term_lock(&term).process(&seed);
        assert_eq!(
            term_lock(&term).get_line_text(0, None).map(|t| t.len()),
            Some(80),
            "precondition: row 0 is full, so the command wraps onto row 1"
        );

        app.select_line_click(wid, 0, 10);

        let copied = term_lock(&term)
            .selection_to_string()
            .expect("the triple-click selected something");
        assert_eq!(
            copied.len(),
            cmd.len(),
            "the clipboard must carry all {} bytes of the command, not the 80 \
             that fit on the clicked row",
            cmd.len()
        );
        assert_eq!(copied, cmd);
        assert!(
            !copied.contains('\n'),
            "a soft wrap must not become a newline in the copy: {copied:?}"
        );
        assert!(
            !copied.contains("echo AFTER"),
            "the HARD newline after the command must still end the selection"
        );
        assert_eq!(
            app.windows[&wid].gesture.map(|g| (g.row, g.end_row)),
            Some((0, 1)),
            "the armed gesture origin must be the whole logical line"
        );
    }

    #[test]
    fn a_double_click_copies_a_word_straddling_the_wrap_whole() {
        use crate::{App, WindowId, term_lock};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        let filler = "word ".repeat(14) + "abc ";
        assert_eq!(filler.len(), 74);
        term_lock(&term).process(format!("{filler}STRADDLEWORD tail").as_bytes());

        app.select_word_click(wid, 0, 76);
        assert_eq!(
            term_lock(&term).selection_to_string().as_deref(),
            Some("STRADDLEWORD"),
            "the double-click must reach across the soft wrap for the rest of the word"
        );
        assert_eq!(
            app.windows[&wid]
                .gesture
                .map(|g| ((g.row, g.start_col), (g.end_row, g.end_col))),
            Some(((0, 74), (1, 5))),
            "the armed origin word must be the whole word, on both rows"
        );
    }

    #[test]
    fn a_drag_from_a_wrapped_line_keeps_the_whole_origin_line() {
        use crate::{App, WindowId, term_lock};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        let (mut seed, cmd) = wrapped_command_screen();
        let mut screen = b"BEFORE\r\n".to_vec();
        screen.append(&mut seed);
        term_lock(&term).process(&screen);
        assert!(
            term_lock(&term).row_continues_previous(2),
            "precondition: the command wraps from row 1 onto row 2"
        );

        app.select_line_click(wid, 1, 10);
        app.drag_selection(wid, 3, 0);
        assert_eq!(
            term_lock(&term)
                .selection_to_string()
                .expect("the drag resolves to text"),
            format!("{cmd}\necho AFTER"),
            "the whole wrapped origin line, then the hovered line — one newline \
             between them, at the HARD break only"
        );

        app.drag_selection(wid, 0, 0);
        assert_eq!(
            term_lock(&term)
                .selection_to_string()
                .expect("the drag resolves to text"),
            format!("BEFORE\n{cmd}"),
            "dragging up past the origin must keep the whole wrapped origin line; \
             anchoring on the clicked ROW would stop the copy at the window width"
        );

        app.drag_selection(wid, 2, 0);
        assert_eq!(
            term_lock(&term).selection_to_string().as_deref(),
            Some(cmd.as_str()),
            "dragging within the origin logical line must keep it whole"
        );
    }

    fn numbered_screen(tag: &str) -> Vec<u8> {
        let mut seed = Vec::new();
        for r in 0..24u16 {
            if r > 0 {
                seed.extend_from_slice(b"\r\n");
            }
            seed.extend_from_slice(format!("{tag}{r:02}").as_bytes());
        }
        seed
    }

    #[test]
    fn blur_or_a_tab_switch_settles_the_in_flight_selection() {
        use aterm_core::selection::SelectionState;

        use crate::{App, WindowId, stub_session, term_lock};

        for tab_switch in [false, true] {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            let term = app
                .front_terminal(wid)
                .expect("front terminal")
                .term
                .clone();
            term_lock(&term).process(b"hello world");

            app.windows.get_mut(&wid).expect("window").last_mouse_cell = (0, 0);
            app.begin_selection(wid, SelectionType::Simple);
            app.drag_selection(wid, 0, 5);
            assert_eq!(
                term_lock(&term).text_selection().state(),
                SelectionState::InProgress,
                "precondition: the sweep leaves the engine mid-gesture"
            );

            if tab_switch {
                let sid = app.next_session_id;
                app.push_stub_tab(wid, stub_session(sid));
            } else {
                app.on_focus(wid, false);
            }

            assert_eq!(
                term_lock(&term).text_selection().state(),
                SelectionState::Complete,
                "tab_switch={tab_switch}: COMPLETE the selection, not strand it InProgress"
            );
            assert_eq!(
                term_lock(&term).selection_to_string().as_deref(),
                Some("hello"),
                "tab_switch={tab_switch}: the settled selection still names the swept text"
            );
            if !tab_switch {
                assert!(
                    app.extend_selection_to(wid, 0, 10, SelectionSide::Right),
                    "the settled selection is extendable — the InProgress zombie was not"
                );
            }
        }
    }

    #[test]
    fn a_selection_on_a_double_width_line_takes_the_characters_under_the_pointer() {
        use crate::{App, WindowId, term_lock};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        term_lock(&term).process(b"\x1b#6HELLO WORLD\r\nplain text");

        app.windows.get_mut(&wid).expect("window").last_mouse_cell = (0, 0);
        app.begin_selection(wid, SelectionType::Simple);
        app.drag_selection(wid, 0, 9);
        assert_eq!(
            term_lock(&term).selection_to_string().as_deref(),
            Some("HELLO"),
            "a drag over the cells HELLO is drawn in"
        );
        app.finish_selection(wid, true);

        app.seam_left_press(wid, 0, 12, 2, false);
        assert_eq!(
            term_lock(&term).selection_to_string().as_deref(),
            Some("WORLD"),
            "a double-click on the W drawn at screen cell 12"
        );
        app.finish_selection(wid, true);

        app.windows.get_mut(&wid).expect("window").last_mouse_cell = (0, 0);
        app.begin_selection(wid, SelectionType::Simple);
        app.drag_selection(wid, 0, 3);
        app.finish_selection(wid, true);
        assert_eq!(
            term_lock(&term).selection_to_string().as_deref(),
            Some("HE"),
            "precondition: a selection to extend"
        );
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.last_mouse_cell = (0, 9);
        ws.last_mouse_side = SelectionSide::Left;
        app.shift_extend_press(wid);
        assert_eq!(
            term_lock(&term).selection_to_string().as_deref(),
            Some("HELLO"),
            "a shift-click on the right half of the O"
        );

        app.windows.get_mut(&wid).expect("window").last_mouse_cell = (1, 0);
        app.begin_selection(wid, SelectionType::Simple);
        app.drag_selection(wid, 1, 5);
        assert_eq!(
            term_lock(&term).selection_to_string().as_deref(),
            Some("plain")
        );
    }

    fn stacked_autoscroll_fixture() -> (crate::App, crate::WindowId, u64, u64) {
        use crate::{App, WindowId, pane, term_lock};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let top = app.focused_session_id(wid).expect("top terminal");
        let bottom = app.split_active_stub_tab_dir(wid, pane::SplitDir::Horizontal);
        let plan = app
            .active_visible_leaf_plan(wid)
            .expect("stacked visible plan");
        assert_eq!(plan.dividers.len(), 1);
        assert_eq!(
            plan.dividers[0].axis,
            crate::tab_model::SplitAxis::Vertical,
            "fixture must stack panes rather than place them side by side"
        );
        assert_eq!(app.focused_session_id(wid), Some(bottom));

        for session in [top, bottom] {
            let term = app.pool.get(session).expect("pooled terminal").term.clone();
            let mut terminal = term_lock(&term);
            for line in 0..100 {
                terminal.process(format!("pane {session} history {line}\r\n").as_bytes());
            }
            assert!(terminal.grid().scrollback_lines() > 8);
            assert_eq!(terminal.grid().display_offset(), 0);
        }
        (app, wid, top, bottom)
    }

    #[test]
    fn split_selection_motion_reuses_the_mapped_pane_rect() {
        use crate::{reset_visible_leaf_plan_builds, visible_leaf_plan_builds};

        let (mut app, wid, _, _) = stacked_autoscroll_fixture();
        let (row, col, rows, cols) = app.focused_pane_rect(wid);
        assert!(rows > 2 && cols > 2, "fixture needs an interior pane cell");
        let (x, y) = app
            .cell_centre_px(wid, row + 1, col + 1)
            .expect("interior bottom-pane pixel");

        reset_visible_leaf_plan_builds();
        app.on_cursor_moved(wid, x, y);
        let plain_motion_builds = visible_leaf_plan_builds();
        assert!(
            plain_motion_builds > 0,
            "split fixture must exercise planning"
        );

        app.begin_selection(wid, SelectionType::Simple);
        reset_visible_leaf_plan_builds();
        app.on_cursor_moved(wid, x, y);
        assert_eq!(
            visible_leaf_plan_builds(),
            plain_motion_builds,
            "autoscroll must reuse refresh_mouse_cell's pane rect"
        );
    }

    #[test]
    fn a_stacked_pane_autoscrolls_at_its_own_edge() {
        use crate::term_lock;

        for top_pane in [true, false] {
            let (mut app, wid, top, bottom) = stacked_autoscroll_fixture();
            if top_pane {
                assert!(
                    app.active_tree_mut(wid)
                        .expect("terminal pane tree")
                        .set_focus(top)
                );
                let active = app.windows[&wid].tabs.active;
                assert!(app.sync_tab_model_from_layout(wid, active));
                app.sync_window(wid);
                assert_eq!(app.focused_session_id(wid), Some(top));
            }
            let sid = if top_pane { top } else { bottom };
            let term = app.pool.get(sid).expect("pane terminal").term.clone();
            if top_pane {
                term_lock(&term).scroll_display(5);
            }
            app.windows.get_mut(&wid).expect("window").selecting = true;

            let (pane_row, _, pane_rows, _) = app.focused_pane_rect(wid);
            assert!(pane_rows < app.windows[&wid].rows);
            let ch = app.win_cell_size(wid).1.max(1);
            let strip_px = usize::from(app.chrome_rows(wid)) * ch;
            let top_px = app.win_pad_top(wid) + app.win_head(wid);
            let pointer = if top_pane {
                (top_px + strip_px + usize::from(pane_row.saturating_add(pane_rows)) * ch) as f64
            } else {
                assert!(pane_row > 0, "the new terminal is the bottom pane");
                (top_px + strip_px + usize::from(pane_row) * ch) as f64 - 1.0
            };
            assert_eq!(
                crate::app_render::selection_autoscroll_lines(
                    pointer,
                    top_px,
                    strip_px,
                    ch,
                    app.windows[&wid].rows,
                ),
                0,
                "top_pane={top_pane}: negative control: inside the whole-window grid"
            );
            let (_, frame_y) = app.frame_origin(wid);
            assert!(app.selection_autoscroll(wid, frame_y as f64 + pointer));
            assert_eq!(
                term_lock(&term).grid().display_offset(),
                if top_pane { 4 } else { 1 },
                "top_pane={top_pane}: crossing the pane's own edge scrolls it"
            );
        }
    }

    #[test]
    fn tab_strip_selection_motion_refreshes_and_extends_the_endpoint_immediately() {
        use crate::{App, WindowId, term_lock};

        fn run(with_history: bool) -> (usize, i32, u16, (u16, u16), bool) {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            app.tab_strip_rows = 1;
            app.splice_tab_strip(wid);
            let term = app
                .front_terminal(wid)
                .expect("front terminal")
                .term
                .clone();
            if with_history {
                term_lock(&term).process(&numbered_screen("L"));
                term_lock(&term).process(b"\r\nL24");
                assert_eq!(term_lock(&term).grid().scrollback_lines(), 1);
            }
            app.windows.get_mut(&wid).expect("window").last_mouse_cell = (8, 3);
            app.begin_selection(wid, SelectionType::Simple);

            let (cw, ch) = app.win_cell_size(wid);
            let (frame_x, frame_y) = app.frame_origin(wid);
            let strip_col = 7u16;
            let x =
                frame_x as f64 + (app.win_pad(wid) + usize::from(strip_col) * cw + cw / 2) as f64;
            let y = frame_y as f64 + (app.win_pad_top(wid) + app.win_head(wid) + ch / 2) as f64;
            assert_eq!(app.strip_col_at(wid, x, y), Some(strip_col));

            app.on_cursor_moved(wid, x, y);

            let terminal = term_lock(&term);
            (
                terminal.grid().display_offset(),
                terminal.text_selection().end().row,
                terminal.text_selection().end().col,
                app.windows[&wid].last_mouse_cell,
                app.windows[&wid].next_autoscroll.is_some(),
            )
        }

        assert_eq!(run(true), (1, -1, 7, (0, 7), true));
        assert_eq!(
            run(false),
            (0, 0, 7, (0, 7), true),
            "endpoint motion does not depend on viewport motion"
        );
    }

    #[test]
    fn selection_autoscroll_disarms_when_the_front_terminal_is_gone() {
        use std::time::Instant;

        use crate::{App, WindowId};

        let wid = WindowId(0);
        let mut native = App::headless_for_test();
        assert!(native.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        {
            let window = native.windows.get_mut(&wid).expect("headless window");
            window.selecting = true;
            window.next_autoscroll = Some(Instant::now());
        }
        let past_bottom = 10_000.0;
        let guard_ch = native.win_cell_size(wid).1;
        assert_ne!(
            crate::app_render::selection_autoscroll_lines(
                past_bottom,
                native.win_pad_top(wid) + native.win_head(wid),
                usize::from(native.chrome_rows(wid)) * guard_ch,
                guard_ch,
                native.windows[&wid].rows,
            ),
            0,
            "fixture must place the pointer past a grid edge"
        );
        assert!(!native.selection_autoscroll(wid, past_bottom));
        assert!(
            native.windows[&wid].next_autoscroll.is_none(),
            "a retired front terminal must not leave an already-due wake armed"
        );

        let mut terminal = App::headless_for_test();
        terminal
            .windows
            .get_mut(&wid)
            .expect("headless window")
            .selecting = true;
        assert!(!terminal.selection_autoscroll(wid, past_bottom));
        assert!(
            terminal.windows[&wid].next_autoscroll.is_some(),
            "a live edge drag must still rearm"
        );
    }

    #[test]
    fn a_selection_drag_above_the_window_top_scrolls_into_history() {
        use crate::{App, WindowId};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        {
            let mut terminal = crate::term_lock(&term);
            for line in 0..100 {
                terminal.process(format!("history {line}\r\n").as_bytes());
            }
            assert!(terminal.grid().scrollback_lines() > 8);
            assert_eq!(
                terminal.grid().display_offset(),
                0,
                "the drag starts at the live bottom"
            );
        }
        app.windows
            .get_mut(&wid)
            .expect("headless window")
            .selecting = true;

        assert!(
            app.selection_autoscroll(wid, -1.0),
            "one pixel above the top edge scrolls one line into history"
        );
        assert_eq!(crate::term_lock(&term).grid().display_offset(), 1);
        assert!(app.windows[&wid].next_autoscroll.is_some());

        let ch = app.win_cell_size(wid).1 as f64;
        assert!(app.selection_autoscroll(wid, -4.0 * ch));
        assert_eq!(crate::term_lock(&term).grid().display_offset(), 6);
    }

    fn band_pixel(app: &crate::App, wid: crate::WindowId) -> impl Fn(usize) -> (f64, f64) + use<> {
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f64;
        let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64
            + f64::from(app.tab_strip_rows) * ch as f64;
        move |col: usize| (pad + (col as f64 + 0.5) * cw as f64, top + ch as f64 * 0.5)
    }

    fn toolchain_row() -> aterm_messages::Message {
        aterm_messages::Message::new(
            aterm_messages::tags::TOOLCHAIN,
            aterm_messages::Severity::Info,
            "Installing ALab toolchain",
        )
        .line("trust — extracting")
        .action(aterm_messages::Intent::OpenSettings {
            route: "/packages".into(),
        })
        .hold(aterm_messages::Hold::Live {
            stale_after: aterm_messages::STALE_TAILED,
        })
    }

    fn band_row(app: &crate::App, wid: crate::WindowId) -> aterm_messages::RowLayout {
        let cols = usize::from(app.windows[&wid].cols);
        app.band_presentation(cols).rows[0].clone()
    }

    fn capsule_col(app: &crate::App, wid: crate::WindowId) -> usize {
        band_row(app, wid)
            .capsules
            .iter()
            .find(|c| !c.action.is_details())
            .expect("an authored capsule")
            .col
            + 1
    }

    fn acted(
        app: &crate::App,
        id: aterm_messages::MessageId,
    ) -> Option<aterm_messages::ActionIndex> {
        app.messages.live(id).and_then(|l| l.acted.map(|(k, _)| k))
    }

    fn settings_route(
        app: &crate::App,
        wid: crate::WindowId,
    ) -> Option<crate::native_settings::SettingsRoute> {
        let (_, view) = app.active_native_view(wid)?;
        match app.native_runtime.view_state(view)? {
            crate::native_app::AppViewState::Settings(state) => Some(state.route),
            _ => None,
        }
    }

    fn selected_message(app: &crate::App, wid: crate::WindowId) -> Option<u64> {
        let (_, view) = app.active_native_view(wid)?;
        match app.native_runtime.view_state(view)? {
            crate::native_app::AppViewState::Settings(state) => state.messages_selected,
            _ => None,
        }
    }

    #[test]
    fn a_press_on_a_band_capsule_performs_its_intent_and_stops_there() {
        use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let id = app.post_message(toolchain_row());
        assert_eq!(app.message_band_rows, 1);
        assert_eq!(
            settings_route(&app, wid),
            None,
            "a terminal fronts the window"
        );
        let at = band_pixel(&app, wid);
        let (x, y) = at(capsule_col(&app, wid));
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        assert!(
            app.messages.live(id).is_some(),
            "an OpenSettings press keeps the row"
        );
        assert_eq!(
            acted(&app, id),
            Some(aterm_messages::ActionIndex(0)),
            "the engine recorded the press"
        );
        assert_eq!(
            settings_route(&app, wid),
            Some(crate::native_settings::SettingsRoute::Packages),
            "Settings ▸ Packages opened in the pressed window"
        );
        {
            let ws = &app.windows[&wid];
            assert!(
                !ws.selecting && ws.held_mouse_button.is_none(),
                "the press never reached the grid"
            );
        }

        let ch = app.win_cell_size(wid).1 as f64;
        assert!(
            !app.press_band_at(wid, x, y + ch),
            "one row below the band is the grid's"
        );

        let mut app = crate::App::headless_for_test();
        let id = app.post_message(toolchain_row());
        let details = band_row(&app, wid)
            .capsules
            .into_iter()
            .find(|c| c.action.is_details())
            .expect("the row's Details \u{203a}");
        let at = band_pixel(&app, wid);
        let (x, y) = at(details.col + 1);
        assert!(app.press_band_at(wid, x, y));
        assert_eq!(acted(&app, id), None);
        assert_eq!(selected_message(&app, wid), Some(id.raw()));
    }

    #[test]
    fn a_press_on_the_paste_question_over_the_band_answers_no_first() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.prepare_terminal_capture_grid(wid)
            .expect("the scratch composes headless");
        let id = app.post_message(toolchain_row());
        let question = |wid: crate::WindowId| {
            crate::paste_banner::PendingPaste::new(
                wid,
                Some(0),
                "line one\nline two\n".to_string(),
                crate::input::Source::Human,
                crate::input::PasteFraming::AtDrain,
            )
        };
        app.paste_banner = Some(question(wid));
        assert!(
            app.paste_banner_tray_floor_y(wid) > 0,
            "the question owns the top rows of this window"
        );
        let at = band_pixel(&app, wid);
        let (x, y) = at(capsule_col(&app, wid));
        assert!(app.press_band_at(wid, x, y), "consumed");
        assert!(app.paste_banner.is_none(), "the question is answered: no");
        assert_eq!(
            acted(&app, id),
            None,
            "the capsule under the question was not pressed"
        );
        assert_eq!(settings_route(&app, wid), None);
        assert!(app.press_band_at(wid, x, y));
        assert_eq!(
            acted(&app, id),
            Some(aterm_messages::ActionIndex(0)),
            "answered, the same press is the capsule's"
        );

        let mut app = crate::App::headless_for_test();
        app.prepare_terminal_capture_grid(wid)
            .expect("the scratch composes headless");
        let id = app.post_message(toolchain_row());
        app.paste_banner = Some(question(crate::WindowId(7)));
        assert_eq!(
            app.paste_banner_tray_floor_y(wid),
            0,
            "another window's question owns none of this window's rows"
        );
        let at = band_pixel(&app, wid);
        let (x, y) = at(capsule_col(&app, wid));
        assert!(app.press_band_at(wid, x, y));
        assert_eq!(acted(&app, id), Some(aterm_messages::ActionIndex(0)));
        assert!(
            app.paste_banner.is_some(),
            "the other window's question stands"
        );

        let mut app = crate::App::headless_for_test();
        app.prepare_terminal_capture_grid(wid)
            .expect("the scratch composes headless");
        assert_eq!(app.message_band_rows, 0, "no band row is committed");
        app.paste_banner = Some(question(wid));
        assert!(
            app.paste_banner_tray_floor_y(wid) > 0,
            "the question owns the top rows without a band under it"
        );
        let at = band_pixel(&app, wid);
        let (x, y) = at(4);
        assert!(
            app.press_band_at(wid, x, y),
            "consumed: the question's answer"
        );
        assert!(app.paste_banner.is_none(), "answered: no");
        assert_eq!(settings_route(&app, wid), None);
        app.paste_banner = Some(question(wid));
        let ch = app.win_cell_size(wid).1 as f64;
        let cols = app.windows[&wid].cols as usize;
        let rows = app.paste_banner.as_ref().map_or(0, |p| p.wanted_rows(cols)) as f64;
        assert!(
            !app.press_band_at(wid, x, y + rows * ch),
            "below the question, with no band: nobody's press"
        );
        assert!(app.paste_banner.is_some(), "the question stands");
    }

    #[test]
    fn band_capsules_work_on_a_window_fronting_settings() {
        use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab_in_window(wid, crate::native_settings::SettingsRoute::Home));
        assert_eq!(
            settings_route(&app, wid),
            Some(crate::native_settings::SettingsRoute::Home),
            "Settings fronts the window"
        );
        let id = app.post_message(toolchain_row());
        assert_eq!(app.message_band_rows, 1);
        let at = band_pixel(&app, wid);
        let (x, y) = at(capsule_col(&app, wid));
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        assert_eq!(
            acted(&app, id),
            Some(aterm_messages::ActionIndex(0)),
            "the band took the press over the native view"
        );
        assert_eq!(
            settings_route(&app, wid),
            Some(crate::native_settings::SettingsRoute::Packages),
            "the same Settings tab, now on Packages"
        );
    }

    #[test]
    fn a_pointer_over_the_chrome_above_a_native_page_reads_back_the_chrome_row() {
        use super::PointerAt;
        let (mut app, wid, _) =
            settings_at_1000_by_600(crate::native_settings::SettingsRoute::KeyboardInput);
        let _ = app.post_message(toolchain_row());
        assert!(app.chrome_rows(wid) >= 1, "the band holds a chrome row");
        let col = u16::try_from(capsule_col(&app, wid)).unwrap();
        let cell = app
            .pointer_cmd(PointerAction::MoveChrome { up: 1, col })
            .expect("the band's row is a chrome row");
        assert_eq!(cell, Some((-1, col)));
        assert!(
            app.windows[&wid].band_hover.is_some(),
            "the band is hovered"
        );
        match app.pointer_reply(cell) {
            Ok(PointerAt::Grid { cell: at, .. }) => assert_eq!(at, (-1, col)),
            other => panic!("the reply is the chrome cell: {other:?}"),
        }
    }

    #[test]
    fn the_pointer_verb_reaches_the_band_and_clicks_a_capsule() {
        use crate::message_band::{BandHover, HoverTarget};
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        assert_eq!(
            app.pointer_cmd(PointerAction::Click),
            Err("the pointer holds no position (move it first)".to_string())
        );
        let id = app.post_message(toolchain_row());
        assert_eq!(app.message_band_rows, 1);
        let chrome = app.chrome_rows(wid);
        assert_eq!(
            app.pointer_cmd(PointerAction::MoveChrome {
                up: chrome + 1,
                col: 0
            }),
            Err("cell is outside the chrome rows".to_string())
        );
        assert_eq!(app.pointer_cmd(PointerAction::Status), Ok(None));
        let col = capsule_col(&app, wid);
        let col16 = u16::try_from(col).unwrap();
        assert_eq!(
            app.pointer_cmd(PointerAction::MoveChrome { up: 1, col: col16 }),
            Ok(Some((-1, col16))),
            "the band's row, read back where the pointer is"
        );
        assert_eq!(
            app.windows[&wid].band_hover,
            Some(BandHover {
                row: 0,
                target: HoverTarget::Capsule(aterm_messages::ActionIndex(0))
            }),
            "the capsule is lit, as under a hand"
        );
        assert_eq!(acted(&app, id), None);
        assert_eq!(app.pointer_cmd(PointerAction::Click), Ok(Some((-1, col16))));
        assert_eq!(
            acted(&app, id),
            Some(aterm_messages::ActionIndex(0)),
            "the click pressed the capsule"
        );
        let (_, y) = band_pixel(&app, wid)(col);
        assert!((app.windows[&wid].last_cursor_px.1 - y).abs() < 1.0);
        assert!(app.pointer_cmd(PointerAction::Leave).is_ok());
        assert_eq!(app.windows[&wid].band_hover, None, "off the band, unlit");
    }

    #[test]
    fn a_pointer_click_on_a_capsule_spends_the_bands_press_budget() {
        use aterm_messages::wire::spend_press;
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.post_message(toolchain_row());
        let col = u16::try_from(capsule_col(&app, wid)).unwrap();
        assert!(
            app.pointer_cmd(PointerAction::MoveChrome { up: 1, col })
                .is_ok()
        );
        assert!(app.pointer_cmd(PointerAction::Click).is_ok());
        let now = std::time::Instant::now();
        for _ in 0..9 {
            assert!(spend_press(&mut app.wire_gate, now).is_ok());
        }
        assert!(
            spend_press(&mut app.wire_gate, now).is_err(),
            "the click took one of the ten"
        );
        let mut app = crate::App::headless_for_test();
        let id = app.post_message(toolchain_row());
        let col = u16::try_from(capsule_col(&app, wid)).unwrap();
        for _ in 0..10 {
            assert!(spend_press(&mut app.wire_gate, now).is_ok());
        }
        assert!(
            app.pointer_cmd(PointerAction::MoveChrome { up: 1, col })
                .is_ok()
        );
        let refused = app
            .pointer_cmd(PointerAction::Click)
            .expect_err("no press left in the window");
        assert!(
            refused.starts_with("busy notice: 10 presses a minute retry_ms="),
            "{refused}"
        );
        assert_eq!(acted(&app, id), None, "a refused click presses nothing");
        assert!(
            app.pointer_cmd(PointerAction::Move { row: 3, col: 5 })
                .is_ok()
        );
        assert!(app.pointer_cmd(PointerAction::Click).is_ok());
    }

    #[test]
    fn a_details_press_opens_the_entry_in_the_clicked_window() {
        let mut app = crate::App::headless_for_test();
        let first = crate::WindowId(0);
        let session = app.next_session_id;
        let second = app.insert_logical_window(crate::stub_session(session), 24, 80);
        assert_eq!(
            app.frontmost_window,
            Some(second),
            "the new window is frontmost; the press below lands in the other one"
        );
        let id = app.post_message(toolchain_row());
        assert_eq!(app.message_band_rows, 1);
        let tabs = |app: &crate::App, wid: crate::WindowId| app.windows[&wid].tab_set.tabs().len();
        assert_eq!((tabs(&app, first), tabs(&app, second)), (1, 1));

        let at = band_pixel(&app, first);
        let (x, y) = at(band_row(&app, first).title.0);
        assert!(app.press_band_at(first, x, y), "consumed by the band");
        assert_eq!(
            settings_route(&app, first),
            Some(crate::native_settings::SettingsRoute::Messages),
            "Settings \u{25b8} Messages, in the pressed window"
        );
        assert_eq!(selected_message(&app, first), Some(id.raw()));
        assert_eq!(
            (tabs(&app, first), tabs(&app, second)),
            (2, 1),
            "the Settings tab went to the pressed window, not the frontmost one"
        );
        assert_eq!(settings_route(&app, second), None);
        assert_eq!(acted(&app, id), None, "the body is no capsule");
        assert!(
            app.messages.live(id).is_some_and(|l| l.seen),
            "the row is read and — a live meter — stays"
        );

        let at = band_pixel(&app, second);
        let (x, y) = at(band_row(&app, second).title.0);
        assert!(app.press_band_at(second, x, y));
        assert_eq!(
            settings_route(&app, second),
            Some(crate::native_settings::SettingsRoute::Messages)
        );
        assert_eq!(selected_message(&app, second), Some(id.raw()));
        assert_eq!((tabs(&app, first), tabs(&app, second)), (2, 2));
        let (instance_a, view_a) = app
            .active_native_view(first)
            .expect("Settings fronts the first window");
        let (instance_b, view_b) = app
            .active_native_view(second)
            .expect("Settings fronts the second window");
        assert_eq!(instance_a, instance_b, "one Settings controller");
        assert_ne!(view_a, view_b, "…presented by a view of each window's own");

        let warning = app.post_message(
            aterm_messages::Message::new(
                aterm_messages::tags::CONFIG,
                aterm_messages::Severity::Warn,
                "aterm.toml: unknown key `foo`",
            )
            .line("at line 12"),
        );
        assert_eq!(app.message_band_rows, 2);
        let cols = usize::from(app.windows[&first].cols);
        let rows = app.band_presentation(cols).rows;
        let row = rows
            .iter()
            .position(|r| matches!(r.kind, aterm_messages::RowKind::Message(m) if m == warning))
            .expect("the warning is on the band");
        let at = band_pixel(&app, first);
        let ch = app.win_cell_size(first).1 as f64;
        let (x, y) = at(rows[row].title.0);
        assert!(app.press_band_at(first, x, y + row as f64 * ch));
        assert_eq!(selected_message(&app, first), Some(warning.raw()));
        assert_eq!(tabs(&app, first), 2, "one Settings tab per window");
        assert_eq!(
            selected_message(&app, second),
            Some(id.raw()),
            "the other window's view is its own"
        );
    }

    #[test]
    fn the_overflow_row_opens_the_page_unselected() {
        use crate::message_band::{BandHover, HoverTarget};
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let ids: Vec<_> = (0..5)
            .map(|i| {
                app.post_message(
                    aterm_messages::Message::new(
                        aterm_messages::tags::CONFIG,
                        aterm_messages::Severity::Warn,
                        format!("config warning {i}"),
                    )
                    .line("why"),
                )
            })
            .collect();
        assert_eq!(app.message_band_rows, aterm_messages::MAX_ROWS);
        let cols = usize::from(app.windows[&wid].cols);
        let rows = app.band_presentation(cols).rows;
        let overflow = rows
            .iter()
            .position(|r| matches!(r.kind, aterm_messages::RowKind::Overflow { .. }))
            .expect("five rows on a three-row band: an overflow row");
        let at = band_pixel(&app, wid);
        let ch = app.win_cell_size(wid).1 as f64;
        let dy = overflow as f64 * ch;
        for col in [4, cols - 3] {
            let (x, y) = at(col);
            let target = app.band_hit_at(wid, x, y + dy).expect("on the band");
            assert_eq!(
                app.band_hover_for(wid, target),
                Some(BandHover {
                    row: overflow as u8,
                    target: HoverTarget::Body
                }),
                "the link lights from column {col}"
            );
            assert!(app.press_band_at(wid, x, y + dy), "consumed by the band");
            assert_eq!(
                settings_route(&app, wid),
                Some(crate::native_settings::SettingsRoute::Messages),
                "Settings \u{25b8} Messages, in the pressed window (column {col})"
            );
            assert_eq!(
                selected_message(&app, wid),
                None,
                "nothing expanded (column {col})"
            );
            assert_eq!(
                app.windows[&wid].tab_set.tabs().len(),
                2,
                "the terminal and one Settings tab"
            );
        }
        for id in &ids {
            let live = app.messages.live(*id).expect("every row is still live");
            assert!(!live.seen, "a link to the log reads no row");
            assert_eq!(acted(&app, *id), None);
        }
        assert_eq!(app.message_band_rows, aterm_messages::MAX_ROWS);
    }

    #[test]
    fn hover_over_the_body_lights_details() {
        use crate::message_band::{BandHover, HoverTarget, paint_rows};
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.post_message(toolchain_row());
        let layout = band_row(&app, wid);
        let packages = layout
            .capsules
            .iter()
            .find(|c| !c.action.is_details())
            .expect("Packages")
            .clone();
        let details = layout
            .capsules
            .iter()
            .find(|c| c.action.is_details())
            .expect("Details \u{203a}")
            .clone();
        let cols = usize::from(app.windows[&wid].cols);
        let theme = aterm_render::Theme::default();
        let c = crate::chrome_band::band_colors(theme);
        let lit = |app: &crate::App, cap: &aterm_messages::CapsuleLayout| {
            let p = app.band_presentation(cols);
            let still =
                app.messages
                    .motion(&p, std::time::Instant::now(), aterm_messages::Look::STILL);
            let painted = paint_rows(&p, theme, app.windows[&wid].band_hover, &still);
            painted[0][cap.col].bg == c.capsule_hover
                && painted[0][cap.col + 1].fg == c.capsule_hover_ink
        };
        let hover = |app: &crate::App| app.windows[&wid].band_hover;
        let cursor = |app: &crate::App| {
            let ws = &app.windows[&wid];
            (ws.hover_pointer, ws.native_text_cursor)
        };
        assert_eq!(hover(&app), None);
        assert!(
            !lit(&app, &details) && !lit(&app, &packages),
            "at rest nothing is lit"
        );
        let at = band_pixel(&app, wid);
        let mut body_cols = vec![layout.title.0, layout.glyph.0];
        body_cols.extend(layout.detail.as_ref().map(|(col, _)| *col));
        for col in body_cols {
            let (x, y) = at(col);
            app.on_cursor_moved(wid, x, y);
            assert_eq!(
                hover(&app),
                Some(BandHover {
                    row: 0,
                    target: HoverTarget::Body
                }),
                "column {col} is the body"
            );
            assert!(
                lit(&app, &details),
                "column {col}: the Details \u{203a} lights"
            );
            assert!(
                !lit(&app, &packages),
                "column {col}: the authored chip does not"
            );
            assert_eq!(cursor(&app), (true, false), "column {col}: the hand");
        }
        let (x, y) = at(packages.col + 1);
        app.on_cursor_moved(wid, x, y);
        assert_eq!(
            hover(&app),
            Some(BandHover {
                row: 0,
                target: HoverTarget::Capsule(packages.action)
            })
        );
        assert!(lit(&app, &packages) && !lit(&app, &details));
        assert_eq!(cursor(&app), (true, false), "the hand over a capsule");
        let (x, y) = at(details.col + 1);
        app.on_cursor_moved(wid, x, y);
        assert_eq!(
            hover(&app),
            Some(BandHover {
                row: 0,
                target: HoverTarget::Capsule(aterm_messages::ActionIndex::DETAILS)
            })
        );
        assert!(lit(&app, &details) && !lit(&app, &packages));
        assert_eq!(cursor(&app), (true, false));
        let ch = app.win_cell_size(wid).1 as f64;
        let (x, y) = at(layout.title.0);
        app.on_cursor_moved(wid, x, y + 2.0 * ch);
        assert_eq!(hover(&app), None);
        assert!(!lit(&app, &details) && !lit(&app, &packages));
        assert_eq!(cursor(&app), (false, true), "the grid's I-beam");
        let (x, y) = at(packages.col + 1);
        app.on_cursor_moved(wid, x, y);
        assert!(hover(&app).is_some());
        app.on_cursor_left(wid);
        assert_eq!(hover(&app), None, "leaving the window clears the lit chip");

        let mut app = crate::App::headless_for_test();
        app.post_message(
            aterm_messages::Message::new(
                aterm_messages::tags::CONFIG,
                aterm_messages::Severity::Warn,
                "aterm.toml: unknown key `foo`",
            )
            .line("at line 12"),
        );
        let layout = band_row(&app, wid);
        assert_eq!(layout.capsules.len(), 1, "its Details \u{203a} alone");
        let link = layout.capsules[0].clone();
        assert!(link.action.is_details());
        let at = band_pixel(&app, wid);
        let (x, y) = at(layout.title.0);
        app.on_cursor_moved(wid, x, y);
        assert_eq!(
            hover(&app),
            Some(BandHover {
                row: 0,
                target: HoverTarget::Body
            })
        );
        assert!(lit(&app, &link), "the body lights the row's one link");
    }

    #[test]
    fn r44_hb7_a_pointer_click_that_presses_nothing_on_the_band_spends_nothing() {
        use aterm_messages::wire::spend_press;
        let blank_row_click = |app: &mut crate::App| {
            let wid = crate::WindowId(0);
            let id = app.post_message(toolchain_row());
            assert!(app.withdraw_message(id));
            assert_eq!(app.message_band_rows, 1, "the shrink waits out its quiet");
            let (x, y) = band_pixel(app, wid)(4);
            let target = app.band_hit_at(wid, x, y).expect("still the band");
            assert!(
                !app.band_press_acts(wid, target),
                "a press there does nothing"
            );
            assert!(
                app.pointer_cmd(PointerAction::MoveChrome { up: 1, col: 4 })
                    .is_ok()
            );
            app.pointer_cmd(PointerAction::Click)
        };
        let mut app = crate::App::headless_for_test();
        assert!(blank_row_click(&mut app).is_ok());
        let now = std::time::Instant::now();
        for k in 0..10 {
            assert!(
                spend_press(&mut app.wire_gate, now).is_ok(),
                "press {k}: the click that pressed nothing spent one"
            );
        }
        let mut app = crate::App::headless_for_test();
        for _ in 0..10 {
            assert!(spend_press(&mut app.wire_gate, now).is_ok());
        }
        let clicked = blank_row_click(&mut app);
        assert!(clicked.is_ok(), "{clicked:?}");
    }

    #[test]
    fn r44_review_a_withdrawn_rows_chip_goes_out_under_a_still_pointer() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let id = app.post_message(toolchain_row());
        let packages = band_row(&app, wid).capsules[0].clone();
        let (x, y) = band_pixel(&app, wid)(packages.col + 1);
        app.on_cursor_moved(wid, x, y);
        assert!(app.windows[&wid].band_hover.is_some(), "the chip is lit");
        assert!(app.withdraw_message(id));
        let under = app
            .band_hit_at(wid, x, y)
            .and_then(|target| app.band_hover_for(wid, target));
        assert_eq!(under, None, "nothing a press acts on is under the pointer");
        assert_eq!(
            app.windows[&wid].band_hover, under,
            "the withdrawn row's chip stayed lit"
        );
    }

    #[test]
    fn r44_review_a_native_fronts_hand_leaves_with_its_capsule_under_a_still_pointer() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(
            app.open_settings_tab_in_window(wid, crate::native_settings::SettingsRoute::Messages)
        );
        assert!(app.active_native_view(wid).is_some(), "a native front");
        let id = app.post_message(toolchain_row());
        let packages = band_row(&app, wid).capsules[0].clone();
        let (x, y) = band_pixel(&app, wid)(packages.col + 1);
        app.on_cursor_moved(wid, x, y);
        assert!(app.windows[&wid].hover_pointer, "the hand over the capsule");
        assert!(app.withdraw_message(id));
        assert_eq!(app.message_band_rows, 1, "the shrink waits out its quiet");
        let target = app.band_hit_at(wid, x, y).expect("still the band");
        assert!(
            !app.band_press_acts(wid, target),
            "a press there does nothing now"
        );
        assert_eq!(app.windows[&wid].band_hover, None, "the chip went out");
        assert!(
            !app.windows[&wid].hover_pointer,
            "the hand stood over a band row that presses nothing"
        );
    }

    #[test]
    fn a_band_press_over_a_mixed_splits_other_pane_presses_the_band() {
        use crate::message_band::{BandHover, HoverTarget};
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        assert!(
            app.focused_session_id(wid).is_some(),
            "premise: the terminal half is focused"
        );
        let id = app.post_message(toolchain_row());
        assert_eq!(app.message_band_rows, 1);
        let col = u16::try_from(band_row(&app, wid).title.0).unwrap();
        assert_eq!(
            app.pointer_cmd(PointerAction::MoveChrome { up: 1, col }),
            Ok(Some((-1, col)))
        );
        assert_eq!(
            app.windows[&wid].band_hover,
            Some(BandHover {
                row: 0,
                target: HoverTarget::Body
            }),
            "premise: the body is lit — its press is the band's Details"
        );
        assert!(app.pointer_cmd(PointerAction::Click).is_ok());
        assert!(
            app.messages.live(id).is_some_and(|l| l.seen),
            "the band's Details press never happened: the click went to the pane under the band"
        );
        assert_eq!(
            settings_route(&app, wid),
            Some(crate::native_settings::SettingsRoute::Messages),
            "Settings \u{25b8} Messages opens at the row"
        );
    }

    #[cfg(unix)]
    #[test]
    fn band_hover_never_reports_motion_to_the_front_program() {
        use std::io::{ErrorKind, Read};
        use std::os::unix::net::UnixStream;
        fn read(reader: &mut UnixStream) -> Vec<u8> {
            let mut all = Vec::new();
            let mut bytes = [0; 256];
            loop {
                match reader.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(n) => all.extend_from_slice(&bytes[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) => panic!("reading input: {e}"),
                }
            }
            all
        }
        let (mut reader, writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let mut app = crate::App::headless_for_test_with_sink(std::sync::Arc::new(
            aterm_session::sink::SinkWriter::new_owned(writer.into()),
        ));
        let wid = crate::WindowId(0);
        app.post_message(toolchain_row());
        assert_eq!(app.message_band_rows, 1);
        let term = app.front_terminal(wid).unwrap().term.clone();
        crate::term_lock(&term).process(b"\x1b[?1003h\x1b[?1006h");
        let _ = read(&mut reader);
        let col = u16::try_from(band_row(&app, wid).title.0 + 2).unwrap();
        app.pointer_cmd(PointerAction::MoveChrome { up: 1, col })
            .unwrap();
        app.pointer_cmd(PointerAction::MoveChrome {
            up: 1,
            col: col + 1,
        })
        .unwrap();
        assert!(
            app.windows[&wid].band_hover.is_some(),
            "premise: the band row is lit"
        );
        let bytes = read(&mut reader);
        assert!(
            bytes.is_empty(),
            "a band hover reached the program as a mouse report: {:?}",
            String::from_utf8_lossy(&bytes)
        );
        app.pointer_cmd(PointerAction::Move { row: 3, col: 5 })
            .unwrap();
        let grid = read(&mut reader);
        assert!(grid.starts_with(b"\x1b[<35;"), "{grid:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_pointer_click_on_a_capsule_while_parked_is_refused() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let id = app.post_message(
            aterm_messages::Message::new(
                aterm_messages::tags::PRIVACY,
                aterm_messages::Severity::Info,
                "File access not confirmed",
            )
            .action(aterm_messages::Intent::OpenSystemPane {
                pane: "full-disk-access".into(),
            })
            .action(aterm_messages::Intent::NotNow {
                decision: aterm_messages::Decision::FileAccess,
            }),
        );
        assert_eq!(app.message_band_rows, 1);
        let not_now = band_row(&app, wid)
            .capsules
            .iter()
            .find(|c| c.action == aterm_messages::ActionIndex(1))
            .expect("the Not now capsule")
            .col
            + 1;
        let col = u16::try_from(not_now).unwrap();
        assert!(
            app.pointer_cmd(PointerAction::MoveChrome { up: 1, col })
                .is_ok()
        );
        app.pending_update_handoff = Some(crate::messages_host::tests::parked());
        let reply = app.pointer_cmd(PointerAction::Click);
        let live = app.messages.live(id).is_some();
        let pressed = acted(&app, id);
        assert_eq!(
            reply,
            Err(aterm_messages::wire::switching_reply()
                .strip_prefix("ERR ")
                .unwrap()
                .to_string()),
            "parked, the click is not refused (row live: {live}, acted: {pressed:?})"
        );
        assert!(live, "the question stands");
        assert_eq!(pressed, None, "nothing was pressed");
        let now = std::time::Instant::now();
        for k in 0..10 {
            assert!(
                aterm_messages::wire::spend_press(&mut app.wire_gate, now).is_ok(),
                "press {k}: the refused click spent one"
            );
        }
    }
}

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
