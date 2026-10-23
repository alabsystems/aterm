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

/// The menu bar of a windowed instance, read off AppKit (`NSApplication.mainMenu`): one
/// `menu "<title>": <items>` line per top-level menu, a submenu as its own line after its
/// parent's.
#[cfg(not(test))]
fn read_app_menu_bar(mtm: MainThread) -> Vec<String> {
    fn menu_item_title(item: aterm_objc::Id) -> String {
        use aterm_objc::sel;
        // SAFETY: `item` is a live `NSMenuItem` borrowed from its menu's `itemArray` inside the
        // caller's pool, on the main thread; `-tag` and `-title` are side-effect-free getters with
        // no preconditions, and the title is +0, copied out before the pool drains.
        let (tag, stamped) = unsafe {
            (
                crate::appkit::send_isize(item, sel!(tag)),
                crate::appkit::nsstring_to_rust(crate::appkit::send_id(item, sel!(title))),
            )
        };
        crate::menu::native_row_title(tag, stamped)
    }

    fn menu_item_unshown(item: aterm_objc::Id) -> bool {
        use aterm_objc::sel;
        // SAFETY: `item` is a live `NSMenuItem` borrowed from its menu's `itemArray` inside the
        // caller's pool, on the main thread; both getters are `-(BOOL)` accessors with no
        // preconditions.
        unsafe {
            crate::appkit::send_bool(item, sel!(isHidden))
                || crate::appkit::send_bool(item, sel!(isAlternate))
        }
    }

    use aterm_objc::{Id, autoreleasepool, class, sel};

    use crate::appkit;

    let Some(_windowed) = Windowed::get() else {
        return vec!["menu (none)".to_string()];
    };
    let mut out: Vec<String> = Vec::new();
    autoreleasepool(|_| {
        // SAFETY: `+sharedApplication` and `-mainMenu`/`-itemArray`/`-title`/`-submenu` are getters
        // with no preconditions beyond a live receiver, on the main thread (`mtm`); all are +0 and
        // borrowed for this pool. `+sharedApplication` creates the application object when there
        // is none, which makes a headless process a WindowServer client, so only a windowed
        // instance (`_windowed` above), whose loop built it at launch, may reach this.
        unsafe {
            let _: MainThread = mtm;
            let app = appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication));
            let main = if app.is_null() {
                Id::NIL
            } else {
                appkit::send_id(app, sel!(mainMenu))
            };
            if main.is_null() {
                out.push("menu (none)".to_string());
            } else {
                let tops = appkit::send_id(main, sel!(itemArray));
                let n = if tops.is_null() {
                    0
                } else {
                    appkit::send_usize(tops, sel!(count))
                };
                for i in 0..n {
                    let top = appkit::send_id_usize(tops, sel!(objectAtIndex:), i);
                    if top.is_null() {
                        continue;
                    }
                    let title = appkit::nsstring_to_rust(appkit::send_id(top, sel!(title)));
                    let sub = appkit::send_id(top, sel!(submenu));
                    if sub.is_null() {
                        out.push(format!("menu {title:?}: (no submenu)"));
                        continue;
                    }
                    let kids = appkit::send_id(sub, sel!(itemArray));
                    let kn = if kids.is_null() {
                        0
                    } else {
                        appkit::send_usize(kids, sel!(count))
                    };
                    let mut names: Vec<String> = Vec::with_capacity(kn);
                    let mut nested: Vec<String> = Vec::new();
                    for j in 0..kn {
                        let kid = appkit::send_id_usize(kids, sel!(objectAtIndex:), j);
                        if kid.is_null() {
                            continue;
                        }
                        let name = menu_item_title(kid);
                        if name.is_empty() || menu_item_unshown(kid) {
                            continue;
                        }
                        let sub = appkit::send_id(kid, sel!(submenu));
                        if sub.is_null() {
                            names.push(name);
                            continue;
                        }
                        names.push(format!("{name} \u{25b8}"));
                        let sub_title = format!("{title} \u{25b8} {name}");
                        let grand = appkit::send_id(sub, sel!(itemArray));
                        let gn = if grand.is_null() {
                            0
                        } else {
                            appkit::send_usize(grand, sel!(count))
                        };
                        let rows: Vec<String> = (0..gn)
                            .map(|g| {
                                let item = appkit::send_id_usize(grand, sel!(objectAtIndex:), g);
                                if item.is_null() || menu_item_unshown(item) {
                                    String::new()
                                } else {
                                    menu_item_title(item)
                                }
                            })
                            .filter(|t| !t.is_empty())
                            .collect();
                        nested.push(format!("menu {sub_title:?}: {}", rows.join(", ")));
                    }
                    out.push(format!("menu {title:?}: {}", names.join(", ")));
                    out.append(&mut nested);
                }
            }
        }
    });
    out
}

/// Menu bar reader for headless and unit-test builds.
pub(crate) fn no_app_menu_bar(_mtm: MainThread) -> Vec<String> {
    vec!["menu (none)".to_string()]
}
