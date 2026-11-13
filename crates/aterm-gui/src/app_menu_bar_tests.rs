// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
