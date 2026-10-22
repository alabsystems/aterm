// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pointer, selection, hover, and link handling: the mouse/cursor event handlers
//! plus the selection gesture state machine (click streaks, word/line/block
//! select, drag, copy), pane-under-pointer focus, pixel→cell mapping, and the
//! scroll snap. A verbatim inherent-impl split of `App`.

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

/// The mouse half of the press-custody record: a user gesture that left a selection
/// behind, or one that deliberately took it away. Anchored here, not on
/// [`App::finish_selection`], which already carries `SelectionCustody`'s same-named
/// `UserClear` (the generated const `__aterm_spec_refines_{fn}_{action}` has no machine
/// name, so two machines' same-named actions cannot share a function).
///
/// `kept` comes from `ws.sel_dragged`, which nothing in `TextSelection` carries: a press and
/// release inside one cell is what makes a `clear()` legitimate. Neither variant touches
/// the viewport. Tier-1 drives the real gesture in [`crate::press_custody_conformance`].
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

/// The grid column a selection gesture anchors at for the pointer over pane-local viewport
/// cell `(row, col)`, and, on a doubled line, which half of that character it is over.
/// A DECDWL/DECDHL line is drawn at twice the advance, so grid column `c` covers screen
/// cells `2c` and `2c + 1`. The line size is read as the frame reads it (`Grid::row` of the
/// viewport row). PTY mouse reports keep the screen cell.
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

/// The selection anchor a pointer on viewport cell (`row`, `col`) makes, with its half-cell
/// `side`: the logical cell under it, since selection, copy, the word rules and find read
/// the grid in logical order (`RenderInput::selection_hit`). On a BiDi-reordered row the
/// painted column is not the logical one, and on a right-to-left cell the halves swap (as
/// VTE does); identity on every left-to-right cell. `col` is the painted grid column: any
/// other pointer → cell mapping (a doubled line's halving) happens before this call.
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

/// The logical grid column under the pointer on pane-local viewport cell (`row`, `col`), the
/// cell the frame painted there, and the side of it a press anchors before: a doubled line
/// halves the column, then the BiDi permutation maps it ([`Terminal::painted_cell_logical`],
/// the mapping a web host's `link_at` takes too), so on a right-to-left cell the side is its
/// logical end. The one painted → logical mapping a selection press and the link probes (the
/// hand, the caption, a Cmd-click, a file citation) share: the link they name is the one
/// painted under the pointer, in the cell a press selects. PTY mouse reports keep the screen
/// cell.
pub(crate) fn pointer_cell(t: &Terminal, row: u16, col: u16) -> (u16, SelectionSide) {
    match t.painted_cell_logical(row, col) {
        (logical, true) => (logical, SelectionSide::Right),
        (logical, false) => (logical, SelectionSide::Left),
    }
}

/// [`pointer_anchor`] for a press already in selection rows (`sel_row`, the
/// viewport row minus `display_offset`).
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

/// Map a winit mouse button to the engine's [`aterm_types::mouse::MouseButton`] for an
/// [`InputEvent::MouseButton`].
///
/// The thumb pair maps too: `Back`/`Forward` (XButton1/2 on Windows, `BTN_SIDE`/`BTN_EXTRA`
/// on Linux) are xterm's buttons 8/9. Every pre-dispatch consumer in `on_mouse_input` is
/// gated on a specific button, so nothing local claims them, and with tracking off the
/// `TrackingOff` consumer acts only on Left. `Other(_)` is `None`: an unmapped device button
/// has no xterm code, and inventing one would report a press the TUI cannot name.
pub(crate) fn winit_mouse_button(b: WinitMouseButton) -> Option<aterm_types::mouse::MouseButton> {
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

/// The buttons indexed by [`aterm_types::mouse::MouseButton::slot`]: xterm's
/// `FirstBitN` order (1, 2, 3, then 8 and 9), the order a held chord names its
/// button in.
const BUTTONS_BY_SLOT: [aterm_types::mouse::MouseButton; 5] = [
    aterm_types::mouse::MouseButton::Left,
    aterm_types::mouse::MouseButton::Middle,
    aterm_types::mouse::MouseButton::Right,
    aterm_types::mouse::MouseButton::Back,
    aterm_types::mouse::MouseButton::Forward,
];

/// The button a motion report names while the buttons in `reported` (a `1 << slot()` mask)
/// are held: the lowest, as xterm's `EMIT_BUTTON(FirstBitN(mouse_button))` names it.
/// Derived from the mask rather than set per press, so releasing one button of a chord
/// leaves the other named and its drag still reported under DEC 1002.
pub(crate) fn held_button(reported: u8) -> Option<aterm_types::mouse::MouseButton> {
    BUTTONS_BY_SLOT
        .into_iter()
        .find(|button| reported & (1 << button.slot()) != 0)
}

/// Forget `button`'s press in window state `ws`: its reported bit and owner go,
/// and the held button is re-derived from what is still down.
pub(crate) fn forget_reported_button(
    ws: &mut crate::WindowState,
    button: aterm_types::mouse::MouseButton,
) {
    ws.reported_buttons &= !(1 << button.slot());
    ws.button_owners[usize::from(button.slot())] = None;
    ws.held_mouse_button = held_button(ws.reported_buttons);
}

/// The session a held button's press went to, and the cell (in that session's
/// own grid) and sub-cell pixel last sent to it. Kept per button in
/// [`crate::WindowState::button_owners`] so the button's drag motion and release
/// pair with its press after a tab or pane switch moves the front.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ButtonOwner {
    pub(crate) session: u64,
    pub(crate) cell: (u16, u16),
    pub(crate) px_off: crate::input::PixelOffset,
}

/// Where one pointer event goes in a window; see
/// [`App::pointer_route`] and [`App::owner_route`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PointerRoute {
    /// The focused pane, at the pane-local cell `last_mouse_cell` holds (clamped
    /// into it, so a drag that crosses a divider keeps addressing it).
    Front,
    /// Another visible terminal pane of the window, at that pane's own cell.
    Pane { session: u64, cell: (u16, u16) },
    /// No terminal program: the pointer is over a native pane, or the session a
    /// held button's press went to is no longer on the glass.
    Nowhere,
}

/// A planned leaf's content rect as whole cells `(row_off, col_off, rows, cols)`, with the
/// rounding [`App::focused_pane_rect`] applies, so every pane's cells are derived alike.
fn leaf_cell_rect(rect: crate::tab_model::LogicalRect) -> (u16, u16, u16, u16) {
    (
        rect.origin.y.round().max(0.0) as u16,
        rect.origin.x.round().max(0.0) as u16,
        (rect.size.height.round() as u16).max(1),
        (rect.size.width.round() as u16).max(1),
    )
}

/// Whether window cell `(row, col)` lies inside the pane rect
/// `(row_off, col_off, rows, cols)`.
fn rect_contains((ro, co, rows, cols): (u16, u16, u16, u16), (row, col): (u16, u16)) -> bool {
    (ro..ro.saturating_add(rows)).contains(&row) && (co..co.saturating_add(cols)).contains(&col)
}

/// Window cell `(row, col)` in the pane rect `(row_off, col_off, rows, cols)`'s
/// own grid, clamped into it.
fn cell_in_rect((row, col): (u16, u16), (ro, co, rows, cols): (u16, u16, u16, u16)) -> (u16, u16) {
    (
        row.saturating_sub(ro).min(rows.saturating_sub(1)),
        col.saturating_sub(co).min(cols.saturating_sub(1)),
    )
}

/// Record that `cell`/`px_off` (in `session`'s own grid) was just sent to
/// `session`, on every held button whose press went to it, so a release
/// synthesized later lands where that program last saw the pointer.
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

/// An in-flight drag-to-reorder on the in-grid tab strip: which stable tab is held, and
/// the strip column the gesture is anchored at. Stored per window in
/// [`crate::WindowState::strip_drag`]; [`App::advance_strip_drag`] has the rules.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct StripDrag {
    /// The tab the press grabbed. Identity, not index: a step relayouts the band,
    /// so the index the press saw names a different tab immediately afterwards.
    pub(crate) tab: crate::tab_model::TabId,
    /// The strip column the drag is anchored at — the press column, re-anchored
    /// to the pointer after every step it takes. A motion that has not left this
    /// column takes no step, so a press that never moved cannot reorder.
    pub(crate) origin_col: u16,
}

/// Outcome of one hover resolution ([`App::resolve_hover_cursor`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoverProbe {
    /// The cursor was written; this is the cell whose link the band discloses
    /// (`None` from every arm where the grid is not under the pointer).
    Resolved(Option<crate::link_target::LinkHover>),
    /// The grid probe found the terminal mutex busy and touched nothing; the
    /// caller keeps the previous cursor and caption.
    Deferred,
}

/// The grid arm of one hover resolution, memoised so the terminal mutex is touched once per
/// question rather than per pixel (a trackpad asks the same question ~7 times per cell).
///
/// Keyed on the session, the window cell (pane-local cells are clamped, so two over a
/// sibling would share one), the link modifier, the pointer/pane/chrome gate, and the engine
/// fill ([`HoverFrame`]): a row rewritten or scrolled under a still pointer is a new fill,
/// re-probed by [`App::refresh_hover_for_frame_locked`] before the caption is spliced. The
/// disclosed destination is kept as an `Arc<str>` so the splice reads it lock-free, leaving
/// [`crate::link_target::LinkHover`] `Copy + PartialEq` for the event-boundary compare.
#[derive(Clone, Debug)]
pub(crate) struct HoverMemo {
    session: u64,
    window_cell: (u16, u16),
    /// The pane-local painted cell the pointer was on — the one a `LinkHover`
    /// names; the probe read the logical cell under it ([`pointer_cell`]).
    cell: (u16, u16),
    mod_held: bool,
    /// `pointer_is_inside_focused_pane && !chrome_owns_terminal_row` at
    /// resolution time: part of the key, because a gate that flips (a panel
    /// opening under the pointer) changes the answer without moving anything.
    under_pointer: bool,
    /// The engine fill this answer stands on — see [`HoverFrame`].
    frame: HoverFrame,
    /// The destination the caption discloses: the cell's hyperlink when it
    /// carries one, else a faint plain-text URL run's (`crate::PlainUrl`).
    disclosed: Option<std::sync::Arc<str>>,
    /// A plain-text URL run under the cell (probed only with the modifier held).
    plain_url: bool,
    /// `disclosed` names a resolved file citation (`crate::path_link`), not a
    /// URL: the band says `opens in aterm:` and a click opens aterm's editor.
    path_link: bool,
    /// The file, line and column that citation resolved to: what the caption
    /// names, and so the only target a click under it may open
    /// ([`App::open_link_under_pointer`], through [`ShownCaption`]).
    citation: Option<crate::path_link::PathTarget>,
    /// The answer is whole: no citation under the cell is still being looked at
    /// (`crate::path_link::Looks`). An unsettled memo answers nothing, so the
    /// next pointer event or frame asks again and finds the look's answer.
    settled: bool,
    /// The status sweep's naming of the group in front (`program_pgid`,
    /// `program`) a citation under the cell was answered under, `None` when
    /// none was asked. A citation's answer turns on it ([`App::resolve_citation`])
    /// while nothing on screen moves (a build exits, the shell's name lands a
    /// sweep later), so the sweep re-asks a memo whose naming it has moved
    /// ([`App::reask_renamed_hovers`]).
    named: Option<(i32, Option<String>)>,
}

/// The engine fill a window's frame stands on: the hover memo's epoch. Read from the
/// engine-filled scratch, never a host-bumped counter: `terminal_id`, `engine_fill_seq`
/// (moves on output and on a viewport scroll, which applies display-offset damage) and
/// `display_offset`. Host mutators on the presented path (strip, find panel, tab menu,
/// caption) bump only `snapshot_seq`, so a caption painted every frame cannot make the next
/// look new.
///
/// Two triples, because either scratch can be the engine-filled one: single-pane routes fill
/// `input_scratch`; the composed split route keeps an engine-exact copy of the focused pane
/// (the only one the hover can stand on) in `composed_focus_scratch`. A route switch is one
/// re-probe.
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

    /// Whether this memo answers for `(session, window cell, modifier, gate)`
    /// against the frame stamped `frame`.
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

    /// The cached destination for `hover`, if this memo is the resolution that
    /// produced it (same session, same cell) — the caption splice's lock-free
    /// read.
    pub(crate) fn url_for(
        &self,
        hover: &crate::link_target::LinkHover,
    ) -> Option<&std::sync::Arc<str>> {
        (self.session == hover.session && self.cell == hover.cell)
            .then_some(self.disclosed.as_ref())
            .flatten()
    }

    /// Whether the destination this memo discloses for `hover` is a file
    /// citation (`crate::path_link`) rather than a URL.
    pub(crate) fn path_link_for(&self, hover: &crate::link_target::LinkHover) -> bool {
        self.path_link && self.session == hover.session && self.cell == hover.cell
    }

    /// What the caption this memo draws for `hover` names ([`CaptionNames`]),
    /// or `None` when it draws none for `hover`.
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

    /// Whether this memo answered a citation for `session` under a naming of the
    /// group in front other than `named` (`program_pgid`, `program`): the
    /// status sweep's cue to re-ask it ([`App::reask_renamed_hovers`]).
    pub(crate) fn renamed(&self, session: u64, named: (i32, Option<&str>)) -> bool {
        self.answered_citation_of(session)
            && self
                .named
                .as_ref()
                .is_some_and(|(group, program)| (*group, program.as_deref()) != named)
    }

    /// Whether this memo answered a citation for `session` (it holds the naming it
    /// was answered under).
    pub(crate) fn answered_citation_of(&self, session: u64) -> bool {
        self.session == session && self.named.is_some()
    }

    /// Whether this memo answers for the pointer as the frame finds it: the front
    /// `session`, the window cell the pointer last rested on, its modifier, and the engine
    /// fill the frame just landed. The frame-time refresh's gate
    /// ([`crate::App::refresh_hover_for_frame_locked`]); field reads only, so the
    /// pointer/pane/chrome gate (a layout walk in a split) is left to the motion path. A memo
    /// naming a different cell is what a deferred non-blocking probe leaves behind.
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
    /// A URL (`opens: …`), as the band spelled it.
    Url(std::sync::Arc<str>),
    /// A file citation (`opens in aterm: …`): the canonical file, line and
    /// column.
    Citation {
        path: std::path::PathBuf,
        line: Option<u32>,
        col: Option<u32>,
    },
}

/// The link caption carried by the last accepted application present
/// (`WindowState::shown_caption`): its session, pane-local cell, and target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShownCaption {
    pub(crate) session: u64,
    pub(crate) cell: (u16, u16),
    pub(crate) names: CaptionNames,
}

/// Publish only the caption carried by an accepted application present.
/// Pending acquisition and a refused present leave the prior caption standing.
pub(crate) fn commit_presented_caption(
    shown: &mut Option<ShownCaption>,
    composed: Option<ShownCaption>,
    presented: &Result<Option<u64>, crate::metrics::PresentDropReason>,
) {
    if matches!(presented, Ok(Some(_))) {
        *shown = composed;
    }
}

/// A URL a press reads under the pointer ([`App::open_link_under_pointer`]), by
/// where it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PressedUrl<'a> {
    /// The cell's OSC 8 hyperlink: its visible text need not be its URL, so
    /// only a drawn caption discloses where it goes.
    Osc8(&'a str),
    /// A plain-text URL run, drawn in full: its own disclosure.
    Plain(&'a str),
}

impl<'a> PressedUrl<'a> {
    fn url(self) -> &'a str {
        match self {
            Self::Osc8(url) | Self::Plain(url) => url,
        }
    }
}

/// Whether a press that reads `url` (a URL under the pointer) or `citation`
/// (a resolved file citation, with no URL there) may open it, given the
/// caption drawn for the pressed cell (`shown`, `None` when none was).
/// A citation opens only under a caption that named that very file, line
/// and column: never unseen (no caption drawn yet, a look that came back
/// after the frame), never under a URL's caption, never once the target
/// moved under a still pointer. A URL under a caption opens only when it is
/// the URL the caption named. With no caption drawn, only a plain-text URL
/// opens (drawn in full, it is its own disclosure); an OSC 8 link, whose
/// text need not be its URL, does not (a window too narrow for the band, no
/// free row for it). Nothing to open admits the press, which then opens
/// nothing.
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

/// The grid arm's two probes for one hovered cell, against a terminal the caller already
/// holds (the frame's guard or the motion path's successful `try_lock`): the cell's
/// hyperlink, kept as the grid's `Arc<str>` so the caption splice reads it lock-free, and,
/// with the modifier held, the press's own plain-text reading ([`crate::plain_url_at`] over
/// the viewport row `render_row` resolves, at the renderer's `min_contrast`), so the hand
/// appears exactly where a click opens something. That reading needs the row's inks, not
/// just its characters: a concealed cell keeps its character, and a URL run ends where the
/// ink does. Both are viewport-keyed: the pointer stands on the row the frame drew, and on
/// its painted cell `col`, read at the logical cell under it ([`pointer_cell`]) as a press
/// selects it. A plain URL is its own visible text, so it earns a caption only when part of
/// it is painted faintly. With neither, the same row is read for a plain-text file citation
/// ([`crate::plain_path_at`]), returned with the shell's OSC 7 directory for the caller
/// to resolve once the terminal is no longer needed. Returns `(disclosed, plain_url,
/// citation)`.
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

/// A file citation read off the grid, with what the engine says of the
/// session's directory read under the same hold: resolved against the
/// filesystem only after the terminal is let go ([`App::resolve_citation`]).
#[derive(Clone, Debug)]
pub(crate) struct CitationProbe {
    span: crate::path_link::PathSpan,
    /// The shell's OSC 7 directory (`Terminal::current_working_directory`).
    osc7: Option<std::path::PathBuf>,
    /// The host that report named (`Terminal::reported_cwd_host`).
    osc7_host: Option<String>,
    /// Which report it is (`Terminal::reported_cwd_seq`): whether it still
    /// speaks for the group in front ([`crate::path_link::report_stands`]).
    osc7_seq: Option<u64>,
    /// The command the shell says it is running (`status detail=`), `None`
    /// at its prompt or with no shell integration.
    running: Option<String>,
    /// The shell's integration says a command runs (`quit_safety::shell_executing`:
    /// a block it opened is executing), whether or not it names the command.
    executing: bool,
    /// Where no foreground group can be read (a ConPTY): whether the shell's
    /// own integration says it runs nothing but itself or an agent here
    /// ([`shell_reads_local`]).
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

/// Where no foreground group can be read (a ConPTY has no `tcgetpgrp`), the
/// shell's own integration is the only reading of what runs: an integrated
/// shell that reports its commands (`integrated`: it opened a block, and a
/// command it began has ended, `quit_safety::shell_reports_commands`) at its
/// prompt, or running an agent aterm names by its command's word, or a bare
/// shell (`running`, `status detail=`). An integration that marks only its
/// prompts (pwsh without PSReadLine, cmd.exe's prompt tier) never says a
/// command runs, so its prompt is no reading; nor is a shell given a command
/// (`bash -c 'ssh h'`, `pwsh -c ssh h`), or any other command (fail closed).
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

/// Whether the "open link" modifier is held: Cmd (Super) on macOS — its native
/// convention — and Ctrl on every other platform, because Linux/X11 desktops grab
/// the Super (Windows) key, so a Super-click would never reach aterm. Mirrors the
/// keybinding accelerator choice (Ctrl/Ctrl+Shift on Linux).
pub(crate) fn link_modifier_held(mods: aterm_window::keyboard::ModifiersState) -> bool {
    #[cfg(target_os = "macos")]
    {
        mods.super_key()
    }
    #[cfg(not(target_os = "macos"))]
    {
        mods.control_key()
    }
}

/// Audit M6: whether a left press starts a LOCAL text selection instead of being
/// reported to a tracking app — always when nothing tracks the mouse, and with
/// the bypass modifier held ([`tracking_bypass_held`]) even when something does,
/// so a PID can be copied out of htop. The matching release settles the selection
/// without leaking a release report (`selecting` wins in `on_mouse_input`), keeping
/// press/release symmetric for the app.
pub(crate) fn press_starts_selection(tracking: bool, bypass_held: bool) -> bool {
    !tracking || bypass_held
}

/// Whether the held modifiers take a left press back from a mouse-tracking app
/// ([`press_starts_selection`]). Option/Alt everywhere — iTerm2's gesture, and
/// the wheel's main-screen override (`input::wheel_route`). Off macOS, Shift too:
/// the xterm convention every Linux terminal (VTE, Konsole, kitty, Alacritty,
/// foot, WezTerm) and Windows Terminal follow, the one Shift+wheel already
/// follows here, and the only one left where the window manager takes Alt+drag
/// (Xfce, Cinnamon, MATE, KDE). macOS keeps Shift-click for the program, as
/// Terminal.app and iTerm2 do.
pub(crate) fn tracking_bypass_held(mods: aterm_window::keyboard::ModifiersState) -> bool {
    mods.alt_key() || (cfg!(not(target_os = "macos")) && mods.shift_key())
}

/// What a right press does at its pre-dispatch point in `on_mouse_input`. Pure, so the
/// decision table is unit-pinned; the side effects stay in the handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RightPressPlan {
    /// The press landed on chrome (the tab strip band): swallow it entirely (no copy, no
    /// paste, no seam report), or a tracking app would get a press at whatever grid cell the
    /// clamp put under the strip.
    Chrome,
    /// Tracking off, a selection exists: copy it to the system clipboard and clear it
    /// (conhost's QuickEdit half of the convention).
    Copy,
    /// Tracking off, no selection: paste the clipboard via `App::paste_clipboard_into`,
    /// never a raw `self.input(…Paste…)`: the paste must pass `deliver_paste`'s pastejacking
    /// guard (as the Linux middle-click arm does via `App::paste_primary_into`).
    Paste,
    /// Fall through to the seam untouched: tracking is on (the app owns the button), or the
    /// gesture is configured `off`.
    Seam,
}

/// Resolve a right press against the conhost/Windows Terminal convention
/// (`right_click = "copy_paste"`): copy-if-selection-else-paste, tracking off only, never on
/// chrome. The order is the contract: chrome first (a right press on the strip is chrome
/// whatever the gesture setting), then a tracking app keeps the button (the Option bypass
/// covers selecting over a tracking app, for the left button), then the copy/paste split.
pub(crate) fn right_press_plan(
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

/// Selection kind for a single left press. Option+drag is the rectangular
/// [`SelectionType::Block`] only when the press was local anyway (tracking off); while
/// Option buys the tracking bypass it selects normally (`Simple`), matching iTerm2.
pub(crate) fn press_selection_kind(option_held: bool, tracking: bool) -> SelectionType {
    if option_held && !tracking {
        SelectionType::Block
    } else {
        SelectionType::Simple
    }
}

// Divider-drag relayout throttle, the pane-split analogue of `on_resize_throttled`: each
// applied ratio step resizes both child panes' engines, and a width change rewraps each
// pane's whole scrollback on the event-loop thread. Ratio and relayout apply together, at
// most once per [`crate::RESIZE_THROTTLE`], so layout rects and engine/PTY sizes never
// diverge and no coalesced intermediate size reaches `term.resize`; `finish_divider_drag`
// flushes a coalesced step so the release lands the final position. Pointer events run on
// the event-loop thread, so thread-locals suffice; entries carry the window id.
thread_local! {
    /// When a divider drag last applied a relayout (leading-edge stamp).
    static PANE_DRAG_APPLIED_AT: Cell<Option<(WindowId, Instant)>> = const { Cell::new(None) };
    /// Window with a coalesced (skipped) drag step awaiting the release flush.
    static PANE_DRAG_PENDING: Cell<Option<WindowId>> = const { Cell::new(None) };
    /// The window cell the held divider follows — the pointer's cell passed
    /// through [`divider_drag_cell`]'s boundary hysteresis, seeded with the press
    /// cell. Every applied step (the leading edge and the release flush alike)
    /// maps this cell to the split ratio, never the raw pointer cell.
    static PANE_DRAG_CELL: Cell<Option<(WindowId, (u16, u16))>> = const { Cell::new(None) };
}

/// Forget window `wid`'s divider-drag throttle and hysteresis state, for a path that drops
/// the gesture without a release (focus loss), so the thread-locals never name a drag that
/// no longer exists (`begin_divider_drag` reseeds them on the next press anyway).
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

/// How far, as a fraction `1/N` of the cell along the axis, the pointer must travel past a
/// cell boundary before a held divider follows it into the next cell (and back).
///
/// The pointer is quantized to cells, so a hand resting on a boundary (a grab-and-let-go, a
/// wobble near an edge) crosses it and comes back. Without a dead band each crossing
/// re-grids both sibling panes, N → N±1 → N in one gesture, inside the app's SIGWINCH
/// window: a handler reading the size after both halves sees its old size and never
/// repaints, while an alt-screen shrink has demoted a row it then shows stale. The band is
/// spatial: a deliberate move crosses it and applies on the same event.
const DIVIDER_DRAG_HYSTERESIS_DIV: usize = 4;

/// One axis of the held divider's cell: `held` stays until the pointer (in
/// cell `raw`, `off` device px into it, cells `cell` px long) is more than
/// `cell / DIVIDER_DRAG_HYSTERESIS_DIV` px past `held`'s edge. Pure, so the
/// dead band is testable without a window.
fn divider_drag_axis(held: u16, raw: u16, off: u16, cell: usize) -> u16 {
    if raw == held || cell == 0 {
        return raw;
    }
    let off = usize::from(off).min(cell - 1);
    // Device px of the pointer beyond `held`'s nearer edge, counting the pixel
    // it sits on: 1 for the first pixel of the neighbouring cell either way.
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

/// The window cell a held divider follows after a pointer move to `raw`
/// (sub-cell offset `off`, cell size `cell` = `(w, h)` px), given the cell it
/// followed so far — [`divider_drag_axis`] per axis.
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

/// One axis of the sub-cell offset the divider's dead band reads, made to describe the
/// clamped cell `raw`: `g` is the pointer's unclamped grid pixel along the axis and `cell`
/// the cell length. `raw` comes from the clamping `pixel_to_term_cell` while `g % cell` does
/// not, so past the far edge the offset is the clamped cell's last pixel and before the
/// near edge its first.
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

/// The window geometry every pointer ↔ cell mapping needs, derived once per event. `cell`
/// goes through `Backend::cell_geometry`, which for a variable primary face re-parses the
/// font and re-applies its axes on each call; nothing changes it inside one handler, so the
/// motion path threads this `Copy` snapshot through the `*_with` seams, and the
/// zero-argument wrappers derive it once each.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PointerGeometry {
    /// `(cell_w, cell_h)` in device px.
    cell: (usize, usize),
    pad: usize,
    pad_top: usize,
    head: usize,
}

/// WHERE THE POINTER CAME TO REST over a window's native views, and what it saw
/// there: the control under it ([`crate::native_ui::HitRegion`]) in the frame
/// painted when it moved to `px`, or nothing. A motion event at the same pixel is
/// no motion — macOS queues one ahead of every press, release and wheel event —
/// so it keeps what the pointer saw when it last really moved, whatever the
/// layout did since; a scroll the person asked for forgets it, since they
/// watched the page move ([`App::dispatch_native_event`]). Where a control that
/// asks for a look ([`crate::native_ui::Control::look_first`]) is either end —
/// the one pressed, or the one the pointer came to rest on — the press lands
/// only where the pointer saw that very control, as the Autopilot menu's press
/// does (`AutopilotMenu::press_lands`): a bulk button never takes a press meant
/// for a row's, nor a row's button a press meant for a bulk one. Field reads and
/// one key clone per real move.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NativePointerRest {
    px: (f64, f64),
    seen: Option<(crate::tab_model::ViewId, crate::native_ui::UiKey)>,
    /// Whether what it saw asks for a look.
    asks: bool,
}

impl NativePointerRest {
    /// The rest after a motion to `px` that sees `seen` (and whether that asks
    /// for a look): a new one where the pointer moved (or rested nowhere yet),
    /// else `rest` as it was.
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

    /// Whether a press at `px` on `view`'s control `key` (which `asks` for a
    /// look or not) goes where the pointer looked: no rest known (a scroll
    /// forgot it), the pointer moved since (another pixel), neither the
    /// control nor what the pointer saw asks for a look, or it saw this very
    /// control when it came to rest.
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

    /// The rest on `view`'s control `key`, at `px`, after a press it refused:
    /// the next press there takes it.
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

/// Every input of the pixel → pane-local cell mapping other than the pixel, plus the
/// session the cell is reported to. The same-pixel early return in
/// [`App::route_cursor_moved`] may stand on the last routing only while this is unchanged:
/// a keyboard focus move or tab switch, a split/close/zoom, a font zoom, a strip splice or a
/// window resize changes it without a pointer event. Stamped at the grid arm from the values
/// the mapping just used, and re-derived (lock-free, plan included) by the predicate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HoverMapping {
    /// The front content the seam would report to (a tab switch or a focus
    /// move between splits changes this and nothing the pointer can see).
    front: Option<crate::front_content::FrontContent>,
    /// The window's physical size: the frame origin's leading bands come from
    /// it ([`App::frame_origin_with`]).
    win_px: Option<(u32, u32)>,
    /// The window grid `(rows, cols)` the window cell is clamped into.
    dims: (u16, u16),
    /// The chrome rows (strip, presence row, message band) above the grid.
    chrome_rows: u16,
    /// Cell size and the three insets.
    geom: PointerGeometry,
    /// The focused pane's `(row_off, col_off, rows, cols)` the pane-local cell
    /// is translated into and clamped by.
    pane_rect: (u16, u16, u16, u16),
}

