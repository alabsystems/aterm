// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AppKit status item, mug button, template images, and popup menu.

use aterm_macos_ui::appkit::class;
use aterm_macos_ui::status_button;
use aterm_objc::send::{
    alloc, send_id, send_id_f64, send_id_id_sel_id, send_v, send_v_bool, send_v_id, send_v_isize,
    send_v_usize,
};
use aterm_objc::{CGSize, Id, Obj, Sel, autoreleasepool, sel};

use crate::icon;
use crate::menu::Row;

const VARIABLE_LENGTH: f64 = -1.0;
const IMAGE_ONLY: usize = 1;
const STATE_ON: isize = 1;
const STATE_OFF: isize = 0;
const POINTS: u32 = 18;
const SCALES: [u32; 2] = [1, 2];

/// The mug in the menu bar.
pub(super) struct StatusUi {
    item: Obj,
    button: Obj,
    menu: Obj,
    items: Vec<Option<Obj>>,
    rows: Vec<Row>,
    images: [Obj; 2],
    shown_lit: Option<bool>,
    shown_tip: Option<String>,
}

/// The menu and the button it drops from, retained for one [`pop_up`].
pub(super) struct Popup {
    menu: Obj,
    button: Obj,
}

/// Install the mug status item and menu in the system menu bar.
pub(super) fn install(
    target: Id,
    click: Sel,
    show: Sel,
    choose: Sel,
    rows: Vec<Row>,
) -> Option<StatusUi> {
    autoreleasepool(|_| {
        let images = [template_image(false)?, template_image(true)?];
        let menu = new_menu()?;
        let items = add_rows(menu.id(), target, choose, &rows);
        // SAFETY: main-thread AppKit sends on live receivers, each in its method's prototype; the
        // item and its button answer at +0 and are retained here, and the caller keeps `target`
        // (held weakly) alive, which answers `click` as `v@:@` and `show` as `B@:@`
        // (`status_button::wire_clicks`, `add_show_menu_action`).
        unsafe {
            let bar = send_id(class(c"NSStatusBar").as_id(), sel!(systemStatusBar));
            if bar.is_null() {
                return None;
            }
            let item = Obj::retain(send_id_f64(
                bar,
                sel!(statusItemWithLength:),
                VARIABLE_LENGTH,
            ))?;
            let button = Obj::retain(send_id(item.id(), sel!(button)))?;
            send_v_usize(button.id(), sel!(setImagePosition:), IMAGE_ONLY);
            status_button::wire_clicks(button.id(), target, click);
            if let Some(label) = aterm_objc::ns_string("aCaffeine") {
                send_v_id(button.id(), sel!(setAccessibilityLabel:), label.id());
            }
            status_button::add_show_menu_action(button.id(), target, show);
            Some(StatusUi {
                item,
                button,
                menu,
                items,
                rows,
                images,
                shown_lit: None,
                shown_tip: None,
            })
        }
    })
}

impl StatusUi {
    /// Show the full mug when `lit`, else the empty one, with `tip` as the hover.
    pub(super) fn show(&mut self, lit: bool, tip: &str) {
        autoreleasepool(|_| {
            if self.shown_lit != Some(lit) {
                let image = &self.images[usize::from(lit)];
                // SAFETY: `-setImage:` and `-setAccessibilityLabel:` are `v24@0:8@16` on the live
                // button, handed a live image this holds (the button retains it too) and a live
                // string it copies.
                unsafe {
                    send_v_id(self.button.id(), sel!(setImage:), image.id());
                    if let Some(label) = aterm_objc::ns_string(state_text(lit)) {
                        send_v_id(self.button.id(), sel!(setAccessibilityLabel:), label.id());
                    }
                }
                self.shown_lit = Some(lit);
            }
            if self.shown_tip.as_deref() != Some(tip)
                && let Some(text) = aterm_objc::ns_string(tip)
            {
                // SAFETY: `-setToolTip:` is `v24@0:8@16` on the live button, handed a live string
                // it copies.
                unsafe { send_v_id(self.button.id(), sel!(setToolTip:), text.id()) };
                self.shown_tip = Some(tip.to_string());
            }
        });
    }

    /// Update the menu to show `rows`, retitling in place when the shape matches.
    pub(super) fn refresh_menu(&mut self, target: Id, choose: Sel, rows: Vec<Row>) {
        if let Err(rows) = self.retitle_menu(rows) {
            autoreleasepool(|_| {
                // SAFETY: `-removeAllItems` is `v16@0:8` on the live menu.
                unsafe { send_v(self.menu.id(), sel!(removeAllItems)) };
                self.items = add_rows(self.menu.id(), target, choose, &rows);
            });
            self.rows = rows;
        }
    }

    /// Retitle and recheck the menu's items in place to show `rows`, or hand `rows` back when
    /// their shape differs from the shown rows' ([`changed_rows`]). It adds and removes no item,
    /// so it may run while the menu is open.
    pub(super) fn retitle_menu(&mut self, rows: Vec<Row>) -> Result<(), Vec<Row>> {
        let Some(changed) = changed_rows(&self.rows, &rows) else {
            return Err(rows);
        };
        autoreleasepool(|_| {
            for i in changed {
                if let (Some(Some(item)), Some(row)) = (self.items.get(i), rows.get(i)) {
                    update_item(item.id(), row);
                }
            }
        });
        self.rows = rows;
        Ok(())
    }

    /// Retain the menu and its button for [`pop_up`].
    pub(super) fn popup(&self) -> Popup {
        Popup {
            menu: self.menu.clone_retained(),
            button: self.button.clone_retained(),
        }
    }
}

