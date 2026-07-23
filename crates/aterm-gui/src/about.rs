// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The own-rendered, introspectable ABOUT dialog: a SIMPLE native-style info panel — an
//! opaque rounded [`DrawPrim`] card floating centred over the terminal, showing the app
//! wordmark, tagline, and build provenance (version / build / commit / built /
//! signature) as plain display lines, with a single `OK` button. It replaces the
//! macOS-only native `NSAboutPanel` so "which build am I on" is captured WYSIWYG on
//! every platform + serialised by the `controls about` verb. ONE structured source
//! ([`crate::build_info::about_fields`]) drives the pixels AND the introspection text,
//! and ONE pure [`about_layout`] drives the painter, the mouse hit-test, AND the
//! selectable-text model ([`SelLine`]), so they can never disagree.
//!
//! The card behaves like a native info panel, not terminal cells: its type is sized
//! off a FIXED native base ([`BASE_PT`] × the display scale), the byline's site is a
//! live LINK ([`AboutHit::Site`] — a press opens it in the browser), and the text is
//! POINTER-SELECTABLE — a left drag sweeps a selection over the text model, painted
//! as an accent wash, and `Cmd-C`/`c` puts the selected characters (or, with no
//! selection, the whole [`provenance_text`] block) on the clipboard. No extra
//! buttons: the panel keeps its simple info-panel look.

use aterm_render::Theme;

use crate::settings::{Roles, SettingsGeom, text_w};
use crate::tray_raster::{row_baseline, ui_text_width};
use crate::type_scale::{StepPx, TypeStep};
use crate::widget::{DrawPrim, TextFace, TextWeight, TrayInput, rgba, text_prim};

/// The dialog's NATIVE type base, in logical pt: About sizes its text like a real
/// window's chrome — `BASE_PT × display scale` device px — NOT off the terminal font,
/// so a small terminal font cannot make the provenance unreadable and Cmd-+ zoom
/// doesn't balloon the dialog. 14 pt body ≈ the native dialog/menu text size.
const BASE_PT: f32 = 14.0;

/// A display `scale` sanitized for layout math: non-finite or non-positive values
/// (an unattached window, a zeroed test geom) fall back to 1×.
fn sane_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

/// Width of `s` at size `px` in `face` — the MATCHING metric ([`text_w`] for the mono
/// terminal face, [`ui_text_width`] for the native SF Pro chrome faces) so centered
/// labels and content-sized buttons never drift from the painted glyphs.
fn face_w(s: &str, px: f32, face: TextFace) -> f32 {
    if face == TextFace::Mono {
        text_w(s, px)
    } else {
        ui_text_width(s, px)
    }
}

/// A text position in the About card's selectable model: `(line, char)` — an index
/// into [`AboutLayout::lines`] and a char offset into that line's [`line_atoms`]
/// (`len` = the line's end).
pub(crate) type SelPos = (usize, usize);

/// What the OS pointer should show over the About card ([`about_cursor_at`]):
/// `Pointer` over the site link, `Text` (I-beam) over selectable text, else `Default`.
/// Stored on [`AboutState`] so motion only touches the OS cursor on a CHANGE.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AboutCursor {
    Default,
    Pointer,
    Text,
}

/// Transient per-window state for the About dialog (mirrors `SettingsState`'s `Option`
/// slot). While a window holds `Some(AboutState)`, `Esc` / the `OK` button / the close
/// dot close it. The card's text is pointer-selectable (`sel`/`drag`); `Cmd-C`/`c`
/// copies the selection, or the whole provenance block when nothing is selected.
pub(crate) struct AboutState {
    /// `(key, value)` provenance rows from `build_info::about_fields()`.
    rows: Vec<(&'static str, String)>,
    /// The pointer text selection: `(anchor, head)` positions into the text model —
    /// UNORDERED (the head may precede the anchor); collapsed (`anchor == head`)
    /// means "no visible selection". `None` ⇒ nothing selected.
    sel: Option<(SelPos, SelPos)>,
    /// Whether a left-button selection drag is in flight (pointer held down).
    drag: bool,
    /// Whether the in-flight press LANDED ON the site link: native links activate on
    /// RELEASE (drag off to cancel), so the press only ARMS — the head moving even
    /// one char disarms (the gesture became a text selection), and the release opens
    /// the browser only while still armed and still over the link.
    link_armed: bool,
    /// The OS cursor last applied while the dialog is front (change detection).
    pub(crate) cursor: AboutCursor,
}

/// The whole provenance block as clipboard-ready `key: value` lines — the SAME
/// [`crate::build_info::about_fields`] rows the painter draws and
/// [`AboutState::controls_lines`] serializes, so what lands on the clipboard can
/// never disagree with what is on screen. A free function of the build (no
/// per-window state), so the copy path needs no `AboutState` borrow. The copy
/// verb falls back to this when no pointer selection is active.
#[must_use]
pub(crate) fn provenance_text() -> String {
    let mut out = String::from("aterm\n");
    for (k, v) in crate::build_info::about_fields() {
        out.push_str(&format!("{k}: {v}\n"));
    }
    out
}

/// The byline link's target: the `site` provenance row as an absolute https URL
/// (`None` if the build carries no site row).
pub(crate) fn site_url(state: &AboutState) -> Option<String> {
    let (_, v) = state.rows.iter().find(|(k, _)| *k == "site")?;
    Some(if v.contains("://") {
        v.clone()
    } else {
        format!("https://{v}")
    })
}

impl AboutState {
    pub(crate) fn new() -> Self {
        Self {
            rows: crate::build_info::about_fields(),
            sel: None,
            drag: false,
            link_armed: false,
            cursor: AboutCursor::Default,
        }
    }

    /// Structured read projection for the native Settings `/about` route.  The
    /// legacy overlay painter, accessibility adapter, and native tab app all read
    /// these same provenance rows; no consumer parses `controls_lines` text.
    pub(crate) fn semantic_rows(&self) -> &[(&'static str, String)] {
        &self.rows
    }

    /// Test seam for exercising the optional project-link action without
    /// putting a fabricated site back into shipping build provenance.
    #[cfg(test)]
    pub(crate) fn add_test_site(&mut self, site: &str) {
        self.rows.retain(|(key, _)| *key != "site");
        self.rows.push(("site", site.to_string()));
    }

    /// The header rows (tagline / author / company / site) are shown as the wordmark subhead; the
    /// rest (version..signature) are the provenance lines.
    fn is_provenance(key: &str) -> bool {
        !matches!(key, "tagline" | "author" | "company" | "site")
    }

    fn provenance_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|(k, _)| Self::is_provenance(k))
            .count()
    }

    /// Anchor a fresh selection drag at `pos` (a left press on the card's text area):
    /// collapses any previous selection to the new anchor and arms the drag.
    pub(crate) fn sel_begin(&mut self, pos: SelPos) {
        self.sel = Some((pos, pos));
        self.drag = true;
        self.link_armed = false;
    }

    /// Arm the pending LINK CLICK (the press landed on the site link): the release
    /// opens the browser unless the drag's head moves first (see `link_armed`).
    pub(crate) fn arm_link(&mut self) {
        self.link_armed = true;
    }

    /// Take the pending link-click flag (the release consumes it exactly once).
    pub(crate) fn disarm_link(&mut self) -> bool {
        std::mem::take(&mut self.link_armed)
    }