/// THE GRID A FRAME'S GLASS WAS LAID ON: every input of where a frame cell
/// lands among the window's pixels (the cell size, the insets, the frame's
/// origin in the window) and of where each pane's rows and columns land among
/// the frame's cells (the window grid, the chrome rows above it, the visible
/// tab's split) — read lock-free, with no plan built ([`App::glass_grid`]).
/// Whatever a frame lays in frame cells for the pointer to find — the
/// harness's glass (`crate::claude_lights`), the tab menu's card
/// (`App::splice_tab_menu`), the Autopilot and fact menus' card
/// (`crate::autopilot_menu`) — is stamped with this as it is laid, and
/// answers the pointer only through [`App::laid_cell_at`]: a pointer event
/// that comes between a re-grid (a window resize, a font zoom, a split, a
/// strip or band that grew) and the next frame would otherwise be hit-tested
/// against the last frame's targets at a geometry the window no longer has,
/// and hover or press what is no longer there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GlassGrid {
    /// Cell size and the three insets.
    geom: PointerGeometry,
    /// The frame's top-left in window pixels ([`App::frame_origin_with`]).
    origin: (i64, i64),
    /// The window grid `(rows, cols)`.
    dims: (u16, u16),
    /// The chrome rows (strip, presence row, message band) above the grid.
    chrome_rows: u16,
    /// The visible tab's split, folded ([`split_word`]).
    split: u64,
}

impl GlassGrid {
    /// The window's cell in device pixels, `(w, h)`.
    pub(crate) const fn cell(&self) -> (usize, usize) {
        self.geom.cell
    }

    /// Window pixel `(x, y)` as a frame cell `(row, col)` under this grid:
    /// [`App::frame_cell_at`]'s mapping ([`frame_cell_of`]).
    pub(crate) fn cell_at(&self, x: f64, y: f64) -> Option<(usize, usize)> {
        frame_cell_of(self.geom, self.origin, self.dims.1, x, y)
    }
}

/// Window pixel `(x, y)` as a frame cell `(row, col)` of a frame whose cells
/// are `geom`'s, whose top-left is window pixel `origin` and whose grid is
/// `cols` wide: row 0 the strip's first row, `None` off the grid's interior
/// (the insets, past the right edge). The one mapping [`App::frame_cell_at`]
/// and [`GlassGrid::cell_at`] share.
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

/// Tab `tab`'s split folded into one word: its identity, its zoom (and the
/// leaf a zoom shows), every split's axis and ratio and every leaf's view —
/// what the visible plan's pane rects follow from besides the window grid
/// ([`GlassGrid`]). A walk of the tree's few nodes: no allocation.
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

/// Pane-local result of one pointer-to-cell mapping. The pane rectangle is
/// carried with the cell so selection autoscroll can reuse the same split plan.
#[derive(Clone, Copy)]
struct MouseCellMapping {
    pane_cell: (u16, u16),
    pane_rect: (u16, u16, u16, u16),
}

/// The `pointer` verb's answer: where the front window's pointer resolved to, read
/// back after the real path ran ([`App::pointer_reply`]), or why not.
pub(crate) type PointerReply = Result<PointerAt, String>;

/// Where the front window's pointer IS, as the `pointer` verb says it (cs-25): no
/// position; a window cell (a terminal front, a negative row a chrome row) with its
/// window pixel; or over a native view, the view, the point in its own logical
/// coordinates, the control under it and the window pixel. A native front never
/// answers a grid cell its pointer did not resolve to (it said `at=0,0`).
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

/// What a `pointer move` on a native view aims at: a control by its UI key
/// (its rect's centre in the presented tree), or a point in the view's own
/// logical coordinates.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NativePointerTarget {
    Key(String),
    Point(f32, f32),
}

/// The unit of a `pointer wheel`'s deltas, and so the device it stands in for:
/// a wheel's notches (`MouseScrollDelta::LineDelta`, a fraction banked toward
/// the next whole line as a free-spinning wheel's is), or a trackpad's logical
/// points (`MouseScrollDelta::PixelDelta` at the window's scale, then the
/// zero-delta `Ended` its finger's lift sends).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WheelUnit {
    Lines,
    Points,
}

/// What the `pointer` verb asks of the front window's pointer. Each variant is the
/// winit event it stands in for, so [`App::pointer_cmd`] has one call to make per
/// arm and no pointer behaviour of its own to keep in step.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PointerAction {
    /// `WindowEvent::CursorMoved` onto the centre of terminal cell `(row, col)`.
    Move { row: u16, col: u16 },
    /// `WindowEvent::CursorMoved` onto a native view's control or point, in the
    /// presented tree ([`NativePointerTarget`]).
    MoveNative(NativePointerTarget),
    /// `WindowEvent::CursorMoved` onto the centre of column `col` of the chrome row `up`
    /// rows above the grid's first row (`1` the row right above it, the message band's last
    /// row when one is up): the rows `move -<up> <col>` names.
    MoveChrome { up: u16, col: u16 },
    /// `WindowEvent::MouseInput` — a left press, then its release — where
    /// the pointer is: a click, through the functions winit's own event
    /// calls.
    Click,
    /// `WindowEvent::MouseWheel` where the pointer is, or where `aim` puts it
    /// first (a `CursorMoved` onto a native view's control or point): `dy`
    /// down and `dx` right, in a wheel's lines or a trackpad's points
    /// ([`WheelUnit`]).
    Wheel {
        dx: f64,
        dy: f64,
        unit: WheelUnit,
        aim: Option<NativePointerTarget>,
    },
    /// `WindowEvent::CursorLeft`: the pointer withdraws from the window.
    Leave,
    /// Report only — moves nothing.
    Status,
    /// `WindowEvent::ModifiersChanged`: the modifier keys the window holds from now on,
    /// so a scripted hover or click sees the hand a held Cmd (Ctrl elsewhere) offers.
    Mods(aterm_window::keyboard::ModifiersState),
}

impl App {
    // ---- The overlay CARD plumbing (palette, session picker, Overview): one
    // body each, parameterised only by what names the card.

    /// Put `overlay` in `wid`'s one overlay slot (structural mutual exclusion)
    /// and arm the pointer as every overlay card opens: the wheel residual is
    /// dropped, `raise` brings the window forward, drags settle, the card
    /// `claim`s the pointer and resolves the stationary cursor through
    /// `motion`. `false` (nothing opens) when `wid` is gone.
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

    /// Close `wid`'s overlay card when `is_open` says the slot holds it (a
    /// no-op otherwise), publish the now-empty accessibility tree, and
    /// re-resolve the cursor against the revealed surface.
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

    /// Re-resolve the cursor where the pointer last moved, after the surface
    /// under it changed (an overlay card or the palette closed), ONLY while the
    /// window still holds the pointer ([`crate::WindowState`]'s
    /// `pointer_position_known`). A pointer that left the window, or never
    /// entered it (a keyboard-only session, a headless window), has no position
    /// to replay: the memo then holds where it left, or `(0, 0)`, and the motion
    /// path would claim the pointer again and light whatever sits there. Closing
    /// the palette by keyboard lit the strip's first tab, its hover wash and its
    /// `✕`, with no pointer in the window (the 2026-10-18 judges'
    /// `strip-phantom-hover-after-overlay`).
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

