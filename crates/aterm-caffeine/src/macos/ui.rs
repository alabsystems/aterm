// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The status item's AppKit: the mug button, its two template images, the menu (built once,
//! retitled in place) and the pop-up that shows it.

use aterm_objc::send::{
    alloc, send_bool, send_id, send_id_cptr_usize, send_id_f64, send_id_id, send_id_id_id_sel,
    send_id_id_sel_id, send_id_size, send_isize_usize, send_rect, send_v, send_v_bool, send_v_id,
    send_v_isize, send_v_sel, send_v_size, send_v_usize,
};
use aterm_objc::{Bool, CGPoint, CGRect, CGSize, Id, Obj, Sel, autoreleasepool, class, sel};

use crate::icon;
use crate::menu::Row;

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

/// `NSVariableStatusItemLength`: as wide as the image.
const VARIABLE_LENGTH: f64 = -1.0;
/// `NSImageOnly` (`NSCellImagePosition`): the button shows its image and no title.
const IMAGE_ONLY: usize = 1;
/// `NSEventMaskLeftMouseUp | NSEventMaskRightMouseUp`: the button's action fires on either
/// button's release, and the target tells them apart.
const CLICK_MASK: usize = (1 << 2) | (1 << 4);
/// `NSControlStateValueOn` / `Off`: a menu item's checkmark.
const STATE_ON: isize = 1;
const STATE_OFF: isize = 0;
/// The icon's side in points ([`icon::SIZE_POINTS`]) and its backing scales: 1x and 2x
/// displays.
const POINTS: u32 = 18;
const SCALES: [u32; 2] = [1, 2];
/// How far below the button the menu's top edge sits, in points.
const POPUP_GAP: f64 = 3.0;
/// The button's custom accessibility action, which opens the menu: VoiceOver lists it among the
/// item's actions (VO-Command-Space).
pub(super) const SHOW_MENU: &str = "Show Menu";

/// `-[NSMenu popUpMenuPositioningItem:atLocation:inView:]`, `B48@0:8@16{CGPoint=dd}24@40`:
/// no helper has this shape.
type PopUpFn = unsafe extern "C-unwind" fn(Id, Sel, Id, CGPoint, Id) -> Bool;

/// The mug in the menu bar.
pub(super) struct StatusUi {
    /// The bar item; releasing it would also take it off the bar.
    item: Obj,
    /// Its button, retained so a pop-up can hold it past the borrow it was read under.
    button: Obj,
    /// The menu, built once by [`install`].
    menu: Obj,
    /// The menu's items as built, one per row (`None` for a separator).
    items: Vec<Option<Obj>>,
    /// The rows the items show now.
    rows: Vec<Row>,
    /// The off (empty mug) and lit (full mug) template images.
    images: [Obj; 2],
    /// Which image the button shows, once one is set.
    shown_lit: Option<bool>,
    /// The tooltip the button shows, once one is set.
    shown_tip: Option<String>,
}

/// The menu and the button it drops from, retained for one [`pop_up`].
pub(super) struct Popup {
    menu: Obj,
    button: Obj,
}