    /// Grow the in-flight drag's head to `pos`. `true` when the head actually moved
    /// (the caller repaints); a no-op unless a drag is armed. A real move turns a
    /// pending link click into a text selection (disarms it).
    pub(crate) fn sel_extend(&mut self, pos: SelPos) -> bool {
        if !self.drag {
            return false;
        }
        match &mut self.sel {
            Some((_, head)) if *head != pos => {
                *head = pos;
                self.link_armed = false;
                true
            }
            _ => false,
        }
    }

    /// Whether a selection drag is in flight (the motion handler grows the head).
    pub(crate) fn dragging(&self) -> bool {
        self.drag
    }

    /// Drop any selection outright (a press outside the text area / on the title
    /// bar). `true` when pixels changed (a wash was cleared).
    pub(crate) fn sel_clear(&mut self) -> bool {
        self.drag = false;
        self.link_armed = false;
        self.sel.take().is_some()
    }

    /// Settle the drag on button release. A press-release with NO motion is a plain
    /// click — it DESELECTS (the native text-view convention). `true` when pixels
    /// changed (a wash was cleared).
    pub(crate) fn sel_finish(&mut self) -> bool {
        if !self.drag {
            return false;
        }
        self.drag = false;
        if let Some((a, h)) = self.sel
            && a == h
        {
            self.sel = None;
            return true;
        }
        false
    }

    /// The selection as an ORDERED `(start, end)` pair — `None` when nothing is
    /// selected or the span is collapsed. `SelPos` tuples order lexicographically,
    /// which is exactly (line, then char) display order.
    pub(crate) fn sel_range(&self) -> Option<(SelPos, SelPos)> {
        let (a, h) = self.sel?;
        if a == h {
            return None;
        }
        Some(if a <= h { (a, h) } else { (h, a) })
    }

    /// `(scroll, total, visible)` for `controls front`. About is content-sized
    /// (no scroll), so `visible == total`.
    pub(crate) fn scroll_extent(&self) -> (usize, usize, usize) {
        let n = self.provenance_count();
        (0, n, n)
    }

    /// Machine-readable lines for the `controls about` introspection verb — the same
    /// provenance the card paints, so screen == introspection.
    pub(crate) fn controls_lines(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.rows.len() + 2);
        out.push(format!("about rows={}", self.rows.len()));
        for (k, v) in &self.rows {
            out.push(format!("about {k}={v:?}"));
        }
        out.push("about action=close".to_string());
        if site_url(self).is_some() {
            out.push("about action=open-site".to_string());
        }
        out
    }

    /// A fingerprint of everything the card paints, folded into the frame's `RepaintKey`
    /// so opening forces exactly one present. Covers the pointer SELECTION too (the
    /// accent wash is pixels), but not `drag`/`cursor` (no pixels of their own). Never
    /// `0` while open (`0` is the closed sentinel), matching the settings overlay.
    pub(crate) fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.rows.len().hash(&mut h);
        for (k, v) in &self.rows {
            k.hash(&mut h);
            v.hash(&mut h);
        }
        self.sel.hash(&mut h);
        h.finish() | 1
    }

    /// The dialog CONTENT height in rows: title bar + wordmark/tagline/byline header, the
    /// provenance lines, and the OK-button footer. Test-only ceil bound (live sizing is
    /// fractional inside [`about_layout`]; its native row unit ≈ a terminal cell at the
    /// default font, so a `card_rows`-row tray comfortably fits the card).
    #[cfg(test)]
    pub(crate) fn card_rows(&self) -> usize {
        self.provenance_count() + 10
    }
}

/// The About dialog's accessibility tree — the FIFTH observer of the SAME [`AboutState`] the
/// pixels ([`about_tray`]) and the `controls about` verb ([`AboutState::controls_lines`])
/// read, so a screen reader can never disagree with the glass. Every `(key, value)` row (one
/// per `about {k}={v}` control line) becomes a [`accesskit::Role::Label`] node — label = key,
/// value = value — EXCEPT the `site` row, which is a [`accesskit::Role::Link`] carrying
/// [`accesskit::Action::Click`] (open the browser — the `about action=open-site` line, same
/// as the pointer's byline link / the `o` key). A single [`accesskit::Role::Button`] "OK"
/// also carries `Click` (the OS activate that closes the dialog, matching the
/// `about action=close` line). About has no keyboard focus cycle, so focus stays on the
/// window root.
///
/// Id contract (matched by the About branch of `App::on_accessibility_action`): the window
/// root is `NodeId(0)`; row `i` is `NodeId(i + 1)` (the site Link's id is
/// [`site_node_id`]); the OK button is `NodeId(rows.len() + 1)`. A `Click` on the site
/// Link opens the browser; a `Click` anywhere else closes.
#[cfg(feature = "a11y-accesskit")]
pub(crate) fn about_a11y(state: &AboutState) -> accesskit::TreeUpdate {
    use accesskit::{Action, Node, NodeId, Role, Tree, TreeId, TreeUpdate};

    let root_id = NodeId(0);
    let mut nodes: Vec<(NodeId, Node)> = Vec::with_capacity(state.rows.len() + 2);
    let mut children: Vec<NodeId> = Vec::with_capacity(state.rows.len() + 1);

    // One node per row (all rows, exactly the `about {k}={v}` lines): Labels, except
    // the site row's clickable Link.
    for (i, (k, v)) in state.rows.iter().enumerate() {
        let id = NodeId(i as u64 + 1);
        let mut node = if *k == "site" {
            let mut n = Node::new(Role::Link);
            n.add_action(Action::Click);
            n
        } else {
            Node::new(Role::Label)
        };
        node.set_label(*k);
        node.set_value(v.clone());
        nodes.push((id, node));
        children.push(id);
    }

    // The OK button (the close dot activates the same close): the lone actionable node.
    let ok_id = NodeId(state.rows.len() as u64 + 1);
    let mut ok = Node::new(Role::Button);
    ok.set_label("OK");
    ok.add_action(Action::Click);
    nodes.push((ok_id, ok));
    children.push(ok_id);

    let mut root = Node::new(Role::Window);
    root.set_label(TITLE);
    root.set_children(children);
    nodes.push((root_id, root));

    TreeUpdate {
        nodes,
        tree: Some(Tree::new(root_id)),
        tree_id: TreeId::ROOT,
        focus: root_id,
    }
}

/// The a11y `NodeId` (row index + 1) of the site-link row — the Link node
/// [`about_a11y`] publishes; the About branch of `App::on_accessibility_action`
/// routes its `Click` to the browser open (any other `Click` closes).
#[cfg(all(test, feature = "a11y-accesskit"))]
pub(crate) fn site_node_id(state: &AboutState) -> Option<u64> {
    state
        .rows
        .iter()
        .position(|(k, _)| *k == "site")
        .map(|i| i as u64 + 1)
}

/// What a left click on the About dialog hits: the title-bar close dot / OK button
/// (both close), or the byline's SITE LINK (opens [`site_url`] in the browser).
/// Clicks elsewhere on the card anchor a text selection (but are still swallowed —
/// modal).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AboutHit {
    Close,
    Site,
}

/// A rect in tray px: `(x, y, w, h)`.
type Rect = (f32, f32, f32, f32);

/// Which theme text role a [`SelRun`] is tinted with — the text model is theme-free;
/// the painter resolves the tone against the live [`Roles`] at paint time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Tone {
    Primary,
    Secondary,
    Tertiary,
    Accent,
}

