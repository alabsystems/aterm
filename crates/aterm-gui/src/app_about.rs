// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `App` glue for the in-window ABOUT dialog ([`crate::about`]): open/close (App ▸ About
//! aterm menu item, a keybinding, or the title-bar close dot / OK), the byline's SITE
//! LINK (a click opens it in the browser — activated on RELEASE like a native link, so
//! dragging off cancels and a drag from the link selects its text; the `o` key and the
//! a11y Link node reach the same open), pointer TEXT SELECTION (press anchors, drag
//! grows, release settles — honoring `copy_on_select`; the wash rides the repaint
//! fingerprint), and COPY (`Cmd-C` or a bare `c` while the dialog is front) — the
//! selection if one is live, else the whole provenance block — to the system clipboard
//! via the SAME [`crate::control::pbcopy`] the terminal Cmd-C uses.
//! Modelled on `app_settings.rs`; every mutator just `request_redraw`s — the state change
//! rides in `RepaintKey::settings_fp` via [`crate::WindowState::overlay_fp`], and the
//! native-window-styled card is built by the shared `splice_settings_panel`. Every
//! pointer path resolves through ONE [`Self::about_geom`] + [`crate::about::about_layout`]
//! — the SAME layout the painter draws — so a click/drag always lands on exactly what is
//! on screen.

use winit::window::CursorIcon;

use crate::App;
#[cfg(test)]
use crate::about::AboutState;
use crate::about::{AboutCursor, AboutHit};

