// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The application menu bar the `chrome` verb's `menu …` lines read, and the
//! one place that decides whether AppKit is asked.
//!
//! The AppKit read's first send is `+[NSApplication sharedApplication]`, which
//! answers the application object a windowed loop built at launch and CREATES
//! one when there is none. A headless instance has none on purpose: its
//! display-free loop (`crates/aterm-window/src/platform_impl/macos/headless.rs`)
//! exists so the process never becomes a WindowServer client, and creating the
//! application object is exactly what makes it one (`-[NSApplication init]`
//! checks the process in with LaunchServices and opens its SkyLight connection,
//! and WindowServer then runs a synchronous TCC preflight of the binary on ITS
//! main thread, AGENTS.md). Measured 2026-09-26: an idle headless instance drew
//! no WindowServer preflight, and one `aterm ctl chrome` against it drew one,
//! from this read reached through `read_native_chrome`.
//!
//! So the AppKit reader is private, [`reader`] hands it out only for a
//! [`Windowed`] proof, which only a windowed launch mints, and a unit-test build
//! has no AppKit reader at all: the selection holds by construction.

use crate::appkit::MainThread;
use crate::platform::Windowed;

/// What an `App` reads the menu bar with ([`crate::App`]'s `app_menu_bar`).
pub(crate) type MenuBarReader = fn(MainThread) -> Vec<String>;

/// The reader a launch gets: AppKit's with the [`Windowed`] proof the windowed
/// launch minted, [`no_app_menu_bar`] without one (headless).
#[cfg(not(test))]
pub(crate) fn reader(windowed: Option<Windowed>) -> MenuBarReader {
    match windowed {
        Some(_) => read_app_menu_bar,
        None => no_app_menu_bar,
    }
}

/// A unit-test build reads no AppKit menu bar, whatever it is handed: every
/// `App` it builds answers `menu (none)`.
#[cfg(test)]
pub(crate) fn reader(_windowed: Option<Windowed>) -> MenuBarReader {
    no_app_menu_bar
}

/// The menu bar of a WINDOWED instance, read off AppKit
/// (`NSApplication.mainMenu`): one `menu "<title>": <items>` line per top-level
/// menu, a submenu as its own line after its parent's.
#[cfg(not(test))]
fn read_app_menu_bar(mtm: MainThread) -> Vec<String> {
    /// The title a row shows now: ⌘W's is live state that the item carries
    /// only as last stamped (`menu::native_row_title`).
    fn menu_item_title(item: aterm_objc::Id) -> String {
        use aterm_objc::sel;
        // SAFETY: `item` is a live `NSMenuItem` borrowed from its menu's
        // `itemArray` inside the caller's pool, on the main thread; `-tag`
        // and `-title` are side-effect-free getters with no preconditions,
        // and the title is +0, copied out before the pool drains.
        let (tag, stamped) = unsafe {
            (
                crate::appkit::send_isize(item, sel!(tag)),
                crate::appkit::nsstring_to_rust(crate::appkit::send_id(item, sel!(title))),
            )
        };
        crate::menu::native_row_title(tag, stamped)
    }

    /// A menu item the menu does not show: `-isHidden`, or an
    /// `-isAlternate` item standing in for its neighbour only while a
    /// modifier is held. Both are `-(BOOL)` getters with no side effect.
    fn menu_item_unshown(item: aterm_objc::Id) -> bool {
        use aterm_objc::sel;
        // SAFETY: `item` is a live `NSMenuItem` borrowed from its menu's
        // `itemArray` inside the caller's pool, on the main thread; both
        // getters are `-(BOOL)` accessors with no preconditions.
        unsafe {
            crate::appkit::send_bool(item, sel!(isHidden))
                || crate::appkit::send_bool(item, sel!(isAlternate))
        }
    }

    use aterm_objc::{Id, autoreleasepool, class, sel};

    use crate::appkit;

    // `reader` hands this function out only with the proof; holding it here as
    // well keeps the `+sharedApplication` below beside its own answer.
    let Some(_windowed) = Windowed::get() else {
        return vec!["menu (none)".to_string()];
    };
    let mut out: Vec<String> = Vec::new();
    // Every object read here is borrowed (+0) for this pool; only `String`s
    // escape it (`read_native_chrome`'s reason for its own pool, unchanged).
    autoreleasepool(|_| {
        // SAFETY: `+sharedApplication` and `-mainMenu`/`-itemArray`/`-title`/
        // `-submenu` are getters with no preconditions beyond a live receiver,
        // on the main thread (`mtm`). All are +0 and borrowed for this pool.
        // `+[NSApplication sharedApplication]` CREATES the instance on first
        // call, which is why `objc2` demanded the marker for it — that
        // obligation is why `mtm` is taken and consumed here rather than
        // dropped in the port — and why only a windowed instance, whose loop
        // created it at launch, may reach this (see the module doc).
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
                        // A top-level item with no submenu (uncommon for a bar).
                        out.push(format!("menu {title:?}: (no submenu)"));
                        continue;
                    }
                    let kids = appkit::send_id(sub, sel!(itemArray));
                    let kn = if kids.is_null() {
                        0
                    } else {
                        appkit::send_usize(kids, sel!(count))
                    };
                    // One level of SUBMENU (round 19, File ▸ Driving) prints
                    // exactly as the model serialiser prints it
                    // (`menu::menu_chrome_lines`): `<label> ▸` in this line,
                    // then the submenu as its own `menu "<parent> ▸ <label>"`
                    // line right after.
                    let mut names: Vec<String> = Vec::with_capacity(kn);
                    let mut nested: Vec<String> = Vec::new();
                    for j in 0..kn {
                        let kid = appkit::send_id_usize(kids, sel!(objectAtIndex:), j);
                        if kid.is_null() {
                            continue;
                        }
                        let name = menu_item_title(kid);
                        // Skip separators (empty title) so the listing reads as
                        // the command set, not the dividers — and what the
                        // menu does not SHOW (ruling 267): AppKit keeps hidden
                        // items and Option-key alternates in `itemArray`
                        // (the Edit menu's dictation and emoji entries read
                        // two and three times over).
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

/// The menu-bar reader for a HEADLESS instance and for every unit test:
/// `menu (none)`, the truth for a process with no menu bar, answered without
/// asking AppKit. The same injection as `keymap::no_lock_modifiers` /
/// `platform::no_recent_user_input_event`, for the same reason (AGENTS.md's
/// injected-field pattern). The in-app menu MODEL stays introspectable headless
/// through `controls menu`.
pub(crate) fn no_app_menu_bar(_mtm: MainThread) -> Vec<String> {
    vec!["menu (none)".to_string()]
}