/// One painted run of the card's SELECTABLE text model: a string starting at `x`, in
/// `face`/`weight` at type-scale size `size`, tinted `tone`. Emitted by
/// [`about_layout`] (the ONE geometry source) and consumed by the painter, the
/// pointer hit-test ([`about_pos_at`]), the selection wash, and the clipboard copy
/// ([`about_selection_text`]) — so glyphs, highlight, and copied text cannot drift.
pub(crate) struct SelRun {
    pub(crate) x: f32,
    pub(crate) size: StepPx,
    pub(crate) weight: TextWeight,
    pub(crate) face: TextFace,
    pub(crate) tone: Tone,
    pub(crate) s: String,
}

/// One selectable display line: the `h`-tall slot at row top `y` holding `runs`
/// left-to-right (wordmark / tagline / byline / one provenance row each).
pub(crate) struct SelLine {
    pub(crate) y: f32,
    pub(crate) h: f32,
    pub(crate) runs: Vec<SelRun>,
}

/// The pixel layout of the simple About panel, computed ONCE from (state, geom, scale)
/// and consumed by the painter ([`about_tray`]), the hit-test ([`about_hit`]), and the
/// selection/copy paths.
pub(crate) struct AboutLayout {
    /// The dialog card rect, centred in the tray, content-sized and clamped to fit.
    pub(crate) card: Rect,
    /// Title-bar height (px); the hairline separator sits at `card.1 + title_h`.
    pub(crate) title_h: f32,
    /// The close dot's centre + radius (painted) and its generous hit rect.
    pub(crate) close_dot: (f32, f32, f32),
    pub(crate) close: Rect,
    /// The OK button rect (also closes).
    pub(crate) ok_btn: Rect,
    /// The byline's site-link rect (opens the browser); `None` if no site row.
    pub(crate) site: Option<Rect>,
    /// First provenance line's row top (the divider sits between it and the byline).
    pub(crate) prov_y: f32,
    /// Header slot tops (the byline slot also carries the site link).
    pub(crate) byline_y: f32,
    /// The selectable text model: every content text run the card paints (title-bar
    /// caption and OK label excluded — window chrome, not content).
    pub(crate) lines: Vec<SelLine>,
}

/// The title-bar caption.
const TITLE: &str = "About aterm";

fn tagline_text(state: &AboutState) -> Option<&str> {
    state
        .rows
        .iter()
        .find(|(k, _)| *k == "tagline")
        .map(|(_, v)| v.as_str())
}