    /// Modal pointer boundary of an overlay card. When `is_open` says the card is up on
    /// `wid`, it clears any hover/press retained by the native app underneath, aligns the
    /// cursor through `sync`, and reports that the caller must swallow the gesture. This
    /// runs before native hit testing for motion, buttons, and wheel input, preventing a
    /// visible card from becoming an invisible click-through layer over an editor or
    /// Settings control.
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
            // Hover/pressed are stamp-neutral visual state: damage the one leaf
            // so its wash repaints (the same law as the `on_cursor_moved` hover
            // path), but keep the retained scene — closing the modal re-resolves
            // the pointer against that hit tree immediately, before any repaint.
            self.invalidate_native_view_cache(wid, view, crate::native_app::DamageRegion::All);
            self.request_redraw_all_windows();
        }
        sync(self, wid);
        true
    }

    /// A window-space point in the open card's local geometry, with the transform whose
    /// `geom` the card's own hit test takes. Terminal frames composite the card at
    /// `(pad, pad_top + head)`; native tabs composite it inside the scaled app texture
    /// below host tab chrome. These are the exact two placement laws in `app_render`,
    /// reversed here before calling the shared painter/hit rectangle.
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

    /// Keep the OS cursor aligned with an overlay card's hover: the pointing hand over
    /// one of its targets, the arrow elsewhere. The existing `hover_pointer` bit is safe
    /// to reuse while the modal is open because the underlying native/terminal hover
    /// authority has already been cleared at the boundary above.
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

    /// The filtered palette row under a window-space point.
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

    /// Hover-select the row under a pointer motion; outside points remain swallowed while
    /// clearing the pointer cursor. Selection changes repaint the same ring the hit rect
    /// came from.
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
            self.palette_activate();
        }
    }

    /// Scroll the filtered palette band, then resolve the stationary pointer against the
    /// new painted rows. This keeps hover/selection and hit geometry in lockstep and
    /// cancels an armed click before any row can move beneath its release.
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

    /// `true` when window `wid`'s active tab is one focused pane, so
    /// [`Self::active_visible_leaf_plan`]'s answer is known without allocating:
    /// `Tab::visible_plan`'s zoom branch needs `root.len() > 1`, so a one-leaf tree takes
    /// `plan_into`, which gives the sole leaf the sanitized bounds verbatim, and
    /// `contains(focus)` makes it `plan.focused`.
    fn active_tab_is_single_focused_pane(&self, wid: WindowId) -> bool {
        self.windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active())
            .is_some_and(|tab| tab.root.len() == 1 && tab.root.contains(tab.focus))
    }

    /// `plan.leaves.len() > 1` for window `wid`'s active tab, without building the
    /// plan: `Tab::visible_plan` plans every leaf except when zoom collapses a
    /// multi-leaf tab to its focused one, so the two forms agree on every input.
    fn active_tab_has_multiple_visible_panes(&self, wid: WindowId) -> bool {
        self.windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active())
            .is_some_and(|tab| tab.root.len() > 1 && !tab.zoomed)
    }

    /// The focused pane's rect `(row_off, col_off, rows, cols)` in window `wid`'s grid; the
    /// whole window on the single-pane fast path. Used to translate and clamp a window mouse
    /// cell into the focused pane's own grid, so a drag across a divider still addresses a
    /// cell inside the focused pane (whose engine is sized to this sub-rect).
    pub(crate) fn focused_pane_rect(&self, wid: WindowId) -> (u16, u16, u16, u16) {
        let Some(ws) = self.windows.get(&wid) else {
            return (0, 0, 0, 0);
        };
        if self.active_tab_is_single_focused_pane(wid) {
            // Exactly the plan's answer for a lone pane: the sanitized window bounds, built
            // from `rows.max(1)`/`cols.max(1)`, so a bare `(ws.rows, ws.cols)` would disagree
            // at zero. The no-active-tab default below stays unmaxed.
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

    /// The content rect `(row_off, col_off, rows, cols)`, in window cells, of the
    /// visible pane of window `wid`'s active tab that shows terminal `session`;
    /// `None` when no visible pane does (a background tab, a sibling hidden by
    /// zoom, a closed pane, a session another window shows).
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

    /// Where a pointer event other than a release goes in window `wid` (a hover or drag
    /// step, a wheel notch, a non-left press); a release goes to its press's owner
    /// ([`Self::owner_route`]).
    ///
    /// With nothing in flight: to the pane under the pointer, at its own cell, as kitty
    /// (`window_for_event`), WezTerm and tmux (`send -M -t=`) route it, never to the focused
    /// pane at the clamped `last_mouse_cell`. With a button held: to whoever has the lowest
    /// held button's press, wherever the pointer is, so the gesture's drag steps, chorded
    /// buttons and wheel notches reach the program that has the first press — the session a
    /// reported press went to, or the front native page for a press its boundary took
    /// (`page_buttons`).
    ///
    /// [`PointerRoute::Front`] while the front path is the answer: a modal or the tab menu,
    /// the front's own selection or divider drag, the front page's own press, a single
    /// visible pane, the pointer inside the focused pane or on a divider.
    /// [`PointerRoute::Nowhere`] over a native pane. `focused` is the focused pane rect when
    /// the caller already planned it.
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
        // A press the page took is the page's gesture only while that page is in front: the
        // mark is the window's, and a tab switch mid-press must not hold another tab's pointer.
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

    /// [`Self::pointer_route`] by the pane under the pointer alone, whatever a held button's
    /// press went to: where a NEW press lands. [`PointerRoute::Front`] over the focused pane
    /// (or with one pane), [`PointerRoute::Pane`] over another terminal pane at its own cell,
    /// [`PointerRoute::Nowhere`] over another native pane.
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

    /// Where an event of a gesture already in flight goes: to the session the
    /// button's press went to (`owner`), at the pointer's cell in that session's
    /// pane, clamped as the focused pane's is so a drag that leaves the pane keeps
    /// addressing its edge. kitty sends a tracked drag's motion and release to the
    /// window the press went to (`tracked_drag_in_window`). [`PointerRoute::Front`]
    /// when the owner is the front pane or none was recorded;
    /// [`PointerRoute::Nowhere`] when it is no longer on the glass, whose release
    /// [`Self::settle_hidden_button_owners`] already wrote.
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

    /// Hand a pointer event to its route: the front path, the visible pane it
    /// names (pinned through [`Self::input_to_session`], which presents a pointer
    /// event in any visible pane of `wid`), or nowhere.
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

    /// Release every held button whose press went to a session the window no longer shows,
    /// then forget it. Called from `sync_window`, where every tab switch, pane close, zoom
    /// and migration converges.
    ///
    /// The release is the person's eventual button-up, written now to the program that got
    /// the press, at the cell last sent to it, under that session's own lock
    /// (`write_ui_report`, as a tab switch's focus report goes). Otherwise that program stays
    /// mid-drag (vim visual drag, tmux pane select) and the button-up lands on a new front
    /// that never saw the press; kitty ends a tracked drag the same way. No modifiers. A
    /// button whose owner is still visible keeps it ([`Self::owner_route`]).
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

    /// Re-derive window `wid`'s pane-local pointer cell (`last_mouse_cell`) from
    /// its window cell and the focused pane's rect, clamped as
    /// `refresh_mouse_cell` clamps it. For a front change with the pointer still,
    /// after which the stored cell names a place in the pane focus left.
    pub(crate) fn rederive_pointer_cell(&mut self, wid: WindowId) {
        let rect = self.focused_pane_rect(wid);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_mouse_cell = cell_in_rect(ws.last_mouse_window_cell, rect);
        }
    }

    /// `true` when the pointer's last window cell lies inside the focused pane's rect, i.e.
    /// `last_mouse_cell` translates where the pointer is rather than clamping where it is
    /// not. Gesture routing wants the clamp (a drag across a divider keeps addressing its
    /// pane); anything claiming where the pointer is asks this first. `true` with one pane.
    pub(crate) fn pointer_is_inside_focused_pane(&self, wid: WindowId) -> bool {
        let Some((wr, wc)) = self.windows.get(&wid).map(|ws| ws.last_mouse_window_cell) else {
            return false;
        };
        rect_contains(self.focused_pane_rect(wid), (wr, wc))
    }

    /// Header hit testing uses the unclamped pixel position. Padding and the
    /// parent tab strip must not turn into a press on a first-row pane header.
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

    /// Click-to-focus in window `wid`: if its last pointer position (window cell) lands on a
    /// pane other than the focused one, move focus there (re-mirroring the control socket
    /// and renderer through `resync_active_or_window`, which also re-publishes the global
    /// `ActiveHandle` for a frontmost window) and re-derive the pane-local mouse cell.
    /// Returns whether focus moved (the caller then swallows the press); a press in the
    /// focused pane, on a divider or in a single-pane tab returns `false`.
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
        // Re-derive the pane-local mouse cell for the newly focused pane so a follow-up
        // gesture uses its grid; re-mirror term/master/socket onto it and, for the frontmost
        // window, the global handle.
        let (ro, co) = hit_origin;
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_mouse_cell = (wr.saturating_sub(ro), wc.saturating_sub(co));
        }
        self.resync_active_or_window(wid);
        true
    }

    /// If window `wid`'s last pointer position lands on a pane divider, arm a divider-resize
    /// drag on it and return `true` (the caller swallows the press: no focus change, no
    /// selection); `false` inside a pane or in a single-pane or zoomed tab. The armed hit is
    /// held in `ws.divider_drag` until release; `drag_divider` consumes it on each move.
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
        // Resize-cursor affordance for the drag: E-W for a vertical divider (columns
        // move), N-S for a horizontal one (rows move).
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
        // A fresh drag starts with a clean throttle: its first move must apply
        // immediately (leading edge), and no stale pending from a drag that ended
        // without a release (focus loss drops the gesture) may flush into it.
        PANE_DRAG_APPLIED_AT.with(|c| c.set(None));
        PANE_DRAG_PENDING.with(|c| c.set(None));
        // The divider starts out following the press cell, so a pointer that
        // never leaves its dead band never moves the split.
        PANE_DRAG_CELL.with(|c| c.set(Some((wid, (wr, wc)))));
        true
    }

    /// Mid-drag of a pane divider: map the current pointer (window cell) to the held
    /// divider's split ratio and apply it, then relay out (resize every pane's
    /// engine/PTY) and repaint. A no-op when no divider is being dragged. The ratio
    /// is clamped inside [`pane::PaneTree::set_divider_ratio`], so a drag past either
    /// edge just pins the boundary at the `[MIN_RATIO, MAX_RATIO]` floor/ceiling.
    /// Applies at most once per [`crate::RESIZE_THROTTLE`] window (leading edge);
    /// steps inside the window are coalesced into a pending flag the next
    /// out-of-window move or [`Self::finish_divider_drag`] flushes.
    pub(crate) fn drag_divider(&mut self, wid: WindowId) {
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        let Some(hit) = ws.divider_drag.clone() else {
            return;
        };
        // Boundary hysteresis first: a move inside the held cell's dead band is no step
        // (nothing applied, nothing pending), so a wobble across a cell edge and back never
        // re-grids the siblings.
        let raw = ws.last_mouse_window_cell;
        let off = ws.last_mouse_px_off;
        let held = PANE_DRAG_CELL.with(|c| match c.get() {
            Some((w, cell)) if w == wid => cell,
            _ => raw,
        });
        let cell = divider_drag_cell(held, raw, off, self.pointer_geometry(wid).cell);
        // …unless a throttle-coalesced step is pending: the band holds the divider where it
        // is going (`PANE_DRAG_CELL`), so a move inside the band after the window closes is
        // the out-of-window move that must flush it.
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

    /// One divider-drag step: map the current pointer to the held divider's ratio,
    /// apply it, relay out (resize every pane's engine/PTY), and repaint. Returns
    /// whether the ratio was written (a stale hit whose path no longer names a
    /// split leaves the tree untouched). Ratio + relayout land together, so layout
    /// rects and engine/PTY sizes never diverge mid-drag.
    fn apply_divider_drag(&mut self, wid: WindowId, hit: &crate::DividerDrag) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        // The cell the divider follows (hysteresis-filtered in `drag_divider`),
        // never the raw pointer cell: the release flush must land where the
        // divider was going, not re-cross a boundary the dead band absorbed.
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
        // Resize every pane's engine/PTY to its new sub-rect, then repaint the frame.
        self.resize_panes(wid);
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
        true
    }

    /// End any in-flight divider drag (left release). Returns whether a drag was
    /// active (the caller then swallows the release rather than completing a
    /// selection that never started). Flushes a throttle-coalesced final step
    /// first, so the engines/PTY always land on the released pointer position (a
    /// coalesced intermediate size must never be the terminal one).
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
            // Restore the default cursor (the hover state machine re-asserts the
            // link pointer on the next move if warranted).
            ws.hover_pointer = false;
            ws.native_text_cursor = false;
            if let Some(w) = &ws.os_window {
                w.set_cursor(CursorIcon::Default);
            }
        }
        true
    }

    /// Settle an in-flight text-selection drag only, for the paths about to drop
    /// `selecting`/`gesture` (window blur, a tab or pane switch): completing it leaves a
    /// selection the user can extend and copy rather than an `InProgress` one that is painted
    /// but `extend_selection` refuses.
    ///
    /// Narrower than [`Self::settle_pointer_drags`] in two load-bearing ways:
    ///
    /// - Copy-on-select and PRIMARY are suppressed: losing the window is not the user lifting
    ///   the button.
    /// - `held_mouse_button` and `reported_buttons` are left alone: after a focus flicker
    ///   with the button held, the release returns to this window and must still pair and
    ///   report to a tracking app. The divider drag is left to the caller too.
    pub(crate) fn settle_selection_gesture(&mut self, wid: WindowId) {
        if self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
            self.finish_selection(wid, true);
        }
    }

    /// Settle any in-flight pointer drag on `wid` (divider resize, text selection) as their
    /// left-release paths in [`Self::on_mouse_input`] would. Called when a modal opens (it
    /// steals the release) and on a release a modal gate swallows; otherwise
    /// `divider_drag`/`selecting` stay wedged and every motion keeps resizing or selecting
    /// under the modal.
    pub(crate) fn settle_pointer_drags(&mut self, wid: WindowId) {
        self.scrollbar_release(wid);
        if self.finish_divider_drag(wid)
            && let Some(ws) = self.windows.get_mut(&wid)
        {
            ws.held_mouse_button = held_button(ws.reported_buttons);
        }
        if self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
            // A human drag whose release a modal swallowed (never a scoped-edge gesture), so
            // copy-on-select is not suppressed.
            self.finish_selection(wid, false);
            if let Some(ws) = self.windows.get_mut(&wid) {
                // Clear the left reported bit like the release path does, so a later
                // non-reporting press can't leak an orphan release.
                forget_reported_button(ws, aterm_types::mouse::MouseButton::Left);
            }
        }
    }

    /// Cmd-C: copy the selected text to the system clipboard (`pbcopy`), leaving the
    /// selection so repeated copies work. Returns whether anything was copied.
    ///
    /// Window-scoped: every path with an originating window uses this, not
    /// [`Self::copy_selection`], since its `wid` can differ from `frontmost_window`: the ⌘-C
    /// arm, `dispatch_action`'s `Action::Copy` arm (`ctrl+shift+c` / `ctrl+insert` off
    /// macOS), and the native view's `copy_native_selection(wid)`. Only `MenuAction::Copy`
    /// keeps the frontmost form: a menu item carries no window.
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

    /// The text of window `wid`'s live selection, resolved across panes: the focused pane
    /// when it holds one, else the first visible pane in layout order that does. A selection
    /// in an unfocused pane is painted (`push_pane_selection`), so ⌘-C must copy it. The
    /// fallback runs only when the focused pane holds no selection at all, not when its
    /// selection resolves to empty text.
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
            // Same rule for the siblings: the first pane that holds a selection is
            // the answer, so the scan cannot skip past a whitespace-only highlight
            // to a further pane's text.
            let term = term_lock(&session.term);
            if term.text_selection().has_selection() {
                return term.selection_to_string();
            }
        }
        None
    }

    /// [`Self::copy_selection_in`] against the frontmost window — the menu and
    /// command-registry entry point, which carries no window of its own.
    pub(crate) fn copy_selection(&self) -> bool {
        self.frontmost_window
            .is_some_and(|wid| self.copy_selection_in(wid))
    }

    /// Map a pixel position to a 0-based `(row, col)` terminal grid cell of window `wid`,
    /// clamped to the grid. The insets are stripped first ([`pixel_to_term_cell`]): the
    /// `pad_top + head` band (y only), the `pad` border, then the chrome rows (strip,
    /// presence row, message band), so a click in the strip or border clamps to terminal
    /// row 0; callers intercept strip clicks with [`Self::strip_col_at`] first.
    pub(crate) fn pixel_to_cell(&self, wid: WindowId, x: f64, y: f64) -> (u16, u16) {
        // One geometry derivation, shared with the nested `window_to_frame`.
        self.pixel_to_cell_with(wid, self.pointer_geometry(wid), x, y)
    }

    /// Window `wid`'s [`PointerGeometry`]. The one place the four reads happen.
    fn pointer_geometry(&self, wid: WindowId) -> PointerGeometry {
        PointerGeometry {
            cell: self.win_cell_size(wid),
            pad: self.win_pad(wid),
            pad_top: self.win_pad_top(wid),
            head: self.win_head(wid),
        }
    }

    /// [`Self::pixel_to_cell`] against geometry the caller already derived.
    ///
    /// The cell is the GRID's: where the glass shows a row's cells somewhere
    /// else than the grid holds them — Claude's mode pill relabelled `YOLO
    /// on` and the row's items packed left behind it
    /// (`crate::claude_footer::GlassColumns`) — the glass column under the
    /// pointer is mapped back to the grid column whose cell it shows. Every
    /// reader of the pointer's cell takes it from here (PTY mouse reports,
    /// the hyperlink and path hovers, a selection's anchor, click-to-focus),
    /// so they all land where the person sees the text. Identity on every
    /// row nothing relabelled.
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

    /// Refresh the remembered cells under the pointer: the raw window cell (click-to-focus
    /// hit-testing) and the focused-pane-local cell (PTY mouse reports), clamped to the
    /// pane's grid so a drag across a divider stays inside the focused pane. Returns the
    /// pane-local cell and pane rect, which the motion path reuses for selection autoscroll.
    /// The modal gates that swallow motion call it too, so the first click after a keyboard
    /// close does not act on a stale cell.
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

    /// Sub-cell pixel offset of the pointer inside its cell, from the real winit cursor
    /// (bands, pad and chrome rows stripped as `pixel_to_term_cell` does), so a DEC 1016
    /// (SGR-pixel) report carries a genuine sub-cell coordinate and a held divider can tell
    /// how far past a cell boundary the pointer is. Cell-coordinate encodings ignore it
    /// ([`crate::input::PixelOffset`]).
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

    /// The pointer's pixel inside the terminal grid, insets stripped (the pad on
    /// both axes, the chrome headroom and strip on y — `pixel_to_term_cell`'s
    /// exact insets) and not clamped to the grid: the shared base of
    /// [`Self::pointer_subcell_px`] and the divider drag's clamped offset.
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
        // The chrome headroom sits above the pad on the y-axis (x carries none),
        // matching `pixel_to_term_cell`'s `pad_top + head` inset.
        let gy = (fy - (geom.pad_top + geom.head) as f64).max(0.0) as usize;
        (gx, gy.saturating_sub(strip_px))
    }

    /// The [`HoverMapping`] window `wid`'s pointer → cell mapping runs under right now, with
    /// the geometry and focused pane rect derived afresh (the rect plans the split; no lock).
    fn hover_mapping(&self, wid: WindowId) -> HoverMapping {
        self.hover_mapping_with(wid, self.pointer_geometry(wid), self.focused_pane_rect(wid))
    }

    /// [`Self::hover_mapping`] against a geometry and focused rect the caller
    /// already derived — the grid arm stamps from the mapping it just ran.
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

    /// The window pixel at the centre of terminal cell `(row, col)`: the inverse of
    /// [`pixel_to_term_cell`] plus the frame origin [`Self::window_to_frame_with`] removes,
    /// so [`Self::pixel_to_cell`] maps it back to the same cell. The centre, not the origin:
    /// the origin is one rounding from the neighbouring cell, and the half-cell puts the
    /// pointer on a definite `SelectionSide`. `None` before the first layout or for a cell
    /// outside the grid, so the caller replies `ERR` instead of moving the pointer elsewhere.
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
        // The same three insets `pixel_to_term_cell` strips, put back in its order.
        let strip_px = usize::from(self.chrome_rows(wid)) * ch;
        let fx = usize::from(col) * cw + cw / 2 + geom.pad;
        let fy = usize::from(row) * ch + ch / 2 + strip_px + geom.pad_top + geom.head;
        #[allow(
            clippy::cast_precision_loss,
            reason = "window pixel coordinates; the f64 mantissa covers every representable window size"
        )]
        Some((ox as f64 + fx as f64, oy as f64 + fy as f64))
    }

    /// A mouse button in `wid` exactly as the event loop's `MouseInput` arm delivers it,
    /// shared with `pointer click` so the two cannot drift. The press lands on the layout the
    /// person saw (folding first could re-grid the band out from under the pointer); the fold
    /// law then reads the story of the session that was front before the press, since a
    /// press in another split pane makes that pane front.
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
            // Resolve the hit against the frame that was shown before stopping
            // the timer changes its label and invalidates that frame's stamp.
            self.interact_system_state(wid);
        }
        if let Some(session) = seen {
            self.note_human_acted_on(wid, session);
        }
    }

    /// Drive the front window's pointer and report where it ended up. The verb takes the
    /// pointer's own path: [`Self::on_cursor_moved`] and [`Self::on_cursor_left`] are what
    /// `WindowEvent::CursorMoved` / `CursorLeft` call, so hover resolution, the caption
    /// bracket, strip hover, divider cursors and pane hit-testing run as for a hand. (The
    /// `mouse` verb posts an engine `InputEvent` into the seam, which never touches
    /// `last_cursor_px` and never resolves a hover.)
    ///
    /// The reply is read back from the window after the real path ran, never predicted from
    /// the request. `Ok(None)` means the window holds no pointer position (after a `leave`,
    /// before any motion).
    pub(crate) fn pointer_cmd(
        &mut self,
        action: PointerAction,
    ) -> Result<Option<(i32, u16)>, String> {
        let Some(wid) = self.frontmost_window else {
            return Err("no window".into());
        };
        // A native front in a window with no glass (headless, no video take) has
        // never presented a frame for the glass-commit guard to stand on; it
        // commits the one a capture of it shows first, so a pointer reaches the
        // same tree `image` renders (cs-25), and again before the reply.
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
                // An aimed wheel is a hand that moved there first. On macOS
                // every wheel is: AppKit's `scrollWheel:` queues a
                // `CursorMoved` at the event's point ahead of the
                // `MouseWheel` (`aterm-window`'s macOS `view.rs`).
                if aimed || cfg!(target_os = "macos") {
                    self.on_cursor_moved(wid, x, y);
                }
                // The verb's `dy` is down and `dx` right, toward a list's end;
                // winit's are the content's motion, up and left (a positive
                // `y` reveals what is above).
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
                // What the event loop's `MouseWheel` arm calls, and for a
                // trackpad the zero-delta `Ended` its finger's lift sends.
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
                // A scripted press of a band capsule spends the band's press budget, the one
                // `notice act` spends: each repaints, may open a window and logs a line
                // (`click` is Owner-only at the dispatch gate, as `notice` is). Only a click
                // that presses something: the presence row, an echo's or a padded row, and a
                // question banner over the band spend nothing and are never refused.
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
                // What the event loop runs for a click, press then release
                // (the caller escalates a close the press asked for). On macOS
                // that is what AppKit delivers: `mouseDown:` and `mouseUp:` each
                // queue a `CursorMoved` at the press point ahead of the button
                // (`aterm-window`'s macOS `view.rs`), a motion event in which
                // the pointer did not move, so a scripted click meets every
                // surface that reads motion as a hand's click does.
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
        // The reply reads where the pointer is in the frame the event left: a
        // hover, a click or a wheel that acted changed the page, and input
        // reads only a presented frame. A glassless window presents nothing
        // until it is committed (it answered `ERR no presented native frame
        // under the pointer`); glass shows the change on the window's next
        // redraw, which the reply would not wait for (it answered `ERR the page
        // under the pointer changed …` for every event that worked).
        if !self.commit_glassless_native_frame(wid) {
            self.present_native_frame_on_glass(wid);
        }
        Ok(self.pointer_readback(wid))
    }

    /// Present `wid`'s next frame now, as its next redraw would, where it has
    /// glass and the native page in front changed since its last present, so
    /// a reply reads the page the person sees next rather than the one before
    /// the event. Nothing where the page in front is presented as it stands,
    /// or is not native. The reply waits on the present, as a capture does,
    /// so it is a capture's (a minimized Windows window presents it too); a
    /// present that fails leaves the reply saying the frame is not presented.
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

    /// The `pointer` verb's answer after [`Self::pointer_cmd`] ran: over a native
    /// view of the front window, the view, the logical point and the control there
    /// ([`PointerAt::Native`]); over a terminal, the window cell; `ERR` where a
    /// native front has no presented frame under the pointer, never a cell the
    /// pointer did not resolve to.
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
        // A pointer over the chrome above the page (a negative row: the band,
        // the tab strip) is on the chrome, whatever the tab holds.
        if cell.0 >= 0
            && let Some((_, view)) = self.active_native_view(wid)
            && !self.active_tab_has_multiple_visible_panes(wid)
        {
            // A page that changed since its last present still shows that
            // frame, and input reads only what is shown. The verb presented
            // (glass) or committed (no glass) the page its event left before
            // replying, so this is a present that failed (its frame is staged,
            // not shown), or a change the verb did not make (`pointer status`
            // presents it). With no frame of the page at all, none is there.
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

    /// Commit the frame a capture of `wid` shows as the presented one, where the
    /// window has no glass to present on — no OS window and no video take
    /// (`PresentTarget::Virtual`) — and a native view is in front: the shipping
    /// twin of the tests' `prepare_presented_native`. The glass-commit rule
    /// (input lands only on what was shown) is kept: a glassless window has no
    /// other observer frame, and the staged tree is exactly what `image`
    /// renders. `false` with glass, or with nothing native to commit.
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
        // The committed frame is the one under the pointer now.
        self.refresh_native_hover_for_frame(wid);
        true
    }

    /// The window pixel a `pointer move` on a native view aims at: a control's
    /// rect centre in the presented tree, or a logical point of the view, mapped
    /// through the presented leaf's destination and scale.
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
                // Where a press takes the control, where it takes one: a
                // control a list's edge cuts is drawn on into the strip under
                // the list's room, which is only a picture, so the centre of
                // what is drawn can be a point no press there takes it at.
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

    /// The front window's pointer as a window pixel (rounded), or `None` when it
    /// holds no position: the `pointer` verb's `px=`.
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

    /// THE ONE OWNER OF NATIVE HOVER: set `hit`'s control as hovered in its view
    /// and none in every other visible native view of `wid`'s active tab (hover
    /// is view-owned even in a native/native split), dirtying only the views
    /// whose hovered control changed. The motion path and the pointer's leaving
    /// the window (`None`) both go through it.
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
            // A move that leaves the pointer over what it was over changes
            // nothing the view renders from: its content revision stays.
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

    /// What a pointer at `(x, y)` over window `wid` hovers on the native frame
    /// on glass: the presented leaf's control under it, its view, the
    /// generation that frame was drawn at, whether it is a text field and
    /// whether its press looks first; `None` over the pane header, the tab
    /// strip, no control, or a frame its view has since moved past.
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

    /// A WHEEL'S PAGE IS HOVERED WHERE THE POINTER IS ON THE FRAME IT LEFT:
    /// hover is resolved on pointer motion, and a wheel moves a native page
    /// with no motion after it (macOS sends its `CursorMoved` BEFORE the
    /// scroll, on the old frame), so the control the pointer had been over
    /// went on looking hovered and what reads the hover went on claiming it —
    /// Cursor Styles previewed the row the pointer had left, saying "Click to
    /// save", while a click there saved the row scrolled under it. Called
    /// where a native frame reaches glass (or is committed, with none): after
    /// a wheel moved the page (the window's `native_rehover`), once the
    /// frame on glass is the page as it now stands, the pointer is
    /// hit-tested on it as the motion path does, through the one hover owner.
    /// The pointer is the person's here, as with any motion; a page the
    /// keyboard moved keeps the keyboard's claim (a Cursor Styles row it
    /// moved to takes over from a resting pointer until the pointer moves).
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
        // Only a frame of the page as it now stands: a stale one waits.
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

    /// Where window `wid`'s pointer is, read back from the pixel the last
    /// motion stored: a chrome row as a negative row (`-1` the row right
    /// above the grid), a grid cell as the window cell the motion resolved;
    /// `None` when the window holds no position.
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

    /// The window pixel at the centre of column `col` of the chrome row `up`
    /// rows above the grid (`1` the row right above it) — the rows the strip,
    /// the presence row and the message band occupy, in the order
    /// [`Self::cell_centre_px`] strips them. `None` past the chrome's rows or
    /// the grid's columns, or before the first layout.
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

    /// Where the padded frame's top-left sits inside the raw window: the leading remainder
    /// bands the present paths place the frame behind (`aterm_render::band_offset`; the
    /// render side's `content_off`). Signed, because a transient or tiny destination crops
    /// the centred frame. `(0, 0)` at exact grid fit, headless and before attach. Derived
    /// from the composed frame extent the presenters use (top and bottom padding
    /// independent), so pointer geometry and pixels agree.
    pub(crate) fn frame_origin(&self, wid: WindowId) -> (i64, i64) {
        // The bail-outs are geometry-free, so they come before `pointer_geometry`.
        if !self.windows.get(&wid).is_some_and(|ws| ws.win_px.is_some()) {
            return (0, 0);
        }
        self.frame_origin_with(wid, self.pointer_geometry(wid))
    }

    /// [`Self::frame_origin`] against geometry the caller already derived.
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
            // Vertical placement is the platform policy (top-pinned on Linux):
            // the same `band_offset_y` the presenters place the frame with, so
            // pointer geometry and pixels can't disagree.
            aterm_render::band_offset_y(size.height as usize, frame_h),
        )
    }

    /// Translate a window-space pixel to frame space (strip the leading bands), clamped at 0
    /// so a click in a band maps to the frame edge, as one in the pad border does. The entry
    /// seam for every pointer → frame consumer (cells, strip, settings/about overlays, IME).
    pub(crate) fn window_to_frame(&self, wid: WindowId, x: f64, y: f64) -> (f64, f64) {
        let (ox, oy) = self.frame_origin(wid);
        ((x - ox as f64).max(0.0), (y - oy as f64).max(0.0))
    }

    /// [`Self::window_to_frame`] against geometry the caller already derived.
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

    /// Window → frame on the Y axis without [`Self::window_to_frame`]'s clamp, so the result
    /// is negative above the frame's first pixel row. Hit-testing wants the clamp (a press in
    /// a leading band belongs to the edge cell); edge-overshoot math needs the signed
    /// distance past an edge, and the clamp would read a pointer dragged above a window with
    /// no top inset as resting on row 0.
    fn window_to_frame_y_signed(&self, wid: WindowId, geom: PointerGeometry, y: f64) -> f64 {
        let (_, oy) = self.frame_origin_with(wid, geom);
        y - oy as f64
    }

    /// If pixel position `(x, y)` lands in window `wid`'s tab-strip region (the top
    /// `tab_strip_rows` pixel rows), return its strip column; otherwise `None` (the
    /// click is in the terminal region and maps to a cell as usual). Always `None`
    /// when the strip is disabled. Used by the mouse handlers to intercept strip
    /// clicks before the focused-pane cell mapping.
    pub(crate) fn strip_col_at(&self, wid: WindowId, x: f64, y: f64) -> Option<u16> {
        // The disabled-strip bail (the macOS default) comes before the geometry derivation.
        if !self.tab_strip_enabled() {
            return None;
        }
        self.strip_col_at_with(wid, self.pointer_geometry(wid), x, y)
    }

    /// A left press at window pixel `(px, py)`: if it lands on the message
    /// band (or the presence row above it), consume it — `true` — and perform
    /// what the band says (`App::press_band`). THE PASTE QUESTION FIRST,
    /// FAIL-CLOSED (design §3.4): the multi-line-paste confirmation OVERWRITES
    /// the top composed rows in place — the band's own rows — so while it is
    /// up in this window a press inside its band is its answer (cancel; the
    /// parked text is dropped), never a capsule press underneath it. The
    /// close/quit confirmation (off macOS, [`crate::close_confirm`]) takes the
    /// same band and the same fail-closed reading of a press: cancel.
    fn press_band_at(&mut self, wid: WindowId, px: f64, py: f64) -> bool {
        // Judged against the banner's own floor (`paste_banner_tray_floor_y`, `0` = none)
        // before the band's hit test, whether or not the band has a row under it.
        if let Some(paste_up) = self.band_banner_at(wid, px, py) {
            if paste_up {
                self.answer_paste_banner(false);
            } else {
                // Nothing closes on a press that named no answer.
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

    /// Whether a question banner covers window pixel `(px, py)` where it
    /// overwrites the band's rows — `Some(true)` the paste question,
    /// `Some(false)` the close/quit confirm — so a press there is its answer
    /// ([`Self::press_band_at`]), never the band's.
    fn band_banner_at(&self, wid: WindowId, px: f64, py: f64) -> Option<bool> {
        let paste_up = self.paste_banner.as_ref().is_some_and(|p| p.wid == wid);
        // The close/quit confirm (off macOS) shares the paste banner's slot and
        // floor (`splice_close_banner`); the two are never up together on one
        // window, and the paste question keeps the band when both could be.
        let close_up = self.close_banner.as_ref().is_some_and(|p| p.wid == wid);
        if !(paste_up || close_up) {
            return None;
        }
        let floor = self.paste_banner_tray_floor_y(wid);
        let (_, fy) = self.window_to_frame(wid, px, py);
        let top = (self.win_pad_top(wid) + self.win_head(wid)) as f64;
        (floor > 0 && fy >= top && fy < f64::from(floor)).then_some(paste_up)
    }

    /// [`Self::strip_col_at`] against geometry the caller already derived.
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
        // The stats Deck in the head is not the strip, though a point above
        // the grid clamps onto the strip's row: it takes its own presses
        // (`App::stats_press`), and a pointer resting on it lights no tab.
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

    /// Window pixel `(x, y)` as a frame cell `(row, col)`: the coordinates the tab-strip
    /// splice (and so [`crate::tab_menu::MenuRect`]) works in, row 0 being the strip's first
    /// row. Unclamped, like [`Self::find_bar_pixel_hit`]: `pixel_to_cell` snaps a pad point
    /// onto the nearest cell, so a press in the top border or past the right edge would hit
    /// a menu row it never touched. A point outside the grid interior is `None`, off the card.
    pub(crate) fn frame_cell_at(&self, wid: WindowId, x: f64, y: f64) -> Option<(usize, usize)> {
        let geom = self.pointer_geometry(wid);
        let cols = self.windows.get(&wid).map_or(0, |ws| ws.cols);
        frame_cell_of(geom, self.frame_origin_with(wid, geom), cols, x, y)
    }

    /// The grid window `wid`'s frame is laid on now ([`GlassGrid`]): what
    /// the harness's glass stamps a frame's targets with, and what a pointer
    /// event finds them stamped with or not. `None` for no window.
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

    /// THE ONE GATE for a target a frame laid in frame cells: the frame cell
    /// under window pixel `(x, y)` on grid `laid`, the one the target was
    /// stamped with as it was laid ([`GlassGrid`]) — `None` where nothing
    /// was laid, or where window `wid`'s grid is no longer that one. After a
    /// re-grid, until the next frame lays them again, the last frame's cells
    /// are not where the window now puts them, so the pointer is told nothing
    /// is there rather than what was: the harness's glass
    /// (`App::glass_cell_at`), the tab menu's card ([`Self::tab_menu_probe`])
    /// and the Autopilot and fact menus' card all ask here.
    pub(crate) fn laid_cell_at(
        &self,
        wid: WindowId,
        laid: Option<GlassGrid>,
        x: f64,
        y: f64,
    ) -> Option<(usize, usize)> {
        let laid = laid?;
        (self.glass_grid(wid)? == laid)
            .then(|| laid.cell_at(x, y))
            .flatten()
    }

    /// The open menu's recorded rect plus the frame cell under `(x, y)`, once the menu has
    /// been painted. The rect is the painter's, never re-derived, so a click never resolves
    /// against a placement the glass does not show — and only on the grid the painter laid
    /// it on ([`Self::laid_cell_at`]): between a re-grid and the frame that places the card
    /// again, it answers nothing, as before its first paint.
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

    /// The live action row under `(x, y)`, or `None` on a header, a separator, a disabled
    /// row, a border, or off the card.
    fn tab_menu_action_at(&self, wid: WindowId, x: f64, y: f64) -> Option<usize> {
        let (rect, row, col) = self.tab_menu_probe(wid, x, y)?;
        let ws = self.windows.get(&wid)?;
        let menu = ws.tab_menu.as_ref()?;
        crate::tab_menu::action_at(rect, &menu.entries, row, col)
    }

    /// Whether `(x, y)` is anywhere on the open card, border included. A press there is
    /// swallowed even when it activates nothing: a click on the card must not reach the
    /// terminal beneath.
    fn tab_menu_contains(&self, wid: WindowId, x: f64, y: f64) -> bool {
        self.tab_menu_probe(wid, x, y)
            .is_some_and(|(rect, row, col)| rect.contains(row, col))
    }

    /// Move the open menu's highlight to the row under the pointer (or clear it off the
    /// card). Change-gated like [`Self::track_strip_hover`]: only a row change repaints.
    fn track_tab_menu_hover(&mut self, wid: WindowId, x: f64, y: f64) {
        let hovered = self.tab_menu_action_at(wid, x, y);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let Some(menu) = ws.tab_menu.as_mut() else {
            return;
        };
        // A keyboard-opened menu keeps the row its opener lit until the pointer enters the
        // card (a brushed mouse must not unselect the row a ⇧F10 user is about to press ↵
        // on); the first hover on a row hands the highlight to the pointer for good.
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

    /// If the last pointer position lands on the pet's drawn body (the rect the redraw
    /// stashed post-tick, padded by [`aterm_effects::companion::PET_HIT_SLOP_PX`]), pet it
    /// and consume the press; the caller stops routing on `true`.
    ///
    /// Chrome wins, like the tab strip: a press that pets never starts a selection and is
    /// never reported to a tracking app (it sets no reported bit, so the orphan-release guard
    /// drops its release). Accepted caveat: on the first click after a tab switch, before any
    /// `CursorMoved` on the new layout, `last_cursor_px` is where the pointer last was; the
    /// rect itself is fresh per frame.
    fn pet_press_at(&mut self, wid: WindowId) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let (px, py) = ws.last_cursor_px;
        let (fx, fy) = self.window_to_frame(wid, px, py);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        // The owner hit-tests this frame's drawn body (padded by `PET_HIT_SLOP_PX`) and on a
        // hit latches the pet (note, never act); the redraw consumes it: a purr-flavoured
        // hold, a heart per queued pet, contentment toward the real purr.
        if ws
            .fx
            .companion_mut()
            .press(Instant::now(), (fx as f32, fy as f32))
            != aterm_effects::host::PressOutcome::Pet
        {
            return false;
        }
        // The pet may be settled with the frame lane released — the latch
        // re-arms `needs_frames`, but only a tick reads it, so ask for the
        // frame that runs one.
        if let Some(w) = ws.os_window.as_ref() {
            w.request_redraw();
        }
        true
    }

    /// Track which in-grid strip tab the pointer is over, so the `✕` is a hover-only
    /// affordance as on the native macOS strip. Runs on every `CursorMoved`, including motion
    /// later paths consume, because leaving the strip must clear the reveal as surely as
    /// entering sets it. Change-gated: a redraw only when the hovered target changes; the
    /// redraw re-runs `splice_tab_strip_with`, whose cache key carries the hover.
    fn track_strip_hover(&mut self, wid: WindowId, geom: PointerGeometry, x: f64, y: f64) {
        if !self.tab_strip_enabled() {
            return;
        }
        // Drag-to-reorder rides the same hook, before the change-gated hover below: a reorder
        // that leaves the pointer over the same index must still repaint, which the step does
        // itself.
        self.advance_strip_drag(wid, geom, x, y);
        // One hit test, several hover states: the `+` is not a tab, so an `Option<usize>`
        // cannot carry it. Resolved from the same hit, so two are never lit at once and
        // leaving one clears the other.
        let hit = self.strip_col_at_with(wid, geom, x, y).and_then(|col| {
            let segs = self
                .windows
                .get(&wid)
                .map(|ws| ws.tab_segments.as_slice())?;
            crate::tab_bar::hit_test(segs, col)
        });
        let hovered = match hit {
            Some(crate::tab_bar::TabHit::Select(index)) => Some(index),
            // The `+` and the `↻` are not tabs; the pointer is in the strip but
            // on no tab, so nothing reveals a `✕`.
            _ => None,
        };
        let on_new_tab = matches!(hit, Some(crate::tab_bar::TabHit::NewTab));
        // The trailing version label (Windows) is no tab either; same hit, so at most one of
        // the three is lit.
        let on_version = matches!(hit, Some(crate::tab_bar::TabHit::Version { .. }));
        // Nor is a window button of a band that is the window's header
        // (`crate::client_frame`); same hit again.
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

    /// Pointer motion over this window's own frame, where it draws one
    /// ([`crate::client_frame`]), before any other owner sees it; `true` when the
    /// frame took it.
    ///
    /// - A bare-band press armed a move (`WindowState::caption_move`): the first
    ///   motion past [`crate::client_frame::MOVE_THRESHOLD_LOGICAL_PX`] hands the
    ///   window to the compositor's interactive move (`drag_window`, on the press's
    ///   still-held button).
    /// - A resize handle ([`crate::client_frame::edge_at`]) under the pointer, with no
    ///   gesture in flight: its cursor goes up, every chrome hover is dropped (the
    ///   handle is no chip, `+` or button), and the motion stops here.
    /// - Leaving a handle sets the arrow and the divider's re-issue encoding on the
    ///   hover-cursor detector, so whichever owner the pointer lands on puts its own
    ///   cursor back ([`Self::set_hover_cursor`], the strip's reset).
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
                // The press is the compositor's now: no double-click follows it.
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

    /// How much of window `wid` (of raw `size`) lies outside its terminal grid
    /// along each edge ([`crate::client_frame::FrameMargins`]): the grid's rect
    /// exactly as [`Self::pixel_to_cell`] maps it (the frame origin, the pad, and
    /// above the grid the top pad, the band and the chrome rows), so the resize
    /// handles never take a cell's pixel.
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

    /// The pointer left the window: clear its hover state. `CursorMoved` alone drives
    /// [`Self::track_strip_hover`], so whatever was lit at the last motion inside would stay
    /// lit (a `+` wash, a `✕` on a chip nobody points at). Change-gated like the tracker; the
    /// redraw re-runs `splice_tab_strip_with`, whose cache key carries the flags.
    pub(crate) fn on_cursor_left(&mut self, wid: WindowId) {
        // A native view's hovered control is the same shape of stale claim: its
        // wash, and a Settings cursor-style row's preview, would go on showing
        // what the pointer is no longer over (cs-25). One owner clears it, the
        // one the motion path sets it through.
        self.apply_native_hover(wid, None);
        let native_front = self.active_native_view(wid).is_some();
        if native_front && let Some(ws) = self.windows.get_mut(&wid) {
            ws.hover_pointer = false;
            ws.native_text_cursor = false;
        }
        // The link caption is the same shape of stale claim: it names what a
        // click on the cell the pointer was over would open, and there is no
        // pointer over that cell any more.
        self.retire_link_target(wid);
        // So is a hovered Claude Code chip's title (`crate::claude_lights`).
        self.clear_claude_light_hover(wid);
        // …and a widened scrollbar (`crate::pane_chrome`).
        self.scrollbar_pointer_left(wid);
        // …and a rest armed on a stats instrument; a hover lens starts its leave.
        self.stats_pointer_left(wid, Instant::now());
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        // The pointer memos are kept (a press racing the departure still hit-tests against
        // the last position); what is withdrawn is the claim that they describe a pointer
        // this window holds. Cleared before the strip-hover early return.
        ws.pointer_position_known = false;
        ws.hover_grid_owned = false;
        ws.native_rehover = false;
        // The band's lit chip is the same shape of stale claim as the strip's.
        let band_lit = ws.band_hover.take().is_some();
        // A resize handle's cursor is the window system's to change once the pointer
        // is gone; only the claim is withdrawn, so re-entry re-resolves it.
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

    /// Drag a tab to reorder it, advanced from the hover's per-motion hook
    /// ([`Self::track_strip_hover`]). Armed by the press that selects a chip
    /// (`handle_tab_strip_click`'s [`crate::tab_bar::TabHit::Select`] arm, which still
    /// switches on the press, as Explorer, Edge and Windows Terminal do); disarmed by the
    /// release, any other strip press ([`App::clear_strip_press`]) and focus loss.
    ///
    /// The held tab steps one slot whenever the pointer passes the midpoint of a neighbouring
    /// segment in the `ws.tab_segments` the paint recorded, so chips never swap where the
    /// glass does not show them. A press that never moved cannot reorder (a neighbour's
    /// midpoint lies beyond the held segment, and `origin_col` re-anchors after each step);
    /// the step is self-stabilising (`move_tab` keeps the tab selected, so the next crossing
    /// needs fresh travel). Off the band the drag stays armed but takes no step (the chip
    /// neither tears out nor snaps home), so sweeping back onto the strip resumes it. One
    /// coalesced motion may cross several midpoints, so this steps until the pointer is over
    /// the held chip again, bounded by the tab count.
    fn advance_strip_drag(&mut self, wid: WindowId, geom: PointerGeometry, x: f64, y: f64) {
        let Some(drag) = self.windows.get(&wid).and_then(|ws| ws.strip_drag) else {
            return;
        };
        // A palette opened by keyboard under a held button is modal though it never claimed
        // the pointer: stepping the chip under it would reorder the strip out of sight.
        // Read-only on purpose (`palette_claims_pointer` owns the hover/press side effects);
        // the release still disarms in `on_mouse_input`.
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

    /// One step of [`Self::advance_strip_drag`]: where the held `tab` currently
    /// sits and where a pointer at strip column `col` wants it, or `None` when the
    /// pointer has not passed a neighbour's midpoint (or the tab is gone, or it has
    /// no neighbour on that side — a lone tab always lands here).
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
        // Midpoint of a half-open `[start, end)` column range.
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
        // Past the page's edge: the strip seats a page of the tab list
        // (`tab_bar::strip_window`), so at a page edge the neighbour has no segment and no
        // midpoint. The rule then degenerates to the pointer leaving the held chip on that
        // side: one step turns the page and `advance_strip_drag`'s loop settles the chip under
        // the hand. Otherwise a tab could never leave its page.
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

    /// A left click on the Cmd-F find panel. On the `Aa` (case) / `.*` (regex) indicators it
    /// fires the same toggle as ⌥⌘C / ⌥⌘R; inside the query well it moves the caret to the
    /// character under the pointer (past the text ⇒ the end). `row`/`col` are terminal cell
    /// coordinates (`pixel_to_cell`), matching what `splice_find_bar` recorded in
    /// [`crate::FindBarHit`]. `false` when not searching or off the panel; a click elsewhere
    /// on the panel's band is consumed, since a drag there would select the panel's own text.
    ///
    /// `x` is the press's window pixel, which the query box of the bar's Fluent face
    /// (`find_bar::fluent_bar`) maps through: its query is set in the UI face, so a character
    /// is not a cell, and the click lands on the character boundary nearest the pointer
    /// (`find_bar::well_click`), anywhere in the box. Without a pixel, or where the bar is its
    /// cells, the well maps cell to character.
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
        // In a split the panel covers the focused pane's columns only.
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
            // The toolbar's arrows are Shift+F3 / F3 (`find_bar::FIND_TOOLBAR`).
            self.search_repeat_in(wid, on(&hit.next_cols));
        } else if on(&hit.close_cols) {
            // `✕` is Esc: cancel, the viewport back where find began.
            self.search_cancel_in(wid);
        } else if let Some(slot) = x.and_then(|x| self.find_box_slot(wid, &hit, x)) {
            self.search_click_caret_in(wid, slot);
        } else if hit.field_cols.contains(&col) {
            // The well's first cell shows character `field_scroll`, so the click's
            // distance into it is the character index — the same mapping the paint used.
            self.search_click_caret_in(wid, hit.field_scroll + (col - hit.field_cols.start));
        }
        true
    }

    /// The caret slot a press at window pixel `x` asks for in the query box of the find bar's
    /// Fluent face (`find_bar::well_click`), from the scroll the paint recorded; `None` off
    /// the box, or where the bar is its cells (`find_bar::fluent_ready`).
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

    /// Pixel gate for [`Self::find_bar_click_at`]: `true` only when window pixel `(x, y)` is
    /// inside the find bar's cell band, not merely on the row [`Self::pixel_to_cell`] clamps
    /// it to (a click in the top `pad_top` border snaps to row 0, the bar's usual row; one in
    /// the bottom `pad` to the last row). On the frame's y-axis the band starts at
    /// `pad_top + head + frame_row * ch` and spans `rows * ch`, with
    /// `frame_row = chrome_rows + hit.band.start` as `splice_find_bar` placed it; the x-check
    /// admits the panel's own columns only (`hit.cols`: the grid, or a split's focused pane),
    /// so clicks past the grid's right edge or on a sibling pane miss. No bar ⇒ `false`.
    pub(crate) fn find_bar_pixel_hit(&self, wid: WindowId, x: f64, y: f64) -> bool {
        let Some((band, cols)) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.find_bar_hit.as_ref())
            .map(|h| (h.band.clone(), h.cols.clone()))
        else {
            return false;
        };
        let (cw, ch) = self.win_cell_size(wid);
        let (cw, ch) = (cw.max(1), ch.max(1));
        let pad = self.win_pad(wid);
        let pad_top = self.win_pad_top(wid);
        // Unclamped frame coordinates: `window_to_frame` clamps at 0, which would land a
        // click above the frame (the top pad over a top-anchored bar) in the row-0 band.
        let (ox, oy) = self.frame_origin(wid);
        let (fx, fy) = (x - ox as f64, y - oy as f64);
        let frame_row = usize::from(self.chrome_rows(wid)) + band.start;
        let top = (pad_top + self.win_head(wid) + frame_row * ch) as f64;
        let bottom = top + (band.len() * ch) as f64;
        if fy < top || fy >= bottom || fx < pad as f64 {
            return false;
        }
        // Reject clicks past the right edge — `pixel_to_cell` would clamp their column
        // onto the last grid cell, which can land on an indicator span — and, in a
        // split, clicks on the sibling beside the panel's pane.
        let gx = (fx as usize).saturating_sub(pad);
        cols.contains(&(gx / cw))
    }

    /// Resolve the OS cursor for the pointer's current location. Called on every
    /// `CursorMoved` (via the `_with` twin, reusing the motion path's geometry) and on each
    /// link-modifier edge while the pointer is inside, so the link hand tracks a bare
    /// Cmd/Ctrl press; that caller has no pointer event, so this resolves by location
    /// (`last_cursor_px`, `last_mouse_window_cell`).
    ///
    /// Order: chrome (the strip's arrow; a Claude Code chip's, the band's or a pane header's
    /// hand) → split divider (the axis resize cursor) → the grid: the link hand (modifier
    /// held), else an I-beam, its primary gesture being text selection, or the arrow while a
    /// VT mouse-tracking app owns the pointer (presses are reported then, not selecting).
    pub(crate) fn update_hover_cursor(&mut self, wid: WindowId) {
        // The cold caller's wrapper (modifier changes); the motion path threads its derived
        // geometry instead. Bracketed like the motion path, so a modifier tap with the
        // pointer on chrome retires a standing caption and repaints it away.
        let parked = self.park_link_target(wid);
        self.update_hover_cursor_with(wid, self.pointer_geometry(wid), parked, None);
        self.settle_link_target(wid, parked);
    }

    /// Re-resolve the hover when the frame under a still pointer has changed: the frame-time
    /// half of [`HoverMemo`], called at every engine-fill site while the fill's guard `t` is
    /// held. When the pointer is known, the last routing reached the grid arm
    /// (`hover_grid_owned`, so never under a menu card or a drag's grab cursor), and the memo
    /// no longer stands ([`HoverMemo::stands_for`]: the fill moved, see [`HoverFrame`], or a
    /// deferred probe left another cell), the grid arm re-probes against `t`. A frame the
    /// engine did not refill costs one epoch compare.
    ///
    /// Riding the frame's acquisition, the probe is never deferred: the reader cannot be
    /// mid-slice while this thread holds its mutex. No settle: the result reaches the glass
    /// through the frame's own RepaintKey (`link_caption`). The gate register it reads
    /// (`chrome_rows`) is the previous frame's, as for a pointer event between frames.
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

    /// Take the standing link hover without repainting, so the resolution about to run
    /// decides the whole answer. Every early return in [`Self::route_cursor_moved`] leaves it
    /// taken, which is right for all of them: chrome, a modal or a native view owns the
    /// pointer, and no click can reach a grid link. Pairs with [`Self::settle_link_target`],
    /// which repaints.
    fn park_link_target(&mut self, wid: WindowId) -> Option<crate::link_target::LinkHover> {
        self.windows
            .get_mut(&wid)
            .and_then(|ws| ws.link_hover.take())
    }

    /// Close a [`Self::park_link_target`] bracket: repaint only if the pointer
    /// routing published something different from what it parked. A pointer
    /// resting still on one cell re-publishes the identical hover — the common
    /// case while a caption is up — and must not ask for a frame.
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

    /// Retire any standing caption, repainting it away. For the boundaries that
    /// end a hover outright rather than re-resolving it — the pointer leaving
    /// the window, the window losing focus.
    pub(crate) fn retire_link_target(&mut self, wid: WindowId) {
        let parked = self.park_link_target(wid);
        self.settle_link_target(wid, parked);
    }

    /// [`Self::update_hover_cursor`] against geometry the caller already derived. Publishes
    /// both halves of one hover resolution from one probe, the OS cursor and the cell whose
    /// destination the band discloses, so they never disagree about which link is under the
    /// pointer.
    ///
    /// `parked` is the hover the caller's park/settle bracket took: when the grid probe finds
    /// the terminal mutex busy (the PTY reader mid-slice) the resolution is deferred and the
    /// previous cursor and caption stand until the next pointer event or engine fill
    /// ([`Self::refresh_hover_for_frame_locked`]).
    ///
    /// `probe` is the frame's own hold when the caller is the frame: the grid arm reads
    /// through it, with no acquisition and no deferral. `None` on the motion and modifier
    /// paths.
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
            // A plain write: the park/settle bracket at the event boundary owns
            // the repaint decision, because only it can see what the value was
            // before the whole routing ran.
            ws.link_hover = resolved;
        }
    }

    /// Resolve the hover state, writing the cursor and returning the cell whose
    /// destination is to be disclosed: its OSC 8 hyperlink, or, with the link
    /// modifier held, the faint plain-text URL under it (`crate::PlainUrl`).
    /// `None` from every arm where the grid is not the thing under the pointer.
    /// `probe` is the frame's own terminal hold when there is one (see
    /// [`Self::update_hover_cursor_with`]).
    fn resolve_hover_cursor(
        &mut self,
        wid: WindowId,
        geom: PointerGeometry,
        probe: Option<&Terminal>,
    ) -> HoverProbe {
        use HoverProbe::Resolved;
        // While a modal overlay is open the pointer is its (the About dialog runs
        // its own link/I-beam cursor, the palette its row hand): a Cmd press must
        // not resolve a terminal link hidden under the card and flip the cursor
        // out from under the modal.
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.overlay.is_some())
        {
            return Resolved(None);
        }
        // The tab context menu is a separate window-state surface, not an
        // `Overlay` variant, and it owns the pointer the same way while it is
        // up (the motion ladder returns at its arm): a link hidden under the
        // card earns no hand and no caption, and the cell beneath it is not
        // what a press there would hit.
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.tab_menu.is_some())
        {
            return Resolved(None);
        }
        // So does the Autopilot menu, and a fact's menu with it
        // (`crate::autopilot_menu`): its motion arm owns the pointer, and a
        // chip or fact hidden under the card is not what a press would hit.
        if self.autopilot_menu.as_ref().is_some_and(|m| m.wid == wid) {
            return Resolved(None);
        }
        // A native view owns its cursor through the motion path's native branch (I-beam over
        // text bodies, hand over controls); a modifier tap must not stomp it.
        if self.active_native_view(wid).is_some() {
            return Resolved(None);
        }
        // A held divider drag owns the resize cursor until release
        // (`begin_divider_drag` set it, `finish_divider_drag` restores): mid-drag
        // the pointer routinely leaves the seam, and re-resolving would flicker
        // the icon against the very gesture it advertises.
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
        // Chrome: over the tab strip the arrow, never an I-beam (the strip is not selectable
        // text), as the motion path's strip branch answers, repeated for the eventless caller.
        if self.strip_col_at_with(wid, geom, px, py).is_some() {
            self.set_hover_cursor(wid, CursorIcon::Default, false, false);
            return Resolved(None);
        }
        // A Claude Code chip, the top rule's switch or a fact of the bottom
        // rule: a press acts on it (`crate::claude_lights`) — a hand; the
        // blank between two chips is theirs too, where a press does nothing
        // — the arrow, as the motion path has it.
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
        // The harness banner over Claude Code's update notice
        // (`crate::claude_card`): the hand only where a link-modifier press
        // opens its link; elsewhere on what it hid, what the grid beneath
        // would show — the arrow under mouse tracking, else the I-beam — and
        // never the hidden text's link or its caption.
        if let Some((session, url)) = self.claude_notice_at(wid, px, py) {
            let held = self
                .windows
                .get(&wid)
                .is_some_and(|ws| link_modifier_held(ws.mods));
            let tracking = self
                .pool
                .get(session)
                .is_some_and(|owner| owner.ctx.modes.mouse_tracking_enabled());
            if url.is_some() && held {
                self.set_hover_cursor(wid, CursorIcon::Pointer, true, false);
            } else if tracking {
                self.set_hover_cursor(wid, CursorIcon::Default, false, false);
            } else {
                self.set_hover_cursor(wid, CursorIcon::Text, false, true);
            }
            return Resolved(None);
        }
        // The message band: a hand where a press acts (a capsule, a row body, the overflow
        // link), the plain pointer elsewhere (the presence row). Never an I-beam: the band is
        // not selectable text.
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
        // Split dividers (hover half): the resize cursor says the seam is draggable before a
        // drag arms.
        if let Some(icon) = self.divider_cursor_under_pointer(wid) {
            self.set_hover_cursor(wid, icon, true, true);
            return Resolved(None);
        }
        // The grid. The mouse mode is read lock-free from the session's mode mirror; the two
        // grid probes (the hyperlink on the hovered cell, the plain-text URL under it) are
        // memoised on the question they answer and re-asked, under one non-blocking
        // acquisition, only when it changes: a new cell, a modifier edge, a flipped gate, a
        // new frame under a still pointer (`HoverMemo`).
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
        // Whether the cell about to be probed is the one the pointer is over; a link read
        // otherwise would be a hand promising a click that opens nothing and a caption naming
        // the wrong destination:
        //
        //   * over a sibling pane the pane-local cell is clamped into the focused pane, and a
        //     press there focuses the sibling;
        //   * a chrome band covers the terminal row it took (the find panel, a banner), and a
        //     press there hits the chrome. Asked of the frame's register of claimed rows, so
        //     a new band needs no clause here.
        let cell_is_under_pointer = self.pointer_is_inside_focused_pane(wid)
            && !self.chrome_owns_terminal_row(wid, usize::from(window_row));
        let Some((term, session)) = self
            .front_terminal(wid)
            .map(|terminal| (terminal.term.clone(), terminal.session))
        else {
            self.set_hover_cursor(wid, CursorIcon::Text, false, true);
            return Resolved(None);
        };
        // The mouse mode, from the terminal's published mirror: `process()`
        // republishes it before every hold ends, so this is the mode the last
        // completed batch left — with no queue position behind the reader.
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
                // The frame's own hold, still held from the fill that made this question
                // new: read straight through it.
                Some(t) => {
                    probe_hover_cell(t, row, col, cell_is_under_pointer, mod_held, min_contrast)
                }
                None => {
                    // Non-blocking: a busy mutex means the PTY reader is mid-slice, and a
                    // hover icon never queues the UI thread behind it. Defer (the caller
                    // keeps the previous cursor and caption); the next pointer event or
                    // engine fill re-asks.
                    let Some(t) = crate::term_try_lock(&term) else {
                        return HoverProbe::Deferred;
                    };
                    probe_hover_cell(&t, row, col, cell_is_under_pointer, mod_held, min_contrast)
                }
            };
            // A citation is resolved here, with the terminal let go (the frame's own hold
            // aside). The filesystem is looked at off this thread, waited on for a moment
            // at most (`path_link::Looks`): a look not back by then is no link yet.
            // The naming is read BEFORE the answer: one that moves in between leaves
            // the memo with the older naming, which the sweep re-asks (never the
            // newer, which would hide an answer given under the older one).
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
        // Disclose unconditionally, not only with the open modifier down: an OSC 8 link's
        // text and destination can be unrelated, and a person needs to know while deciding
        // whether to click, before the hand reaches the modifier. A faint plain URL is found
        // only with the modifier down, the moment the hand offers its click.
        Resolved(disclosed.then_some(crate::link_target::LinkHover {
            cell: (row, col),
            window_row,
            session,
        }))
    }

    /// Write one resolved hover-cursor state, touching the OS cursor only on a change. The
    /// two `WindowState` bools are the change detector, with one borrowed encoding:
    /// `(pointer, text) = (true, true)`, impossible for the plain states, means a divider's
    /// resize cursor and always re-issues `set_cursor` (the pair cannot tell Col from Row,
    /// and a hover across a split junction must not keep the wrong axis). Every reset site
    /// (`finish_divider_drag`, focus loss, the About boundary) clears both bools.
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

    /// The resize cursor for the pane divider under the pointer, if any: the hover half of
    /// the divider affordance, the same two probes [`Self::begin_divider_drag`] arms with
    /// (`pane::PaneTree::divider_at` for the terminal projection, the canonical plan's
    /// `divider_at` otherwise), without arming.
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

    /// Resolve a native editor source-body hit through the exact compiled node
    /// and geometry used for paint. The byte is `None` only when the pointer is
    /// in an as-yet-unmaterialized visible row; the I-beam still belongs to the
    /// editor in that case.
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

    /// Apply one source-addressable pointer position to the active editor view.
    /// Document bytes stay in `DocumentStore`; this mutates only the view-local
    /// primary selection and viewport anchor.
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
        // The pointer reaches the buffer through amacs's input reducer, the
        // one path the keys take too.
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
        // One park/settle bracket around the whole routing: a branch that returns before the
        // grid's hover resolution never republishes the caption, which is exactly "nothing
        // under the pointer a click could open".
        let parked = self.park_link_target(wid);
        self.route_cursor_moved(wid, x, y, parked);
        self.settle_link_target(wid, parked);
    }

    /// The pointer-motion routing itself: chrome, modals, native views, drags,
    /// then the grid. See [`Self::on_cursor_moved`] for the caption bracket that
    /// wraps it.
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
        // The same pixel again: macOS synthesises a `CursorMoved` before every wheel event
        // (winit's `scrollWheel:` calls `mouse_motion` first), so a trackpad glide is a stream
        // of stationary events, and under tracking the seam would lock to report motion that
        // did not happen. An unmoved pointer with no gesture in flight stops here and hands the
        // parked caption back; the frame-time refresh owns re-resolution.
        //
        // Only while nothing has changed the answer: the last decision reached the grid
        // (`hover_grid_owned`) and no modal, tab menu or native view has taken the pointer
        // since; no held button, selection or divider drag; the pointer has not left
        // (`pointer_position_known`); and the mapping is unchanged (`hover_mapping`, which the
        // keyboard can move without a pointer event). Cheap terms first.
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
        // Remember the raw pixel position so a follow-up button press can tell
        // whether it landed in the tab strip (intercepted before cell mapping).
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_cursor_px = (x, y);
            // Written here, beside the coordinate it qualifies, so the flag and the
            // position it vouches for cannot be set by two different events.
            ws.pointer_position_known = true;
            // The grid's claim on the pointer is withdrawn until the ladder below reaches the
            // grid arm: every earlier return hands the pointer to another owner, for which
            // the frame-time refresh must not resolve (`hover_grid_owned`).
            ws.hover_grid_owned = false;
            // This motion resolves a native view's hover itself, on the frame
            // on glass ([`Self::refresh_native_hover_for_frame`]).
            ws.native_rehover = false;
        }
        // This window's own frame (`crate::client_frame`): a bare-band press becoming a
        // move, and the resize handles at the window's edges, ahead of every surface
        // drawn inside it.
        if self.route_frame_motion(wid, x, y) {
            return;
        }
        // Derive the pointer geometry once per event (each derivation re-parses a variable
        // primary face). Nothing before its last use can move it: only `ws.metrics` and the
        // backend's face carry it, and the one handler-reachable mutation (`drag_divider` →
        // `resize_panes`) returns right after.
        let geom = self.pointer_geometry(wid);
        self.track_strip_hover(wid, geom, x, y);
        self.track_band_hover(wid, x, y);
        let over_claude_chips = self.track_claude_light_hover(wid, x, y);
        if self.palette_claims_pointer(wid) {
            self.palette_pointer_motion(wid, x, y);
            return;
        }
        // While the tab context menu is up it owns the pointer: motion only moves its
        // highlight (change-gated, cleared when the pointer leaves the card) and stops; no
        // grid hover, selection drag or PTY report. After the palette, as in
        // `on_mouse_input`: the palette is the more modal.
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.tab_menu.is_some())
        {
            // The cell caches still refresh: the first click after a keyboard dismiss
            // (Esc/↵, no intervening motion) must not act on the pre-open cell.
            self.refresh_mouse_cell(wid, geom, x, y);
            self.track_tab_menu_hover(wid, x, y);
            return;
        }
        // The Autopilot menu owns the motion the same way while it is open
        // here: its highlight follows the pointer, nothing under it hovers.
        if self.autopilot_menu.as_ref().is_some_and(|m| m.wid == wid) {
            self.refresh_mouse_cell(wid, geom, x, y);
            self.autopilot_menu_motion(wid, x, y);
            return;
        }
        if self.overview_claims_pointer(wid) {
            self.overview_pointer_motion(wid, x, y);
            return;
        }
        // The stats surfaces (the Deck's band, a sill instrument, an open lens)
        // own the motion over them: it arms the lens's rest and never reaches a
        // pane, so a mouse-reporting program sees no motion there. A drag that
        // began in the grid keeps going (a selection crossing the sill still
        // selects).
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
        // Native tabs own the full content region below host tab chrome. Track
        // hover in the view-local controller so the same typed tree drives the
        // visual wash, pointer cursor, semantics, and later press activation;
        // never leak motion into the parked PTY beneath the app.
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
            // Over a native front the band is a hand where a press acts, as over a terminal
            // (`press_band_at` in the native press path); `track_band_hover` lit its chip.
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
            // Over a terminal pane beside the page (or during a gesture its press went
            // to), the motion is that terminal's report: on to the grid ladder, whose
            // `pointer_route` sends it there. Not while a press the page took is held
            // (`page_buttons`): that drag is the page's wherever the pointer goes, as a
            // selection drag is its pane's.
            if !self.pointer_routes_to_a_terminal_pane(wid) {
                return;
            }
        }
        // While the pointer is over the tab strip, it is not over the terminal grid:
        // show the default cursor and do not report a mouse-move to any pane's app
        // (the strip is GUI chrome). A no-op when the strip is disabled.
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
            // A selection drag held up into the strip is still past the grid's top edge, the
            // one strip motion that continues to the grid seam: `refresh_mouse_cell` clamps
            // it to the focused pane's top row at the current strip column, autoscroll runs,
            // and the MouseMove arm grows the endpoint on this same event, consumed locally
            // before PTY egress. A plain strip hover returns here.
            if !self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
                return;
            }
        }
        // The window cell is cached by the helper; the seam below consumes pane-local.
        let mapped_cell = self.refresh_mouse_cell(wid, geom, x, y);
        let (lr, lc) = mapped_cell.pane_cell;
        // While a divider is held, motion resizes the split and skips the selection and
        // report paths: the drag is chrome, not terminal input.
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.divider_drag.is_some())
        {
            // The divider's boundary hysteresis reads how far into its cell the
            // pointer is, so the drag records the sub-cell offset too — the
            // offset into the clamped cell it reads, so a pointer past the grid's
            // edge (or up in the chrome) is not read as partway into that cell.
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
        // The overlay scrollbar (`crate::pane_chrome`): a thumb drag in flight
        // scrolls its pane, and a bar under the pointer widens. Neither is the
        // grid's: no hover resolution, selection drag or motion report.
        if !self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.selecting || ws.held_mouse_button.is_some())
            && self.scrollbar_pointer_motion(wid, x, y)
        {
            self.set_hover_cursor(wid, CursorIcon::Default, false, false);
            return;
        }
        // A gesture begun in content retains its capture while crossing the
        // header; plain header hover never becomes a terminal mouse report.
        if over_pane_header
            && !self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.selecting || ws.held_mouse_button.is_some())
        {
            self.set_hover_cursor(wid, CursorIcon::Pointer, true, false);
            return;
        }
        // A Claude Code chip, the switch or a fact (`crate::claude_lights`) owns the motion
        // over it, and the chips the blank between two: no grid hover, selection drag or PTY
        // motion report beneath either. Over a target, where a press acts, a hand and its
        // title on the row; over the blank, where a press does nothing, the arrow.
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
        // The message band and the presence row are chrome, like the strip, the header and the
        // Claude Code chips: a plain hover has lit its band chip (`track_band_hover` above),
        // takes the band's cursor (the hand where a press acts, else the arrow) and never
        // becomes a terminal mouse report (a band pixel clamps to grid row 0). A gesture begun
        // in content keeps its capture across it.
        if self.band_hit_at(wid, x, y).is_some()
            && !self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.selecting || ws.held_mouse_button.is_some())
        {
            self.update_hover_cursor_with(wid, geom, parked, None);
            return;
        }
        // The pet brain senses the pointer only on a tick, and with the frame lane released
        // motion alone produces none: ask for a frame on the edge `pet_wake_wanted` defines.
        // A request per event would be a full frame attempt at device rate (90-125/s on a
        // trackpad). Below the chrome and modal returns on purpose: a pointer over the strip
        // or a card is no toy (`pet_pointer_cell` is None off the grid).
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
        // From here the pointer is the grid's, and the frame-time refresh may continue this
        // resolution when the frame under a still pointer changes. Set just before the
        // resolution, with the mapping stamp from the same geometry and pane rect the cell
        // walk used, so the premise and the cell it vouches for share one layout.
        let mapping = self.hover_mapping_with(wid, geom, mapped_cell.pane_rect);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.hover_grid_owned = true;
            ws.hover_mapping = mapping;
        }
        self.update_hover_cursor_with(wid, geom, parked, None);
        // Which half of the cell the pointer is in (the right half includes the hovered
        // cell), remembered so a shift-click, which has no pixel of its own, anchors by the
        // half pressed. `pointer_subcell_px` strips the leading remainder bands and then the
        // pad, matching `pixel_to_cell`.
        let cw = geom.cell.0.max(1);
        let px_off = self.pointer_subcell_px(wid, geom, x, y);
        let side = if usize::from(px_off.x) * 2 >= cw {
            SelectionSide::Right
        } else {
            SelectionSide::Left
        };
        // The cell half (`side`) needs the pixel x, so it is derived here and handed to the
        // seam as data. The seam runs the local drag and the tracking-on motion report under
        // one mode read and keeps `last_mouse_cell`/`last_mouse_side` for both sources.
        let mods = self.mouse_modifiers(wid);
        // The X10 button code of the held button (the lowest held, `held_button`),
        // or `3` (no button held) for a true hover. `encode_mouse_motion` ORs in
        // the 32 motion bit, so a drag in 1002/1003 reports the held button
        // correctly and a button-less hover still reports 3 (which 1002 drops, as
        // it should).
        let buttons = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.held_mouse_button)
            .map_or(3u8, |button| button.code());
        // Remember the sub-cell offset so a follow-up button press / wheel notch
        // (winit delivers no pixel position on those) reports the same pixel the
        // pointer last hovered, under DEC 1016.
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_mouse_px_off = px_off;
        }
        // A selection drag past the grid's top or bottom edge scrolls the scrollback, before
        // the MouseMove drag below so `drag_selection` (mapping through the updated
        // `display_offset`) grows to the revealed edge row. `row`/`col` are already clamped
        // by `pixel_to_cell`, so the edge row is 0 or rows-1.
        self.selection_autoscroll_with(wid, geom, y, mapped_cell.pane_rect);
        // A plain hover with tracking off (the shell-prompt default) produces nothing
        // downstream, and the seam's MouseMove arm would take a second terminal-mutex
        // acquisition just to answer `TrackingOff`. Read the mode from the session's
        // lock-free mirror and stop here, keeping the arm's one remaining effect
        // (`last_mouse_side`); a byte-silent hover keeps the pending key's licence. The seam
        // runs for motion with work: a tracking app's report or a live selection drag.
        //
        // The motion goes where `pointer_route` says: a held button's drag to the session
        // its press went to (a tab or pane switch mid-drag hands the new front nothing), the
        // front's selection drag to the front, a bare hover to the pane under the pointer,
        // whose mirror the tracking read uses.
        let route = self.pointer_route(wid, Some(mapped_cell.pane_rect));
        let (session, (row, col)) = match route {
            // Pane-local cells clamped to the focused pane's grid: the seam drives
            // `drag_selection` and reports to the PTY in that grid, not in window
            // coordinates, and the clamp keeps a divider-crossing drag inside the pane.
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

    /// Mid-drag: grow the selection to the hovered viewport cell — by cell for
    /// simple/block drags, by whole words/lines when the drag began as a
    /// double/triple click (the gesture origin stays fully selected whichever
    /// direction the drag goes).
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
            // A drag can produce a selection, not only extend one: every arm below re-issues
            // `start_selection` (unconditionally `InProgress`) on each move, so a drag turns
            // `has_selection()` back on if something cleared it mid-gesture. `Some(g)` guards
            // a live gesture, never a live selection.
            let selection_before = term.text_selection().has_selection();
            let sel_row = i32::from(row) - term.grid().display_offset() as i32;
            // The hovered cell as a logical anchor (BiDi: the painted column
            // is not the logical one; see `pointer_anchor`). Everything below,
            // the press-cell test after the lock included, is logical.
            let (col, side) = pointer_anchor(&term, row, col, side);
            // Re-derive the gesture origin from the live selection each move, not from
            // `GestureOrigin.row`: that row is captured at press time and never compensated,
            // while the engine's anchors ride `adjust_for_scroll` on every parser batch, so
            // once output scrolls the grid mid-drag the captured row names the wrong unit.
            // `start()` is the origin anchor (every arm below re-anchors with
            // `start_selection(origin)` then `update_selection(moving end)`); the captured
            // row is the fallback when no live selection remains. Read as a cell, not a row:
            // a logical unit can straddle a soft wrap, and `logical_line_span` / `word_span`
            // at that cell answer the whole span from either end.
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
                // Triple-click drag: whole logical lines from the origin line to
                // the hovered line. Rebuilt from the origin each move so the
                // anchor sides stay inclusive in either drag direction, and both
                // ends snap to a logical line so a soft-wrapped row is never
                // half-selected.
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
                // Double-click drag: snap the moving end to the hovered word
                // (or bare cell on whitespace); the origin word stays fully
                // selected by anchoring at its far boundary.
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
            // Record only the 0 -> 1 edge. A drag that merely widens a selection
            // already alive changes no projected variable, so filing `UserSelect` on
            // every pointer move would bury the answer the `custody` verb exists to
            // give under motion noise.
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

    /// While a left-drag selection is in flight, autoscroll the scrollback when the pointer
    /// is dragged past the viewport's top or bottom edge, so the selection extends into
    /// off-screen content. Returns whether the viewport moved; a no-op unless `selecting`.
    /// The line count and direction come from the pure
    /// [`crate::app_render::selection_autoscroll_lines`]; `scroll_display` clamps at the
    /// history ends.
    pub(crate) fn selection_autoscroll(&mut self, wid: WindowId, y: f64) -> bool {
        let geom = self.pointer_geometry(wid);
        let pane_rect = self.focused_pane_rect(wid);
        self.selection_autoscroll_with(wid, geom, y, pane_rect)
    }

    /// [`Self::selection_autoscroll`] against geometry and, on the motion hot path,
    /// the focused pane rect the cell-mapping step already derived.
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
        // Not dragging a selection → disarm the repeat timer and bail.
        if !selecting {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.next_autoscroll = None;
            }
            return false;
        }
        let ch = geom.cell.1.max(1);
        let (pane_row, _, pane_rows, _) = pane_rect;
        // The chrome headroom stacks above the pad, then the focused pane's row offset.
        // Autoscroll belongs to that pane's viewport: in a vertical split, crossing the top
        // pane's bottom or the bottom pane's top is past an edge inside the window.
        let pane_top = geom
            .pad_top
            .saturating_add(geom.head)
            .saturating_add(usize::from(pane_row).saturating_mul(ch));
        let strip_px = usize::from(self.chrome_rows(wid)) * ch;
        // Window → frame first, on the signed seam: a drag past the top edge lives in the
        // negative half of frame space (`selection_autoscroll_lines(-1.0, ..) == 1`), which
        // the hit-testing clamp cannot express.
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
            // `Terminal::scroll_display` records custody itself: `UserScroll` on a rise,
            // `UserScrollTowardLive` / `SnapToLive` dragging back toward live.
            term.scroll_display(lines);
            term.grid().display_offset() != before
        };
        // Held past an edge: arm the repeat so the scroll continues even with the
        // pointer still (`about_to_wait` folds this deadline, `new_events` re-fires it).
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.next_autoscroll = Some(Instant::now() + crate::SELECTION_AUTOSCROLL_INTERVAL);
        }
        if moved && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.clone()) {
            w.request_redraw();
        }
        moved
    }

    /// Repeat-timer tick (`new_events`): while a selection drag is held past a grid
    /// edge, scroll one step and grow the selection to the freshly-revealed edge cell —
    /// even though the pointer has not moved. Re-uses the held pointer's last pixel-y
    /// and cell; `selection_autoscroll` re-arms `next_autoscroll` while still past the
    /// edge and disarms it once the pointer re-enters the grid.
    pub(crate) fn tick_selection_autoscroll(&mut self, wid: WindowId) {
        let Some((last_y, (lr, lc))) = self
            .windows
            .get(&wid)
            .map(|ws| (ws.last_cursor_px.1, ws.last_mouse_cell))
        else {
            return;
        };
        if self.selection_autoscroll(wid, last_y) {
            // Map the held pointer cell through the new display_offset so the selection
            // grows to the row just scrolled into view (same as the CursorMoved path).
            self.drag_selection(wid, lr, lc);
            if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.clone()) {
                w.request_redraw();
            }
        }
    }

    /// Shift-click: extend the existing selection to the pressed cell
    /// ([`Self::extend_selection_to`]) and reset the click streak (this press is not part of
    /// a double-click). Human-only: keyed on `self.mods`, which a controller never sets (its
    /// analogue is the `select extend` verb).
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

    /// Advance the `multi_click_ms` streak and return the resulting click count (1, 2, 3; a
    /// fourth rapid click wraps to 1). The human handler owns this state (`last_press`,
    /// `click_count`); a controller passes an authoritative count without touching it. The
    /// dispatch on the count lives in the seam (`seam_left_press`), shared by both sources.
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

    /// Shift-click: extend an existing non-empty selection so the pressed cell
    /// becomes its new endpoint (`side`: the half of the character the pointer
    /// is over), then complete it again. Returns false (no-op) when there is
    /// nothing to extend.
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
        // The pressed cell as a logical anchor (BiDi; see `pointer_anchor`):
        // `col` and `side` arrive in painted columns, a doubled line already
        // halved (`pointer_char`).
        let (col, side) = pointer_anchor_at_sel_row(&term, sel_row, col, side);
        let sel = term.text_selection_mut();
        if !sel.has_selection() || sel.is_empty() {
            return false;
        }
        sel.extend_selection(sel_row, col, side);
        sel.complete_selection();
        true
    }

    /// Double-click: word-select the pressed cell (builtin smart rules: URLs, paths, words;
    /// just the cell on whitespace), completed immediately, and arm the gesture so a drag
    /// before release extends by whole words. The word is the one on the logical line: a
    /// word straddling a soft wrap is selected whole, across both rows.
    pub(crate) fn select_word_click(&mut self, wid: WindowId, sel_row: i32, col: u16) {
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return;
        };
        let (start, end, col) = {
            let mut term = term_lock(&term);
            // The word under the pointer is found in logical order (BiDi; see
            // `pointer_anchor`), and the press cell is kept logical for the drag.
            let (col, _) = pointer_anchor_at_sel_row(&term, sel_row, col, SelectionSide::Left);
            let (start, end) = control::select_word(&mut term, sel_row, col);
            // The double-click press makes the highlight, so it records custody now; the
            // release (`sel_dragged` pre-set by `arm_gesture_drag`) records `UserSelect` only
            // at the gesture's end.
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

    /// Triple-click: select the full logical line under the press, completed immediately,
    /// and arm the gesture so a drag extends by whole lines. A soft-wrapped line is selected
    /// from its first physical row through its last, so the copy matches the edge-to-edge
    /// highlight; a hard newline ends it.
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
            // As for the double-click: the press that made the highlight records it.
            note_selection_custody(&mut term, true);
            // The press cell in the logical columns the drag compares against.
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

    /// Keep a completed double/triple-click selection draggable while the
    /// button stays down: `sel_dragged` is pre-set so the release completes
    /// the selection instead of treating it as a deselecting plain click.
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

    /// Single press with mouse tracking off: start a text selection of `kind` (`Simple`, or
    /// `Block` for alt-drag) at the cell under the pointer, in live-screen selection
    /// coordinates (viewport row minus `display_offset`, so a scrolled-back press lands in
    /// scrollback).
    ///
    /// A selection exists from here (`start_selection` leaves `InProgress`, which the
    /// conformance's `has_selection()` projection counts), so custody is stamped here as
    /// well as on release; `finish_selection` re-stamps the same `UserSelect`, idempotent in
    /// the model.
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
            // The press anchors before the painted cell under it: its logical
            // start, or on a right-to-left cell its logical end (`pointer_cell`).
            let (col, side) = pointer_cell(&term, row, col);
            term.text_selection_mut()
                .start_selection(sel_row, col, side, kind);
            // Inside the same guard as the mutation, so no PTY-reader output can
            // land between the selection appearing and the record naming it.
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

    /// Left release ending a drag: complete the selection, unless the pointer never left the
    /// press cell (a plain click deselects).
    ///
    /// With `copy_on_select` on ([`crate::app_config::Config::copy_on_select_or_default`]: on
    /// except on Linux, where a selection owns PRIMARY) a completed selection is copied to the
    /// clipboard, leaving the highlight. Returns whether that fired, so the condition is
    /// testable without the clipboard. `suppress_copy_on_select` is the exfil fence the
    /// control layer stamps on a scoped-edge (non-Owner) `mouse` gesture: no clipboard copy
    /// and no PRIMARY auto-own, though the selection still completes.
    ///
    /// The release carries four `SelectionCustody` actions: `SelectLow`, `SelectOldest` and
    /// `SelectHigh` on the `complete_selection()` arm (the model splits by where the interval
    /// landed to reach the eviction and damage branches; the code has one arm), and
    /// `UserClear` on the `clear()` arm. `ws.sel_dragged`, read here and nowhere below, tells
    /// them apart. Tier-1 drives the real gesture in [`crate::selection_custody_conformance`].
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
            // Only `ws.sel_dragged` separates `UserSelect` from `UserClear`; nothing in
            // `TextSelection` carries it, so the record is stamped here, under the mutation's
            // guard.
            note_selection_custody(&mut term, completed);
        }
        ws.selecting = false;
        ws.gesture = None;
        // Button released: stop any selection autoscroll repeat immediately (→ pure
        // `Wait`) rather than waiting for the next tick to notice `!selecting`.
        ws.next_autoscroll = None;
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
        // A completed drag auto-copies when opted in, after the borrow above ends (it
        // re-locks the term); never for a deselecting click or a fenced scoped-edge gesture.
        // Window-routed (`copy_selection_in(wid)`), so CLIPBOARD and PRIMARY are written
        // from the same `term`.
        let fired = completed && self.copy_on_select && !suppress_copy_on_select;
        if fired {
            self.copy_selection_in(wid);
        }
        // X11: a completed selection always owns PRIMARY (independent of copy-on-select), so
        // middle-click paste works in other apps without clobbering CLIPBOARD. Linux only,
        // like `primary_set`: elsewhere the lock-held `selection_to_string` would feed a
        // no-op, a multi-ms stall on a large scrollback selection. PRIMARY is an exfil
        // channel too, so the scoped-edge fence skips it.
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

    /// The URL a press at the pointer would open, if any: an (authorized) OSC 8
    /// hyperlink on the cell wins; else a plain-text `http(s)://` URL detected
    /// in the row. `None` wherever a press does not reach the grid at all.
    #[cfg(test)]
    pub(crate) fn link_under_pointer(&self, wid: WindowId) -> Option<String> {
        self.sourced_link_under_pointer(wid).map(|(url, _)| url)
    }

    /// The URL a press at the pointer would open, and whether it is the cell's OSC 8 hyperlink
    /// (`true`) rather than a plain-text URL run ([`PressedUrl`]).
    fn sourced_link_under_pointer(&self, wid: WindowId) -> Option<(String, bool)> {
        let ws = self.windows.get(&wid)?;
        let (row, col) = ws.last_mouse_cell;
        let window_row = usize::from(ws.last_mouse_window_cell.0);
        // A chrome band covers the row it took: a press lands on what is painted there, never
        // the grid cell beneath. The hover asks the same register, so the hand, the caption
        // and the click agree.
        if self.chrome_owns_terminal_row(wid, window_row) {
            return None;
        }
        // The harness banner over Claude Code's update notice: its link on the
        // banner's words, none on what it hid — never the hidden text's.
        let (px, py) = ws.last_cursor_px;
        if let Some((_, url)) = self.claude_notice_at(wid, px, py) {
            return url.map(|url| (url.to_owned(), false));
        }
        let term = term_lock(&ws.front_terminal()?.term);
        // Viewport rows for both probes: `render_row` resolves through `display_offset`, so
        // a screen-keyed OSC 8 lookup would name a scrolled-back row's neighbour while the
        // plain-text arm names the row under the pointer. Both read the logical cell under
        // the painted one, as the hover does (`probe_hover_cell`).
        let col = pointer_cell(&term, row, col).0;
        term.hyperlink_at_visible(row, col)
            .map(|url| (url.to_string(), true))
            .or_else(|| {
                let cells = term.render_row(row as usize);
                plain_url_at(&cells, col as usize, self.render_knobs.minimum_contrast)
                    .map(|plain| (plain.url, false))
            })
    }

    /// The file citation a press at the pointer would open (`crate::path_link`): the
    /// plain-text `path[:line[:col]]` or `path#L12` in the row under the pointer,
    /// resolved against the session's own directories to an existing regular file.
    /// The same gates as [`Self::sourced_link_under_pointer`]; the row and the OSC 7 directory
    /// are read under one hold, and the filesystem only after it is let go.
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
        // The harness banner hides the update notice's text: no citation in it.
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
        // A look still out opens nothing: the press is not held behind a slow mount.
        self.resolve_citation(session, probe).flatten()
    }

    /// The status sweep's naming of `session`'s group in front, as its timeline
    /// holds it now: `(program_pgid, program)`.
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

    /// Re-ask every standing hover over `session` whose citation was answered under
    /// another naming of the group in front ([`HoverMemo::renamed`]): the status
    /// sweep calls it once it has named the group anew, so a caption refused while a
    /// build held the front (or while the shell's name was not resolved yet) comes
    /// back under a still pointer on an idle screen, and the first press under it
    /// opens. The re-ask is the motion path's (`update_hover_cursor`), so a moved
    /// answer repaints.
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

    /// Resolve a citation read off `session`'s grid against that session's own
    /// directories, in order: the foreground program's OS working directory (its
    /// `agent_cwd`, the directory an agent shortened its citation against), then the
    /// shell's OSC 7 report while it stands. The filesystem is looked at off this
    /// thread ([`crate::path_link::Looks`]): `None` while that look is not back,
    /// `Some(None)` for anything [`crate::path_link::resolve`] refuses.
    ///
    /// Only while the session is known to run HERE, read now, before any directory
    /// is: `Some(None)` otherwise (`crate::path_link`'s module doc). The group in front
    /// is read afresh (`tcgetpgrp`, through the factory's probe), and the status
    /// sweep's name for it is believed only for that very group and only when it
    /// names a program that runs on this machine ([`crate::path_link::runs_here`]): a
    /// group it has not named yet, an `ssh`, a `tmux`, a `sudo`, a shell that shares
    /// its group with what it runs (`SessionFactory::lone_leader`, asked now) is no
    /// citation. The shell's running command must not be a remote shell, and the
    /// shell's OSC 7, while it stands ([`crate::path_link::report_stands`]), must name
    /// this machine. Where no group can be read at all (a ConPTY), the shell's own
    /// integration stands in ([`shell_reads_local`]).
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
        // The timeline's naming, copied out under its leaf lock; the kernel is
        // asked of the group's leader only after it is let go.
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
        // The person's home folder, `USERPROFILE` on Windows, which sets no
        // `HOME`: there `~/…` citations resolved nothing and no caption was
        // shortened to `~`.
        let home = aterm_types::dirs::home_dir();
        self.citation_looks
            .target(&probe.span, &bases, home.as_deref())
            .map(Result::ok)
    }

    /// Cmd-click: open what is under the pointer and report `true`, through the one
    /// decision [`crate::path_link::decide`]. A URL (OSC 8 or plain text) opens via the
    /// OS only past the `is_safe_url` allowlist, the security boundary that keeps a
    /// hostile program's link from making `open` launch an app or touch the filesystem.
    /// With no URL there, a plain-text file citation opens aterm's OWN editor tab at its
    /// line ([`App::open_document_tab_at`]), never the OS.
    ///
    /// The press resolves what is under the pointer afresh, so it can name something
    /// other than what the caption drawn for that cell named (the session's directory
    /// moved under a still pointer, its program changed directory, a look came back
    /// after the frame, the program rewrote the cell's link). Fail closed: it opens
    /// only what [`caption_admits`] lets it, against the caption of the last
    /// accepted application present for the cell (`WindowState::shown_caption`),
    /// never a capture or a frame whose present was pending or refused. A citation
    /// opens only under a caption that named that very file, line and column, and
    /// an OSC 8 link only under one that named its URL (its text need not be its
    /// URL); a plain-text URL, drawn in full, also with none. Otherwise nothing
    /// opens and the caption is asked again, so the band names what the next press
    /// would open.
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

    /// Open a resolved citation in `wid`: aterm's editor tab on the file, its line
    /// revealed. A refusal from the document host (too large, not UTF-8, …) is logged;
    /// the press was the person's and is consumed either way.
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

    /// `MouseInput` -> when no app is tracking the mouse, left presses run the
    /// selection gestures (drag select, double-click word, triple-click line,
    /// shift-click extend, alt-drag block; a plain left click deselects); when
    /// tracking is on, encode the press/release for the cell under the pointer
    /// and write it to the PTY.
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

    /// A button's release ends its press, whoever consumed the release (as
    /// [`Self::on_mouse_input`] ends a press the native page took, `page_buttons`). The
    /// routing ladder ([`Self::route_mouse_input`]) forgets a reported press where it
    /// delivers the release; every gate that swallows a release instead (a modal card, the
    /// tab or Autopilot menu, a native page) returns before that, and left the button marked
    /// held, so each later motion reported a drag to the press's program and routed to it.
    /// Here the program that got the press is told the button went up, at the cell it last
    /// saw, as a tab switch tells it ([`Self::settle_hidden_button_owners`]), and the button
    /// is forgotten. A no-op for a release the ladder delivered and for a press that never
    /// reported.
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

    /// Write `button`'s release to the program its press went to (`owner`), at the cell last
    /// sent to it, under that session's own lock (`write_ui_report`). No modifiers.
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

    /// The innermost node of the native view under window `wid`'s pointer —
    /// the target of a wheel there ([`crate::native_app::AppEvent::ScrollAt`]).
    /// `None` where the pointer's place is unknown or over no native view.
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

    /// Whether a motion or wheel event in a tab a native page fronts belongs to a terminal
    /// pane beside the page: the pane under the pointer, or the pane a held button's press
    /// went to (`pointer_route`), as between two terminals. Everything else over the page is
    /// the page's, as is any event while no terminal is beside it.
    fn pointer_routes_to_a_terminal_pane(&self, wid: WindowId) -> bool {
        matches!(self.pointer_route(wid, None), PointerRoute::Pane { .. })
    }

    /// Whether a new press in a tab a native page fronts lands on a terminal pane beside the
    /// page: the pane under the pointer only ([`Self::pane_under_pointer`]). A button held on
    /// that terminal keeps its own drag and release, but does not take a press made over the
    /// page.
    fn press_lands_on_a_terminal_pane(&self, wid: WindowId) -> bool {
        matches!(
            self.pane_under_pointer(wid, None),
            PointerRoute::Pane { .. }
        )
    }

    /// [`Self::on_mouse_input`]'s routing ladder: modals, chrome, native pages, then the
    /// terminal's selection and report paths.
    fn route_mouse_input(&mut self, wid: WindowId, state: ElementState, button: WinitMouseButton) {
        self.clear_move_license(wid);
        // The GUI-only prefix (link open, shift-extend, the `multi_click_ms` streak that
        // yields `click_count`) stays in this handler; the seam takes `click_count` and
        // `side` as data.
        let pressed = state == ElementState::Pressed;
        // This window's own frame (`crate::client_frame`) is the window's edge, outside
        // every surface drawn in it, modals included: a left press on a resize handle
        // starts the compositor's interactive resize and goes no further (its release,
        // if one reaches the window at all, finds nothing held), and a left release
        // disarms a bare-band move that never moved.
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
        // A press anywhere lets a keyboard-selected Claude Code chip go: the
        // person has moved on, and a chip left selected would swallow their
        // next Return (`crate::claude_lights`).
        if pressed {
            self.clear_claude_light_selection(wid);
        }
        // A button press is a wait on the UI thread: arm the reader's hint before the
        // acquisitions below. Releases and motion do not arm it; arming on motion would
        // double the reader's lock round-trips during any mouse movement.
        if pressed {
            crate::input_hint::arm();
        }
        // Lifting the left button ends drag-to-reorder wherever the pointer is, above every
        // gate below: a release the palette or tab menu swallows must still disarm the chip.
        // Each step already committed through `move_tab`, so there is no drop to settle.
        if !pressed
            && button == WinitMouseButton::Left
            && let Some(ws) = self.windows.get_mut(&wid)
        {
            ws.strip_drag = None;
        }
        // …and ends a scrollbar thumb drag, which owns its release wherever the
        // pointer has gone (`crate::pane_chrome`).
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
        // While the tab context menu is up it is modal over the pointer; every button,
        // press and release, dies here:
        //   * a press on a live action row runs it and dismisses;
        //   * a press elsewhere on the card (border, header, separator, greyed row) is
        //     swallowed and the card stays;
        //   * a press off the card dismisses it and is consumed: the gesture that closes a
        //     menu never also acts on what is underneath (the Windows/AppKit rule).
        // Only the press is examined; its release sets no `reported_buttons` bit, so a
        // tracking app sees neither half.
        // THE AUTOPILOT MENU (`crate::autopilot_menu`, design §9.3.3): the same
        // modal boundary — a left press on a choice chooses it, a press on the
        // card is swallowed, a press off it closes it and is spent on that.
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
        // …and the double-click's second press on the card a choice just
        // closed is spent there too (`App::spends_menu_follow_up`).
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
                // A swallowed release still settles a drag in flight when the menu popped,
                // or a divider/selection drag would keep tracking motion this gate no longer
                // delivers.
                self.settle_pointer_drags(wid);
            }
            return;
        }
        // OVERVIEW: same boundary; a left press and release on the same row
        // activate it as ⏎ does — a chip goes to its tab, a manages line to
        // the manager.
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
        // Subtabs are host chrome for every button, including the focused
        // pane's header. Keep releases paired with gestures begun in content:
        // a selection, divider, native press, or reported terminal button still
        // receives its release after crossing a header.
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
        // The stats surfaces (`crate::stats_deck`, `crate::stats_sill`,
        // `crate::stats_lens`) are chrome: a left press on an instrument pins
        // the lens there, any press on the Deck's band goes nowhere (the grid's
        // clamp would make it a press on row 0), and a press elsewhere closes
        // an open lens and goes on. Their release goes nowhere either, unless
        // a press the grid took is still held (a drag crossing the sill).
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
        // Mixed tabs use the canonical tree before either content-specific
        // boundary. A divider consumes the press; otherwise a press in a sibling
        // focuses that stable view first, so the matching keyboard/pointer router
        // can never target the formerly focused leaf.
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
            // The band is chrome in a mixed tab too: a press on its rows, or on a question
            // banner over them, is taken by `press_band_at` below, never a focus click,
            // divider drag or chip press in the pane the band pixel clamps to.
            let on_band = self.band_banner_at(wid, px, py).is_some()
                || self.band_hit_at(wid, px, py).is_some();
            let in_content = !on_band && self.strip_col_at(wid, px, py).is_none();
            // A press on a Claude Code chip, or the blank between two, in a
            // terminal pane of this mixed tab is theirs, not a focus click, as
            // in a terminal-only tab (`crate::claude_lights`).
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
        // Native tab pointer boundary: canonical tab chrome remains host-owned,
        // while every point below it is hit-tested against the exact semantic
        // tree used for paint and control inspection. Even a miss is swallowed
        // so a click can never reach the parked PTY underneath the app. A
        // terminal pane beside the page keeps its own pointer: a press over it,
        // and the release of a press that went to it, take the terminal path
        // below, routed as between two terminals. A press over the page is the
        // page's even while a button the terminal took is held.
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
                                // A colour wheel: a release inside the disk
                                // carries the hue and saturation under it; the
                                // picker keeps its own brightness.
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
                // The band lies above `native_content_origin_y` like the strip: a press on it
                // over a native-fronted tab is the band's, never the view's.
                if self.press_band_at(wid, px, py) {
                    return;
                }
                // Below the strip on a native view: still a click away from an open in-grid
                // rename, so it commits, as on the terminal path.
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
                // A press on a control's PICTURE in the strip under a list's
                // room, where nothing is live, is the list's: it scrolls that
                // control into the room and works nothing
                // ([`crate::native_ui::CompiledUi::list_picture_at`]).
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
                // THE PRESS GOES WHERE THE POINTER LOOKED: a control the layout
                // moved under a still pointer since it came to rest (a session
                // opened or closed above it, a press moved its row) is not
                // pressed where it, or what the pointer saw, asks for a look. It
                // is lit, and the pointer now rests on it, so the next press
                // takes it.
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
        // The message band is chrome rows: a press performs the row's capsule or primary
        // intent (`App::press_band`) and is consumed. Only the press is swallowed; the
        // orphan-release guard below drops its release, so a tracking app sees a balanced
        // stream.
        if pressed && button == WinitMouseButton::Left {
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
            if self.press_band_at(wid, px, py) {
                return;
            }
            // A left press on a Claude Code chip acts on it, and one on the blank between two
            // is taken and does nothing (`crate::claude_lights`), in any pane: after the
            // modals, before pane focus, so a chip in an unfocused pane is not a focus click.
            // Only the press is swallowed.
            if self.press_claude_light(wid) {
                return;
            }
        }
        // Every other button on the harness's glass — a chip or the blank between two, the
        // switch, a fact — is theirs too and does nothing: never the X11 PRIMARY paste, the
        // right-click copy/paste or the mouse report below, which would hand the pane a press
        // on a cell it does not show. Only the press is swallowed; the orphan guard drops its
        // release.
        if pressed && button != WinitMouseButton::Left {
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
            // A top line's lockup is decoration: its press is the pane's.
            if self
                .claude_glass_at(wid, px, py)
                .is_some_and(|(_, at)| at != crate::claude_lights::GlassAt::Mark)
            {
                // A right press that finds the band's window-menu latch armed means its
                // release was lost (the Windows right-press handling below): this press
                // decides afresh, to do nothing, so its own release must not open the menu.
                #[cfg(windows)]
                if button == WinitMouseButton::Right
                    && let Some(ws) = self.windows.get_mut(&wid)
                {
                    ws.band_menu_release_pending = false;
                }
                // A right press on the Autopilot chip opens its menu
                // (`crate::autopilot_menu`, design §9.3.3), and one on a fact
                // that fact's (`crate::fact_menu`); on any other chip, the
                // switch or the blank between two it does nothing, as before.
                if button == WinitMouseButton::Right && !self.open_autopilot_menu_at(wid, px, py) {
                    self.open_fact_menu_at(wid, px, py);
                }
                return;
            }
            // The in-grid TAB STRIP and the MESSAGE BAND are chrome for every
            // button but Right (whose strip press pops the tab's context menu,
            // `RightPressPlan::Chrome` below). A middle press on a chip closes
            // that tab — the GTK, browser and Windows Terminal convention —
            // through the chip ✕'s own path; any other press there does nothing.
            // Never the X11 PRIMARY paste into the front pane, nor a mouse
            // report at the last grid cell the pointer crossed. Only the press
            // is swallowed; the orphan guard drops its release.
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
        // The update nudge is a strip segment (`TabHit::Update`) or, on macOS, the version
        // menu, so it needs no mouse gate here.
        //
        // A non-left press goes where `pointer_route` says: over an unfocused pane with
        // nothing held, that pane's program decides from its own tracking mode and gets it at
        // its own cell; with a reported button held, the held gesture's owner gets it as the
        // next button of its chord. The left press is the focus click (below).
        let press_route = if pressed && button != WinitMouseButton::Left {
            self.pointer_route(wid, None)
        } else {
            PointerRoute::Front
        };
        // X11 middle-click paste: with no app tracking the mouse, a middle press pastes
        // PRIMARY through `App::paste_primary_into`, the same asynchronous pipeline and
        // pastejacking guard (`deliver_paste`) as Ctrl+Shift+V; a foreign owner's blocking
        // `ConvertSelection` round-trip runs on a worker, never in this handler. Tracking on,
        // the press falls through to the report encoding. The decision and the paste are
        // about the pane under the pointer (`press_route`), focused first
        // (`front_pane_for_local_press`); over a native pane nothing is pasted.
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
        // Right-click copy/paste (the conhost/Windows Terminal convention, `right_click =
        // "copy_paste"`; `RightClickGesture::PLATFORM_DEFAULT` is `off` on macOS and Linux):
        // over the grid with tracking off a right press copies and clears the selection, else
        // pastes through `paste_clipboard_into` → `deliver_paste` (the pastejacking guard);
        // tracking on leaves the button to the seam. The table is [`right_press_plan`]. Only
        // the press is examined: a reported press's release still reports, and a consumed
        // press's release dies at the `was_reported` guard below.
        //
        // Windows: the bare-band right press below arms the system menu without opening it
        // (DefWindowProc refuses menu mode under winit's press capture); the release opens it
        // after winit's `ReleaseCapture` and is consumed. A press that finds the latch still
        // armed means the release was lost: disarm and decide afresh.
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
            // The pane the press is about (`press_route`): the one under the
            // pointer. Its program says whether the button is reported, its
            // selection is what Copy takes, and Paste lands in it, so the
            // decision and the action never name two different panes. `None`
            // over a native pane: nothing to report to, copy or paste into.
            let term = self.route_term(wid, press_route);
            // One lock window for both facts and the selection text: deciding
            // on a cheap "is there a selection" probe and re-extracting later
            // would materialize the selection twice (multi-ms on a huge
            // scrollback sweep, see `finish_selection`'s PRIMARY note).
            let (tracking, selection) = term.as_ref().map_or((false, None), |t| {
                let tl = term_lock(t);
                (
                    tl.mouse_tracking_enabled(),
                    tl.selection_to_string().filter(|s| !s.is_empty()),
                )
            });
            match right_press_plan(gesture_on, over_strip, tracking, selection.is_some()) {
                RightPressPlan::Chrome => {
                    // On a chip, pop that tab's context menu (`session_chrome::compose_tab_menu`);
                    // on the bare band, which is caption (it drags and double-click-maximizes
                    // the window), the window's system menu.
                    //
                    // Windows and Linux only, where the in-grid strip is the tab chrome: macOS
                    // chips carry a real `NSMenu` on the native strip (`toolbar.rs`) from the
                    // same model, and a second menu would be two answers to one gesture. On
                    // Linux this is the mouse route to Copy Session ID, Copy CWD and the identity
                    // card; Menu / Shift+F10 ride the same `TabMenuChord` policy, and the card's
                    // keyboard mode is the same `tab_menu_input_event` state machine.
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
                            // The bare band (or the `+`, `↻`, version label or a window
                            // button, which have no menu of their own): the window's menu,
                            // the band being caption.
                            //
                            // Windows: the caption's system menu. The press only arms
                            // `band_menu_release_pending`; the release opens the menu.
                            // winit's win32 backend holds `SetCapture` for the whole press and
                            // `DefWindowProc` will not enter its modal menu loop under
                            // capture, so a menu posted on the press opens only for a
                            // machine-speed click.
                            //
                            // Linux, where the band is the window's own header
                            // (`crate::client_frame`): the compositor's window menu at the
                            // pointer, on the press (`show_window_menu` needs this press's
                            // serial). Elsewhere on Linux the window system's frame has
                            // the menu, and the press is swallowed (in fullscreen too,
                            // where there is no header).
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
                        // Clear and repaint even if the clipboard write fails (conhost clears
                        // its QuickEdit selection unconditionally too): the gesture promises
                        // the selection is taken.
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
                // Tracking on, or the gesture off: fall through. The seam reports, or
                // (tracking off) returns `TrackingOff`, whose consumer acts only on Left.
                RightPressPlan::Seam => {}
            }
        }
        // A left press in the tab strip switches, closes or opens a tab and stops there,
        // never reaching selection or pane focus.
        if pressed && button == WinitMouseButton::Left {
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
            if let Some(col) = self.strip_col_at(wid, px, py) {
                self.handle_tab_strip_click(wid, col);
                return;
            }
            // Outside the strip with an in-grid rename open: a click away commits what was
            // typed and still lands in the terminal (as on macOS); a grid press also ends the
            // chip's double-click streak.
            if self.inline_rename_edit(wid).is_some() {
                self.clear_strip_press(wid);
                self.settle_rename_edit(wid);
            }
        }
        // A left press on the find bar's `Aa` / `.*` indicators flips match-case / regex (as
        // ⌥⌘C / ⌥⌘R do) and stops there; a no-op elsewhere or when not searching. The pixel
        // gate ([`Self::find_bar_pixel_hit`]) rejects pad clicks the clamping `pixel_to_cell`
        // would snap onto the bar row.
        if pressed && button == WinitMouseButton::Left {
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
            if self.find_bar_pixel_hit(wid, px, py) {
                let (row, col) = self.pixel_to_cell(wid, px, py);
                if self.find_bar_click_at(wid, row, col, Some(px)) {
                    return;
                }
            }
        }
        // A left press on the pet's drawn body pets it and stops here ([`Self::pet_press_at`]).
        // Below the modals, the strip and the find bar, which cover the pet; above the
        // divider, pane-focus, selection and report layers, which its body occludes.
        if pressed && button == WinitMouseButton::Left && self.pet_press_at(wid) {
            return;
        }
        // A left press on an overlay scrollbar's thumb grabs it; one on the track beside the
        // window edge pages toward the pointer (`crate::pane_chrome`). Before the divider:
        // an inner pane's thumb sits in the left half of the divider's gap.
        if pressed && button == WinitMouseButton::Left && self.scrollbar_press(wid) {
            return;
        }
        // A left press on a split divider grabs it (no focus change, no selection); release
        // ends the drag. Before pane focus, so a press on the gap resizes rather than
        // focusing.
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
        // A left press in a different pane focuses it and stops there (no selection in the
        // old pane). A no-op with one pane.
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
            // A left press starts a LOCAL selection when nothing tracks the mouse OR
            // (audit M6) the bypass modifier is held over a tracking app — Option
            // (iTerm2's gesture) and, off macOS, Shift (xterm's) — so a PID can be
            // copied out of htop. The GUI-only prefix (link-open, shift-extend, the
            // click-streak FSM) runs for BOTH cases.
            if pressed && press_starts_selection(tracking, tracking_bypass_held(mods_state)) {
                // Cmd-click (macOS) / Ctrl-click (elsewhere) on a link opens it (safe
                // schemes only) instead of starting a selection; GUI-only, never the seam.
                if link_modifier_held(mods_state) && self.open_link_under_pointer(wid) {
                    return;
                }
                // Shift-click extends an existing selection (a GUI affordance keyed on
                // `self.mods`) without reaching the seam.
                // Not while Shift is the bypass itself (tracking on, no Option): that
                // press starts a fresh selection, as xterm's does.
                if mods_state.shift_key() && !(tracking && !option_held) {
                    self.shift_extend_press(wid);
                    return;
                }
                // Advance the streak and capture the count for the seam's gesture.
                click_count = self.advance_click_streak(wid);
                // M6 BYPASS: tracking is ON, so the seam would REPORT this press to the
                // app. Run the local selection gesture HERE and return without reporting
                // — press/release stay symmetric for the app. The modifier is spent on
                // the bypass, so the kind is Simple (`press_selection_kind`), never the
                // local Option+drag Block.
                if tracking {
                    let (row, col) = self
                        .windows
                        .get(&wid)
                        .map_or((0, 0), |ws| ws.last_mouse_cell);
                    let block = press_selection_kind(option_held, tracking) == SelectionType::Block;
                    self.seam_left_press(wid, row, col, click_count, block);
                    return;
                }
                // Tracking off: the seam returns `Egress::TrackingOff` and runs the same
                // `seam_left_press`.
            }
            // The release of a local selection settles it here and reports nothing: an
            // Option-bypass press, a selection the app began tracking mid-drag, and the
            // ordinary tracking-off release alike. Not gated on tracking: a bypass press sets
            // no Left bit, so if tracking flips off mid-drag (the app exits, or sends
            // `CSI ?1000l`) the `!was_reported` guard below would return first and leave
            // `selecting` stuck.
            if !pressed && self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
                // A human release: copy-on-select is not suppressed.
                self.finish_selection(wid, false);
                if let Some(ws) = self.windows.get_mut(&wid) {
                    // Clear the Left bit too: this branch returns before the bookkeeping
                    // below, and a stuck bit would let a later non-reporting press leak an
                    // orphan release.
                    forget_reported_button(ws, aterm_types::mouse::MouseButton::Left);
                }
                return;
            }
        }
        let Some(button) = winit_mouse_button(button) else {
            return;
        };
        // Per-button "press was reported" bit, so a release pairs with its own press (a
        // two-button chord under tracking keeps both releases). Keyed on the dense `slot()`,
        // never `code()`: the thumb buttons' codes are 128/129, which `& 7` would alias onto
        // Left and Middle.
        let slot = usize::from(button.slot());
        let bit = 1u8 << (button.slot() & 7);
        let (was_reported, owner) = self.windows.get(&wid).map_or((false, None), |ws| {
            (ws.reported_buttons & bit != 0, ws.button_owners[slot])
        });
        // A press goes to `press_route` (the pane under the pointer when it begins nothing
        // there, else the front); a release to the session its press went to.
        let route = if pressed {
            press_route
        } else {
            self.owner_route(wid, owner)
        };
        // A non-left press with no terminal program under the pointer (a native
        // pane) is no one's: it is never the focused pane's at a clamped cell.
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
        // The sub-cell pixel offset of the last pointer move (a press carries no
        // pixel of its own) so a DEC 1016 press/release lands on the genuine pixel.
        let px_off = self
            .windows
            .get(&wid)
            .map_or(crate::input::PixelOffset::CELL_ORIGIN, |ws| {
                ws.last_mouse_px_off
            });
        // Track the held buttons so a later motion report (tracking on) names one
        // instead of the hover code, and each press's session so its drag and
        // release follow it. Harmless when tracking is off (the motion then takes
        // the local selection path and never reads these).
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
        // A release whose press never reported must not send a tracking app an orphan
        // release (the split-pane focusing press returns early without setting the bit).
        if !pressed && !was_reported {
            return;
        }
        // The block-select intent (held Alt/Option) is snapshotted into event data here, so
        // the seam's selection-type decision is source-blind (a controller sends `block=1`)
        // and a later Alt release cannot change this press's type.
        let block = mods_state.alt_key();
        // The seam reads `mouse_tracking_enabled()` once under one lock and either reports
        // the press/release (tracking on, real `mods`) or runs the local selection gesture
        // (tracking off) by `click_count` at `side`, typed by `block`. Both sources share it.
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
                // A human gesture never suppresses copy-on-select: the exfil fence
                // covers only scoped-edge gestures the `mouse` control verb injects.
                suppress_copy_on_select: false,
                px_off,
            },
        );
    }

    /// Make the pane a local press gesture was decided for the front before it acts there:
    /// right-click copy/paste (`right_click = "copy_paste"`) and X11 middle-click PRIMARY
    /// paste, which a program that does not track the mouse leaves to aterm. An unfocused
    /// pane under the pointer is focused, as a left press focuses it: what a paste starts
    /// is finished at the keyboard, in that pane. `false` with no pane to act on (a native
    /// pane under the pointer, or a focus that would not move).
    fn front_pane_for_local_press(&mut self, wid: WindowId, route: PointerRoute) -> bool {
        match route {
            PointerRoute::Front => true,
            PointerRoute::Pane { session, .. } => {
                self.focus_pane_under_pointer(wid) && self.session_is_front(wid, session)
            }
            PointerRoute::Nowhere => false,
        }
    }

    /// The terminal a pointer route addresses: the front's, or the named pane's.
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

    /// Normalize one winit wheel delta into whole notches: its [`WheelDir`] and how many
    /// reports or lines it is worth, drained from the axis's own per-window residual
    /// (`scroll_residual`, `scroll_residual_x`). `None` while the gesture is sub-notch or
    /// carries no axis. The dominant component picks the axis; horizontal travels as
    /// `WheelDir::Left/Right`, which no grid scrolls but a tracking app (Neovim, tmux)
    /// receives as buttons 66/67.
    pub(crate) fn wheel_notches(
        &mut self,
        wid: WindowId,
        delta: MouseScrollDelta,
    ) -> Option<(WheelDir, i32)> {
        match delta {
            MouseScrollDelta::LineDelta(x, y) => {
                // Strict dominance: a tie, the all-zero event included, carries no axis.
                let axis = wheel_axis(f64::from(x), f64::from(y), 0.0)?;
                let ws = self.windows.get_mut(&wid)?;
                match axis {
                    // Positive x means "the content moves right", i.e. the reader
                    // is walking left (winit's documented sign, and the reason its
                    // Windows backend negates WM_MOUSEHWHEEL's tilt-right).
                    WheelAxis::Horizontal => {
                        // sub-notch motion banks and emits nothing yet
                        let (positive, n) =
                            bank_scroll_lines(&mut ws.scroll_residual_x, f64::from(x))?;
                        Some((horizontal_dir(positive), n))
                    }
                    // Fractional deltas (Windows precision touchpads and free-spinning
                    // wheels emit many |y| << 1 events) bank and emit only whole lines; a
                    // classic ±1 notch still moves exactly one line.
                    WheelAxis::Vertical => {
                        let (up, n) = bank_scroll_lines(&mut ws.scroll_residual, f64::from(y))?;
                        Some((vertical_dir(up), n))
                    }
                }
            }
            MouseScrollDelta::PixelDelta(p) => {
                // The same axis split for trackpad pixel deltas, with an EPSILON floor. The
                // horizontal half is measured in cell widths, a report count in the same
                // unit as the vertical half's cell heights: a column is the horizontal
                // quantum as a row is the vertical one, so a sideways swipe is worth about
                // 2x the reports. `MAX_WHEEL_BURST` at the seam bounds the volume.
                let axis = wheel_axis(p.x, p.y, f64::EPSILON)?;
                let (cw, ch) = self.win_cell_size(wid);
                let (cw, ch) = (cw.max(1) as f64, ch.max(1) as f64);
                let ws = self.windows.get_mut(&wid)?;
                match axis {
                    WheelAxis::Horizontal => {
                        let (positive, n) = bank_scroll_lines(&mut ws.scroll_residual_x, p.x / cw)?;
                        Some((horizontal_dir(positive), n))
                    }
                    // Bank the sub-line fraction and emit only whole lines, so slow trackpad
                    // scrolling moves pixel by pixel instead of snapping a line per event.
                    WheelAxis::Vertical => {
                        let (up, n) = bank_scroll_lines(&mut ws.scroll_residual, p.y / ch)?;
                        Some((vertical_dir(up), n))
                    }
                }
            }
        }
    }

    /// `MouseWheel` -> when an app is tracking the mouse, report wheel up/down at
    /// the cell under the pointer; otherwise scroll the scrollback viewport (the
    /// everyday "scroll up to see history" gesture). The phase-less form: a
    /// `TouchPhase::Moved` event — the tests' entry (production enters through
    /// [`Self::on_mouse_wheel_phased`] from the winit seam).
    #[cfg(test)]
    pub(crate) fn on_mouse_wheel(&mut self, wid: WindowId, delta: MouseScrollDelta) {
        self.on_mouse_wheel_phased(wid, delta, TouchPhase::Moved);
    }

    /// [`Self::on_mouse_wheel`] with winit's gesture phase, which changes nothing about
    /// routing or bytes: a precise gesture's `Ended`/`Cancelled` (the finger lifted)
    /// releases the tracked band so its rest-settle starts now rather than `TRACK_REST_MS`
    /// later. Read after the event is handled, because `Ended` usually carries a zero delta
    /// the axis guard drops.
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
        // A wheel gesture is a human waiting on the UI thread, as a key press is: arm the
        // reader's hint first so its next lock holds are the fine slices, not the 16 KiB
        // idle ones.
        crate::input_hint::arm();
        self.clear_move_license(wid);
        // The modal claim is decided before normalization. Sub-line gestures return below
        // without reaching native or terminal scroll consumers, and so do horizontal ones
        // over the palette or a native view: only the terminal seam answers that axis.
        let palette = self.palette_claims_pointer(wid);
        let overview = self.overview_claims_pointer(wid);
        // A native page scrolls by the wheel only while the pointer is its: over a terminal
        // pane beside it the notch is that terminal's (`pointer_route`, below).
        let native_page =
            self.active_native_view(wid).is_some() && !self.pointer_routes_to_a_terminal_pane(wid);
        // Do not bank fractional wheel deltas from chrome: otherwise a later
        // scroll in content would inherit motion that began over its title.
        if !(palette || overview) && self.pointer_is_over_pane_header(wid) {
            return;
        }
        // Nor from the stats surfaces: a wheel over the Deck, a sill instrument
        // or the lens reaches no pane.
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
        // A deliberate gesture stops setup even before it banks a whole row,
        // or when its horizontal axis scrolls no native list. Gesture-end
        // callbacks with no motion leave an untouched offer running.
        if native_page
            && !(seam_owned || palette || overview)
            && dx.is_finite()
            && dy.is_finite()
            && (dx != 0.0 || dy != 0.0)
        {
            self.interact_system_state(wid);
        }
        // The vertical pixel delta, taken before `wheel_notches` banks it: banking keeps
        // only whole rows and a direction flip forfeits the remainder (right for the seam's
        // bytes, wrong for a finger that reversed by 3 px). `None` for a notch, a horizontal
        // swipe, or no axis.
        let track_px = match delta {
            MouseScrollDelta::PixelDelta(p)
                if wheel_axis(p.x, p.y, f64::EPSILON) == Some(WheelAxis::Vertical) =>
            {
                Some(p.y)
            }
            _ => None,
        };
        let Some((dir, lines)) = self.wheel_notches(wid, delta) else {
            // Sub-notch: the seam's bank holds the fraction for its next whole row, and a
            // precise delta also moves the local band by its pixels now, unless an owner
            // that swallows the whole rows claims the pointer: a modal overlay
            // (`overlay_open`), the tab context menu (`tab_menu_input_event`; not an
            // `Overlay`, so no `*_claims_pointer` names it), a modal card or a native view.
            // Otherwise the band would move under a card whose rows move nothing.
            if let Some(dy_px) = track_px
                && !(seam_owned || palette || overview || native_page)
            {
                // The band belongs to the pane under the pointer, as the whole
                // rows below do (`pointer_route`).
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
        // The palette card and a native tab scroll one list on one axis: a horizontal flick
        // over them is nothing, and must not reach the terminal seam (the PTY under a
        // native tab is parked). Over the grid it continues to the seam, which reports it to
        // a tracking app and yields zero local motion otherwise; like any real gesture it
        // also trips `note_update_handoff_activity`, the `is_present_recovery_stimulus`
        // rearm and `note_alt_scroll`.
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
                // The page moves under the still pointer: its next frame on
                // glass is hovered where the pointer then is.
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.native_rehover = true;
                }
                // Aimed at what the pointer is over, so a page with a second
                // scrolling region (Settings' sidebar) scrolls that one.
                let event = match self.native_scroll_target(wid) {
                    Some(key) => crate::native_app::AppEvent::ScrollAt { lines: signed, key },
                    None => crate::native_app::AppEvent::ScrollLines(signed),
                };
                let _ = self.dispatch_native_event(wid, event);
                return;
            }
        }
        // A wheel over an unfocused pane is that pane's — its program's report at
        // its own cell, or its own scrollback — never the focused pane's at a
        // clamped edge cell; during a held button's gesture it is that gesture
        // owner's; over a native pane it is no terminal's (`pointer_route`).
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
        // The seam decides tracking on (N reports) versus off (scroll the viewport `lines`)
        // under one mode read, the horizontal axis included (reported while an app tracks,
        // no local motion otherwise). Reading the mode here would split the decision across
        // two locks and give the `mouse` verb a different answer than a hand.
        //
        // A precise delta's pixels ride beside the call in `wheel_precise_px`, not in the
        // event: they change only how the local band moves (`scroll_wheel_animated_with`:
        // a notch eases, a precise delta tracks 1:1), never the seam's bytes, so
        // Human/Controller byte equality holds. Cleared after the call.
        //
        // Whether the delta counts detents DOES change bytes (the DEC-1007 arrow count is
        // the platform's lines-per-detent), so that bit rides in the event, where the
        // `mouse` verb's twin sets it too: winit's `LineDelta` is in detents (a wheel
        // notch, an X11 scroll valuator), its `PixelDelta` is a precise device's pixels,
        // banked into rows above.
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

    /// The sub-notch half of direct manipulation: a precise vertical delta that banked no
    /// whole row moves the local band by `dy_px` now. It produces no bytes, so the seam is not
    /// consulted, and the `mouse` verb has no pixel field, so no Controller twin can diverge.
    ///
    /// Tracking on with no bypass modifier (read from the lock-free `ModeMirror`) means an app
    /// owns the wheel: release any tracked band, take no lock. Otherwise a glide of this
    /// engine already armed (`scroll_wheel_animated_with`'s `same` test) was Viewport when
    /// armed, and the delta joins it lock-free; an app that takes the mouse mid-gesture gets
    /// local motion for at most the rest of the row, since the next banked row reaches the
    /// seam and `input_wheel` releases the band. A gesture's first delta takes one short lock
    /// to evaluate [`crate::input::wheel_route_for`] (the mirror has no alt-screen bit to tell
    /// AltScroll from Viewport). Reduced motion has no sub-row band and is checked first.
    fn track_precise_scroll(&mut self, wid: WindowId, dy_px: f64, pane: Option<u64>) {
        use aterm_types::mouse::{ALT_MASK, SHIFT_MASK};
        // The gate the whole-row glide resolves (`effect_policy`: OS Reduce Motion,
        // `motion = "reduced"`, an unfocused window; never the load-shed latch, which sheds
        // decoration, not the reader's own hand).
        if !self.smooth_scroll_animates(wid) {
            return;
        }
        // The pane under the pointer (`pane`), else the front.
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
        // While this session's rewrap has the history out, a sub-row delta moves nothing:
        // the band would land its rows on the reader's aim per event and again at the
        // settle, while the same pixels bank toward the whole rows `input_wheel` applies.
        // The bank alone carries the finger; a band left from before the rewrap settles.
        if self.rewrap_in_flight(session) {
            self.release_scroll_track(wid);
            return;
        }
        let mods = self.mouse_modifiers(wid);
        // Tier 1 honours only the lock-free half of the seam's bypass: Shift reserves the
        // wheel for the terminal on every screen; Alt's bypass is alt-screen-conditional
        // (`wheel_route_for`) and the mirror has no alt-screen bit, so Alt falls through to
        // the locked probe. Releasing here on Alt would drop every sub-row delta over a
        // main-screen tracking app, which the seam routes to the local viewport.
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

    /// Start a tracked band's rest-settle now (`Glide::release`): the gesture ended, or the
    /// seam just reported a precise delta to an app. A no-op for a notch ease, a settle
    /// already in flight, or no glide.
    pub(crate) fn release_scroll_track(&mut self, wid: WindowId) {
        self.release_scroll_track_at(wid, Instant::now());
    }

    /// [`Self::release_scroll_track`] at an explicit `now`. A parked band's anchored tick is
    /// its old rest boundary (the parked arm of `apply_scroll_glide_sample`) and the loop
    /// wakes only on it, so the release pulls the anchor in to one panel period out, as a
    /// chained notch does; otherwise the settle waits for the old boundary and lands in one
    /// frame.
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

    /// Snap the viewport back to the live bottom (called on keyboard input, the
    /// standard "start typing and jump to the prompt" behavior).
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
        // Typing snaps instantly and whole-row: drop any in-flight wheel glide (its eased
        // tail would scroll back away from the prompt), banked sub-row residual and
        // overscroll bounce. Only the front pane's: a glide on another pane keeps running.
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
            // Also at offset 0 when the reader aimed up while a rewrap held the history
            // away: typing is them coming back to the live bottom, not to the old aim.
            let away = term.grid().display_offset() != 0 || term.grid().reader_aim_held();
            // A non-typing snap (⌘-V, a composing IME key) has no press class, so this
            // records `SnapToLive` itself when the view moved. Called at live too: a paste
            // at the prompt is a reader gesture (`Terminal::reader_gesture_count`), which an
            // update's history import reads to leave a used session where it was.
            term.return_to_live();
            away
        };
        if scrolled && let Some(w) = &ws.os_window {
            w.request_redraw();
        }
    }

    /// Edit ▸ Select All: select the visible screen as whole lines (top row to bottom, full
    /// width), as a triple-click dragged top to bottom, and repaint. The rows are
    /// `-off..=rows-1-off` for the live `display_offset`, not `0..rows`: the viewport stays
    /// where the reader is.
    pub(crate) fn select_all(&mut self) {
        // A window-level command (menu Select All): targets the frontmost window.
        let Some(ws) = self.front() else { return };
        let Some(terminal) = ws.front_terminal() else {
            return;
        };
        let last = i32::from(ws.rows.saturating_sub(1));
        let max_col = ws.cols.saturating_sub(1);
        {
            let mut term = term_lock(&terminal.term);
            // No snap to live first: Select All selects what is on screen. Visible rows
            // `0..=last` map through the live `display_offset` to selection rows
            // `-off..=last-off` (negative rows are scrollback), as the mouse path's
            // `sel_row = row - display_offset` does.
            let off = term.grid().display_offset() as i32;
            let sel = term.text_selection_mut();
            sel.start_selection(-off, 0, SelectionSide::Left, SelectionType::Lines);
            sel.update_selection(last - off, max_col, SelectionSide::Right);
            sel.expand_lines(max_col);
            sel.complete_selection();
            // ⌘-A reaches the `TextSelection` primitive directly, with no mouse-down
            // before it and no `finish_selection` after, so it records custody itself.
            note_selection_custody(&mut term, true);
        }
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
    }
}

