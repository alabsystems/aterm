// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Application menu bar reader for the `chrome` control verb.

use crate::appkit::MainThread;
use crate::platform::Windowed;

/// Function signature for reading the application menu bar.
pub(crate) type MenuBarReader = fn(MainThread) -> Vec<String>;

/// Returns the AppKit menu bar reader for [`Windowed`] launches or [`no_app_menu_bar`] otherwise.
#[cfg(not(test))]
pub(crate) fn reader(windowed: Option<Windowed>) -> MenuBarReader {
    match windowed {
        Some(_) => read_app_menu_bar,
        None => no_app_menu_bar,
    }
}

/// Returns [`no_app_menu_bar`] in unit-test builds.
#[cfg(test)]
pub(crate) fn reader(_windowed: Option<Windowed>) -> MenuBarReader {
    no_app_menu_bar
}

/// Read the application menu bar from `NSApplication.mainMenu`.
#[cfg(not(test))]
fn read_app_menu_bar(mtm: MainThread) -> Vec<String> {
    use aterm_objc::{Id, autoreleasepool, class, sel};

    use crate::appkit;

    /// A live `NSMenuItem` borrowed from its menu's `itemArray` inside the caller's pool, on the
    /// main thread ([`menu_items`]).
    struct Item(Id);

    impl MenuItemRead for Item {
        fn title(&self) -> String {
            // SAFETY: `self.0` is a live `NSMenuItem` borrowed from its menu's `itemArray` inside
            // the caller's pool, on the main thread; `-tag` and `-title` are side-effect-free
            // getters with no preconditions, and the title is +0, copied out before the pool
            // drains.
            let (tag, stamped) = unsafe {
                (
                    appkit::send_isize(self.0, sel!(tag)),
                    appkit::nsstring_to_rust(appkit::send_id(self.0, sel!(title))),
                )
            };
            crate::menu::native_row_title(tag, stamped)
        }

        fn unshown(&self) -> bool {
            // SAFETY: `self.0` is a live `NSMenuItem` borrowed from its menu's `itemArray` inside
            // the caller's pool, on the main thread; both getters are `-(BOOL)` accessors with no
            // preconditions.
            unsafe {
                appkit::send_bool(self.0, sel!(isHidden))
                    || appkit::send_bool(self.0, sel!(isAlternate))
            }
        }

        fn submenu(&self) -> Option<Vec<Self>> {
            // SAFETY: `self.0` is a live `NSMenuItem` borrowed inside the caller's pool, on the
            // main thread; `-submenu` is a getter with no preconditions, +0 and borrowed for the
            // pool, so it is nil or a live `NSMenu` that pool outlives, as `menu_items` requires.
            unsafe { menu_items(appkit::send_id(self.0, sel!(submenu))) }
        }
    }

    /// The items of `menu`, a nil one skipped; `None` for a nil `menu`.
    ///
    /// # Safety
    ///
    /// `menu` is nil or a live `NSMenu`, read on the main thread inside an autorelease pool that
    /// outlives every returned [`Item`].
    unsafe fn menu_items(menu: Id) -> Option<Vec<Item>> {
        if menu.is_null() {
            return None;
        }
        // SAFETY: `menu` is a live `NSMenu` read +0 inside the caller's pool, on the main thread
        // (this function's contract);
        // `-itemArray`, `-count` and `-objectAtIndex:` below that count are getters with no
        // further preconditions, each +0 and borrowed for the pool.
        unsafe {
            let items = appkit::send_id(menu, sel!(itemArray));
            let n = if items.is_null() {
                0
            } else {
                appkit::send_usize(items, sel!(count))
            };
            Some(
                (0..n)
                    .map(|i| appkit::send_id_usize(items, sel!(objectAtIndex:), i))
                    .filter(|item| !item.is_null())
                    .map(Item)
                    .collect(),
            )
        }
    }

    let Some(_windowed) = Windowed::get() else {
        return vec!["menu (none)".to_string()];
    };
    let mut out: Vec<String> = Vec::new();
    autoreleasepool(|_| {
        // SAFETY: `+sharedApplication` and `-mainMenu`/`-title` are getters with no preconditions
        // beyond a live receiver, on the main thread (`mtm`); all are +0 and borrowed for this
        // pool, which outlives every read below. `+sharedApplication` creates the application
        // object when there is none, which makes a headless process a WindowServer client, so
        // only a windowed instance (`_windowed` above), whose loop built it at launch, may reach
        // this. `main` is nil or the live main menu, read in this pool, as `menu_items` requires.
        unsafe {
            let _: MainThread = mtm;
            let app = appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication));
            let main = if app.is_null() {
                Id::NIL
            } else {
                appkit::send_id(app, sel!(mainMenu))
            };
            out = match menu_items(main) {
                None => vec!["menu (none)".to_string()],
                Some(tops) => bar_lines(tops.into_iter().map(|top| {
                    let title = appkit::nsstring_to_rust(appkit::send_id(top.0, sel!(title)));
                    (title, top.submenu())
                })),
            };
        }
    });
    out
}