impl Drop for StatusUi {
    fn drop(&mut self) {
        autoreleasepool(|_| {
            // SAFETY: `+systemStatusBar` is `@16@0:8` and `-removeStatusItem:` `v24@0:8@16`, handed
            // the item it made; the item is released after, with the rest.
            unsafe {
                let bar = send_id(class(c"NSStatusBar").as_id(), sel!(systemStatusBar));
                if !bar.is_null() {
                    send_v_id(bar, sel!(removeStatusItem:), self.item.id());
                }
            }
        });
    }
}

/// Show the menu below its button and return when it closes.
pub(super) fn pop_up(popup: &Popup) {
    // SAFETY: on the main thread (the caller is the target's click handler), with the live
    // menu and button `popup` retains across the call, and outside every borrow of the mug.
    unsafe { status_button::pop_up(popup.menu.id(), popup.button.id()) };
}

/// The rows of `now` that differ from `shown`, by index.
pub(super) fn changed_rows(shown: &[Row], now: &[Row]) -> Option<Vec<usize>> {
    let same_kind = |a: &Row, b: &Row| std::mem::discriminant(a) == std::mem::discriminant(b);
    if shown.len() != now.len() || !shown.iter().zip(now).all(|(a, b)| same_kind(a, b)) {
        return None;
    }
    Some(
        shown
            .iter()
            .zip(now)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, _)| i)
            .collect(),
    )
}

/// What the mug says it is, lit or not.
pub(super) const fn state_text(lit: bool) -> &'static str {
    if lit { "aCaffeine on" } else { "aCaffeine off" }
}

fn template_image(lit: bool) -> Option<Obj> {
    let side = icon::SIZE_POINTS;
    let pngs = SCALES.map(|scale| icon::png(lit, POINTS * scale));
    status_button::template_image(
        &pngs.each_ref().map(Vec::as_slice),
        CGSize {
            width: side,
            height: side,
        },
        state_text(lit),
    )
}

fn new_menu() -> Option<Obj> {
    // SAFETY: `-init` is `@16@0:8`, consuming the `+alloc` and answering +1;
    // `-setAutoenablesItems:` is `v20@0:8B16`.
    unsafe {
        let menu = Obj::from_owned(send_id(alloc(class(c"NSMenu")), sel!(init)))?;
        send_v_bool(menu.id(), sel!(setAutoenablesItems:), false);
        Some(menu)
    }
}

fn add_rows(menu: Id, target: Id, choose: Sel, rows: &[Row]) -> Vec<Option<Obj>> {
    rows.iter()
        .map(|row| match row {
            Row::Separator => {
                add_separator(menu);
                None
            }
            Row::Info(_) | Row::Action { .. } => {
                let item = new_item(row, target, choose)?;
                // SAFETY: `-addItem:` is `v24@0:8@16` on the live menu, which retains the item.
                unsafe { send_v_id(menu, sel!(addItem:), item.id()) };
                Some(item)
            }
        })
        .collect()
}

const fn parts(row: &Row) -> Option<(&str, Option<(isize, bool)>)> {
    match row {
        Row::Info(text) => Some((text.as_str(), None)),
        Row::Action {
            label,
            action,
            checked,
        } => Some((label.as_str(), Some((action.tag(), *checked)))),
        Row::Separator => None,
    }
}

fn new_item(row: &Row, target: Id, choose: Sel) -> Option<Obj> {
    let (title, tagged) = parts(row)?;
    let title = aterm_objc::ns_string(title)?;
    let empty = aterm_objc::ns_string("")?;
    let action = if tagged.is_some() { choose } else { Sel::NULL };
    // SAFETY: `-initWithTitle:action:keyEquivalent:` is `@40@0:8@16:24@32`, consuming the `+alloc`
    // and answering +1; a nil action is its "no action". `-setTarget:` (`v24@0:8@16`) holds
    // `target` weakly: the caller keeps it alive. `-setEnabled:` is `v20@0:8B16`, `-setTag:` and
    // `-setState:` `v24@0:8q16`.
    unsafe {
        let item = Obj::from_owned(send_id_id_sel_id(
            alloc(class(c"NSMenuItem")),
            sel!(initWithTitle:action:keyEquivalent:),
            title.id(),
            action,
            empty.id(),
        ))?;
        if let Some((tag, checked)) = tagged {
            send_v_id(item.id(), sel!(setTarget:), target);
            send_v_isize(item.id(), sel!(setTag:), tag);
            send_v_isize(item.id(), sel!(setState:), state(checked));
        }
        send_v_bool(item.id(), sel!(setEnabled:), tagged.is_some());
        Some(item)
    }
}

fn update_item(item: Id, row: &Row) {
    let Some((title, tagged)) = parts(row) else {
        return;
    };
    // SAFETY: `-setTitle:` is `v24@0:8@16`, handed a live string it copies; `-setTag:` and
    // `-setState:` are `v24@0:8q16`; all on the live item.
    unsafe {
        if let Some(text) = aterm_objc::ns_string(title) {
            send_v_id(item, sel!(setTitle:), text.id());
        }
        if let Some((tag, checked)) = tagged {
            send_v_isize(item, sel!(setTag:), tag);
            send_v_isize(item, sel!(setState:), state(checked));
        }
    }
}

const fn state(checked: bool) -> isize {
    if checked { STATE_ON } else { STATE_OFF }
}

fn add_separator(menu: Id) {
    // SAFETY: `+separatorItem` is `@16@0:8` and answers a fresh item at +0, inside the caller's
    // pool; `-addItem:` retains it into the live menu.
    unsafe {
        let separator = send_id(class(c"NSMenuItem").as_id(), sel!(separatorItem));
        if !separator.is_null() {
            send_v_id(menu, sel!(addItem:), separator);
        }
    }
}

#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;