/// Compute the panel layout: a content-sized card centred in the `cols × panel_rows`
/// tray, with an OK button at the bottom-right. Text sizes come off the named type
/// scale at the NATIVE base ([`BASE_PT`] × `scale`); the spacing units derive from the
/// same base (`u` ≈ the mono em advance, `rh` = the native line height), so the card
/// is a fixed logical size like a real dialog, whatever the terminal font is doing.
pub(crate) fn about_layout(state: &AboutState, g: &SettingsGeom, scale: f32) -> AboutLayout {
    let tray_w = g.cols as f32 * g.cw;
    let tray_h = g.panel_rows as f32 * g.ch;
    let base = BASE_PT * sane_scale(scale);
    let u = 0.6 * base; // column/pad unit (≈ the mono em advance at `base`)
    let rh = 1.45 * base; // row unit (the native line height)
    // Type-scale steps the painter draws at: wordmark = Display; title caption and
    // the provenance rows = Body; tagline / byline / button labels = Secondary.
    let word = TypeStep::Display.px(base);
    let title = TypeStep::Body.px(base);
    let sec = TypeStep::Secondary.px(base);

    // Content-driven width: the provenance block (widest key + widest value), the
    // header lines; clamped into the tray. Keys render in the native UI face (measured
    // with `ui_text_width`), values in the mono terminal face (measured with `text_w`),
    // separated by a `gutter` so the right-aligned key column reads as a clean label
    // gutter against the left-aligned monospace values.
    let gutter = 1.5 * u;
    let prov_cols = |px: f32| -> (f32, f32) {
        let key_col = state
            .rows
            .iter()
            .filter(|(k, _)| AboutState::is_provenance(k))
            .map(|(k, _)| ui_text_width(k, px))
            .fold(0.0, f32::max);
        let val_col = state
            .rows
            .iter()
            .filter(|(k, _)| AboutState::is_provenance(k))
            .map(|(_, v)| text_w(v, px))
            .fold(0.0, f32::max);
        (key_col, val_col)
    };
    // Provenance size LADDER: Body normally; ONE named step down (Secondary) when the
    // Body-sized block cannot fit the tray (a narrow window vs the long `compiler`
    // row) — a discrete fallback like the settings sidebar's, never an arbitrary
    // multiplier. Below Secondary the card clip is accepted.
    let (body, (key_col, val_col)) = {
        let b = TypeStep::Body.px(base);
        let cols = prov_cols(b.get());
        // `body_w + 5u` is the content-sized card width; it must clear `tray_w - u`.
        if cols.0 + gutter + cols.1 + 5.0 * u > tray_w - u {
            let s = TypeStep::Secondary.px(base);
            (s, prov_cols(s.get()))
        } else {
            (b, cols)
        }
    };
    let body_w = key_col + gutter + val_col;
    // Byline parts: the author lead (separator glued on when a site follows) and the
    // SITE LINK run — measured separately so the link's x/width are exact.
    let author = state
        .rows
        .iter()
        .find(|(k, _)| *k == "author")
        .map(|(_, v)| v.as_str());
    let site_s = state
        .rows
        .iter()
        .find(|(k, _)| *k == "site")
        .map(|(_, v)| v.as_str());
    let company = state
        .rows
        .iter()
        .find(|(k, _)| *k == "company")
        .map(|(_, v)| v.as_str());
    let lead = match (author, company, site_s) {
        (Some(a), Some(c), Some(_)) => Some(format!("{a} \u{00b7} {c} \u{00b7} ")),
        (Some(a), Some(c), None) => Some(format!("{a} \u{00b7} {c}")),
        (Some(a), None, Some(_)) => Some(format!("{a} \u{00b7} ")),
        (Some(a), None, None) => Some(a.to_string()),
        (None, Some(c), Some(_)) => Some(format!("{c} \u{00b7} ")),
        (None, Some(c), None) => Some(c.to_string()),
        (None, None, _) => None,
    };
    let lead_w = lead.as_deref().map_or(0.0, |s| ui_text_width(s, sec.get()));
    let site_w = site_s.map_or(0.0, |s| ui_text_width(s, sec.get()));
    let byline_w = lead_w + site_w;
    let content_w = [
        body_w,
        text_w("aterm", word.get()),
        tagline_text(state).map_or(0.0, |t| ui_text_width(t, sec.get())),
        byline_w,
        ui_text_width(TITLE, title.get()) + 6.0 * u,
    ]
    .into_iter()
    .fold(0.0, f32::max);
    let card_w = (content_w + 5.0 * u).max(26.0 * u).min(tray_w - u).max(u);
    let n = state.provenance_count() as f32;
    let card_h = ((9.6 + n) * rh).min(tray_h - 0.4 * rh).max(rh);
    let cx0 = ((tray_w - card_w) * 0.5).max(0.0);
    let cy0 = ((tray_h - card_h) * 0.5).max(0.0);

    let title_h = 1.4 * rh;
    let r = (0.27 * rh).clamp(4.0, 7.5);
    let (dot_cx, dot_cy) = (cx0 + 1.5 * u, cy0 + title_h * 0.5);
    // Header block, walked top-to-bottom with generous vertical rhythm.
    let mut y = cy0 + title_h + 0.55 * rh;
    let wordmark_y = y;
    y += 1.75 * rh;
    let tagline_y = y;
    y += 0.95 * rh;
    let byline_y = y;
    y += 1.6 * rh; // byline + the divider gap before the provenance block
    // The provenance block: a right-aligned key column and a left-aligned value column,
    // both starting `key_x` in from the card edge; `val_x` clears the widest key + gutter.
    let key_x = cx0 + 2.5 * u;
    let key_right = key_x + key_col;
    let val_x = key_right + gutter;
    let prov_y = y;
    // OK button, anchored bottom-right so the card reads as a proper native dialog.
    let btn_h = 1.2 * rh;
    let btn_w = ui_text_width("OK", sec.get()) + 2.8 * u;
    let ok_btn = (
        cx0 + card_w - btn_w - 1.4 * u,
        cy0 + card_h - btn_h - 0.5 * rh,
        btn_w,
        btn_h,
    );

    // ---- The selectable text model (painter + hit-test + copy all read THIS) ----
    // SHORT-WINDOW degradation: the card CLAMPS to the tray, but the model must stay
    // WYSIWYG — a line whose slot would sit under the OK button or past the card's
    // bottom edge is dropped ENTIRELY (not painted, not selectable, not copyable),
    // never invisible-but-selectable. The full block stays reachable through the
    // no-selection Cmd-C fallback ([`provenance_text`]). A no-op at natural size.
    let content_bottom = (cy0 + card_h).min(ok_btn.1) - 0.05 * rh;
    let visible = |y: f32| y + rh <= content_bottom;
    let centered_x = |s: &str, size: StepPx, face: TextFace| {
        (cx0 + (card_w - face_w(s, size.get(), face)) * 0.5).max(cx0 + u)
    };
    let mut lines: Vec<SelLine> = Vec::with_capacity(3 + n as usize);
    // The wordmark stays MONO — the monospace mark is the terminal's brand identity
    // and, surrounded by SF Pro chrome, reads as a deliberate logotype.
    if visible(wordmark_y + 0.35 * rh) {
        lines.push(SelLine {
            y: wordmark_y + 0.35 * rh,
            h: rh,
            runs: vec![SelRun {
                x: centered_x("aterm", word, TextFace::Mono),
                size: word,
                weight: TextWeight::Bold,
                face: TextFace::Mono,
                tone: Tone::Primary,
                s: "aterm".to_string(),
            }],
        });
    }
    if let Some(t) = tagline_text(state)
        && visible(tagline_y)
    {
        lines.push(SelLine {
            y: tagline_y,
            h: rh,
            runs: vec![SelRun {
                x: centered_x(t, sec, TextFace::Ui),
                size: sec,
                weight: TextWeight::Regular,
                face: TextFace::Ui,
                tone: Tone::Secondary,
                s: t.to_string(),
            }],
        });
    }
    // Byline: the author lead (tertiary) + the SITE LINK (accent), centered together.
    let mut site: Option<Rect> = None;
    if byline_w > 0.0 && visible(byline_y) {
        let bx = (cx0 + (card_w - byline_w) * 0.5).max(cx0 + u);
        let mut runs: Vec<SelRun> = Vec::with_capacity(2);
        if let Some(l) = lead.filter(|l| !l.is_empty()) {
            runs.push(SelRun {
                x: bx,
                size: sec,
                weight: TextWeight::Regular,
                face: TextFace::Ui,
                tone: Tone::Tertiary,
                s: l,
            });
        }
        if let Some(s) = site_s {
            let sx = bx + lead_w;
            site = Some((sx, byline_y, site_w, rh));
            runs.push(SelRun {
                x: sx,
                size: sec,
                weight: TextWeight::Regular,
                face: TextFace::Ui,
                tone: Tone::Accent,
                s: s.to_string(),
            });
        }
        lines.push(SelLine {
            y: byline_y,
            h: rh,
            runs,
        });
    }
    // Provenance: right-aligned label keys (native UI face) + left-aligned monospace
    // values (mono figures are tabular by construction, so version/build/commit digits
    // line up). key + value share one size, hence one baseline.
    let mut row_i = 0usize;
    for (k, v) in &state.rows {
        if !AboutState::is_provenance(k) {
            continue;
        }
        let ry = prov_y + row_i as f32 * rh;
        if !visible(ry) {
            break; // rows are monotonic in y — everything below is clipped too
        }
        lines.push(SelLine {
            y: ry,
            h: rh,
            runs: vec![
                SelRun {
                    x: key_right - ui_text_width(k, body.get()),
                    size: body,
                    weight: TextWeight::Regular,
                    face: TextFace::Ui,
                    tone: Tone::Secondary,
                    s: (*k).to_string(),
                },
                SelRun {
                    x: val_x,
                    size: body,
                    weight: TextWeight::Regular,
                    face: TextFace::Mono,
                    tone: Tone::Primary,
                    s: v.clone(),
                },
            ],
        });
        row_i += 1;
    }

    AboutLayout {
        card: (cx0, cy0, card_w, card_h),
        title_h,
        close_dot: (dot_cx, dot_cy, r),
        close: (dot_cx - 1.8 * r, dot_cy - 1.8 * r, 3.6 * r, 3.6 * r),
        ok_btn,
        site,
        prov_y,
        byline_y,
        lines,
    }
}

/// Map a tray-px point to what it hits ([`AboutHit`]) — the EXACT rects [`about_tray`]
/// painted (close dot + OK button both close; the byline site link opens the browser).
/// Points inside the card but on no control return `None` (the caller anchors a text
/// selection there and still swallows: the dialog is modal). Points outside the card
/// also return `None`.
pub(crate) fn about_hit(
    state: &AboutState,
    g: &SettingsGeom,
    scale: f32,
    x: f32,
    y: f32,
) -> Option<AboutHit> {
    let l = about_layout(state, g, scale);
    let hit = |r: Rect| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3;
    if !hit(l.card) {
        return None;
    }
    if hit(l.close) || hit(l.ok_btn) {
        return Some(AboutHit::Close);
    }
    if let Some(sr) = l.site
        && hit(sr)
    {
        return Some(AboutHit::Site);
    }
    None
}

/// Per-CHARACTER pixel cells `(x0, x1, ch)` of one model line, left to right. An
/// inter-run gutter wider than a third of an em contributes ONE synthetic space atom
/// (so a copied provenance row reads `key value`), while glued runs (the byline's
/// author lead + link) contribute none. Prefix widths are measured with the run's own
/// face metric, so the cells sit exactly under the painted glyphs.
pub(crate) fn line_atoms(line: &SelLine) -> Vec<(f32, f32, char)> {
    let mut out: Vec<(f32, f32, char)> = Vec::new();
    for run in &line.runs {
        if let Some(&(_, prev_x1, _)) = out.last()
            && run.x - prev_x1 > 0.33 * run.size.get()
        {
            out.push((prev_x1, run.x, ' '));
        }
        let mut w0 = 0.0;
        let mut buf = String::new();
        for ch in run.s.chars() {
            buf.push(ch);
            let w1 = face_w(&buf, run.size.get(), run.face);
            out.push((run.x + w0, run.x + w1, ch));
            w0 = w1;
        }
    }
    out
}