/// What [`bar_lines`] reads of a menu item.
trait MenuItemRead: Sized {
    /// Its title as its row shows it.
    fn title(&self) -> String;
    /// Whether it is hidden or an Option-alternate.
    fn unshown(&self) -> bool;
    /// Its submenu's items; `None` with no submenu.
    fn submenu(&self) -> Option<Vec<Self>>;
}

/// The `chrome` verb's `menu` lines for a menu bar whose top menus are `tops`, each its title and
/// its submenu's items (`None` with no submenu): a top menu's line lists its titled, shown items,
/// `▸` marking one with a submenu, and each such submenu's line, listing its own titled,
/// shown items, follows it.
fn bar_lines<I: MenuItemRead>(
    tops: impl IntoIterator<Item = (String, Option<Vec<I>>)>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (title, kids) in tops {
        let Some(kids) = kids else {
            out.push(format!("menu {title:?}: (no submenu)"));
            continue;
        };
        let mut names: Vec<String> = Vec::with_capacity(kids.len());
        let mut nested: Vec<String> = Vec::new();
        for kid in kids {
            let name = kid.title();
            if name.is_empty() || kid.unshown() {
                continue;
            }
            let Some(grand) = kid.submenu() else {
                names.push(name);
                continue;
            };
            names.push(format!("{name} \u{25b8}"));
            let sub_title = format!("{title} \u{25b8} {name}");
            let rows: Vec<String> = grand
                .iter()
                .map(|item| {
                    if item.unshown() {
                        String::new()
                    } else {
                        item.title()
                    }
                })
                .filter(|t| !t.is_empty())
                .collect();
            nested.push(format!("menu {sub_title:?}: {}", rows.join(", ")));
        }
        out.push(format!("menu {title:?}: {}", names.join(", ")));
        out.append(&mut nested);
    }
    out
}

/// Menu bar reader for headless and unit-test builds.
pub(crate) fn no_app_menu_bar(_mtm: MainThread) -> Vec<String> {
    vec!["menu (none)".to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A menu item as a test writes it: its title, whether it is unshown, its submenu.
    #[derive(Clone)]
    struct Row(&'static str, bool, Option<Vec<Row>>);

    impl MenuItemRead for Row {
        fn title(&self) -> String {
            self.0.to_owned()
        }

        fn unshown(&self) -> bool {
            self.1
        }

        fn submenu(&self) -> Option<Vec<Self>> {
            self.2.clone()
        }
    }

    /// The `chrome` verb lists each top menu's titled, shown items (a hidden or Option-alternate
    /// item and an untitled one left out), `▸` marking an item with a submenu, whose own
    /// line follows its menu's, in item order; a top item with no submenu says so, and one with
    /// an empty submenu lists nothing.
    #[test]
    fn the_menu_bar_lists_shown_items_and_each_submenu_after_its_menu() {
        let leaf = |title, unshown| Row(title, unshown, None);
        let find = vec![
            leaf("Find\u{2026}", false),
            leaf("Find Next", true),
            leaf("", false),
            leaf("Use Selection", false),
        ];
        let edit = vec![
            leaf("Copy", false),
            leaf("Close Pane", true),
            leaf("", false),
            Row("Find", false, Some(find)),
            Row("Gone", true, Some(vec![leaf("Under Gone", false)])),
            Row("AutoFill", false, Some(vec![leaf("Contact", false)])),
        ];
        assert_eq!(
            bar_lines([
                ("Edit".to_owned(), Some(edit)),
                ("Bare".to_owned(), None),
                ("Empty".to_owned(), Some(Vec::new())),
            ]),
            [
                "menu \"Edit\": Copy, Find \u{25b8}, AutoFill \u{25b8}",
                "menu \"Edit \u{25b8} Find\": Find\u{2026}, Use Selection",
                "menu \"Edit \u{25b8} AutoFill\": Contact",
                "menu \"Bare\": (no submenu)",
                "menu \"Empty\": ",
            ]
        );
    }
}