impl App {
    /// Open the About dialog on the front window. Settings and About are MUTUALLY
    /// EXCLUSIVE modal overlays (they share the one card slot), so this closes Settings
    /// first. No-op if About is already open. Any in-flight pointer drag (divider
    /// resize / text selection) is settled FIRST: the modal steals the mouse, so a
    /// drag left un-settled would keep resizing/growing on every pointer motion
    /// underneath the open dialog (its release gets swallowed by the modal gate).
    #[cfg(test)]
    pub(crate) fn about_enter(&mut self) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        self.settle_pointer_drags(wid);
        if let Some(ws) = self.windows.get_mut(&wid)
            && ws.about().is_none()
        {
            // Mutual exclusion is STRUCTURAL now: assigning the one `overlay` slot drops
            // whatever Settings/Palette was there — no manual "clear the other two", and no
            // "hidden palette swallows keys under the About card" gate-ordering hazard.
            ws.overlay = Some(crate::overlay::Overlay::About(AboutState::new()));
            // The dialog owns the pointer now: drop the terminal's Cmd-hover link
            // affordance so a hand cursor left by `update_hover_cursor` can't sit
            // stale over the fresh card (whose own tracking starts from Default).
            ws.hover_pointer = false;
            if let Some(w) = &ws.os_window {
                w.set_cursor(CursorIcon::Default);
                w.request_redraw();
            }
        }
        // Refresh the accessibility tree so a screen reader sees the About dialog (no-op
        // without the `a11y-accesskit` feature / no attached adapter).
        self.overlay_a11y_update();
    }

    /// Close the About dialog on window `wid` (no-op if not open there). Takes the
    /// WINDOW, not "the front": the mouse/key gates fire per-window, and frontmost can
    /// lag the event window (tab detach, X11 focus races) — a front-based close would
    /// no-op on the wrong window and leave the visible dialog undismissable.
    pub(crate) fn about_exit(&mut self, wid: crate::WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid)
            && ws.about().is_some()
        {
            ws.overlay = None;
            // Match the arrow we set below: the terminal's Cmd-hover tracking is
            // change-detected off this flag, so leaving it stale would suppress the
            // next legitimate cursor flip.
            ws.hover_pointer = false;
            if let Some(w) = &ws.os_window {
                // The dialog's hover tracking may have left a link/I-beam pointer;
                // restore the arrow for the terminal underneath.
                w.set_cursor(CursorIcon::Default);
                w.request_redraw();
            }
        }
        // Publish the now-empty tree (the overlay closed).
        self.overlay_a11y_update();
    }

    /// The About dialog's live pointer geometry — `(geom, display scale)` built from
    /// the SAME per-window metrics the splice paints with (`win_*`, mixed-DPI safe),
    /// so every pointer/copy path resolves against the pixels actually on the glass.
    /// `None` when About is closed on `wid` (or the frame is degenerate).
    fn about_geom(&self, wid: crate::WindowId) -> Option<(crate::settings::SettingsGeom, f32)> {
        let ws = self.windows.get(&wid)?;
        ws.about()?;
        let transform = self.overlay_coordinate_transform(wid)?;
        Some((transform.geom, transform.scale))
    }

    /// Window-px → tray-px: strip the leading remainder bands (window→frame, W1),
    /// then the horizontal pad inset — and, on the y-axis, the effective top pad
    /// plus chrome headroom (the card is composited at
    /// `(pad, pad_top + head)`) — like every other pointer consumer.
    fn about_tray_xy(&self, wid: crate::WindowId, x: f64, y: f64) -> (f32, f32) {
        let Some(transform) = self.overlay_coordinate_transform(wid) else {
            return (-1.0, -1.0);
        };
        let (x, y) = self.window_to_frame(wid, x, y);
        (
            ((x - transform.origin_x) / f64::from(transform.scale)) as f32,
            ((y - transform.origin_y) / f64::from(transform.scale)) as f32,
        )
    }

    /// A left PRESS while the dialog is front: the close dot / OK close it, and
    /// anywhere on the card below the title bar ANCHORS a text-selection drag (a
    /// press on nothing clears any selection). A press on the site link ALSO anchors
    /// — plus ARMS a pending link click, so the link activates on RELEASE like a
    /// native link (drag off to cancel, or drag to select its text). The caller
    /// swallows the gesture — the dialog is modal.
    pub(crate) fn on_about_press(&mut self, wid: crate::WindowId) {
        let Some((geom, scale)) = self.about_geom(wid) else {
            return;
        };
        let (px, py) = self
            .windows
            .get(&wid)
            .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
        let (tx, ty) = self.about_tray_xy(wid, px, py);
        let hit = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.about())
            .and_then(|a| crate::about::about_hit(a, &geom, scale, tx, ty));
        if hit == Some(AboutHit::Close) {
            self.about_exit(wid);
            return;
        }
        let Some(a) = self.windows.get_mut(&wid).and_then(|ws| ws.about_mut()) else {
            return;
        };
        let l = crate::about::about_layout(a, &geom, scale);
        let (cx, cy, cw, ch) = l.card;
        // The content area (card minus title bar) is the text surface; the
        // title bar and the glass outside the card just deselect.
        if tx >= cx && tx < cx + cw && ty >= cy + l.title_h && ty < cy + ch {
            a.sel_begin(crate::about::about_pos_at(&l, tx, ty));
            if hit == Some(AboutHit::Site) {
                a.arm_link();
            }
        } else if !a.sel_clear() {
            return; // nothing changed — skip the repaint
        }
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
    }

    /// `CursorMoved` while the About dialog is open on `wid`: grow an in-flight
    /// selection drag, else track the hover cursor (Pointer over the site link,
    /// I-beam over selectable text — set only on a CHANGE). Returns `true` whenever
    /// the dialog is open: the modal swallows pointer motion from every terminal
    /// path below (grid hover, selection, PTY mouse reports).
    pub(crate) fn on_about_motion(&mut self, wid: crate::WindowId, x: f64, y: f64) -> bool {
        // Swallow whenever the dialog is OPEN — matching the press gate — even when
        // the geometry is transiently degenerate (`overlay_rows() == 0` right after
        // open / a zero-row window): motion must not leak hover state or PTY mouse
        // reports under a nominally-front modal that is still eating the clicks.
        if self.windows.get(&wid).is_none_or(|ws| ws.about().is_none()) {
            return false;
        }
        let Some((geom, scale)) = self.about_geom(wid) else {
            return true;
        };
        let (tx, ty) = self.about_tray_xy(wid, x, y);
        let Some((dragging, pos, mut cur)) =
            self.windows.get(&wid).and_then(|ws| ws.about()).map(|a| {
                let l = crate::about::about_layout(a, &geom, scale);
                (
                    a.dragging(),
                    crate::about::about_pos_at(&l, tx, ty),
                    crate::about::about_cursor_at(&l, tx, ty),
                )
            })
        else {
            return true;
        };
        if dragging {
            let grew = self
                .windows
                .get_mut(&wid)
                .and_then(|ws| ws.about_mut())
                .is_some_and(|a| a.sel_extend(pos));
            if grew && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                w.request_redraw();
            }
            // Mid-sweep the gesture is TEXT wherever the pointer wanders (blank band,
            // buttons, off-card) — hold the I-beam, like a native text view.
            cur = AboutCursor::Text;
        }
        let changed = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.about_mut())
            .is_some_and(|a| {
                let flip = a.cursor != cur;
                a.cursor = cur;
                flip
            });
        if changed && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.set_cursor(match cur {
                AboutCursor::Default => CursorIcon::Default,
                AboutCursor::Pointer => CursorIcon::Pointer,
                AboutCursor::Text => CursorIcon::Text,
            });
        }
        true
    }

    /// A left RELEASE while the dialog is front: settle the selection drag. A
    /// no-motion click DESELECTS (the native text-view convention) — and if that
    /// click was ARMED on the site link and is still over it, it OPENS the browser
    /// (native link semantics: activate on release, drag off to cancel). A COMPLETED
    /// sweep honors `copy_on_select` (default on), same as a terminal selection — so
    /// select-then-paste needs no extra keystroke. Returns whether copy-on-select
    /// fired (the test seam, like the terminal's `finish_selection`).
    pub(crate) fn on_about_release(&mut self, wid: crate::WindowId) -> bool {
        // Resolve where the release LANDED before settling state (link check).
        let over_site = self.about_geom(wid).is_some_and(|(geom, scale)| {
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
            let (tx, ty) = self.about_tray_xy(wid, px, py);
            self.windows
                .get(&wid)
                .and_then(|ws| ws.about())
                .and_then(|a| crate::about::about_hit(a, &geom, scale, tx, ty))
                == Some(AboutHit::Site)
        });
        let Some(a) = self.windows.get_mut(&wid).and_then(|ws| ws.about_mut()) else {
            return false;
        };
        let was_dragging = a.dragging();
        let link_click = a.disarm_link();
        let cleared = a.sel_finish();
        if cleared && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
        if link_click && cleared && over_site {
            self.open_about_site(wid);
            return false;
        }
        let fired = was_dragging && !cleared && self.copy_on_select;
        if fired {
            self.about_copy(wid);
        }
        fired
    }

    /// Open the byline's site link in the browser. First-party URL from the build's
    /// own `site` row — still allowlisted through the same `is_safe_url` boundary as
    /// terminal links. Reached by the pointer (release on the link), the `o` key,
    /// and the a11y Link node's Click.
    pub(crate) fn open_about_site(&self, wid: crate::WindowId) {
        if let Some(url) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.about())
            .and_then(crate::about::site_url)
            && crate::is_safe_url(&url)
        {
            crate::app_mouse::open_url_external(&url);
        }
    }

    /// The text a copy while the dialog is front puts on the clipboard: the pointer
    /// SELECTION, or the whole provenance block when nothing is selected. Split from
    /// [`Self::about_copy`] so tests can assert the content without the OS clipboard.
    pub(crate) fn about_copy_text(&self, wid: crate::WindowId) -> String {
        self.about_geom(wid)
            .and_then(|(geom, scale)| {
                let a = self.windows.get(&wid)?.about()?;
                crate::about::about_selection_text(a, &geom, scale)
            })
            .unwrap_or_else(crate::about::provenance_text)
    }

    /// Put [`Self::about_copy_text`] on the clipboard via the same
    /// [`crate::control::pbcopy`] the terminal Cmd-C uses.
    pub(crate) fn about_copy(&self, wid: crate::WindowId) {
        crate::control::pbcopy(&self.about_copy_text(wid));
    }

    /// While the About dialog is open on `wid`, SWALLOW every key (return `true`); the
    /// simple info panel closes on `Esc` / `Enter` (the OK default), `Cmd-C` (or a
    /// bare `c`) copies the pointer selection — or, with none, the WHOLE provenance
    /// block — via [`Self::about_copy`], and `o` opens the byline's site link (the
    /// keyboard twin of the pointer/a11y link). Closed ⇒ `false` (keys flow
    /// normally). Mirrors `on_key_settings_mode`.
    #[cfg(test)]
    pub(crate) fn on_key_about_mode(
        &mut self,
        wid: crate::WindowId,
        ev: &winit::event::KeyEvent,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if self.windows.get(&wid).and_then(|ws| ws.about()).is_none() {
            return false;
        }
        if matches!(&ev.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("c")) {
            self.about_copy(wid);
            return true;
        }
        if matches!(&ev.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("o")) {
            self.open_about_site(wid);
            return true;
        }
        if matches!(
            &ev.logical_key,
            Key::Named(NamedKey::Escape | NamedKey::Enter)
        ) {
            self.about_exit(wid);
        }
        true
    }

    /// The ENGINE-NEUTRAL twin of [`Self::on_key_about_mode`] — reached by controller
    /// `key`/`text` verbs (introspection CONTROL of the overlay), mirroring
    /// `settings_input_event`. The caller still swallows the event from the PTY.
    #[cfg(test)]
    pub(crate) fn about_input_event(
        &mut self,
        wid: crate::WindowId,
        ev: &crate::input::InputEvent,
    ) {
        use crate::input::InputEvent;
        use aterm_types::keyboard::{Key as TKey, KeyEventType, NamedKey as TNamed};
        if self.windows.get(&wid).and_then(|ws| ws.about()).is_none() {
            return;
        }
        if let InputEvent::Key {
            key, event_type, ..
        } = ev
            && !matches!(event_type, KeyEventType::Release)
        {
            if matches!(key, TKey::Character(c) if c.eq_ignore_ascii_case(&'c')) {
                self.about_copy(wid);
                return;
            }
            if matches!(key, TKey::Character(c) if c.eq_ignore_ascii_case(&'o')) {
                self.open_about_site(wid);
                return;
            }
            if matches!(key, TKey::Named(TNamed::Escape | TNamed::Enter)) {
                self.about_exit(wid);
            }
        }
    }
}