/// Open an already-allowlisted URL with the OS default handler: macOS `open`, Linux
/// `xdg-open`, Windows `ShellExecuteW`. Shared by the terminal's Cmd-click link open and
/// the About dialog's site link; callers gate through `is_safe_url` first. Best-effort: a
/// failed spawn is a silent no-op. The opener is not waited for: dropping its `spawnfd`
/// child hands it to the reaper, so it never stays a zombie of the window. A test
/// build opens nothing: it notes the URL (`platform::take_opened_urls`).
pub(crate) fn open_url_external(url: &str) {
    #[cfg(test)]
    crate::platform::record_opened_url(url);
    #[cfg(all(not(test), target_os = "macos"))]
    drop(
        aterm_uds::spawnfd::command("/usr/bin/open")
            .arg(url)
            .spawn(),
    );
    // The press's time goes with it, so the browser's window may come forward.
    #[cfg(all(not(test), target_os = "linux"))]
    drop(
        crate::startup_token::for_a_press(aterm_uds::spawnfd::command("xdg-open"))
            .arg(url)
            .spawn(),
    );
    #[cfg(all(not(test), windows))]
    open_url_windows(url);
}

/// Windows arm of the Ctrl-click link open: hand the allowlisted URL to the shell's
/// default handler via `ShellExecuteW`, never `cmd.exe /C start`, whose metacharacter
/// parsing (`&`, `^`, …) would reopen the injection hole `is_safe_url` closes. shell32 is
/// on the approved tiny-FFI list for this use (nothing in kernel32 or std opens a URL by
/// its registered handler). Failure (return <= 32) is a silent no-op, like `xdg-open`.
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
    // `is_safe_url` already rejected control bytes, so the URL cannot embed a
    // NUL that would truncate the wide string.
    let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: both strings are NUL-terminated UTF-16 buffers that outlive the
    // call; the null hwnd/parameters/directory are documented as optional.
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