/// Put the mug in the menu bar: a variable-length item whose button sends `click` to `target` on a
/// left or right release and `show` (`- (BOOL)x:(id)action`) for its [`SHOW_MENU`] accessibility
/// action, and the menu of `rows` whose items send `choose` to `target` with their action's tag.
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
            if let Some(label) = aterm_objc::ns_string("Caffeine") {
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

    /// Make the menu show `rows`: the rows that changed are retitled (and their checkmark and tag
    /// set) in place; a menu of another shape is built again, items sending `choose` to `target`.
    pub(super) fn refresh_menu(&mut self, target: Id, choose: Sel, rows: Vec<Row>) {
        autoreleasepool(|_| match changed_rows(&self.rows, &rows) {
            Some(changed) => {
                for i in changed {
                    if let (Some(Some(item)), Some(row)) = (self.items.get(i), rows.get(i)) {
                        update_item(item.id(), row);
                    }
                }
            }
            None => {
                // SAFETY: `-removeAllItems` is `v16@0:8` on the live menu.
                unsafe { send_v(self.menu.id(), sel!(removeAllItems)) };
                self.items = add_rows(self.menu.id(), target, choose, &rows);
            }
        });
        self.rows = rows;
    }

    /// The menu and its button, retained for [`pop_up`], which runs while no borrow of the app's
    /// state is held (a choice in the menu changes that state).
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

/// Show the menu just below its button, the button highlighted while it is open, and return when it
/// closes (a choice has been sent to its target by then or will be from the run loop).
pub(super) fn pop_up(popup: &Popup) {
    autoreleasepool(|_| {
        // SAFETY: main-thread AppKit sends on the live menu and button `popup` retains, each in its
        // method's prototype (`PopUpFn` for the pop-up).
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

/// Where the menu's top-left corner goes for a button with `bounds`: its left edge, a gap below its
/// bottom edge, which is the larger y in a flipped view and the smaller in an unflipped one.
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

/// What the mug says it is, lit or not: its image's description and the button's accessibility
/// label once it shows one ([`StatusUi::show`]).
pub(super) const fn state_text(lit: bool) -> &'static str {
    if lit { "Caffeine on" } else { "Caffeine off" }
}

/// A template image of the mug, lit or not, from the icon's 1x and 2x PNGs: an `NSImage`
/// (+1) of [`icon::SIZE_POINTS`] holding one bitmap per scale, marked as a template so the
/// menu bar tints it for its appearance and for the highlighted state.
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

/// `png` decoded into an `NSBitmapImageRep` (retained, +1); `None` when it is empty or
/// ImageIO cannot read it. Raster work, no WindowServer.
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

/// Give `button` the one custom accessibility action [`SHOW_MENU`], which sends `target`
/// `show`. A refusal leaves the button without it (the clicks still work).
fn set_show_menu_action(button: Id, target: Id, show: Sel) {
    let Some(name) = aterm_objc::ns_string(SHOW_MENU) else {
        return;
    };
    // SAFETY: main-thread AppKit sends on live receivers.
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

/// A fresh `NSMenu` (+1) whose items' enabled flags are the ones set here.
fn new_menu() -> Option<Obj> {
    // SAFETY: `-init` is `@16@0:8`, consuming the `+alloc` and answering +1;
    // `-setAutoenablesItems:` is `v20@0:8B16`.
    unsafe {
        let menu = Obj::from_owned(send_id(alloc(class(c"NSMenu")), sel!(init)))?;
        send_v_bool(menu.id(), sel!(setAutoenablesItems:), false);
        Some(menu)
    }
}

/// Append `rows` to `menu`: the items, one per row as built (`None` for a separator or
/// a row Foundation refused).
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

/// A row's title, and an action row's tag and checkmark; `None` for a separator.
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

/// An `NSMenuItem` (+1) for an info or action row: an info row has no action and is
/// disabled; an action row sends `choose` to `target` with its tag.
fn new_item(row: &Row, target: Id, choose: Sel) -> Option<Obj> {
    let (title, tagged) = parts(row)?;
    let title = aterm_objc::ns_string(title)?;
    let empty = aterm_objc::ns_string("")?;
    let action = if tagged.is_some() { choose } else { Sel::NULL };
    // SAFETY: `-initWithTitle:action:keyEquivalent:` is `@40@0:8@16:24@32`, consuming the `+alloc`
    // and answering +1; a nil action is its "no action". Both strings are live and copied.
    // `-setTarget:` is `v24@0:8@16` (held weakly: the caller keeps `target` alive), `-setEnabled:`
    // `v20@0:8B16`, `-setTag:` and `-setState:` `v24@0:8q16`.
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

/// Give `item` `row`'s title, and an action row's tag and checkmark.
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

/// The `NSControlStateValue` of a checkmark.
const fn state(checked: bool) -> isize {
    if checked { STATE_ON } else { STATE_OFF }
}

/// Append a separator line.
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
mod tests {
    use aterm_objc::Encode;

    use super::*;
    use crate::launchd::Startup;
    use crate::menu::{self, MenuAction};
    use crate::model::{Choice, Mode};

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect {
            origin: CGPoint { x, y },
            size: CGSize {
                width: w,
                height: h,
            },
        }
    }

    #[test]
    fn the_image_is_the_icons_size() {
        assert_eq!(f64::from(POINTS), icon::SIZE_POINTS);
    }

    /// The icon's PNGs decode, through the path the menu-bar images take, into bitmaps of
    /// 18 and 36 pixels a side (raster work only, as aterm-gui's wallpaper decode through
    /// `NSBitmapImageRep` in its tests); what is not one does not.
    #[test]
    fn the_icons_pngs_decode_to_a_bitmap_per_scale() {
        use aterm_objc::send::send_isize;
        for lit in [false, true] {
            for scale in SCALES {
                let px = POINTS * scale;
                let rep = bitmap(&icon::png(lit, px)).expect("decodes");
                // SAFETY: `-pixelsWide` and `-pixelsHigh` are `q16@0:8` on the live bitmap.
                let (w, h) = unsafe {
                    (
                        send_isize(rep.id(), sel!(pixelsWide)),
                        send_isize(rep.id(), sel!(pixelsHigh)),
                    )
                };
                let px = isize::try_from(px).expect("px");
                assert_eq!((w, h), (px, px), "lit={lit} scale={scale}");
            }
        }
        assert!(bitmap(&[]).is_none());
        assert!(bitmap(b"not a png").is_none());
    }

    #[test]
    fn the_menu_drops_from_the_buttons_bottom_left_corner() {
        let flipped = popup_point(rect(0.0, 0.0, 24.0, 22.0), true);
        assert_eq!((flipped.x, flipped.y), (0.0, 22.0 + POPUP_GAP));
        let unflipped = popup_point(rect(0.0, 0.0, 24.0, 22.0), false);
        assert_eq!((unflipped.x, unflipped.y), (0.0, -POPUP_GAP));
        let moved = popup_point(rect(5.0, 2.0, 24.0, 22.0), true);
        assert_eq!((moved.x, moved.y), (5.0, 24.0 + POPUP_GAP));
        let moved = popup_point(rect(5.0, 2.0, 24.0, 22.0), false);
        assert_eq!((moved.x, moved.y), (5.0, 2.0 - POPUP_GAP));
    }

    #[test]
    fn a_menu_of_the_same_shape_is_retitled_in_place() {
        const NOW: u64 = 1_790_000_000;
        let off = menu::compose(Mode::Off, NOW, Startup::Off, "1");
        assert_eq!(changed_rows(&off, &off), Some(vec![]));
        let forever = menu::compose(Mode::Forever, NOW, Startup::Off, "1");
        let forever_row = off
            .iter()
            .position(|r| {
                matches!(
                    r,
                    Row::Action {
                        action: MenuAction::Activate(Choice::Forever),
                        ..
                    }
                )
            })
            .expect("forever row");
        assert_eq!(changed_rows(&off, &forever), Some(vec![forever_row]));
        let timed = Mode::chosen(Choice::Hours1, NOW);
        let a = menu::compose(timed, NOW, Startup::On, "1");
        let b = menu::compose(timed, NOW + 60, Startup::On, "1");
        assert_eq!(changed_rows(&a, &b).map(|c| c.len()), Some(1));
        assert_eq!(changed_rows(&off, &off[1..]), None);
        let mut kinds = off.clone();
        kinds[1] = Row::Info("x".into());
        assert_eq!(changed_rows(&off, &kinds), None);
    }

    /// `ret@:args`, the offset-free encoding a prototype has.
    fn types(ret: &str, args: &[&str]) -> String {
        format!("{ret}@:{}", args.concat())
    }

    /// The runtime's encoding of `sel` on the class named `cls` (on its metaclass for a
    /// class method), offsets stripped.
    fn runtime(cls: &'static std::ffi::CStr, sel: Sel, class_method: bool) -> Option<String> {
        let cls = class(cls);
        assert!(!cls.is_null(), "{cls:?}");
        // SAFETY: `cls` is a live, registered class, and so is its metaclass; reading a method's
        // encoding has no side effects and opens no WindowServer connection.
        unsafe {
            let cls = if class_method {
                aterm_objc::class_of(cls.as_id())
            } else {
                cls
            };
            aterm_objc::method_types(cls, sel).map(|t| aterm_objc::strip_method_offsets(&t))
        }
    }

    /// Every prototype this file sends with, against what the runtime says the selector
    /// takes: a wrong width or sign here would corrupt registers in an app no test runs.
    #[test]
    fn every_send_here_has_the_prototype_the_runtime_reports() {
        let (v, id, b, sel_, q, uq, d) = (
            <() as Encode>::ENCODING,
            Id::ENCODING,
            Bool::ENCODING,
            Sel::ENCODING,
            isize::ENCODING,
            usize::ENCODING,
            f64::ENCODING,
        );
        let (pt, sz, rc) = (CGPoint::ENCODING, CGSize::ENCODING, CGRect::ENCODING);
        let rows: &[(&'static std::ffi::CStr, Sel, bool, String)] = &[
            (c"NSStatusBar", sel!(systemStatusBar), true, types(id, &[])),
            (
                c"NSStatusBar",
                sel!(statusItemWithLength:),
                false,
                types(id, &[d]),
            ),
            (
                c"NSStatusBar",
                sel!(removeStatusItem:),
                false,
                types(v, &[id]),
            ),
            (c"NSStatusItem", sel!(button), false, types(id, &[])),
            (
                c"NSStatusBarButton",
                sel!(setImagePosition:),
                false,
                types(v, &[uq]),
            ),
            (
                c"NSStatusBarButton",
                sel!(setTarget:),
                false,
                types(v, &[id]),
            ),
            (
                c"NSStatusBarButton",
                sel!(setAction:),
                false,
                types(v, &[sel_]),
            ),
            (
                c"NSStatusBarButton",
                sel!(sendActionOn:),
                false,
                types(q, &[uq]),
            ),
            (
                c"NSStatusBarButton",
                sel!(setAccessibilityLabel:),
                false,
                types(v, &[id]),
            ),
            (
                c"NSStatusBarButton",
                sel!(setAccessibilityCustomActions:),
                false,
                types(v, &[id]),
            ),
            (
                c"NSAccessibilityCustomAction",
                sel!(initWithName:target:selector:),
                false,
                types(id, &[id, id, sel_]),
            ),
            (c"NSArray", sel!(arrayWithObject:), true, types(id, &[id])),
            (
                c"NSStatusBarButton",
                sel!(setImage:),
                false,
                types(v, &[id]),
            ),
            (
                c"NSStatusBarButton",
                sel!(highlight:),
                false,
                types(v, &[b]),
            ),
            (c"NSStatusBarButton", sel!(bounds), false, types(rc, &[])),
            (c"NSStatusBarButton", sel!(isFlipped), false, types(b, &[])),
            (
                c"NSMenu",
                sel!(popUpMenuPositioningItem:atLocation:inView:),
                false,
                types(b, &[id, pt, id]),
            ),
            (c"NSMenu", sel!(init), false, types(id, &[])),
            (c"NSMenu", sel!(setAutoenablesItems:), false, types(v, &[b])),
            (c"NSMenu", sel!(addItem:), false, types(v, &[id])),
            (c"NSMenu", sel!(removeAllItems), false, types(v, &[])),
            (
                c"NSMenuItem",
                sel!(initWithTitle:action:keyEquivalent:),
                false,
                types(id, &[id, sel_, id]),
            ),
            (c"NSMenuItem", sel!(setTarget:), false, types(v, &[id])),
            (c"NSMenuItem", sel!(setEnabled:), false, types(v, &[b])),
            (c"NSMenuItem", sel!(setTitle:), false, types(v, &[id])),
            (c"NSMenuItem", sel!(setTag:), false, types(v, &[q])),
            (c"NSMenuItem", sel!(setState:), false, types(v, &[q])),
            (c"NSImage", sel!(initWithSize:), false, types(id, &[sz])),
            (c"NSImage", sel!(addRepresentation:), false, types(v, &[id])),
            (c"NSImage", sel!(setTemplate:), false, types(v, &[b])),
            (
                c"NSImage",
                sel!(setAccessibilityDescription:),
                false,
                types(v, &[id]),
            ),
            (
                c"NSBitmapImageRep",
                sel!(imageRepWithData:),
                true,
                types(id, &[id]),
            ),
            (c"NSBitmapImageRep", sel!(setSize:), false, types(v, &[sz])),
            (
                c"NSData",
                sel!(initWithBytes:length:),
                false,
                types(id, &["r^v", uq]),
            ),
        ];
        for (cls, sel, class_method, want) in rows {
            assert_eq!(
                runtime(cls, *sel, *class_method).as_deref(),
                Some(want.as_str()),
                "{cls:?} {sel:?}"
            );
        }
    }
}
