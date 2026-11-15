// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::run;
use super::*;

const HEADER: &str = "// SPDX-License-Identifier: Apache-2.0\n";

fn doors(src: &str) -> Vec<Finding> {
    run(&TEST_APPKIT_DOOR, &[("crates/aterm-gui/src/menu.rs", src)])
}

const DOOR_HELPER: &str = "fn add_separator(menu: &Obj) {
    unsafe {
        let sep = appkit::send_id(class(c\"NSMenuItem\").as_id(), sel!(separatorItem));
        appkit::send_v_id(menu.id(), sel!(addItem:), sep);
    }
}
";

#[test]
fn every_test_that_reaches_a_door_is_found_with_its_chain() {
    let cases: &[(&str, String, &str)] = &[
        (
            "a test calls the separator helper",
            format!(
                "{DOOR_HELPER}#[cfg(test)]\nmod tests {{\n    use super::*;\n\n    #[test]\n    fn a_new_row() {{\n        add_separator(&new_menu().expect(\"NSMenu\"));\n    }}\n}}\n"
            ),
            "add_separator -> +[NSMenuItem separatorItem]",
        ),
        (
            "a brace in a string ends no test",
            format!(
                "{DOOR_HELPER}#[cfg(test)]\nmod tests {{\n    #[test]\n    fn a_brace() {{\n        let label = \"}}\";\n        add_separator(&new_menu().expect(label));\n    }}\n}}\n"
            ),
            "add_separator -> +[NSMenuItem separatorItem]",
        ),
        (
            "an ignored test exempts nothing",
            format!(
                "{DOOR_HELPER}fn build_section(e: &[&str]) -> Option<Obj> {{\n    let menu = new_menu()?;\n    add_separator(&menu);\n    Some(menu)\n}}\n#[cfg(test)]\nmod tests {{\n    #[test]\n    #[ignore = \"needs a Mac\"]\n    fn the_section() {{\n        let menu = build_section(&[\"one\"]).expect(\"menu\");\n    }}\n}}\n"
            ),
            "build_section -> add_separator -> +[NSMenuItem separatorItem]",
        ),
        (
            "an ignore above the test exempts nothing",
            "fn item_for_identifier(identifier: Id) -> Id {\n    unsafe {\n        appkit::send_id_id(\n            appkit::alloc(class(c\"NSToolbarItem\")),\n            sel!(initWithItemIdentifier:),\n            identifier,\n        )\n    }\n}\n#[cfg(test)]\nmod tests {\n    #[ignore = \"builds AppKit objects\"]\n    #[test]\n    fn the_item() {\n        let item = item_for_identifier(ident);\n    }\n}\n".to_string(),
            "item_for_identifier -> NSToolbarItem construction",
        ),
        (
            "a method call reaches the door",
            "impl Chip {\n    fn set_help(&self, help: &str) {\n        unsafe { appkit::send_v_id(self.0.id(), sel!(setToolTip:), s.id()) };\n    }\n}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn a_chip() {\n        chip.set_help(\"New Tab\");\n    }\n}\n".to_string(),
            "set_help -> -setToolTip:",
        ),
        (
            "a test builds a toolbar item",
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn an_item() {\n        let item = unsafe {\n            appkit::send_id_id(\n                appkit::alloc(class(c\"NSToolbarItem\")),\n                sel!(initWithItemIdentifier:),\n            )\n        };\n    }\n}\n".to_string(),
            "NSToolbarItem construction, directly",
        ),
        (
            "a wrapper two hops up",
            "fn add_item_mods(menu: &Obj, title: &str, mods: usize) {\n    unsafe {\n        appkit::send_v_id(item.id(), sel!(setToolTip:), tip.id());\n    }\n}\n\nfn add_item(menu: &Obj, title: &str) {\n    add_item_mods(menu, title, 0);\n}\n\nfn build_section(entries: &[&str]) -> Option<Obj> {\n    for e in entries {\n        add_item(&menu, e);\n    }\n    Some(menu)\n}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn rows() {\n        let menu = super::build_section(&[\"one\"]).expect(\"menu\");\n    }\n}\n".to_string(),
            "build_section -> add_item -> add_item_mods -> -setToolTip:",
        ),
        (
            "a test helper reaches the door",
            "fn stamp_native_tip(item: Id, action: MenuAction) {\n    unsafe { appkit::send_v_id(item, sel!(setToolTip:), tip.id()) };\n}\n#[cfg(test)]\nmod tests {\n    fn menu_with_tips(actions: &[MenuAction]) -> Vec<Id> {\n        for a in actions {\n            stamp_native_tip(x, *a);\n        }\n        items\n    }\n\n    #[test]\n    fn the_halt_pair() {\n        let items = menu_with_tips(&[MenuAction::HoldSession]);\n    }\n}\n".to_string(),
            "menu_with_tips -> stamp_native_tip -> -setToolTip:",
        ),
        (
            "a test sends the door itself",
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn shared() {\n        let a = unsafe { appkit::send_id(class(c\"NSMenuItem\").as_id(), sel!(separatorItem)) };\n    }\n}\n".to_string(),
            "+[NSMenuItem separatorItem], directly",
        ),
        (
            "a module's name exempts nothing",
            format!(
                "{DOOR_HELPER}#[cfg(test)]\nmod tests {{\n    mod window_server {{\n        #[test]\n        fn a_row() {{\n            add_separator(&new_menu().expect(\"NSMenu\"));\n        }}\n    }}\n}}\n"
            ),
            "add_separator -> +[NSMenuItem separatorItem]",
        ),
    ];
    for (why, src, chain) in cases {
        let found = doors(&format!("{HEADER}{src}"));
        assert_eq!(found.len(), 1, "MISS or extra — {why}: {found:?}\n{src}");
        assert!(
            found[0].detail.ends_with(chain),
            "{why}: wrong chain {:?}, wanted {chain}",
            found[0].detail
        );
    }
}

#[test]
fn production_doors_and_tests_that_touch_none_are_silent() {
    let src = format!(
        "{HEADER}{DOOR_HELPER}fn new_item(title: &str) -> Option<Obj> {{\n    unsafe {{ send(class(c\"NSMenuItem\"), sel!(initWithTitle:)) }}\n}}\n\
         fn build_section(e: &[&str]) -> Option<Obj> {{\n    add_separator(&menu);\n    Some(menu)\n}}\n\
         #[cfg(test)]\nmod tests {{\n    use super::*;\n\n    // add_separator(&menu) and sel!(setToolTip:) in prose\n\
         #[test]\n    fn plain() {{\n        let item = new_item(\"row\").expect(\"item\");\n        let why = \"add_separator( would; sel!(separatorItem) too\";\n    }}\n\n\
         #[test]\n    fn three_args() {{\n        let enc = unsafe {{ method_types(cls, sel!(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:)) }};\n        let s: unsafe extern \"C\" fn(Id, Sel) -> Id = msg();\n    }}\n}}\n"
    );
    let found = doors(&src);
    assert!(found.is_empty(), "CRIES WOLF: {found:?}");
}
