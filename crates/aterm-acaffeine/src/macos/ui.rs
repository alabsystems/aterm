// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AppKit status item, mug button, template images, and popup menu.

use aterm_macos_ui::appkit::class;
use aterm_objc::send::{
    alloc, send_bool, send_id, send_id_cptr_usize, send_id_f64, send_id_id, send_id_id_id_sel,
    send_id_id_sel_id, send_id_size, send_isize_usize, send_rect, send_v, send_v_bool, send_v_id,
    send_v_isize, send_v_sel, send_v_size, send_v_usize,
};
use aterm_objc::{Bool, CGPoint, CGRect, CGSize, Id, Obj, Sel, autoreleasepool, sel};

use crate::icon;
use crate::menu::Row;

const VARIABLE_LENGTH: f64 = -1.0;
const IMAGE_ONLY: usize = 1;
const CLICK_MASK: usize = (1 << 2) | (1 << 4);
const STATE_ON: isize = 1;
const STATE_OFF: isize = 0;
const POINTS: u32 = 18;
const SCALES: [u32; 2] = [1, 2];
const POPUP_GAP: f64 = 3.0;
/// The button's custom accessibility action, which opens the menu.
pub(super) const SHOW_MENU: &str = "Show Menu";

type PopUpFn = unsafe extern "C-unwind" fn(Id, Sel, Id, CGPoint, Id) -> Bool;

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
        // (held weakly) alive.
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
            send_v_id(button.id(), sel!(setTarget:), target);
            send_v_sel(button.id(), sel!(setAction:), click);
            let _ = send_isize_usize(button.id(), sel!(sendActionOn:), CLICK_MASK);
            if let Some(label) = aterm_objc::ns_string("aCaffeine") {
                send_v_id(button.id(), sel!(setAccessibilityLabel:), label.id());
            }
            set_show_menu_action(button.id(), target, show);
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
    autoreleasepool(|_| {
        // SAFETY: main-thread AppKit sends on the live menu and button `popup` retains.
        // `-highlight:` is `v20@0:8B16`, `-bounds` `{CGRect=…}16@0:8`, `-isFlipped` `B16@0:8`, and
        // the pop-up `PopUpFn`'s prototype: a nil positioning item puts the menu's top-left corner
        // at the point, in the button's coordinates. It runs the menu's tracking loop and answers
        // whether an item was chosen.
        unsafe {
            let button = popup.button.id();
            send_v_bool(button, sel!(highlight:), true);
            let at = popup_point(
                send_rect(button, sel!(bounds)),
                send_bool(button, sel!(isFlipped)),
            );
            let pop: PopUpFn = aterm_objc::checked_msg();
            let _ = pop(
                popup.menu.id(),
                sel!(popUpMenuPositioningItem:atLocation:inView:),
                Id::NIL,
                at,
                button,
            );
            send_v_bool(button, sel!(highlight:), false);
        }
    });
}

/// Where the menu's top-left corner goes for a button with `bounds`.
pub(super) fn popup_point(bounds: CGRect, flipped: bool) -> CGPoint {
    let y = if flipped {
        bounds.origin.y + bounds.size.height + POPUP_GAP
    } else {
        bounds.origin.y - POPUP_GAP
    };
    CGPoint {
        x: bounds.origin.x,
        y,
    }
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
    let size = CGSize {
        width: side,
        height: side,
    };
    // SAFETY: main-thread sends, inside the caller's pool; `-initWithSize:` is
    // `@32@0:8{CGSize=dd}16`, consuming the `+alloc` and answering +1.
    let image = unsafe {
        Obj::from_owned(send_id_size(
            alloc(class(c"NSImage")),
            sel!(initWithSize:),
            size,
        ))
    }?;
    for scale in SCALES {
        let rep = bitmap(&icon::png(lit, POINTS * scale))?;
        // SAFETY: `-setSize:` (`v32@0:8{CGSize=dd}16`) makes the bitmap's pixels that many points;
        // `-addRepresentation:` (`v24@0:8@16`) retains it into the live image.
        unsafe {
            send_v_size(rep.id(), sel!(setSize:), size);
            send_v_id(image.id(), sel!(addRepresentation:), rep.id());
        }
    }
    // SAFETY: `-setTemplate:` is `v20@0:8B16` and `-setAccessibilityDescription:` `v24@0:8@16`, on
    // the live image.
    unsafe {
        send_v_bool(image.id(), sel!(setTemplate:), true);
        if let Some(text) = aterm_objc::ns_string(state_text(lit)) {
            send_v_id(image.id(), sel!(setAccessibilityDescription:), text.id());
        }
    }
    Some(image)
}

fn bitmap(png: &[u8]) -> Option<Obj> {
    if png.is_empty() {
        return None;
    }
    autoreleasepool(|_| {
        // SAFETY: `-initWithBytes:length:` is `@32@0:8r^v16Q24`, copying the `png.len()` bytes for
        // the call and answering +1; `+imageRepWithData:` is `@24@0:8@16`, answering +0, retained
        // before this pool pops.
        unsafe {
            let data = Obj::from_owned(send_id_cptr_usize(
                alloc(class(c"NSData")),
                sel!(initWithBytes:length:),
                png.as_ptr().cast(),
                png.len(),
            ))?;
            Obj::retain(send_id_id(
                class(c"NSBitmapImageRep").as_id(),
                sel!(imageRepWithData:),
                data.id(),
            ))
        }
    })
}

fn set_show_menu_action(button: Id, target: Id, show: Sel) {
    let Some(name) = aterm_objc::ns_string(SHOW_MENU) else {
        return;
    };
    // SAFETY: main-thread AppKit sends on live receivers. `-initWithName:target:selector:` is
    // `@40@0:8@16@24:32` and consumes the +alloc (the target held weakly: the caller keeps it
    // alive); `+arrayWithObject:` is `@24@0:8@16` and answers +0; `-setAccessibilityCustomActions:`
    // is `v24@0:8@16` and copies the array.
    unsafe {
        let Some(action) = Obj::from_owned(send_id_id_id_sel(
            alloc(class(c"NSAccessibilityCustomAction")),
            sel!(initWithName:target:selector:),
            name.id(),
            target,
            show,
        )) else {
            return;
        };
        let actions = send_id_id(
            class(c"NSArray").as_id(),
            sel!(arrayWithObject:),
            action.id(),
        );
        if !actions.is_null() {
            send_v_id(button, sel!(setAccessibilityCustomActions:), actions);
        }
    }
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