/// Classify a raw `(x, y)` wheel delta. `floor` is the magnitude below which a component
/// is no motion (0 for `LineDelta`, `f64::EPSILON` for `PixelDelta`). Dominance is strict
/// both ways: a tie (|x| == |y|, the all-zero event included) belongs to neither axis and
/// is dropped, as a 45° drift has no defensible answer.
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

/// A banked horizontal notch's [`WheelDir`]. `positive` is winit's sign, where +x means
/// the content moves right: the reader walks left (xterm's button 6).
fn horizontal_dir(positive: bool) -> WheelDir {
    if positive {
        WheelDir::Left
    } else {
        WheelDir::Right
    }
}

/// A banked vertical notch's [`WheelDir`] (+y = wheel up = older content).
fn vertical_dir(up: bool) -> WheelDir {
    if up { WheelDir::Up } else { WheelDir::Down }
}

/// Bank a signed scroll delta (in lines) into the per-window residual and drain
/// the whole lines: `Some((dir_up, n))` with `n >= 1`, or `None` while the
/// accumulated motion is still sub-line. A direction flip forfeits the old
/// remainder first, so a reversal responds immediately instead of paying off
/// leftover opposite-direction motion.
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

/// Whether one pointer-motion event should ask for a frame so the pet brain can sample
/// the pointer.
///
/// * `pet_visible`: a pet body was drawn last frame; a petless window never pays.
/// * `brain_running`: `CompanionOwner::needs_frames()`. The brain already owns a frame
///   cadence that samples the pointer itself; pursuit and pounce keep it true throughout,
///   so a chase cannot stall here.
/// * `sampled_px`: where the brain last saw the pointer (`None` = never). Fires only after
///   a full cell of travel on either axis: the brain's sensor is in cells.
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

    /// The glow's echo admission as of `now`: a test whose key is pressed at one
    /// instant and judged later must not let a loaded machine age the typed hint
    /// (it lives 250 ms) between the two.
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

    /// Drive the real key, pointer router/input seam, wire, and delayed terminal
    /// echo. DEC1000/1002 are tracking modes with byte-silent buttonless motion;
    /// DEC1003 reports motion and must retire the pending typed licence.
    #[test]
    fn byte_silent_pointer_preserves_the_keys_delayed_echo() {
        let wid = WindowId(0);
        let model = aterm_models_effects::cursor_hint_license_model();
        // Clearing a hint leaves earned echo credits alive, so a delayed +1 may still paint
        // as `inflight`; reported motion must match that.
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
                    // Every freshness check is made as of the key: the typed hint lives
                    // 250 ms, which a loaded machine can overrun before the checks.
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
                    // Only a move to a new cell reports, for either source (xterm reports
                    // motion only to a different character cell), so a move onto the cell
                    // already reported is byte-silent and keeps the licence.
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
                    // Run external conformance after the echo, so verifier time
                    // cannot age the real pending key before its test frame.
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
                        // Negative control: the route clearing the hint unconditionally.
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
                // A controller's held-button motion is a gesture even without
                // a local selection and without a terminal tracking report.
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

    /// THE PRESS GOES WHERE THE POINTER LOOKED: a motion at the pixel the
    /// pointer rests on is no motion and keeps what it saw there; a motion
    /// to another pixel sees afresh. Where the control pressed asks for a
    /// look, or the one the pointer saw does, the press lands where the
    /// pointer saw that very control, where it has moved since, or where no
    /// rest is known (a scroll forgot it), and not where it rests still on
    /// what it saw before the layout moved — another control, the same key in
    /// another view, or nothing. Where neither asks, the press lands as it
    /// always has. CONTROL: the same press after the pointer moves lands.
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
        // A bulk button moved under a pointer resting on a row's.
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
        // A row's button moved under a pointer resting on a bulk one.
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
        // CONTROL: once the pointer moves, it sees what is there now.
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

    /// A HEADLESS POINTER REACHES A NATIVE VIEW, SAYS WHERE, AND LEAVING CLEARS ITS
    /// HOVER (cs-25): a window with no glass never presented a native frame, so the
    /// glass-commit guard refused every pointer pixel and `pointer move` hovered
    /// nothing while answering `at=0,0` (a grid memo the native path never writes);
    /// and `pointer leave` cleared the strip's hover but left a Settings row hovered,
    /// its cursor-style preview showing what the pointer had left. Now the move
    /// commits the frame a capture shows, aims at a control by its key, the reply
    /// names the view, the point and the control, and leaving clears the hover.
    /// NEGATIVE CONTROL: a terminal front answers its window cell as before.
    #[test]
    fn a_glassless_native_pointer_hovers_says_where_and_leaving_clears_it() {
        use super::{NativePointerTarget, PointerAt};
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        // NEGATIVE CONTROL first: the terminal front reads back its cell.
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

    /// A NATIVE PAGE SCROLLED UNDER A STILL POINTER IS HOVERED WHERE THE
    /// POINTER NOW IS (review of audit r14 PV8): hover was resolved on pointer
    /// motion alone, and a wheel moves the page with no motion after it (on
    /// macOS its `CursorMoved` comes first, on the old frame), so the row the
    /// pointer had been over stayed hovered and Cursor Styles went on trying
    /// it, saying "Click to save", while a click there saved the row
    /// scrolled under it. After a wheel over the style list, the hovered
    /// control is the one the pointer is over in the frame the wheel left,
    /// and the preview tries that row's style. CONTROL: before the wheel,
    /// the pointer's row is hovered.
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

    /// A headless App whose 1,000×600 window has Settings open at `route`:
    /// the sidebar beside a list.
    fn settings_at_1000_by_600(
        route: crate::native_settings::SettingsRoute,
    ) -> (crate::App, crate::WindowId, crate::tab_model::ViewId) {
        settings_at_1000_by_600_in(crate::App::headless_for_test(), route)
    }

    /// [`settings_at_1000_by_600`] in `app`: a window with glass (a scripted
    /// surface) as well as without.
    fn settings_at_1000_by_600_in(
        app: crate::App,
        route: crate::native_settings::SettingsRoute,
    ) -> (crate::App, crate::WindowId, crate::tab_model::ViewId) {
        settings_sized_in(app, route, (1_000, 600))
    }

    /// [`settings_at_1000_by_600_in`] in a window of about `width`×`height`
    /// pixels (whole cells of it).
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

    /// A GLASSLESS POINTER'S REPLY READS THE FRAME ITS EVENT LEFT: a click that
    /// acted on a native page changed it after the one commit the verb made,
    /// before the click, and the reply answered `ERR no presented native frame
    /// under the pointer` although the click landed. Where glass would still
    /// show the frame before a change (no commit since), the answer says that
    /// rather than that no native frame is there, and `status` commits and
    /// reads the new one.
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

    /// ON GLASS A POINTER'S REPLY READS THE FRAME ITS EVENT LEFT: a click,
    /// a wheel or a hover that changed a native page leaves the frame on
    /// glass a page behind until the window's next present, and the reply,
    /// read from the presented frame as input is, answered `ERR the page
    /// under the pointer changed` for every one that worked. The verb presents
    /// the frame its event left before it replies, as the window's next
    /// redraw would, so each answer names the control the presented page
    /// holds under the pointer.
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
        // An unchanged page is not presented again for a reply.
        let surface = crate::scripted_redraw_fixture::surface;
        let commits = surface(&mut app).commits();
        let _ = reply(&mut app, PointerAction::Status);
        assert_eq!(surface(&mut app).commits(), commits, "nothing changed");
        // A present that fails leaves glass a page behind, and the reply says
        // so rather than name a control the person cannot see there.
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

    /// A HEADLESS WHEEL TAKES THE WHEEL'S OWN PATH INTO A NATIVE VIEW: with no
    /// `pointer wheel`, a wheel over Settings was checked only below the App,
    /// as `ScrollLines` handed to the reducer. The verb delivers `MouseWheel`
    /// through `on_mouse_wheel_phased`, where the pointer is: aimed at a list
    /// row the list scrolls one stop a line, the plan's step; unaimed it turns
    /// where the pointer rests; a trackpad's points bank into whole stops; over
    /// the sidebar it is the sidebar's (the target finder under the pointer),
    /// which no shortcut into the page's scroll would be; and the reply says
    /// where the pointer is in the frame the wheel left.
    /// NEGATIVE CONTROLS: no position and no aim is refused and scrolls
    /// nothing; a sideways wheel scrolls no native view.
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
        // NEGATIVE CONTROL: a window that never saw the pointer has nowhere
        // to turn a wheel.
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
        // NEGATIVE CONTROL: sideways scrolls no native view.
        let sideways = PointerAction::Wheel {
            dx: 3.0,
            dy: 0.0,
            unit: WheelUnit::Lines,
            aim: None,
        };
        assert!(app.pointer_cmd(sideways).is_ok());
        assert_eq!(page_scroll(&app), 1);
        // A trackpad's two rows of points bank into two stops.
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
        // Over the sidebar the wheel is the sidebar's: the list stays.
        assert!(app.pointer_cmd(lines(1.0, Some(&nav))).is_ok());
        assert_eq!(page_scroll(&app), 3, "the sidebar's wheel leaves the list");
    }

    #[test]
    fn system_state_wheel_interaction_precedes_notch_normalization() {
        use super::NativePointerTarget;
        use crate::system_state::SystemStateAction;
        use crate::workstation_setup::Request;
        use aterm_types::macos_setup::Plan;
        use aterm_window::event::{MouseScrollDelta, TouchPhase};
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        for (delta, phase, interacts) in [
            (
                MouseScrollDelta::PixelDelta(aterm_window::dpi::PhysicalPosition::new(0.0, 0.25)),
                TouchPhase::Moved,
                true,
            ),
            (
                MouseScrollDelta::LineDelta(0.0, 0.25),
                TouchPhase::Moved,
                true,
            ),
            (
                MouseScrollDelta::PixelDelta(aterm_window::dpi::PhysicalPosition::new(1.0, 0.0)),
                TouchPhase::Moved,
                true,
            ),
            (
                MouseScrollDelta::LineDelta(1.0, 0.0),
                TouchPhase::Moved,
                true,
            ),
            (
                MouseScrollDelta::PixelDelta(aterm_window::dpi::PhysicalPosition::new(0.0, 0.0)),
                TouchPhase::Ended,
                false,
            ),
        ] {
            let mut app = crate::App::headless_for_test();
            let wid = crate::WindowId(0);
            app.on_resize(wid, aterm_window::dpi::PhysicalSize::new(1_000, 750));
            assert!(app.open_system_state_tab());
            app.pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Key(
                "system-state/url".to_owned(),
            )))
            .unwrap();
            assert!(!app.pointer_routes_to_a_terminal_pane(wid));
            assert!(!app.pointer_is_over_pane_header(wid));
            assert!(!app.stats_owns_pointer(wid));
            let (send, _receive) = mpsc::channel();
            let request = Request::new(Plan::default(), send);
            assert!(app.native_runtime.offer_system_state(request.clone()));
            let start = Instant::now();
            app.native_runtime.tick_system_state(start, true);

            app.on_mouse_wheel_phased(wid, delta, phase);

            app.native_runtime
                .tick_system_state(start + Duration::from_secs(31), true);
            let action = app.native_runtime.take_system_state_action();
            if interacts {
                assert!(action.is_none(), "{delta:?} must stop the timer");
                assert!(request.pending());
            } else {
                assert!(
                    matches!(action, Some(SystemStateAction::Apply(_))),
                    "a zero-delta end must leave the timer running"
                );
            }
        }
    }

    /// `pointer wheel` is a hand's wheel over a terminal too: where the pointer
    /// rests on the grid, a turn up reaches the scrollback (the seam's local
    /// scroll, a glide or the offset), and a turn down at the bottom moves
    /// nothing.
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

    /// THE WHEEL OVER AN OPEN PICKER, BY THE WHEEL'S OWN PATH: the theme
    /// picker opened by a click scrolls under `pointer wheel`, and once the
    /// list moved the tile the pointer hovered is no longer hovered: the one
    /// scrolled under the pointer is, on the frame the wheel left (review of
    /// audit r14 PV8: a hover that outlived its frame); at the end the wheel
    /// turns toward nothing moves, and the tile, still under the pointer,
    /// stays hovered (the wheel cleared it there too, so a tile under a still
    /// pointer lost its wash until the pointer moved).
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
        // At the top, a wheel up moves nothing: the tile stays hovered.
        assert!(app.pointer_cmd(wheel(-1.0)).is_ok());
        assert_eq!(tiles(&app), top, "the list was at its top");
        assert_eq!(hovered(&app), Some(tile.clone()), "nothing moved under it");
        // Down, the list moves and the tile is no longer under the pointer:
        // the one now under it is hovered.
        let cell = app.pointer_cmd(wheel(1.0)).expect("the wheel scrolls");
        assert_ne!(tiles(&app), top, "the list scrolled");
        let under = match app.pointer_reply(cell) {
            Ok(PointerAt::Native { key, .. }) => key,
            other => panic!("a native answer: {other:?}"),
        };
        assert_ne!(under.as_deref(), Some(tile.as_str()), "the tile moved away");
        assert_eq!(hovered(&app), under, "the hover went with the move");
    }

    /// THE WHEEL PUTS A PRESS'S NUDGE BACK ON THE LIST'S STOPS, BY THE WHEEL'S
    /// OWN PATH: a click on a switch the list's room's edge cuts — the
    /// screen's peek, a picture fading from its top — scrolls the list by the
    /// switch's part under that edge (the press nudge,
    /// `settings_scroll::ScrollPlan::press_seat_span`), whole and live under
    /// the pointer, turning nothing, and the list holds that place; a
    /// `pointer wheel` one line down then shows the screen one stop on that
    /// an un-nudged wheel shows, and one line up at the top shows the top
    /// again, as it was before the press.
    /// NEGATIVE CONTROL: `pointer status` after the press scrolls nothing, so
    /// the nudge stands until the wheel turns.
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
        // Where each control the list's room shows live sits (the strip under
        // the room only pictures the stops after it, under a status bar the
        // click's change can bring up).
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
        // Security at `size`, and a switch the list's room's edge cuts there:
        // the screen's peek, fading from its top, its picture over what shows
        // (a press there is the list's); with its top and where its picture
        // shows.
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
        // The press on the switch's picture, at its top: the list moves up by
        // the switch's part under the room's edge, so the switch shows whole
        // and live at that edge, and turns nothing.
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
            // NEGATIVE CONTROL: nothing that scrolls, nothing that moves.
            assert!(app.pointer_cmd(PointerAction::Status).is_ok());
            assert_eq!(place(&app, wid), nudged, "{size:?}: the nudge stands");
            assert!(app.pointer_cmd(wheel(1.0)).is_ok());
            assert_eq!(
                place(&app, wid),
                one_on,
                "{size:?}: down a line from the nudge is a stop on"
            );
            // Up a line at the top: no stop moves, and the top shows again.
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

    /// A file URI built as the shipping path builds it; a hand-rolled `file://` is malformed
    /// on Windows.
    fn file_uri(path: &std::path::Path) -> String {
        crate::native_document_host::path_to_file_uri(path).unwrap()
    }

    /// The hand, the press and the caption agree on one plain URL: a concealed tail (SGR 8)
    /// never takes the hand; a tail a shade off its background (1.31:1) is part of the URL,
    /// so the band names it whole; an ordinary URL, or a 1.11:1 one `minimum_contrast` lifts
    /// to 4.5:1, takes no row.
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
        // That band still claims the bottom row (only the strip splice clears
        // the register), so a later one would land on top: ask the hover itself
        // whether there is a destination to disclose.
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

    /// A press on the divider, nudged a pixel across the cell boundary and back, then
    /// released, re-grids neither pane: a net-zero re-grid desyncs an alt-screen app whose
    /// SIGWINCH handler reads an unchanged size. A deliberate move to the next cell's centre
    /// still applies on that event.
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
        // 1 px into the next cell: the pixel row just past the boundary, + 1.
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

        // A deliberate one-row drag: the move to the next cell's centre is the
        // fresh drag's leading edge and applies on that same event.
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

    /// The dead band must not strand a throttle-coalesced step: once
    /// [`crate::RESIZE_THROTTLE`] has passed, a small move inside the pending row's band
    /// flushes the step rather than leaving both panes a row behind until release. The
    /// throttle stamp is set and backdated by hand, so the test never sleeps.
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
        // Inside the throttle window: the second step is coalesced.
        super::PANE_DRAG_APPLIED_AT.with(|c| c.set(Some((wid, std::time::Instant::now()))));
        app.on_cursor_moved(wid, x, two);
        assert_eq!(
            super::PANE_DRAG_PENDING.with(|c| c.get()),
            Some(wid),
            "the in-window step is pending, not applied"
        );
        assert_eq!(engine_rows(&app), (before.0 + 1, before.1 - 1));
        // After the window: a 1 px move that stays inside the same row flushes it.
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

    /// The divider band's sub-cell offset describes the clamped cell: a pointer past the
    /// grid's far edge sits at that cell's last pixel, one before its near edge at its first,
    /// and one inside it at its real offset.
    #[test]
    fn divider_drag_offset_is_read_against_the_clamped_cell() {
        use super::divider_drag_off_axis as off;
        // 20 px cells, clamped to cell 9 (a 10-row grid).
        assert_eq!(off(187, 9, 20), 7, "inside the cell: the real offset");
        assert_eq!(off(203, 9, 20), 19, "past the last row: its far pixel");
        assert_eq!(off(455, 9, 20), 19, "far past it: still its far pixel");
        assert_eq!(off(3, 1, 20), 0, "before the cell: its first pixel");
        assert_eq!(off(5, 0, 0), 0, "no geometry: no offset");
        // And the band then reads it right: a pointer below the last row,
        // with row 8 held, has crossed the whole of row 9 — it follows.
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

        // Already focused is still chrome, with or without VT mouse tracking.
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
        // Negative control: the first content row remains selectable.
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
        // No clamping from parent chrome or the left padding into a header.
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

    /// A window that has never seen pointer motion, or that the pointer has left, holds no
    /// position, and the verb says so instead of reporting the coordinate memos (`(0, 0)`
    /// before any motion; afterwards the departed position, kept so a press racing the
    /// departure can still hit-test). A cell outside the grid is refused, not clamped, and
    /// the refusal leaves the position alone.
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
        // The memo still holds (3, 5): `Ok(None)` is the flag at work, not an emptied memo.
        assert_eq!(app.windows[&wid].last_mouse_window_cell, (3, 5));
    }

    /// Every grid cell maps to a pixel that maps back to it, so the verb never drives the
    /// pointer to a neighbouring cell.
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

    /// Tests must drive the same retained, successfully presented artifact as
    /// production pointer routing. Preparing a speculative tree is deliberately
    /// insufficient: the `presented` bit is the glass-commit boundary.
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

    /// THE STRIP UNDER A LIST'S ROOM TAKES A PRESS AS A SCROLL (#51): a click
    /// on the middle of what a list draws of a switch fading in the strip
    /// under its room — a picture, never live, so a status bar that comes
    /// takes nothing a person can press — scrolls that switch into the room,
    /// whole and live, as a press on one the room's edge cuts does, and
    /// works nothing: its value stays and no change is asked for. The click
    /// used to do nothing at all.
    #[test]
    fn a_press_on_a_switch_drawn_in_the_lists_strip_scrolls_it_in_and_works_nothing() {
        use crate::native_ui::UiContent;
        use crate::{App, WindowId};
        use aterm_window::event::{ElementState, MouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // A window whose Settings list keeps a strip under its room for a
        // status bar (a short compact one docks the bar and keeps none).
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
        // The first screen of Top Settings whose strip draws a switch, the
        // wheel turned a stop at a time.
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

    /// A press on a looping preview drawn in the strip under a list's room —
    /// its Pause/Play is no list control — scrolls the stop that draws it
    /// into the room and pauses nothing: the click found no stop, and was
    /// lost (Top Settings' cursor-trail preview at 724×488).
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
        // The first window and screen of Top Settings whose strip draws its
        // cursor-trail preview, the wheel turned a stop at a time.
        let wid = WindowId(0);
        let mut found = None;
        for (width, height) in [(760, 520), (1_100, 740), (760, 460)] {
            let mut app = App::headless_for_test();
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

        // Sanity: without the modal, this exact point belongs to the underlying control.
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

        // Release at a different boundary in the same painted key. The press
        // establishes the reducer-owned anchor; release extends it.
        let (release_x, release_y) = native_text_boundary_point(&mut app, wid, &key, woman_end);
        app.on_cursor_moved(wid, release_x, release_y);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
            panic!("Settings state");
        };
        assert_eq!(settings.search_input.selected_text(), "👩‍💻");
        assert!(settings.common.pressed.is_none());

        // A plain click collapses again, even after a drag selection.
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

        // Shift-press preserves that caret as the anchor and extends over whole
        // multibyte graphemes. Release uses the same-key guard as buttons.
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
        // The search lists the matching field under its pinned live preview.
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

        // Drag from padding to swatch selects the whole committed value.
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

        // The tab reference deliberately outlives its runtime view generation.
        // Release must not redirect the captured text action anywhere else.
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
        // The category list's first screen holds Modified, the second route.
        settings.scroll_page_to(0);

        // Ordinary buttons still arm on press and activate only on a matched
        // release; no text position exists for their painted node.
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
        // Short compact search results disclose their live preview first, and
        // the numeric control is on the first bounded, user-reachable slice
        // that has room for it: the next under macOS's cell, the first where
        // the host's cell (and so the headless window's page) is larger
        // (Windows' 17 px face on its 1.1 line).
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

        // A real editor viewport still follows its source-addressable document
        // selection path. It never receives the Settings-only semantic payload.
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

    /// A native palette row under the pointer advertises the hand; after a
    /// wheel scroll the post-scroll re-hit selects the row now painted there,
    /// a press only ARMS it, and the matched release activates it.
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

        // Keep the pointer on the last visible slot. Scrolling to the bottom replaces the
        // command under that exact painted rectangle, and the post-scroll re-hit selects it.
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

        // Detach advances the lifecycle generation after the painted command was armed.
        // The active tab's stale reference remains solely to exercise dispatch rejection.
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

    /// A terminal overlay card strips the sub-cell remainder band (`window_to_frame`) and the
    /// `pad_top + head` origin (`overlay_coordinate_transform`) exactly once: with a nonzero
    /// remainder (x only on Linux) and pad 12, pad_top 4, head 3, each palette row's inner
    /// corners hit it and its one-pixel exterior bands miss.
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
        // A strict sub-cell remainder produces a nonzero leading frame band.
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
        };
        // A freshly opened palette is unscrolled and lists every command, so the
        // first three painted slots are filtered rows 0..3.
        let visible = 3;
        // Card-local → window: the frame remainder, then the terminal overlay origin.
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

        // A fractional palette gesture is discarded on close rather than draining into
        // the terminal's next fractional event.
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 0.75));
        app.palette_exit();
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 0.5));
        assert!(app.windows[&wid].scroll_glide.is_none());
        assert_eq!(crate::term_lock(&term).grid().display_offset(), 0);

        // Negative control: a whole wheel after closing reaches the terminal path.
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

        // A tab switch makes the captured view stale. The same pointer target
        // must not mutate the parked editor or leak into the terminal tab.
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

    /// A click on a popup-chip Enum row opens the ChoicePicker with no config edit, and
    /// Cancel returns to the unchanged value (the option commit itself is pinned by
    /// `pointer_activation_opens_choice_surface_then_selection_patches`). The row is
    /// Background material's, which Appearance offers on macOS and Windows alone
    /// (`row_available_here`): elsewhere no Appearance row is a chip.
    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn enum_chip_click_opens_the_choice_picker_and_commits_nothing() {
        use crate::native_ui::UiContent;
        use crate::{App, WindowId};
        use aterm_window::event::{ElementState, MouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // The typing sound's many voices open a picker; two or three choices
        // are segments in their row and open none. Its row is on every
        // platform (Background material's, a picker of four, has none on
        // Linux; the cursor trail's chip opens the Cursor Styles page).
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Sound));
        let key = crate::native_ui::UiKey::new(format!(
            "settings/control/{}",
            crate::prefs::EDIT_TRAIL_SOUND_STYLE
        ));
        // The page window is virtualized: scroll until the enum chip paints.
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

        // One click on the chip opens the pull-down list and commits nothing.
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

        // Dismiss via Cancel: the chip shows the unchanged committed value.
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

    /// The wheel axis table: strict vertical dominance is vertical (the whole set
    /// tracking-off scrolling accepts), strict horizontal dominance is horizontal, and ties
    /// and the all-zero event carry no axis.
    #[test]
    fn wheel_axis_matches_the_old_vertical_guard_and_adds_the_horizontal_one() {
        use super::{WheelAxis, wheel_axis};
        // Vertical dominance (either sign).
        assert_eq!(wheel_axis(0.0, 1.0, 0.0), Some(WheelAxis::Vertical));
        assert_eq!(wheel_axis(0.0, -1.0, 0.0), Some(WheelAxis::Vertical));
        assert_eq!(wheel_axis(0.4, -1.0, 0.0), Some(WheelAxis::Vertical));
        // Horizontal dominance.
        assert_eq!(wheel_axis(1.0, 0.0, 0.0), Some(WheelAxis::Horizontal));
        assert_eq!(wheel_axis(-1.0, 0.4, 0.0), Some(WheelAxis::Horizontal));
        // No axis: the all-zero event (which must not scroll down) and a perfect diagonal,
        // which has no defensible answer.
        assert_eq!(wheel_axis(0.0, 0.0, 0.0), None);
        assert_eq!(wheel_axis(1.0, 1.0, 0.0), None);
        assert_eq!(wheel_axis(-2.0, 2.0, 0.0), None);
        // The PixelDelta floor rejects sub-EPSILON motion on both axes.
        assert_eq!(wheel_axis(0.0, f64::EPSILON / 2.0, f64::EPSILON), None);
        assert_eq!(wheel_axis(f64::EPSILON / 2.0, 0.0, f64::EPSILON), None);
    }

    /// `App::wheel_notches` (the normalizer `on_mouse_wheel` runs first) banks the axes into
    /// separate window fields, `scroll_residual_x` and `scroll_residual`, so a sideways flick
    /// never pays off or cancels pending vertical motion.
    #[test]
    fn the_two_wheel_axes_bank_into_separate_window_fields() {
        use crate::{App, WindowId};
        use aterm_window::event::MouseScrollDelta;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // `LineDelta` is f32, so the banked f64 carries the f32 rounding of 0.6.
        let near = |got: f64, want: f64, what: &str| {
            assert!((got - want).abs() < 1e-6, "{what}: got {got}, want ~{want}");
        };
        let residuals = |app: &App| {
            let ws = &app.windows[&wid];
            (ws.scroll_residual_x, ws.scroll_residual)
        };
        assert_eq!(residuals(&app), (0.0, 0.0), "both banks start empty");

        // A sub-notch flick on each axis: neither emits, and each lands in its
        // own field. If the handler shared one residual these two 0.6s would
        // already have made a whole notch.
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

        // Each axis needs its own second half to emit; neither borrows the other.
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

    /// The horizontal axis end to end, both directions, through the `App` handler. winit's
    /// +x is "the content moves right", the reader walking left: xterm button 66; -x is
    /// button 67 (the bytes per direction are pinned in `aterm_core::terminal::mouse`).
    /// Backwards would invert every horizontal scroll in Neovim and tmux.
    #[test]
    fn horizontal_flicks_report_both_xterm_buttons_in_the_right_direction() {
        use aterm_window::dpi::PhysicalPosition;
        use aterm_window::event::MouseScrollDelta;

        use crate::{App, WindowId};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);

        // Classic tilt-wheel notches, both signs.
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
        // A multi-notch flick carries its count.
        assert_eq!(
            app.wheel_notches(wid, MouseScrollDelta::LineDelta(-3.0, 0.0)),
            Some((WheelDir::Right, 3))
        );

        // Trackpad pixel deltas take the same signs, measured in cell widths.
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

    /// The horizontal axis has no local viewport, on the grid or over chrome: with tracking
    /// off a sideways flick over the grid moves nothing (`WheelPlan::Fallback` emits
    /// `wheel_lines: 0`), and over the palette or a native tab it is swallowed before it can
    /// reach the PTY.
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

        // Over the grid, tracking off: the flick reaches the seam and yields
        // zero rows of motion and no glide.
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(3.0, 0.0));
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(-3.0, 0.0));
        assert_eq!(crate::term_lock(&term).grid().display_offset(), 0);
        assert!(
            app.windows[&wid].scroll_glide.is_none(),
            "a horizontal flick never arms the vertical smooth scroll"
        );
        // Negative control: the vertical twin of the same gesture does move.
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 3.0));
        assert!(
            app.windows[&wid].scroll_glide.is_some()
                || crate::term_lock(&term).grid().display_offset() > 0,
            "negative control: the vertical axis still scrolls"
        );

        // Over a native tab: swallowed before `dispatch_native_event`. The About
        // route admits a first scroll before any render has measured it
        // (`settings_page_scroll_limit`), so the vertical negative control below
        // really can move.
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
        // Negative control: the vertical twin does reach the native scroller.
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, -5.0));
        assert_ne!(
            page_scroll(&app),
            before,
            "negative control: the vertical axis still reaches the native scroller"
        );

        // Over the palette: swallowed before `palette_pointer_wheel`.
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

    /// The bypass modifier: Option/Alt everywhere, and off macOS Shift too (the
    /// xterm convention); Ctrl and Super are never it.
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

    /// Option+drag is the rectangular block selection only when the press was local anyway;
    /// while Option buys the tracking bypass it selects normally.
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

    /// The right-press decision table (the conhost/Windows Terminal convention): chrome
    /// swallows first, even with the gesture off; a tracking app keeps the button; only then
    /// copy-if-selection-else-paste. All 16 rows, so the precedence cannot silently reorder.
    #[test]
    fn right_press_plan_matches_the_windows_convention() {
        use super::{RightPressPlan, right_press_plan};
        for gesture_on in [false, true] {
            for has_selection in [false, true] {
                for tracking in [false, true] {
                    // Chrome (the strip band) wins over everything: no paste on
                    // a chip, and no report at the clamped cell under the band.
                    assert_eq!(
                        right_press_plan(gesture_on, true, tracking, has_selection),
                        RightPressPlan::Chrome,
                        "chrome must swallow (gesture_on={gesture_on}, tracking={tracking})"
                    );
                }
                // Tracking on over the grid: the app owns the button, selection or not,
                // config on or off.
                assert_eq!(
                    right_press_plan(gesture_on, false, true, has_selection),
                    RightPressPlan::Seam,
                    "tracking keeps the button (gesture_on={gesture_on})"
                );
            }
        }
        // The gesture itself, tracking off over the grid.
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
        // `right_click = "off"`: the seam keeps the press.
        assert_eq!(
            right_press_plan(false, false, false, true),
            RightPressPlan::Seam
        );
        assert_eq!(
            right_press_plan(false, false, false, false),
            RightPressPlan::Seam
        );
    }

    /// The pointer geometry mirrors the band placement: horizontally the centred
    /// `band_offset`, vertically the platform `band_offset_y` (top-pinned on Linux, centred
    /// elsewhere); headless (no `win_px`) and an exact grid fit stay at origin 0. The fixture
    /// pads 2px on top and 12px elsewhere.
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
        // Headless: the raw window size is unknown -> identity mapping.
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
        // Exact grid fit: zero remainder, origin stays 0.
        app.windows.get_mut(&wid).expect("window").win_px = Some(fit);
        assert_eq!(
            app.frame_origin(wid),
            (0, 0),
            "exact fit must be the identity"
        );
        let (px, py) = ((pad + 5 * cw) as f64 + 1.0, (pad_top + 2 * ch) as f64 + 1.0);
        let at_fit = app.pixel_to_cell(wid, px, py);

        // Grid fit plus a sub-cell remainder per axis (strictly under one cell, so it centres
        // into a leading band rather than adding a column or row), from the actual cell size.
        let (rw, rh) = (cw.saturating_sub(1).max(1), ch.saturating_sub(1).max(1));
        app.windows.get_mut(&wid).expect("window").win_px = Some(PhysicalSize::new(
            fit.width + rw as u32,
            fit.height + rh as u32,
        ));
        // Horizontally the leading band is the low half of the remainder; vertically it is
        // top-pinned on Linux (the chrome band stays glued to the titlebar) and centred
        // elsewhere: the `band_offset`/`band_offset_y` pair the presenters place with.
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
        // A click in the leading band clamps to the frame edge (col 0), as in the pad border.
        let (_, col) = app.pixel_to_cell(wid, 1.0, py + oy as f64);
        assert_eq!(col, 0, "a band click clamps to the leading cell");

        // A transient surface smaller than the composed frame is a source crop, placed like
        // the remainder. The origin stays signed so window pixel 0 maps to the real frame
        // coordinate instead of clamping at 0.
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

    /// An Option-bypass local selection started under mouse tracking settles on release
    /// even when tracking flips off mid-drag (the program exits or sends `CSI ?1000l`);
    /// otherwise `selecting` sticks and every later hover grows the selection.
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
        // Foreground app turns mouse tracking on.
        term_lock(&term).process(b"\x1b[?1000h");
        assert!(term_lock(&term).mouse_tracking_enabled());
        // Option/Alt held: the left press takes the bypass (a local selection, no reported
        // bit) despite the tracking.
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::ALT;
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert!(
            app.windows.get(&wid).unwrap().selecting,
            "the Option-bypass press starts a local selection"
        );
        // Tracking flips off mid-gesture (the program exited / sent CSI ?1000l).
        term_lock(&term).process(b"\x1b[?1000l");
        assert!(!term_lock(&term).mouse_tracking_enabled());
        // The release must settle the in-flight local selection regardless of the
        // live tracking mode, and clear the Left reported bit.
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

    /// A thumb button gets its own `reported_buttons` slot, so pressing it mid-drag cannot
    /// clear the Left bit and orphan the drag's release (`Back.code()` is 128, and
    /// `128 & 7 == Left.code() & 7`).
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
        // A foreground app is tracking, so every button press reports and sets
        // its bit (no local selection gesture claims the Left press).
        term_lock(&term).process(b"\x1b[?1000h");
        assert!(term_lock(&term).mouse_tracking_enabled());
        let bits = |app: &App| app.windows[&wid].reported_buttons;
        let bit = |b: MouseButton| 1u8 << (b.slot() & 7);

        // Left goes down and stays down (a live drag).
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert_ne!(
            bits(&app) & bit(MouseButton::Left),
            0,
            "Left press reported"
        );
        // A thumb click lands and lifts entirely inside that drag.
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
        // The drag's own release still pairs.
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        assert_eq!(bits(&app), 0, "every bit paired off");

        // Forward gets a third distinct slot, and `Other(_)` is dropped outright
        // (no bogus report for a device button aterm has no xterm code for).
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

    /// The pet's pointer wake is an edge: never with no pet on glass or with the brain's own
    /// lane armed; once for a brain that has never sampled; thereafter only after a full cell
    /// of travel (either axis) from the position the brain last consumed.
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
        // Degenerate metrics never divide and never spin.
        assert!(pet_wake_wanted(true, false, seen, (101.0, 200.0), (0, 0)));
    }

    /// A left press inside the drawn pet's body pets it and is consumed (no selection);
    /// outside the body it still selects. The pure hit test and its slop are pinned in
    /// `aterm_effects::companion`.
    #[test]
    fn a_click_on_the_pet_pets_and_never_starts_a_selection() {
        use crate::{App, WindowId};
        use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // Draw the resident the way a frame does: its owner stashes the body
        // it drew (frame px, right/bottom exclusive) as the hit target.
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
            // `on_cursor_moved` takes window px: the frame origin plus the
            // body's centre in frame px (the inverse of `window_to_frame`).
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
        // The control: the same gesture outside the padded rect selects.
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

    /// A right press on a chip pops the composed menu and never reaches the grid; a left
    /// press away from the card dismisses it without acting on what was underneath.
    /// In-grid-strip platforms only, like the opener: macOS chips carry a real `NSMenu`.
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
        // Lay the strip out so `tab_segments` is real (the hit-test reads it).
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
        // `reported_buttons` is set only by the seam at the end of `on_mouse_input`: zero
        // proves the press never reached the grid.
        assert_eq!(
            app.windows[&wid].reported_buttons, 0,
            "a press on chrome is never reported to the terminal"
        );

        // A left press on the `+` dismisses the card and is consumed: click-away must not
        // also open a tab.
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

    /// A right press on the bare band arms `band_menu_release_pending` (a menu entered under
    /// winit's capture is discarded) and its release spends it. Windows-only; headless has no
    /// HWND, so the latch is the observable half.
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
        // A column just past the `+`, where `hit_test` answers None: the bare band. Found by
        // kind: with the UI face installed (an earlier test on this thread may have done it)
        // the last segment is the version label, flush against the right edge.
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

        // A right press on a chip arms nothing: the chip owns the tab card.
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

    /// Hovering the `+` records it, the strip cache key carries the flag so the next splice
    /// rebuilds, and the painted row takes the wash. `CursorMoved` alone drives
    /// [`App::track_strip_hover`], so leaving the window must clear the `+`'s wash, and the
    /// redraw carries the clear (a chip's hover through the leave:
    /// `a_strip_hover_change_moves_the_repaint_key`).
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
        // Takes `app` as an argument: the pointer events below need `&mut app`.
        let bg_of =
            |app: &crate::App, col: u16| app.windows[&wid].cached_strip_rows[0][col as usize].bg;
        let resting = bg_of(&app, button_col);

        // (1) the motion hook records the button…
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

        // (2) …the cache key carries it, so the next splice rebuilds…
        // (3) …and the rebuilt row shows the wash.
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

        // Leaving the window clears the `+` wash, and the clear reaches the row.
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

    /// Closing the palette or an overlay card re-resolves the cursor only where the
    /// window still holds the pointer ([`App::replay_held_pointer`]). The remembered
    /// position sits on the strip's first tab, as a pointer that left over the strip
    /// leaves it (and as a headless window's `(0, 0)` hit-tests). NEGATIVE CONTROL:
    /// the close replayed that memo through the motion path whatever the pointer
    /// did, which claimed the pointer again and lit the tab, its hover wash and its
    /// `✕`, with no pointer in the window (the 2026-10-18 judges'
    /// `strip-phantom-hover-after-overlay`). CONTROL: a held pointer is still
    /// re-resolved on close.
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
        // The pointer crossed the tab and left the window.
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

        // CONTROL: a pointer the window holds is re-resolved when the palette goes.
        app.on_cursor_moved(wid, on_tab.0, on_tab.1);
        app.palette_enter();
        app.palette_exit();
        assert_eq!(
            app.windows[&wid].strip_hover,
            Some(0),
            "a held pointer is still replayed on close"
        );
    }

    /// A strip hover change moves `redraw_tab_strip_state`'s fingerprint (the RepaintKey's
    /// strip term), or the settled-screen early-out skips its frame; motion within a chip
    /// leaves it, and nothing lit restores the resting value.
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

    /// The trailing version label (Windows) hovers like the `+`: recorded alone, carried by
    /// the cache key and RepaintKey, painted in the band's hover material, cleared on leaving,
    /// handed to the `+`. It needs the UI face, installed and cleared here.
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

        // (1) the motion hook records the label, and only the label…
        let p = point(label.start_col + 3);
        app.on_cursor_moved(wid, p.0, p.1);
        assert!(
            app.windows[&wid].strip_hover_version,
            "the pointer is on the label"
        );
        assert!(!app.windows[&wid].strip_hover_new_tab, "…not on the `+`");
        assert_eq!(app.windows[&wid].strip_hover, None, "…and on no chip");
        // (1b) …and the RepaintKey's strip term moves, so the frame it asks for
        // is not swallowed by the settled-screen early-out.
        assert_ne!(
            app.redraw_tab_strip_state(wid),
            resting_key,
            "the label's hover reaches the glass on its own"
        );

        // (2) …the cache key carries it, so the next splice rebuilds…
        // (3) …and the rebuilt row shows the band's hover material.
        app.splice_tab_strip_with(wid, 1);
        let hot = bg_of(&app, label.start_col);
        assert_ne!(resting, hot, "the repaint must reach the rows");
        assert_eq!(
            hot,
            crate::tab_bar::strip_hover_bg_for_test(app.chrome_palette_theme()),
            "the label takes the band's own hover material"
        );

        // Leaving the window clears it, and the clear reaches the row.
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

        // Back on the label, then onto the `+`: the light moves to the button.
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

    /// The hover cursor resolves by location: I-beam over the grid, arrow under VT mouse
    /// tracking, resize over a divider, no I-beam for a modifier tap over the tab strip.
    /// Headless, the asserts read the `WindowState` bools `set_cursor` keys from.
    #[test]
    fn hover_cursor_resolves_grid_tracking_divider_and_strip_by_location() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f64;
        let strip_px = app.tab_strip_rows as f64 * ch as f64;
        // A fixture point well inside the grid: below any strip band, ~10 cols in.
        let grid = (pad + cw as f64 * 10.5, pad + strip_px + ch as f64 * 5.5);
        assert!(
            app.strip_col_at(wid, grid.0, grid.1).is_none(),
            "fixture point must be off the strip"
        );
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.last_cursor_px = grid;
            ws.last_mouse_window_cell = (5, 10);
        }

        // Plain grid, tracking off: the I-beam state.
        app.update_hover_cursor(wid);
        let state = |app: &crate::App| {
            let ws = &app.windows[&wid];
            (ws.hover_pointer, ws.native_text_cursor)
        };
        assert_eq!(state(&app), (false, true), "grid = I-beam");

        // A VT mouse-tracking app owns the pointer: revert to the arrow.
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        crate::term_lock(&term).process(b"\x1b[?1000h");
        app.update_hover_cursor(wid);
        assert_eq!(state(&app), (false, false), "tracking = arrow");
        crate::term_lock(&term).process(b"\x1b[?1000l");
        app.update_hover_cursor(wid);
        assert_eq!(state(&app), (false, true), "tracking off = I-beam again");

        // Split 1|2 (divider at window col 40): hovering the seam advertises the resize
        // cursor before any drag.
        let _sid = app.split_active_stub_tab(wid);
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.last_mouse_window_cell = (5, 40);
        }
        app.update_hover_cursor(wid);
        assert_eq!(state(&app), (true, true), "divider = resize encoding");
        // Back off the seam: the grid I-beam returns.
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.last_mouse_window_cell = (5, 10);
        }
        app.update_hover_cursor(wid);
        assert_eq!(state(&app), (false, true), "off the seam = I-beam");

        // `on_modifiers_changed` resolves with no pointer event: with the pointer parked over
        // the tab strip, a modifier tap must resolve the strip (arrow), not paint an I-beam
        // over chrome. `headless_for_test` seeds `tab_strip_rows: 0`, so enable one row.
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
                // The fixture teleports the pointer without a `CursorMoved`;
                // vouch for it the way motion would, or the modifier path
                // (correctly) declines to resolve a pointer it cannot place.
                ws.pointer_position_known = true;
            }
            // The link modifier (Cmd on macOS, Ctrl elsewhere): the one edge the modifier
            // path resolves on.
            app.on_modifiers_changed(wid, link_modifier());
            assert_eq!(
                state(&app),
                (false, false),
                "a link-modifier tap over the strip paints chrome's arrow, not an I-beam"
            );
            app.on_modifiers_changed(wid, aterm_window::keyboard::ModifiersState::empty());
        }
        app.tab_strip_rows = 0;

        // Only a link-modifier edge re-resolves: with the pointer memo moved back onto the
        // grid (no motion event), a Shift tap leaves the stale strip arrow; a link-modifier
        // tap then resolves the grid.
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

        // The grid probe never parks behind the reader. The split above moved focus to a
        // new pane session: probe that terminal.
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        let acq = crate::term_lock_acquisitions_on_this_thread;
        {
            let mut held = crate::term_lock(&term);
            // The mode half needs no lock: `process()` published it to the session's
            // mirror and the memo answers the grid half, so the arrow lands while the
            // reader holds the mutex.
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
            // A new question (another cell) while the mutex is busy is deferred:
            // tracking is back off, so the answer would be the I-beam, but the
            // grid probe must not queue behind the reader — the arrow stands.
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

    /// The platform's "open link" modifier, as `link_modifier_held` reads it.
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

    // ---- drag a tab to reorder it ----------------------------------------------
    // Driven end to end through the real handlers (press, motion, release) against the
    // laid-out `ws.tab_segments`, so these pin pixels to a reorder, not just the FSM. Three
    // tabs on the 80-column harness strip sit well above `PREFERRED_MIN_TAB_COLS`, so the
    // layout is selection-independent and the spans can be read once.

    /// The window's canonical tab order, by stable id.
    fn tab_order(app: &crate::App, wid: crate::WindowId) -> Vec<crate::tab_model::TabId> {
        app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .map(|tab| tab.id)
            .collect()
    }

    /// The half-open column span of chip `index` on the laid-out strip.
    fn chip_span(app: &crate::App, wid: crate::WindowId, index: usize) -> (u16, u16) {
        let seg = app.windows[&wid]
            .tab_segments
            .iter()
            .find(|seg| seg.kind == crate::tab_bar::TabHit::Select(index))
            .expect("the tab has a laid-out chip");
        (seg.start_col, seg.end_col)
    }

    /// The window pixel at the centre of strip column `col`.
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

    /// A three-tab window with the one-row in-grid strip laid out.
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

    /// The same gesture leftward, and across two chips in one sweep: a coalesced
    /// motion that crosses two midpoints takes two steps, not one.
    #[test]
    fn a_sweep_across_two_chips_walks_the_tab_two_places() {
        use aterm_window::event::{ElementState, MouseButton as WinitMouseButton};

        let (mut app, wid, before) = app_with_three_chips();
        let grab = strip_point(&app, wid, chip_span(&app, wid, 2).0 + 1);
        app.on_cursor_moved(wid, grab.0, grab.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);

        // One motion event, landing on the first chip's leading half.
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

    /// A press, jitter, or a sweep inside the held chip's own share never reorders: a
    /// neighbour's midpoint lies beyond the held segment's far edge.
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

        // A hand resting on the mouse: sub-cell jitter that never leaves the
        // pressed column.
        app.on_cursor_moved(wid, grab.0 + 0.25, grab.1 + 0.25);
        assert_eq!(tab_order(&app, wid), before, "jitter is not a drag");

        // And a full sweep to the far end of the chip's own share.
        let inside = strip_point(&app, wid, end - 1);
        app.on_cursor_moved(wid, inside.0, inside.1);
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        assert_eq!(
            tab_order(&app, wid),
            before,
            "travel within the chip's own share crosses nothing"
        );
    }

    /// AN INTERRUPTION MID-DRAG STOPS THE HELD CHIP. Focus loss (an alt-tab
    /// can leave the release undelivered, and a wedged gesture would make every
    /// later hover shuffle) and the tab's context menu (the right press that
    /// can arrive with the left button still down: `open_tab_context_menu` →
    /// `clear_strip_press`) drop the grab; a palette opened by KEYBOARD
    /// freezes it — the modal owns the window though it never claimed the
    /// pointer — and the release still disarms through the hoisted disarm.
    /// Either way, motion past a neighbour's midpoint reorders nothing.
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

    /// A PRESS THAT IS NO GRAB ARMS NO REORDER: one on bare strip background
    /// is the window-move / maximize gesture (W2); a stale press (past the
    /// double-click window) on the chip whose inline RENAME editor is open is
    /// "keep editing" — the editor stays, and a drag would reshuffle the strip
    /// out from under the live field. A sweep past a midpoint from either
    /// moves no tab.
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
                // Past the trailing `+` (found by kind: with the UI face installed the
                // last segment is the version label), short of the strip's right edge:
                // bare band.
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

    /// One tab, dragged the length of its own chip, is a no-op: no panic, no phantom move.
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
        // The anchor is re-anchored only by a step, so an unmoved press column proves no
        // step was taken (`move_tab`'s range guard would hide a bogus one).
        assert_eq!(
            app.windows[&wid].strip_drag.map(|drag| drag.origin_col),
            Some(start),
            "a lone chip has no neighbour to cross, so it never took a step"
        );
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        assert_eq!(tab_order(&app, wid), before);
        assert_eq!(app.windows[&wid].tab_set.len(), 1);
    }

    /// Under pressure eight tabs on the 80-column harness strip seat a page of equal chips
    /// (`tab_bar::strip_window`). Re-splicing after every motion, as the repaint does, the held
    /// chip marches to the back, turning the page where its neighbour has no chip.
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

        // The first chip the page seats, the leftmost a hand can reach.
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

        // Grab it: the press selects it, and the band is re-recorded around the
        // page that selection is on.
        let grab = strip_point(&app, wid, chip_span(&app, wid, held).0 + 1);
        app.on_cursor_moved(wid, grab.0, grab.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert_eq!(
            app.windows[&wid].tabs.active, held,
            "the press selected the chip under it"
        );
        app.splice_tab_strip(wid); // the repaint the selection requested

        // The pressure signature: fewer seated chips than tabs, each wider than an equal
        // split of the band (the strip paged instead of compressing).
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

        // March the held chip to the far end, re-splicing after every motion as the runtime
        // repaints. Each motion advances it at least one slot; crossing a page edge can
        // take several, as `advance_strip_drag` steps until the chip is back under the hand.
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
            // Past the neighbour's midpoint while it is seated, else past the held chip's own
            // end (`strip_drag_step`'s off-strip rule); always right of the last motion,
            // since a pointer that has not moved takes no step.
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
            // The same pointer re-delivered over the re-recorded band takes no step, even
            // when the repaint turned the page (`advance_strip_drag`'s stationary guard).
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

    /// A word/line drag re-derives its origin from the live selection on every move, not
    /// from the press-time `GestureOrigin.row`: output moves the engine anchors
    /// (`adjust_for_scroll`) but not the gesture row. The line (triple-click) and word
    /// (double-click) arms both re-derive through `origin_row`. The oracle is the text the
    /// drag copies.
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

            // Row 5's word or line; the gesture origin is captured at 5.
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

            // Three lines of output scroll the grid: plain writes keep the selection,
            // whose anchors ride up to row 2, while `GestureOrigin.row` stays 5.
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

            // Drag down to viewport row 8 (now "<tag>11").
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

    /// With no live selection to read, the drag falls back to the captured
    /// `GestureOrigin.row`, so a first move after the engine cleared the selection still
    /// builds one.
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
        // The engine drops the selection out from under the drag (an overlapping
        // damage band would do exactly this).
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

    /// A 130-byte command auto-wrapped across rows 0/1 of the headless 24x80 screen (its
    /// 80-byte head fills row 0 exactly), then a separate logical line.
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

    /// A triple-click on a soft-wrapped command copies the whole command. The oracle is the
    /// bytes `selection_to_string` gives ⌘-C and X11 PRIMARY: the highlight can look complete
    /// while the copy stops at the window width.
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

    /// A double-click on a word straddling the wrap selects the whole word —
    /// `STRADDLEWORD`, never the `STRADD` fragment on the clicked row.
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

    /// A drag from a wrapped line keeps the whole origin logical line while the moving end
    /// extends, either way. The upward drag is the discriminating half: only anchoring at the
    /// origin's last row, which a physical-row gesture cannot name, separates the whole line
    /// from its 80-byte head.
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
        // Row 0 is an ordinary line; the wrapped command is rows 1-2; row 3 is
        // the next logical line. The origin therefore has content on both sides.
        let (mut seed, cmd) = wrapped_command_screen();
        let mut screen = b"BEFORE\r\n".to_vec();
        screen.append(&mut seed);
        term_lock(&term).process(&screen);
        assert!(
            term_lock(&term).row_continues_previous(2),
            "precondition: the command wraps from row 1 onto row 2"
        );

        app.select_line_click(wid, 1, 10);
        // Drag down onto the next logical line.
        app.drag_selection(wid, 3, 0);
        assert_eq!(
            term_lock(&term)
                .selection_to_string()
                .expect("the drag resolves to text"),
            format!("{cmd}\necho AFTER"),
            "the whole wrapped origin line, then the hovered line — one newline \
             between them, at the HARD break only"
        );

        // Drag back up, past the origin, onto the line above it. The origin must
        // stay whole: anchored at its last row, not at the row that was clicked.
        app.drag_selection(wid, 0, 0);
        assert_eq!(
            term_lock(&term)
                .selection_to_string()
                .expect("the drag resolves to text"),
            format!("BEFORE\n{cmd}"),
            "dragging up past the origin must keep the whole wrapped origin line; \
             anchoring on the clicked ROW would stop the copy at the window width"
        );

        // And back into the origin's own continuation row: still the whole line.
        app.drag_selection(wid, 2, 0);
        assert_eq!(
            term_lock(&term).selection_to_string().as_deref(),
            Some(cmd.as_str()),
            "dragging within the origin logical line must keep it whole"
        );
    }

    /// 24 rows of `<tag>NN`, cursor left on the last row so one more newline scrolls
    /// the whole screen — the fixture the drag-origin tests share.
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

    /// Blur, and a tab switch (`sync_window`), settle the in-flight selection into a
    /// completed one, not merely drop the window's gesture flags: an `InProgress` selection
    /// stays painted, refuses `extend_selection` and never fires copy-on-select.
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
                // The user opens another tab with the button still down.
                let sid = app.next_session_id;
                app.push_stub_tab(wid, stub_session(sid));
            } else {
                // Focus is stolen mid-sweep; the release never comes back.
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
                // A completed selection is one the user can act on: extending it
                // works, which is exactly what an InProgress zombie refuses.
                assert!(
                    app.extend_selection_to(wid, 0, 10, SelectionSide::Right),
                    "the settled selection is extendable — the InProgress zombie was not"
                );
            }
        }
    }

    /// On a DEC double-width line (DECDWL) each character is drawn two cells
    /// wide, so the pointer column is halved before a gesture anchors: a drag
    /// over the screen cells `HELLO` covers (0-9) copies `HELLO`, and a
    /// double-click on the `W` drawn at screen cell 12 selects `WORLD`.
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

        // Shift-click on the right half of the `O` drawn at screen cells 8-9,
        // with the pointer on the left half of screen cell 9: the extension
        // takes the `O`, as the drag's endpoint would.
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

        // A single-width row keeps its cells.
        app.windows.get_mut(&wid).expect("window").last_mouse_cell = (1, 0);
        app.begin_selection(wid, SelectionType::Simple);
        app.drag_selection(wid, 1, 5);
        assert_eq!(
            term_lock(&term).selection_to_string().as_deref(),
            Some("plain")
        );
    }

    /// Two terminal panes stacked top/bottom through the canonical
    /// `SplitAxis::Vertical` layout. Both receive enough history for either edge
    /// direction to move the viewport; the new bottom pane remains focused.
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

    /// Selection-autoscroll consumes the pane rect the normal cell-mapping step
    /// already planned. Enabling selection must not add another owned visible plan
    /// to the split-pane pointer hot path.
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

    /// A stacked pane's own edge is an autoscroll edge: the top pane's lower edge, though it
    /// lies mid-window, and one pixel into the divider above the bottom pane scroll it into
    /// history rather than waiting for the pointer to leave the window. Negative control:
    /// each point is inside the whole-window grid.
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

    /// The strip is chrome for hover and reporting but not a dead zone for a text drag: one
    /// coalesced motion refreshes its current x, clamps to row 0 and extends before
    /// returning, with or without history to reveal.
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
            // Intentionally stale: the coalesced strip event lands at column 7.
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
        // Past the bottom edge: at `display_offset == 0` a downward scroll clamps to nothing,
        // so the no-motion assert holds however much scrollback the fixture gains
        // (`a_selection_drag_above_the_window_top_scrolls_into_history` owns the top edge).
        let past_bottom = 10_000.0;
        // From the app's own geometry, so a padding or chrome default cannot make the
        // fixture vacuous.
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

        // Negative control: with a live terminal, the same held edge drag owns
        // the next repeat deadline even when history clamping prevents motion.
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

    /// A drag past the top edge is reported at a negative frame y, where
    /// `selection_autoscroll_lines` measures the overshoot. An at-zero clamp (as hit-testing
    /// uses) would read a window with no top inset as inside the grid and scroll nothing.
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

        // The magnitude is the overshoot itself: four cell-heights further out
        // moves five lines on the tick, not one.
        let ch = app.win_cell_size(wid).1 as f64;
        assert!(app.selection_autoscroll(wid, -4.0 * ch));
        assert_eq!(crate::term_lock(&term).grid().display_offset(), 6);
    }

    /// The pixel at the middle of column `col` on the band's first row, through the window's
    /// real pad, strip and cell size, as the band's hit test reads it.
    fn band_pixel(app: &crate::App, wid: crate::WindowId) -> impl Fn(usize) -> (f64, f64) + use<> {
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f64;
        let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64
            + f64::from(app.tab_strip_rows) * ch as f64;
        move |col: usize| (pad + (col as f64 + 0.5) * cw as f64, top + ch as f64 * 0.5)
    }

    /// A toolchain row with its `Packages` capsule — the row every band press
    /// test presses.
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

    /// The band's first row at this window's width.
    fn band_row(app: &crate::App, wid: crate::WindowId) -> aterm_messages::RowLayout {
        let cols = usize::from(app.windows[&wid].cols);
        app.band_presentation(cols).rows[0].clone()
    }

    /// A column inside the row's first authored capsule.
    fn capsule_col(app: &crate::App, wid: crate::WindowId) -> usize {
        band_row(app, wid)
            .capsules
            .iter()
            .find(|c| !c.action.is_details())
            .expect("an authored capsule")
            .col
            + 1
    }

    /// The capsule the engine recorded a press on, if any.
    fn acted(
        app: &crate::App,
        id: aterm_messages::MessageId,
    ) -> Option<aterm_messages::ActionIndex> {
        app.messages.live(id).and_then(|l| l.acted.map(|(k, _)| k))
    }

    /// The Settings route the window's front native view is on, if Settings
    /// fronts it.
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

    /// The entry Settings ▸ Messages has selected in the window's front
    /// Settings view, if any.
    fn selected_message(app: &crate::App, wid: crate::WindowId) -> Option<u64> {
        let (_, view) = app.active_native_view(wid)?;
        match app.native_runtime.view_state(view)? {
            crate::native_app::AppViewState::Settings(state) => state.messages_selected,
            _ => None,
        }
    }

    /// Through the whole press path, a capsule press is recorded and opens Settings ▸
    /// Packages in the pressed window; the row stays and the grid never sees the press; one
    /// row below the band the press is the grid's. The `Details ›` chip is the body's press:
    /// Settings ▸ Messages at the entry, no capsule press.
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

        // One row below the band: the grid's, not the band's.
        let ch = app.win_cell_size(wid).1 as f64;
        assert!(
            !app.press_band_at(wid, x, y + ch),
            "one row below the band is the grid's"
        );

        // The `Details ›` chip: the body's press (Settings ▸ Messages at
        // this entry), nothing recorded as a capsule press.
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

    /// While the multi-line-paste question overwrites this window's band rows, a press inside
    /// them answers it no (fail-closed) and never presses the capsule underneath; answered,
    /// the same press is the capsule's. Another window's question does not stand in the way.
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

        // No band row: the question still owns its window's top rows, so a press there is
        // its answer (never a grid click) and a press below its floor is nobody's.
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

    /// The band lies above native content like the strip: a press on it over a
    /// Settings-fronted tab is the band's, not the native view's.
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

    /// A POINTER OVER THE CHROME ABOVE A NATIVE PAGE IS ON THE CHROME: with
    /// Settings in front, `pointer move -1 <c>` onto the band's row moved the
    /// pointer there (the band lit its hover) and answered `ERR` that no
    /// presented native frame was under the pointer, as though it were over
    /// the page. The reply names the chrome cell, as over a terminal.
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

    /// The `pointer` verb reaches the band: `move -1 <c>` is the chrome row right above the
    /// grid; it lights a capsule as a hand does, reads back as that negative row, and `click`
    /// presses it once. A row past the chrome, and `click` with no position, are refused.
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
        // The pixel lands in the band's row, as the band test helper puts it.
        let (_, y) = band_pixel(&app, wid)(col);
        assert!((app.windows[&wid].last_cursor_px.1 - y).abs() < 1.0);
        // Off the band the chip goes unlit.
        assert!(app.pointer_cmd(PointerAction::Leave).is_ok());
        assert_eq!(app.windows[&wid].band_hover, None, "off the band, unlit");
    }

    /// `pointer click` on a capsule spends one of the ten presses a minute `notice act`
    /// spends; with the budget spent it is refused, names the retry and presses nothing. A
    /// click off the band never asks the budget.
    #[test]
    fn a_pointer_click_on_a_capsule_spends_the_bands_press_budget() {
        use aterm_messages::wire::spend_press;
        // The click spends one: nine more fit, the tenth does not.
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
        // A spent budget refuses the click and presses nothing.
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
        // Off the band the budget is never asked.
        assert!(
            app.pointer_cmd(PointerAction::Move { row: 3, col: 5 })
                .is_ok()
        );
        assert!(app.pointer_cmd(PointerAction::Click).is_ok());
    }

    /// A body press opens Settings ▸ Messages at the entry in the pressed window, not the
    /// frontmost one: each window gets its own view of the one Settings controller, and a
    /// press on another row moves only the pressed window's selection.
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

        // The body, in the first (background) window.
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

        // The same press in the second window: its own view of the one
        // controller, at the same entry.
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

        // Another row, pressed in the first window: that window's view moves
        // to it — no second Settings tab — and the second window's view
        // keeps its own selection.
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

    /// A press anywhere on the overflow row opens Settings ▸ Messages with nothing selected;
    /// it reads no row, records no press and folds nothing. The link lights from anywhere on
    /// the row.
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
        // The words, then the far end of the row: the same press, the same page.
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

    /// Through the real motion path and painter: the body (title, glyph, detail) lights the
    /// row's `Details ›`, not the authored chip; the authored chip lights alone; the link
    /// lights as a chip; the cursor is the hand on all of them; off the band nothing is lit
    /// and the cursor is the I-beam again; the pointer leaving the window clears the lit
    /// chip; a capsule-less row lights its lone link.
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
        // What the frame carries for `cap` on row 0 under the window's own
        // recorded hover: the painter's hover fill and ink.
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
        // The title, the glyph, the detail: the body — and the Details ›
        // lit, the authored chip not.
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
        // The authored chip: it lights, the link goes out.
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
        // The link itself: lit as a chip.
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
        // Off the band: cleared, nothing lit, the grid's I-beam.
        let ch = app.win_cell_size(wid).1 as f64;
        let (x, y) = at(layout.title.0);
        app.on_cursor_moved(wid, x, y + 2.0 * ch);
        assert_eq!(hover(&app), None);
        assert!(!lit(&app, &details) && !lit(&app, &packages));
        assert_eq!(cursor(&app), (false, true), "the grid's I-beam");
        // Back onto the capsule, then out of the window.
        let (x, y) = at(packages.col + 1);
        app.on_cursor_moved(wid, x, y);
        assert!(hover(&app).is_some());
        app.on_cursor_left(wid);
        assert_eq!(hover(&app), None, "leaving the window clears the lit chip");

        // A capsule-less row: its lone Details › is what the body lights.
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

    // ---- band hover under a still pointer ------------------------------------

    /// A band click that presses nothing (the row withdrawn, held for the shrink's quiet)
    /// spends none of `notice`'s presses and is not refused when the budget is spent.
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
        // With the budget left: the click takes none of the ten.
        let mut app = crate::App::headless_for_test();
        assert!(blank_row_click(&mut app).is_ok());
        let now = std::time::Instant::now();
        for k in 0..10 {
            assert!(
                spend_press(&mut app.wire_gate, now).is_ok(),
                "press {k}: the click that pressed nothing spent one"
            );
        }
        // With the budget spent: the click is not refused.
        let mut app = crate::App::headless_for_test();
        for _ in 0..10 {
            assert!(spend_press(&mut app.wire_gate, now).is_ok());
        }
        let clicked = blank_row_click(&mut app);
        assert!(clicked.is_ok(), "{clicked:?}");
    }

    /// A retire through the host's own verbs (`withdraw_message`, a resolve, a dismissal, a
    /// restate) changes the glass before the step's settle; the chip still goes out under a
    /// still pointer.
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

    /// Over a native front, a capsule withdrawn under a still pointer takes the hand with its
    /// chip.
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

    // ---- the pointer verb and the band as chrome

    /// A body press on the band is its Details press (Settings ▸ Messages at the row), even
    /// over a mixed split's unfocused pane: the focus click and divider drag never take it.
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

    /// The band is chrome: hovering it under any-motion tracking (`?1003h`, SGR) sends the
    /// PTY no motion report; grid motion (the control) does. Unix-only: the PTY's input is
    /// read back through a socket pair.
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
        // Control: motion over the grid is reported.
        app.pointer_cmd(PointerAction::Move { row: 3, col: 5 })
            .unwrap();
        let grid = read(&mut reader);
        assert!(grid.starts_with(b"\x1b[<35;"), "{grid:?}");
    }

    /// While a seamless update has parked the process every `notice` answers the switching
    /// reply (a press would be undone at Commit); a `pointer click` on a capsule is refused
    /// the same way, spends nothing and presses nothing.
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
        // And it spent none of the ten presses.
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