/// Map a tray-px point to the nearest `(line, char)` position in the text model —
/// CLAMPING, never failing (above the first line resolves into it; below the last,
/// into the last; past a line's ends, to `0`/`len`; the char boundary nearest `x`
/// wins). The standard text-drag semantics, so a drag that wanders off the card
/// still grows a sensible selection.
pub(crate) fn about_pos_at(l: &AboutLayout, x: f32, y: f32) -> SelPos {
    if l.lines.is_empty() {
        return (0, 0);
    }
    let mut li = l.lines.len() - 1;
    for (i, ln) in l.lines.iter().enumerate() {
        if y < ln.y + ln.h {
            li = i;
            break;
        }
    }
    let atoms = line_atoms(&l.lines[li]);
    let mut col = atoms.len();
    for (i, &(x0, x1, _)) in atoms.iter().enumerate() {
        if x < (x0 + x1) * 0.5 {
            col = i;
            break;
        }
    }
    (li, col)
}

/// The selected text (display order, lines joined with `\n`) — `None` when the
/// selection is empty or collapsed. Reads the SAME [`line_atoms`] the painter's
/// wash reads, so the clipboard gets exactly the highlighted characters.
pub(crate) fn about_selection_text(
    state: &AboutState,
    g: &SettingsGeom,
    scale: f32,
) -> Option<String> {
    let (s, e) = state.sel_range()?;
    let l = about_layout(state, g, scale);
    let mut parts: Vec<String> = Vec::new();
    for li in s.0..=e.0 {
        let Some(line) = l.lines.get(li) else { break };
        let atoms = line_atoms(line);
        let from = if li == s.0 { s.1.min(atoms.len()) } else { 0 };
        let to = if li == e.0 {
            e.1.min(atoms.len())
        } else {
            atoms.len()
        };
        parts.push(
            atoms[from..to.max(from)]
                .iter()
                .map(|&(_, _, c)| c)
                .collect(),
        );
    }
    Some(parts.join("\n"))
}

/// What the OS pointer should be over `(x, y)` while the dialog is front:
/// `Pointer` over the site link, `Text` over a selectable line's glyph band
/// (with a small grace margin), `Default` everywhere else (buttons included —
/// native buttons keep the arrow).
pub(crate) fn about_cursor_at(l: &AboutLayout, x: f32, y: f32) -> AboutCursor {
    let hit = |r: Rect| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3;
    // Card containment FIRST — the site rect can overhang the card's edge when a
    // narrow tray clamps the byline, and the cursor must never promise a link on
    // bare glass that `about_hit` (card-gated) would not open.
    if !hit(l.card) || hit(l.close) || hit(l.ok_btn) {
        return AboutCursor::Default;
    }
    if let Some(sr) = l.site
        && hit(sr)
    {
        return AboutCursor::Pointer;
    }
    for ln in &l.lines {
        if y >= ln.y && y < ln.y + ln.h {
            let atoms = line_atoms(ln);
            if let (Some(&(fx, _, _)), Some(&(_, bx, _))) = (atoms.first(), atoms.last())
                && x >= fx - 4.0
                && x < bx + 4.0
            {
                return AboutCursor::Text;
            }
        }
    }
    AboutCursor::Default
}

/// The close dot's darker rim: the theme's danger role dimmed toward black.
fn dim(c: [u8; 3]) -> [u8; 3] {
    c.map(|v| (u16::from(v) * 3 / 4) as u8)
}

/// Paint the simple About panel: drop shadow, opaque rounded surface + hairline border,
/// a title bar with the close dot + "About aterm", the selection wash (if any), the
/// text model (wordmark / tagline / byline-with-link / provenance), and an OK button.
/// PURE DrawPrims; every text run is minted through the [`text_prim`] funnel
/// (TypeStep-sized, baseline-positioned).
pub(crate) fn about_tray(
    state: &AboutState,
    g: &SettingsGeom,
    theme: Theme,
    scale: f32,
) -> TrayInput {
    let r = Roles::from_theme(theme);
    let l = about_layout(state, g, scale);
    let base = BASE_PT * sane_scale(scale);
    let u = 0.6 * base;
    let rh = 1.45 * base;
    let (cx0, cy0, card_w, card_h) = l.card;
    let radius = (rh * 0.5).min(11.0);
    let mut prims: Vec<DrawPrim> = vec![
        // Two-step drop shadow.
        DrawPrim::Panel {
            x: cx0 - 3.0,
            y: cy0 + 2.0,
            w: card_w + 6.0,
            h: card_h + 6.0,
            radius: radius + 3.0,
            fill: rgba([0, 0, 0], 0x2A),
            blur: false,
        },
        DrawPrim::Panel {
            x: cx0 - 1.0,
            y: cy0 + 2.0,
            w: card_w + 2.0,
            h: card_h + 3.0,
            radius: radius + 1.0,
            fill: rgba([0, 0, 0], 0x30),
            blur: false,
        },
        // Opaque window body.
        DrawPrim::Panel {
            x: cx0,
            y: cy0,
            w: card_w,
            h: card_h,
            radius,
            fill: rgba(r.surface, 0xFF),
            blur: false,
        },
        DrawPrim::ClipPush {
            x: cx0,
            y: cy0,
            w: card_w,
            h: card_h,
        },
        // Title bar (rounded top via the clipped full-card panel), hairline, close dot, caption.
        DrawPrim::ClipPush {
            x: cx0,
            y: cy0,
            w: card_w,
            h: l.title_h,
        },
        DrawPrim::Panel {
            x: cx0,
            y: cy0,
            w: card_w,
            h: card_h,
            radius,
            fill: rgba(r.elevated, 0xFF),
            blur: false,
        },
        DrawPrim::ClipPop,
        DrawPrim::Stroke {
            x: cx0,
            y: cy0 + l.title_h,
            w: card_w,
            h: 1.0,
            radius: 0.0,
            width: 1.0,
            color: rgba(r.separator, 0xFF),
        },
    ];
    let (dot_cx, dot_cy, dot_r) = l.close_dot;
    prims.push(DrawPrim::Dot {
        cx: dot_cx,
        cy: dot_cy,
        r: dot_r,
        color: rgba(r.danger, 0xFF),
        breathe: false,
    });
    prims.push(DrawPrim::Stroke {
        x: dot_cx - dot_r,
        y: dot_cy - dot_r,
        w: 2.0 * dot_r,
        h: 2.0 * dot_r,
        radius: dot_r,
        width: 1.0,
        color: rgba(dim(r.danger), 0xFF),
    });

    // Title bar caption: semibold native face, centered in the card (the System
    // Settings title ramp).
    let tsize = TypeStep::Body.px(base);
    let tx = (cx0 + (card_w - face_w(TITLE, tsize.get(), TextFace::UiBold)) * 0.5).max(cx0 + u);
    prims.push(text_prim(
        tx,
        row_baseline(cy0 + (l.title_h - rh) * 0.5, rh, tsize.get()),
        TITLE.to_string(),
        tsize,
        TextWeight::Regular,
        TextFace::UiBold,
        rgba(r.text_primary, 0xFF),
    ));

    // SELECTION WASH: accent-tinted spans behind the selected text — painted UNDER
    // the runs so the glyphs stay crisp. Geometry comes from the SAME `line_atoms`
    // the copy path reads, so highlight == clipboard.
    if let Some((s, e)) = state.sel_range() {
        for li in s.0..=e.0 {
            let Some(line) = l.lines.get(li) else { break };
            let atoms = line_atoms(line);
            if atoms.is_empty() {
                continue;
            }
            let from = if li == s.0 { s.1.min(atoms.len()) } else { 0 };
            let to = if li == e.0 {
                e.1.min(atoms.len())
            } else {
                atoms.len()
            };
            if from >= to {
                continue;
            }
            let (x0, x1) = (atoms[from].0, atoms[to - 1].1);
            prims.push(DrawPrim::Panel {
                x: x0 - 1.5,
                y: line.y,
                w: x1 - x0 + 3.0,
                h: line.h,
                radius: 3.0,
                fill: rgba(r.accent, 0x46),
                blur: false,
            });
        }
    }

    // The text model: wordmark / tagline / byline (with the accent site link) /
    // provenance rows — the SAME lines the selection and copy paths read.
    for line in &l.lines {
        for run in &line.runs {
            let color = match run.tone {
                Tone::Primary => r.text_primary,
                Tone::Secondary => r.text_secondary,
                Tone::Tertiary => r.text_tertiary,
                Tone::Accent => r.accent,
            };
            prims.push(text_prim(
                run.x,
                row_baseline(line.y, line.h, run.size.get()),
                run.s.clone(),
                run.size,
                run.weight,
                run.face,
                rgba(color, 0xFF),
            ));
        }
    }

    // A hairline divider separating the header from the build provenance (an inset
    // separator, like a native panel's section rule).
    let div_y = (l.byline_y + rh + l.prov_y) * 0.5;
    prims.push(DrawPrim::Stroke {
        x: cx0 + 2.5 * u,
        y: div_y,
        w: card_w - 5.0 * u,
        h: 1.0,
        radius: 0.0,
        width: 1.0,
        color: rgba(r.separator, 0xFF),
    });

    // OK button: the accent-filled native default button, native UI label.
    let (bx, by, bw, bh) = l.ok_btn;
    prims.push(DrawPrim::Panel {
        x: bx,
        y: by,
        w: bw,
        h: bh,
        radius: bh * 0.3,
        fill: rgba(r.accent, 0xFF),
        blur: false,
    });
    let bsize = TypeStep::Secondary.px(base);
    prims.push(text_prim(
        bx + (bw - ui_text_width("OK", bsize.get())) * 0.5,
        row_baseline(by, bh, bsize.get()),
        "OK".to_string(),
        bsize,
        TextWeight::Regular,
        TextFace::Ui,
        rgba(r.on_accent, 0xFF),
    ));

    prims.push(DrawPrim::ClipPop);
    prims.push(DrawPrim::Stroke {
        x: cx0,
        y: cy0,
        w: card_w,
        h: card_h,
        radius,
        width: 1.0,
        color: rgba(r.separator, 0xFF),
    });

    TrayInput {
        prims,
        card: l.card,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom(s: &AboutState) -> SettingsGeom {
        SettingsGeom {
            cw: 9.0,
            ch: 20.0,
            font_px: 14.0,
            cols: 100,
            panel_rows: s.card_rows() + 8,
        }
    }

    fn state_with_site() -> AboutState {
        let mut s = AboutState::new();
        s.add_test_site("example.test");
        s
    }

    /// Index of the provenance line whose key is `k` in the layout's text model.
    fn prov_line(l: &AboutLayout, k: &str) -> usize {
        l.lines
            .iter()
            .position(|ln| ln.runs.first().is_some_and(|r| r.s == k))
            .unwrap_or_else(|| panic!("no model line for key {k}"))
    }

    #[test]
    fn controls_lines_serialize_provenance() {
        let s = AboutState::new();
        let lines = s.controls_lines();
        assert!(lines[0].starts_with("about rows="));
        assert!(lines.iter().any(|l| l.starts_with("about commit=")));
        assert!(lines.iter().any(|l| l.starts_with("about signature=")));
    }

    #[test]
    fn ok_and_close_dot_both_close() {
        let s = AboutState::new();
        let g = geom(&s);
        let l = about_layout(&s, &g, 1.0);
        let (bx, by, bw, bh) = l.ok_btn;
        assert_eq!(
            about_hit(&s, &g, 1.0, bx + bw * 0.5, by + bh * 0.5),
            Some(AboutHit::Close),
            "OK closes"
        );
        let (cx, cy, _) = l.close_dot;
        assert_eq!(
            about_hit(&s, &g, 1.0, cx, cy),
            Some(AboutHit::Close),
            "close dot closes"
        );
        // A click on the card body (not a control) is inert at the hit layer (the
        // caller anchors a selection there instead).
        let (px0, py0, pw, _) = l.card;
        assert_eq!(
            about_hit(&s, &g, 1.0, px0 + pw * 0.5, py0 + l.title_h + 2.0),
            None
        );
        assert_eq!(
            about_hit(&s, &g, 1.0, px0 - 4.0, py0 - 4.0),
            None,
            "outside the card"
        );
    }

    /// The byline's site text is a live link: the layout publishes its rect inside the
    /// card, `about_hit` resolves it to `Site`, the hover cursor turns `Pointer` over
    /// it, and `site_url` is the absolute https target.
    #[test]
    fn site_link_hits_and_resolves_url() {
        let s = state_with_site();
        let g = geom(&s);
        let l = about_layout(&s, &g, 1.0);
        let (sx, sy, sw, sh) = l.site.expect("site rect");
        assert!(sw > 0.0 && sh > 0.0);
        let (cx0, cy0, cw, ch) = l.card;
        assert!(sx >= cx0 && sx + sw <= cx0 + cw, "link inside the card (x)");
        assert!(sy >= cy0 && sy + sh <= cy0 + ch, "link inside the card (y)");
        let (mx, my) = (sx + sw * 0.5, sy + sh * 0.5);
        assert_eq!(about_hit(&s, &g, 1.0, mx, my), Some(AboutHit::Site));
        assert_eq!(about_cursor_at(&l, mx, my), AboutCursor::Pointer);
        let url = site_url(&s).expect("site url");
        assert!(url.starts_with("https://"), "absolute scheme: {url}");
        // Just left of the link is the author lead — selectable text, not the link.
        assert_eq!(about_hit(&s, &g, 1.0, sx - 2.0, my), None);
        assert_eq!(about_cursor_at(&l, sx - 2.0, my), AboutCursor::Text);
    }

    /// The card's type is NATIVE-sized: a fixed pt base × display scale — invariant
    /// under the terminal font (`font_px`) and linear in the scale.
    #[test]
    fn text_size_tracks_display_scale_not_terminal_font() {
        let s = AboutState::new();
        let g1 = geom(&s);
        let mut g2 = geom(&s);
        g2.font_px = 40.0; // a huge terminal font must not move the dialog's type
        let l1 = about_layout(&s, &g1, 1.0);
        let l2 = about_layout(&s, &g2, 1.0);
        let sz = |l: &AboutLayout| l.lines.last().unwrap().runs[1].size.get();
        assert_eq!(sz(&l1), sz(&l2), "terminal font must not resize About text");
        assert_eq!(
            sz(&l1),
            TypeStep::Body.px(BASE_PT).get(),
            "body text = Body step of the native base"
        );
        // 2× needs a tray wide enough that the fit ladder stays on Body.
        let mut g2x = geom(&s);
        g2x.cols = 220;
        let l2x = about_layout(&s, &g2x, 2.0);
        assert_eq!(
            sz(&l2x),
            2.0 * sz(&l1),
            "2× display scale doubles the device px"
        );
        // A degenerate scale (unattached window / zeroed test geom) falls back to 1×.
        let l0 = about_layout(&s, &g1, 0.0);
        assert_eq!(sz(&l0), sz(&l1));
    }

    /// The provenance size LADDER: in a tray too narrow for the Body-sized block the
    /// rows step down to Secondary (one named step, no arbitrary multiplier) so long
    /// compiler rows keep reading instead of clipping.
    #[test]
    fn narrow_tray_steps_provenance_down_one_step() {
        let s = AboutState::new();
        let mut g = geom(&s);
        g.cols = 30; // 270 px tray — far too narrow for the Body-sized compiler row
        let l = about_layout(&s, &g, 1.0);
        let sz = l.lines.last().unwrap().runs[1].size.get();
        assert_eq!(sz, TypeStep::Secondary.px(BASE_PT).get(), "one step down");
    }

    /// Drag selection end-to-end: anchoring on a provenance value and sweeping to the
    /// line end selects exactly the value's characters; sweeping a whole row reads
    /// `key value` (the gutter contributes ONE space); a cross-line sweep joins with
    /// `\n`; a no-motion click deselects.
    #[test]
    fn drag_selects_and_copies_the_model_text() {
        let mut s = AboutState::new();
        let g = geom(&s);
        let l = about_layout(&s, &g, 1.0);
        let li = prov_line(&l, "version");
        let version = l.lines[li].runs[1].s.clone();
        let atoms = line_atoms(&l.lines[li]);
        let key_len = l.lines[li].runs[0].s.chars().count();
        assert_eq!(
            atoms.len(),
            key_len + 1 + version.chars().count(),
            "key + ONE gutter space + value"
        );

        // Sweep the value only (anchor at the char after the gutter space).
        s.sel_begin((li, key_len + 1));
        assert!(s.sel_extend((li, atoms.len())));
        assert_eq!(
            about_selection_text(&s, &g, 1.0).as_deref(),
            Some(version.as_str())
        );
        assert!(!s.sel_finish(), "a real sweep survives release");

        // Sweep the whole row: `key value`.
        s.sel_begin((li, 0));
        s.sel_extend((li, atoms.len()));
        assert_eq!(
            about_selection_text(&s, &g, 1.0),
            Some(format!("version {version}"))
        );

        // Cross-line sweep joins with newline.
        s.sel_begin((li, 0));
        s.sel_extend((li + 1, 0));
        let t = about_selection_text(&s, &g, 1.0).expect("cross-line text");
        assert_eq!(
            t,
            format!("version {version}\n"),
            "full first line + empty tail"
        );

        // A reversed (upward) sweep reads the same as the forward one.
        s.sel_begin((li, atoms.len()));
        s.sel_extend((li, key_len + 1));
        assert_eq!(
            about_selection_text(&s, &g, 1.0).as_deref(),
            Some(version.as_str())
        );

        // No-motion click: press + release with no extend clears the selection.
        s.sel_begin((li, 3));
        assert!(s.sel_finish(), "a collapsed click deselects");
        assert_eq!(s.sel_range(), None);
        assert_eq!(
            about_selection_text(&s, &g, 1.0),
            None,
            "copy falls back to the whole block"
        );
    }

    /// `about_pos_at` clamps: above the first line → its start, below the last → its
    /// end, and x past a line's ends → 0 / len. And it round-trips an atom midpoint.
    #[test]
    fn pos_at_clamps_and_round_trips() {
        let s = AboutState::new();
        let g = geom(&s);
        let l = about_layout(&s, &g, 1.0);
        assert_eq!(about_pos_at(&l, -1e6, -1e6), (0, 0));
        let last = l.lines.len() - 1;
        let last_len = line_atoms(&l.lines[last]).len();
        assert_eq!(about_pos_at(&l, 1e6, 1e6), (last, last_len));
        let li = prov_line(&l, "commit");
        let atoms = line_atoms(&l.lines[li]);
        let (x0, x1, _) = atoms[2];
        let y = l.lines[li].y + l.lines[li].h * 0.5;
        assert_eq!(
            about_pos_at(&l, (x0 + x1) * 0.5 - 0.1, y),
            (li, 2),
            "left half of atom 2"
        );
        assert_eq!(
            about_pos_at(&l, x1 + 0.1, y),
            (li, 3),
            "right of atom 2's boundary"
        );
    }

    /// The selection is PIXELS: setting it changes the repaint fingerprint (so the
    /// splice re-rasterizes) and paints exactly one accent wash panel whose rect is
    /// the selected atoms' span, UNDER that line's text runs (z-order) — the wash is
    /// the right pixels, not just an extra prim.
    #[test]
    fn selection_repaints_and_washes() {
        let mut s = AboutState::new();
        let g = geom(&s);
        let fp0 = s.fingerprint();
        let plain = about_tray(&s, &g, Theme::default(), 1.0).prims.len();
        let l = about_layout(&s, &g, 1.0);
        let li = prov_line(&l, "build");
        s.sel_begin((li, 0));
        s.sel_extend((li, 3));
        assert_ne!(s.fingerprint(), fp0, "selection must ride the repaint key");
        let t = about_tray(&s, &g, Theme::default(), 1.0);
        assert_eq!(
            t.prims.len(),
            plain + 1,
            "exactly one wash panel for a one-line span"
        );
        // The wash rect comes from the SAME atoms the copy path reads (chars [0, 3)),
        // in the accent fill the painter promises.
        let atoms = line_atoms(&l.lines[li]);
        let (x0, x1) = (atoms[0].0, atoms[2].1);
        let want_fill = rgba(Roles::from_theme(Theme::default()).accent, 0x46);
        let wash_idx = t
            .prims
            .iter()
            .position(|p| {
                matches!(p, DrawPrim::Panel { x, y, w, h, fill, .. }
                    if (*x - (x0 - 1.5)).abs() < 0.01
                        && (*y - l.lines[li].y).abs() < 0.01
                        && (*w - (x1 - x0 + 3.0)).abs() < 0.01
                        && (*h - l.lines[li].h).abs() < 0.01
                        && *fill == want_fill)
            })
            .expect("a wash panel spanning exactly the selected atoms");
        let key_idx = t
            .prims
            .iter()
            .position(|p| matches!(p, DrawPrim::Text { s, .. } if s == "build"))
            .expect("the line's key run");
        assert!(wash_idx < key_idx, "the wash paints UNDER the text");
    }

    /// The byline's two runs are GLUED (author lead + link): its atoms concatenate to
    /// exactly the painted glyphs — no phantom synthetic space at the run seam (the
    /// gutter atom is reserved for real gaps like the provenance key/value gutter).
    #[test]
    fn byline_atoms_have_no_phantom_space() {
        let s = state_with_site();
        let g = geom(&s);
        let l = about_layout(&s, &g, 1.0);
        let (sx, ..) = l.site.expect("site rect");
        let li = l
            .lines
            .iter()
            .position(|ln| ln.runs.iter().any(|r| (r.x - sx).abs() < 0.01))
            .expect("byline line");
        let text: String = line_atoms(&l.lines[li])
            .iter()
            .map(|&(_, _, c)| c)
            .collect();
        let expect: String = l.lines[li].runs.iter().map(|r| r.s.as_str()).collect();
        assert_eq!(text, expect, "atoms == the glyphs painted, exactly");
    }

    /// SHORT-WINDOW WYSIWYG: when the tray clamps the card, rows that would paint
    /// under the OK button / past the card bottom are DROPPED from the model — never
    /// invisible-but-selectable. Every surviving line sits fully above the button.
    #[test]
    fn short_tray_drops_clipped_lines_from_the_model() {
        let s = AboutState::new();
        let mut g = geom(&s);
        g.panel_rows = 12; // 240 px tray — far shorter than the natural card
        let l = about_layout(&s, &g, 1.0);
        let full = about_layout(&s, &geom(&s), 1.0);
        assert!(
            l.lines.len() < full.lines.len(),
            "clipped rows leave the model"
        );
        for ln in &l.lines {
            assert!(
                ln.y + ln.h <= l.ok_btn.1 + 0.01,
                "no line under the OK button"
            );
            assert!(
                ln.y + ln.h <= l.card.1 + l.card.3 + 0.01,
                "no line past the card"
            );
        }
    }

    #[test]
    fn about_tray_paints_simple_panel() {
        let s = AboutState::new();
        let g = geom(&s);
        let t = about_tray(&s, &g, Theme::default(), 1.0);
        assert!(
            t.prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Text { s, .. } if s == "aterm")),
            "wordmark"
        );
        assert!(
            t.prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Text { s, .. } if s == "OK")),
            "OK button"
        );
        assert!(
            t.prims.iter().any(
                |p| matches!(p, DrawPrim::Text { s, .. } if s == "By Andrew Yates \u{00b7} ALab")
            ),
            "author and company byline"
        );
        assert!(
            !t.prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Text { s, .. } if s.contains("Copy"))),
            "no Copy button"
        );
        let l = about_layout(&s, &g, 1.0);
        assert_eq!(t.card, l.card);
        let pushes = t
            .prims
            .iter()
            .filter(|p| matches!(p, DrawPrim::ClipPush { .. }))
            .count();
        let pops = t
            .prims
            .iter()
            .filter(|p| matches!(p, DrawPrim::ClipPop))
            .count();
        assert_eq!(pushes, pops, "clip stack balanced");
    }

    /// ANTI-DIVERGENCE: the a11y tree's node/label set is in bijection with the
    /// `controls about` key set — every `about {k}={v}` line has a Label node (label=k,
    /// value=v), the `about rows={n}` count equals the Label-node count, and `about
    /// action=close` maps to exactly one clickable OK button. One model fans out to both,
    /// so pixels/introspection/a11y cannot drift.
    #[cfg(feature = "a11y-accesskit")]
    #[test]
    fn about_a11y_nodeset_matches_controls_keys() {
        use accesskit::{Action, Role};
        let s = AboutState::new();
        let lines = s.controls_lines();
        let update = about_a11y(&s);

        let mut matched = 0usize;
        for line in &lines {
            let Some(rest) = line.strip_prefix("about ") else {
                continue;
            };
            let Some((k, vq)) = rest.split_once('=') else {
                continue;
            };
            if k == "rows" || k == "action" {
                continue;
            }
            matched += 1;
            // `about {k}={v:?}` debug-quotes the String value.
            let v = vq.trim_matches('"').to_string();
            // The site row is the clickable Link; every other row is a plain Label.
            let want_role = if k == "site" { Role::Link } else { Role::Label };
            let node = update
                .nodes
                .iter()
                .find(|(_, n)| n.role() == want_role && n.label() == Some(k))
                .unwrap_or_else(|| panic!("no a11y {want_role:?} node for about key {k}"));
            assert_eq!(node.1.value(), Some(v.as_str()), "value for {k}");
        }

        let n: usize = lines
            .iter()
            .find_map(|l| l.strip_prefix("about rows="))
            .and_then(|s| s.parse().ok())
            .expect("rows= line");
        let row_nodes = update
            .nodes
            .iter()
            .filter(|(_, nd)| matches!(nd.role(), Role::Label | Role::Link))
            .count();
        assert_eq!(row_nodes, n, "one Label/Link node per row");
        assert_eq!(matched, n, "every row line matched a node");

        // The ALab build intentionally advertises company identity without
        // inventing a web URL, so the default model has no clickable link.
        let links: Vec<_> = update
            .nodes
            .iter()
            .filter(|(_, nd)| nd.role() == Role::Link)
            .collect();
        assert!(links.is_empty(), "no fabricated company URL");
        assert_eq!(site_node_id(&s), None);
        assert!(!lines.iter().any(|l| l == "about action=open-site"));
        assert!(
            lines
                .iter()
                .any(|l| l == "about author=\"By Andrew Yates\"")
        );
        assert!(lines.iter().any(|l| l == "about company=\"ALab\""));

        // `about action=close` ⇒ exactly one OK Button carrying Click.
        let buttons: Vec<_> = update
            .nodes
            .iter()
            .filter(|(_, nd)| nd.role() == Role::Button)
            .collect();
        assert_eq!(buttons.len(), 1, "one OK button");
        assert!(
            buttons[0].1.supports_action(Action::Click),
            "OK button is clickable"
        );
        assert!(lines.iter().any(|l| l == "about action=close"));
    }

    /// NEGATIVE CONTROL (non-vacuity): dropping a provenance row from the model drops its
    /// a11y node too — proving the conformance test would catch a stale/hard-coded tree
    /// rather than passing against a fixed node list.
    #[cfg(feature = "a11y-accesskit")]
    #[test]
    fn about_a11y_tree_tracks_the_model() {
        use accesskit::Role;
        let full = AboutState::new();
        let full_labels = about_a11y(&full)
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == Role::Label)
            .count();
        let mut trimmed = AboutState::new();
        trimmed.rows.pop();
        let trimmed_labels = about_a11y(&trimmed)
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == Role::Label)
            .count();
        assert_eq!(
            trimmed_labels,
            full_labels - 1,
            "removing a row removes its a11y node"
        );
    }

    /// Gated visual preview (`ATERM_ABOUT_PREVIEW=path`) → PNG. Set
    /// `ATERM_ABOUT_PREVIEW_SEL=1` to render with a live selection wash.
    #[test]
    fn preview_about_overlay() {
        let Ok(path) = std::env::var("ATERM_ABOUT_PREVIEW") else {
            return;
        };
        let mut s = AboutState::new();
        let (cw, ch, px) = (16.0_f32, 34.0_f32, 26.0_f32);
        let cols = 90usize;
        let panel_rows = s.card_rows() + 4;
        let g = SettingsGeom {
            cw,
            ch,
            font_px: px,
            cols,
            panel_rows,
        };
        if std::env::var("ATERM_ABOUT_PREVIEW_SEL").is_ok() {
            let l = about_layout(&s, &g, 2.0);
            let li = prov_line(&l, "commit");
            let key_len = l.lines[li].runs[0].s.chars().count();
            s.sel_begin((li, key_len + 1));
            s.sel_extend((li, line_atoms(&l.lines[li]).len()));
        }
        let tray = about_tray(&s, &g, Theme::default(), 2.0);
        let (buf, pw, ph) = crate::tray_raster::rasterize_tray(
            &tray.prims,
            (cols as f32 * cw) as u32,
            (panel_rows as f32 * ch) as u32,
            1.0,
            [22, 24, 30, 255],
        );
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, pw, ph);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(&buf).unwrap();
        }
        std::fs::write(&path, &out).unwrap();
    }
}
