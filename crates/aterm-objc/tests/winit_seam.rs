// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Census of the rows `vendor/winit`'s macOS backend registers, checked
//! against the runtime's own authority.
//!
//! [`ROWS`] lists every declared method (all seventy-one are declared with
//! [`aterm_objc::declare_class!`]). Each row's authority is the live runtime:
//! `protocol_getMethodDescription` for a delegate protocol,
//! `class_getInstanceMethod` for an override of an AppKit class. The `winit`
//! column is the encoding the fork's Rust signature produces; the two are
//! compared modulo byte offsets.
//!
//! Guards against staleness:
//!
//! * [`the_table_still_covers_every_declared_method`] counts `@sel(…)` /
//!   `#[method(…)]` sites in the fork against the table.
//! * [`every_site_points_at_the_line_that_declares_it`] follows each `site:`
//!   into the fork and requires that line to declare the selector.
//! * [`the_ported_signatures_encode_to_the_authority`] re-derives the
//!   `window_delegate.rs` rows from compiled Rust signatures through a mirror
//!   class declared here.
//!
//! This file is a mirror, not the fork: it never loads the class `vendor/winit`
//! registers. `crates/aterm-gui/examples/objc_live_class_audit.rs` (an
//! `[[example]]`, since libtest cannot host AppKit) enumerates the registered
//! classes and finds each row's authority, but cannot see a deleted method;
//! the count here can.
//!
//! Findings pinned here: no row takes a block (`@?`) or a function pointer
//! (`^?`); the disagreement set is empty (`draggingEntered:` returns
//! `NSDragOperation`, `Q`); two rows have no runtime authority
//! (`frameDidChange:`, `insertBackTab:`), and
//! [`the_key_binding_family_is_measured_not_asserted`] checks why.
//!
//! The census `dlopen`s AppKit, since a bare test process does not load it.
//! Some hosts' AppKit does not register every protocol it declares
//! (`NSApplicationDelegate`); the product then supplies a name-only protocol
//! with no methods, which is never an authority. Rows whose authority the host
//! lacks are reported unchecked, and [`HOST_MAY_LACK`] names which protocols
//! may be absent, so a new absence still fails.

#![cfg(target_os = "macos")]

use std::ffi::{CString, c_void};

use aterm_objc::{
    Bool, ClassType, Encode, class, declare_class, method_types, protocol, protocol_method_types,
    sel, sel_uncached, strip_method_offsets,
};

#[path = "support/appkit.rs"]
mod appkit;
#[path = "support/winit_fork.rs"]
mod winit_fork;

use appkit::load_appkit;
use winit_fork::{fork_backend, fork_sources};

// ------------------------------------------------------------------ the table

/// Where a row's TRUE encoding lives.
#[derive(Clone, Copy, Debug)]
enum Authority {
    /// A protocol declares it (required or optional table).
    Proto(&'static str),
    /// A real AppKit class implements it and the declared class overrides it.
    Class(&'static str),
    /// Nothing in the runtime declares it — an informal convention. The string
    /// is the reason, printed in the census.
    None(&'static str),
}

/// One declared method of one winit class.
struct Row {
    /// `vendor/winit/src/platform_impl/macos/<file>:<line>`.
    site: &'static str,
    /// The Objective-C selector, exactly as `#[method(...)]` spells it.
    sel: &'static str,
    /// Where the runtime keeps the truth.
    authority: Authority,
    /// The encoding the PORT will register, from `vendor/winit`'s Rust
    /// signature TODAY. `B` is substituted with this target's `BOOL`.
    winit: &'static str,
}

/// Expand the one target-dependent letter: `BOOL` is `"B"` on arm64 and `"c"`
/// on x86_64.
fn expand(spec: &str) -> String {
    spec.replace('B', Bool::ENCODING)
}

/// Protocols a host's AppKit may not register although AppKit is loaded.
///
/// A row whose authority is on this list and absent is reported unchecked on
/// this host, never as agreement or failure.
/// [`the_host_allowance_is_exactly_what_this_host_lacks`] makes an absent
/// protocol not on the list fail by name.
const HOST_MAY_LACK: &[&str] = &["NSApplicationDelegate"];

/// Whether the host's own frameworks registered `name`: false when absent, or
/// when the only object under that name is the name-only one `aterm_objc`
/// supplied.
fn host_registers(name: &str) -> bool {
    let p = protocol(static_cstr(name));
    !p.is_null()
        && !aterm_objc::protocols_registered_by_aterm()
            .iter()
            .any(|n| n.to_str() == Ok(name))
}

/// What the runtime says about one row.
enum Reading {
    /// The authority declares it with this offset-free encoding.
    Declared(String),
    /// Nothing in the runtime declares it (an [`Authority::None`] row); the
    /// string is the written reason.
    NoAuthority(&'static str),
    /// The authority is a protocol this host's AppKit does not register; the
    /// string is the protocol.
    HostLacksProtocol(&'static str),
}

/// Asserts the unchecked rows are exactly the allowance: empty when the host
/// registers every protocol in [`HOST_MAY_LACK`], otherwise precisely the rows
/// whose authority it lacks (printed).
fn assert_host_lacking_is_the_allowance(host_lacking: &[(&str, &str, &str)]) {
    let expected: Vec<(&str, &str, &str)> = ROWS
        .iter()
        .filter_map(|r| match r.authority {
            Authority::Proto(name) if HOST_MAY_LACK.contains(&name) && !host_registers(name) => {
                Some((r.site, r.sel, name))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        host_lacking,
        &expected[..],
        "the rows this host cannot check are exactly the rows whose protocol it does not register"
    );
    if expected.is_empty() {
        return;
    }
    assert_eq!(
        expected.len(),
        3,
        "the application delegate's three rows, and no other"
    );
    for (site, s, name) in &expected {
        eprintln!(
            "winit_seam: {site} {s}: {name} is not registered by this host's AppKit — \
             unchecked here, checked on a host that registers it"
        );
    }
}

/// The declared rows, in source order per file.
/// [`the_table_still_covers_every_declared_method`] re-counts them each run.
#[rustfmt::skip]
const ROWS: &[Row] = &[
    // ---- ApplicationDelegate : NSObject <NSApplicationDelegate>
    Row { site: "app_state.rs:143",  sel: "applicationDidFinishLaunching:", authority: Authority::Proto("NSApplicationDelegate"), winit: "v@:@" },
    Row { site: "app_state.rs:148",  sel: "applicationWillTerminate:",      authority: Authority::Proto("NSApplicationDelegate"), winit: "v@:@" },
    Row { site: "app_state.rs:153",  sel: "applicationShouldTerminate:",    authority: Authority::Proto("NSApplicationDelegate"), winit: "Q@:@" },
    // ---- WinitWindow : NSWindow
    Row { site: "window.rs:214",    sel: "canBecomeMainWindow",            authority: Authority::Class("NSWindow"), winit: "B@:" },
    Row { site: "window.rs:220",    sel: "canBecomeKeyWindow",             authority: Authority::Class("NSWindow"), winit: "B@:" },
    // ---- WinitApplication : NSApplication
    // ---- WindowDelegate : NSObject <NSWindowDelegate, NSDraggingDestination>
    Row { site: "window_delegate.rs:231", sel: "windowShouldClose:",                 authority: Authority::Proto("NSWindowDelegate"), winit: "B@:@" },
    Row { site: "window_delegate.rs:238", sel: "windowWillClose:",                   authority: Authority::Proto("NSWindowDelegate"), winit: "v@:@" },
    Row { site: "window_delegate.rs:251", sel: "windowDidResize:",                   authority: Authority::Proto("NSWindowDelegate"), winit: "v@:@" },
    Row { site: "window_delegate.rs:258", sel: "windowWillStartLiveResize:",         authority: Authority::Proto("NSWindowDelegate"), winit: "v@:@" },
    Row { site: "window_delegate.rs:266", sel: "windowDidEndLiveResize:",            authority: Authority::Proto("NSWindowDelegate"), winit: "v@:@" },
    Row { site: "window_delegate.rs:273", sel: "windowDidMove:",                     authority: Authority::Proto("NSWindowDelegate"), winit: "v@:@" },
    Row { site: "window_delegate.rs:279", sel: "windowDidChangeBackingProperties:",  authority: Authority::Proto("NSWindowDelegate"), winit: "v@:@" },
    Row { site: "window_delegate.rs:295", sel: "windowDidBecomeKey:",                authority: Authority::Proto("NSWindowDelegate"), winit: "v@:@" },
    Row { site: "window_delegate.rs:303", sel: "windowDidResignKey:",                authority: Authority::Proto("NSWindowDelegate"), winit: "v@:@" },
    Row { site: "window_delegate.rs:328", sel: "windowWillEnterFullScreen:",         authority: Authority::Proto("NSWindowDelegate"), winit: "v@:@" },
    Row { site: "window_delegate.rs:355", sel: "windowWillExitFullScreen:",          authority: Authority::Proto("NSWindowDelegate"), winit: "v@:@" },
    Row { site: "window_delegate.rs:375", sel: "window:willUseFullScreenPresentationOptions:", authority: Authority::Proto("NSWindowDelegate"), winit: "Q@:@Q" },
    Row { site: "window_delegate.rs:402", sel: "windowDidEnterFullScreen:",          authority: Authority::Proto("NSWindowDelegate"), winit: "v@:@" },
    Row { site: "window_delegate.rs:429", sel: "windowDidExitFullScreen:",           authority: Authority::Proto("NSWindowDelegate"), winit: "v@:@" },
    Row { site: "window_delegate.rs:460", sel: "windowDidFailToEnterFullScreen:",    authority: Authority::Proto("NSWindowDelegate"), winit: "v@:@" },
    Row { site: "window_delegate.rs:484", sel: "windowDidChangeOcclusionState:",     authority: Authority::Proto("NSWindowDelegate"), winit: "v@:@" },
    Row { site: "window_delegate.rs:493", sel: "windowDidChangeScreen:",             authority: Authority::Proto("NSWindowDelegate"), winit: "v@:@" },
    // Returns `NSDragOperation` (`usize`), as `NSDraggingDestination` declares.
    Row { site: "window_delegate.rs:533", sel: "draggingEntered:",                   authority: Authority::Proto("NSDraggingDestination"), winit: "Q@:@" },
    Row { site: "window_delegate.rs:552", sel: "prepareForDragOperation:",           authority: Authority::Proto("NSDraggingDestination"), winit: "B@:@" },
    Row { site: "window_delegate.rs:559", sel: "performDragOperation:",              authority: Authority::Proto("NSDraggingDestination"), winit: "B@:@" },
    Row { site: "window_delegate.rs:575", sel: "concludeDragOperation:",             authority: Authority::Proto("NSDraggingDestination"), winit: "v@:@" },
    Row { site: "window_delegate.rs:581", sel: "draggingExited:",                    authority: Authority::Proto("NSDraggingDestination"), winit: "v@:@" },
    Row { site: "window_delegate.rs:593", sel: "observeValueForKeyPath:ofObject:change:context:", authority: Authority::Class("NSObject"), winit: "v@:@@@^v" },
    // ---- View : NSView <NSTextInputClient>
    Row { site: "view.rs:235", sel: "isFlipped",                    authority: Authority::Class("NSView"), winit: "B@:" },
    Row { site: "view.rs:241", sel: "viewDidMoveToWindow",          authority: Authority::Class("NSView"), winit: "v@:" },
    Row { site: "view.rs:247", sel: "frameDidChange:",              authority: Authority::None("an NSNotificationCenter callback registered by `WinitView::new`; no protocol declares it and no loaded class implements it — measured, not assumed. Upstream's Rust signature typed the argument `&NSEvent` when the notification centre passes an NSNotification (same `@`, wrong name); the port took it to `Id`, so the misnaming is closed and only the missing authority is left"), winit: "v@:@" },
    Row { site: "view.rs:260", sel: "drawRect:",                    authority: Authority::Class("NSView"), winit: "v@:{CGRect={CGPoint=dd}{CGSize=dd}}" },
    Row { site: "view.rs:273", sel: "acceptsFirstResponder",        authority: Authority::Class("NSResponder"), winit: "B@:" },
    Row { site: "view.rs:286", sel: "touchBar",                     authority: Authority::Class("NSResponder"), winit: "@@:" },
    Row { site: "view.rs:292", sel: "resetCursorRects",             authority: Authority::Class("NSView"), winit: "v@:" },
    Row { site: "view.rs:332", sel: "hasMarkedText",                authority: Authority::Proto("NSTextInputClient"), winit: "B@:" },
    Row { site: "view.rs:343", sel: "markedRange",                  authority: Authority::Proto("NSTextInputClient"), winit: "{_NSRange=QQ}@:" },
    Row { site: "view.rs:355", sel: "selectedRange",                authority: Authority::Proto("NSTextInputClient"), winit: "{_NSRange=QQ}@:" },
    Row { site: "view.rs:362", sel: "setMarkedText:selectedRange:replacementRange:", authority: Authority::Proto("NSTextInputClient"), winit: "v@:@{_NSRange=QQ}{_NSRange=QQ}" },
    Row { site: "view.rs:475", sel: "unmarkText",                   authority: Authority::Proto("NSTextInputClient"), winit: "v@:" },
    Row { site: "view.rs:511", sel: "validAttributesForMarkedText", authority: Authority::Proto("NSTextInputClient"), winit: "@@:" },
    Row { site: "view.rs:522", sel: "attributedSubstringForProposedRange:actualRange:", authority: Authority::Proto("NSTextInputClient"), winit: "@@:{_NSRange=QQ}^{_NSRange=QQ}" },
    Row { site: "view.rs:534", sel: "characterIndexForPoint:",      authority: Authority::Proto("NSTextInputClient"), winit: "Q@:{CGPoint=dd}" },
    Row { site: "view.rs:543", sel: "firstRectForCharacterRange:actualRange:", authority: Authority::Proto("NSTextInputClient"), winit: "{CGRect={CGPoint=dd}{CGSize=dd}}@:{_NSRange=QQ}^{_NSRange=QQ}" },
    Row { site: "view.rs:579", sel: "insertText:replacementRange:", authority: Authority::Proto("NSTextInputClient"), winit: "v@:@{_NSRange=QQ}" },
    Row { site: "view.rs:607", sel: "doCommandBySelector:",         authority: Authority::Proto("NSTextInputClient"), winit: "v@::" },
    Row { site: "view.rs:633", sel: "keyDown:",                     authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:712", sel: "keyUp:",                       authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:731", sel: "flagsChanged:",                authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:738", sel: "insertTab:",                   authority: Authority::Proto("NSStandardKeyBindingResponding"), winit: "v@:@" },
    Row { site: "view.rs:744", sel: "insertBackTab:",               authority: Authority::None("declared by no protocol and implemented by no loaded class — measured: NSStandardKeyBindingResponding declares its two siblings `insertTab:` and `cancelOperation:` and not this one, and NSResponder implements none of the three. The auditor DERIVES a shape for it from those siblings; this census only records that the runtime holds no description of its own"), winit: "v@:@" },
    Row { site: "view.rs:752", sel: "cancelOperation:",             authority: Authority::Proto("NSStandardKeyBindingResponding"), winit: "v@:@" },
    Row { site: "view.rs:826", sel: "mouseDown:",                   authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:833", sel: "mouseUp:",                     authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:840", sel: "rightMouseDown:",              authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:847", sel: "rightMouseUp:",                authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:854", sel: "otherMouseDown:",              authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:861", sel: "otherMouseUp:",                authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:870", sel: "mouseMoved:",                  authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:875", sel: "mouseDragged:",                authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:880", sel: "rightMouseDragged:",           authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:885", sel: "otherMouseDragged:",           authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:890", sel: "mouseEntered:",                authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:898", sel: "mouseExited:",                 authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:907", sel: "scrollWheel:",                 authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:967", sel: "magnifyWithEvent:",            authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:984", sel: "smartMagnifyWithEvent:",       authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:995", sel: "rotateWithEvent:",             authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:1014", sel: "pressureChangeWithEvent:",     authority: Authority::Class("NSResponder"), winit: "v@:@" },
    Row { site: "view.rs:1030", sel: "_wantsKeyDownForEvent:",       authority: Authority::Class("NSResponder"), winit: "B@:@" },
    Row { site: "view.rs:1037", sel: "acceptsFirstMouse:",           authority: Authority::Class("NSView"), winit: "B@:@" },
];

/// A `&'static CStr` from a table entry. `class` and `protocol` take
/// `&'static CStr`, so the census leaks one small allocation per name.
fn static_cstr(name: &str) -> &'static std::ffi::CStr {
    Box::leak(
        CString::new(name)
            .expect("no interior NUL")
            .into_boxed_c_str(),
    )
}

/// The runtime's own answer for one row, offset-free, or why there is none.
fn authority_encoding(row: &Row) -> Reading {
    let s = sel_uncached(static_cstr(row.sel));
    let raw = match row.authority {
        Authority::Proto(name) => {
            if !host_registers(name) {
                // Absence is an allowance only if AppKit is loaded, told apart
                // by a class AppKit always registers.
                assert!(
                    !class(c"NSApplication").is_null(),
                    "AppKit did not load, so this census would pass vacuously"
                );
                assert!(
                    HOST_MAY_LACK.contains(&name),
                    "protocol {name} is absent although AppKit is loaded — this host's \
                     AppKit does not register it, and the census has no allowance written \
                     for that; measure it and add it to HOST_MAY_LACK"
                );
                return Reading::HostLacksProtocol(name);
            }
            let p = protocol(static_cstr(name));
            // SAFETY: `p` is a live protocol object the host registered.
            let Some(t) = (unsafe { protocol_method_types(p, s, true) }) else {
                // A real protocol that does not declare the row is a census
                // failure, never a skip.
                panic!(
                    "{} {}: {name} is registered by this host and does not declare it",
                    row.site, row.sel
                );
            };
            t
        }
        Authority::Class(name) => {
            let c = class(static_cstr(name));
            assert!(!c.is_null(), "class {name} is absent — AppKit did not load");
            // SAFETY: `c` is a live class object, asserted non-null.
            let Some(t) = (unsafe { method_types(c, s) }) else {
                panic!("{} {}: {name} does not implement it", row.site, row.sel);
            };
            t
        }
        Authority::None(reason) => return Reading::NoAuthority(reason),
    };
    Reading::Declared(strip_method_offsets(&raw))
}

// --------------------------------------------------------------------- tests

/// Count the declaration sites in one of the fork's files: objc2's
/// `#[method(…)]` / `#[method_id(…)]` and `aterm_objc`'s `@sel(…)`, only at
/// the start of a line so a mention in a comment (such as a `LOCAL PATCH`
/// note) does not count.
fn declaration_sites(src: &str) -> usize {
    src.lines()
        .map(str::trim_start)
        .filter(|l| {
            l.starts_with("#[method(") || l.starts_with("#[method_id(") || l.starts_with("@sel(")
        })
        .count()
}

/// The declaration a line makes, if it makes one: the selector inside
/// `@sel(…)`, `#[method(…)]` or `#[method_id(…)]`.
fn declared_selector(line: &str) -> Option<&str> {
    let l = line.trim_start();
    let rest = l
        .strip_prefix("@sel(")
        .or_else(|| l.strip_prefix("#[method("))
        .or_else(|| l.strip_prefix("#[method_id("))?;
    rest.split(')').next()
}

/// Every `site:` points at the line that declares that selector. The count
/// guard cannot see where declarations are, so a line inserted above one is
/// caught only here.
#[test]
fn every_site_points_at_the_line_that_declares_it() {
    let base = fork_backend();
    let mut sources = std::collections::BTreeMap::new();
    let mut checked = 0_usize;
    for row in ROWS {
        let (file, line_no) = row.site.split_once(':').unwrap_or_else(|| {
            panic!("{} is not a `file.rs:line` site", row.site);
        });
        let line_no: usize = line_no
            .parse()
            .unwrap_or_else(|_| panic!("{} does not end in a line number", row.site));
        let src: &String = sources.entry(file.to_owned()).or_insert_with(|| {
            std::fs::read_to_string(base.join(file)).expect("the fork is readable")
        });
        let line = src.lines().nth(line_no - 1).unwrap_or_else(|| {
            panic!(
                "{} names line {line_no} of a file with {} lines",
                row.site,
                src.lines().count()
            )
        });
        assert_eq!(
            declared_selector(line),
            Some(row.sel),
            "{} should be where `{}` is declared, and that line is `{}`",
            row.site,
            row.sel,
            line.trim()
        );
        checked += 1;
    }
    assert_eq!(checked, 71, "every row's site was followed, none skipped");
}

/// Measures the key-binding facts behind the `insertBackTab:` row:
/// `NSStandardKeyBindingResponding` declares `insertTab:` and
/// `cancelOperation:` (`v@:@`) but not `insertBackTab:`; nothing on the view's
/// superclass chain implements any of the three; and the implementations that
/// do exist elsewhere agree with the protocol. The live-class auditor holds
/// `insertBackTab:` to the siblings' `v@:@`.
#[test]
fn the_key_binding_family_is_measured_not_asserted() {
    assert!(
        load_appkit(),
        "AppKit must load for this census to mean anything"
    );
    let p = protocol(c"NSStandardKeyBindingResponding");
    assert!(!p.is_null(), "the protocol is present in this process");

    // The two the protocol declares, and they agree with each other: that
    // agreement is what makes them usable as a third row's authority.
    let mut sibling = None;
    for name in ["insertTab:", "cancelOperation:"] {
        let s = sel_uncached(static_cstr(name));
        // SAFETY: `p` is a live protocol object (asserted non-null) and `s` a
        // live selector. `true` means instance methods; both the required and
        // optional tables are searched (these rows are `@optional`).
        let types = unsafe { protocol_method_types(p, s, true) }
            .unwrap_or_else(|| panic!("NSStandardKeyBindingResponding declares {name}"));
        let types = strip_method_offsets(&types);
        assert_eq!(types, "v@:@", "{name} is a void, one-object action");
        match &sibling {
            None => sibling = Some(types),
            Some(first) => assert_eq!(first, &types, "the siblings agree"),
        }
    }

    // The one it does not declare. If AppKit adds it, this fails and the row
    // no longer needs a derived shape.
    let back_tab = sel_uncached(c"insertBackTab:");
    // SAFETY: `p` is live and `back_tab` a live selector. Asked as instance and
    // class method, so "not declared" covers all four tables.
    for instance in [true, false] {
        assert!(
            unsafe { protocol_method_types(p, back_tab, instance) }.is_none(),
            "NSStandardKeyBindingResponding has started declaring insertBackTab: \
             (as {a} method) — the derived shape in objc_live_class_audit.rs \
             should become a direct one",
            a = if instance { "an instance" } else { "a class" }
        );
    }

    // None of the three is on `WinitView`'s superclass chain (otherwise the row
    // would have a real inherited authority). `class_getInstanceMethod` walks
    // the chain, so asking these classes asks the chain. Other classes do
    // implement `insertTab:` and `cancelOperation:`; see below.
    for class_name in ["NSView", "NSResponder", "NSObject"] {
        let c = class(static_cstr(class_name));
        assert!(!c.is_null(), "{class_name} is present");
        for name in ["insertTab:", "insertBackTab:", "cancelOperation:"] {
            let s = sel_uncached(static_cstr(name));
            // SAFETY: `c` is a live class object and `s` a live selector.
            assert!(
                unsafe { method_types(c, s) }.is_none(),
                "{class_name} is on WinitView's superclass chain and implements \
                 {name} — so that row has a real inherited authority and must \
                 be held to it rather than to a shape derived from siblings"
            );
        }
    }

    // Every implementation that exists agrees with the protocol's declaration,
    // which is what makes the sibling shape usable for the undeclared row.
    for (name, implementors) in [
        (
            "insertTab:",
            &["NSCollectionView", "NSColorPickerPencilView", "NSTextView"][..],
        ),
        (
            "cancelOperation:",
            &["NSWindow", "NSTableView", "NSPopover"][..],
        ),
    ] {
        let s = sel_uncached(static_cstr(name));
        for class_name in implementors {
            let c = class(static_cstr(class_name));
            assert!(!c.is_null(), "{class_name} is present");
            // SAFETY: `c` is a live class object and `s` a live selector.
            let types = unsafe { method_types(c, s) }.unwrap_or_else(|| {
                panic!(
                    "{class_name} was measured as an implementor of {name}; if AppKit has \
                     dropped it, re-measure the family rather than deleting this line"
                )
            });
            assert_eq!(
                strip_method_offsets(&types),
                "v@:@",
                "{class_name}'s {name} is a void, one-object action, like the \
                 protocol's declaration and like the fork's row"
            );
        }
    }
}

/// The table has not gone stale against the fork it describes; the count is
/// re-derived from `vendor/winit`'s source. A moved fork fails loudly.
#[test]
fn the_table_still_covers_every_declared_method() {
    let base = fork_backend();
    let mut found = 0_usize;
    for file in [
        "app_state.rs",
        "window.rs",
        "app.rs",
        "window_delegate.rs",
        "view.rs",
    ] {
        let src = std::fs::read_to_string(base.join(file)).expect("the fork is readable");
        found += declaration_sites(&src);
    }
    assert_eq!(
        found,
        ROWS.len(),
        "vendor/winit declares {found} methods across its five macOS classes \
         and this table has {}; the census is stale",
        ROWS.len()
    );
    assert_eq!(ROWS.len(), 71);

    // Per-file counts of `@sel(…)` rows, so a half-converted file is caught.
    let mut ported_total = 0;
    for (file, want) in [
        ("window_delegate.rs", 23),
        ("view.rs", 43),
        ("app_state.rs", 3),
        ("window.rs", 2),
    ] {
        let src = std::fs::read_to_string(base.join(file)).expect("the fork is readable");
        let ported = declaration_sites(&src);
        assert_eq!(ported, want, "{file}'s {want} rows are ported ones");
        assert_eq!(
            src.lines()
                .map(str::trim_start)
                .filter(|l| l.starts_with("#[method(") || l.starts_with("#[method_id("))
                .count(),
            0,
            "{file} is ported, so it must hold no objc2 declaration attribute"
        );
        ported_total += ported;
    }
    assert_eq!(ported_total, 71, "all 71 rows are declared by aterm_objc");

    // `app.rs` declares no methods (it swizzles `-[NSApplication sendEvent:]`,
    // covered by `the_swizzled_row_still_encodes_the_way_app_rs_types_it`). A
    // declaration reappearing there owes a table row and an audit target.
    let app_src = std::fs::read_to_string(base.join("app.rs")).expect("the fork is readable");
    assert_eq!(
        declaration_sites(&app_src),
        0,
        "app.rs declares a method again; add it to the table and to the live-class audit"
    );
    for file in ["app.rs", "app_state.rs", "window.rs"] {
        let src = std::fs::read_to_string(base.join(file)).expect("the fork is readable");
        assert_eq!(
            src.lines()
                .map(str::trim_start)
                .filter(|l| l.starts_with("#[method("))
                .count(),
            0,
            "{file} is ported off objc2's declare_class!"
        );
    }
}

/// No row takes a block (`@?`) or a C function pointer (`^?`), checked over the
/// runtime's encodings for every row with an authority; the rows without one
/// are named.
#[test]
fn no_declared_winit_method_takes_a_block() {
    assert!(
        load_appkit(),
        "AppKit must load for this census to mean anything"
    );
    let mut unchecked = Vec::new();
    let mut host_lacking = Vec::new();
    for row in ROWS {
        match authority_encoding(row) {
            Reading::Declared(enc) => {
                assert!(
                    !enc.contains("@?"),
                    "{} {} takes or returns a BLOCK ({enc}) — `@?` is on the \
                     port's critical path after all",
                    row.site,
                    row.sel
                );
                assert!(
                    !enc.contains("^?"),
                    "{} {} takes a C FUNCTION POINTER ({enc}) — `^?` has no \
                     Encode impl",
                    row.site,
                    row.sel
                );
            }
            Reading::NoAuthority(reason) => {
                unchecked.push((row.site, row.sel, expand(row.winit), reason));
            }
            Reading::HostLacksProtocol(name) => {
                host_lacking.push((row.site, row.sel, name));
            }
        }
        assert!(
            !expand(row.winit).contains("@?") && !expand(row.winit).contains("^?"),
            "{} {} — winit's own signature is a block",
            row.site,
            row.sel
        );
    }
    // The rows with no runtime authority form a fixed set, each with a reason;
    // a new one is a method the census cannot check.
    for (site, s, _, why) in &unchecked {
        assert!(
            !why.is_empty(),
            "{site} {s} has no runtime authority and no reason written"
        );
    }
    let bare: Vec<_> = unchecked
        .iter()
        .map(|(site, s, ours, _)| (*site, *s, ours.clone()))
        .collect();
    assert_eq!(
        bare,
        vec![
            ("view.rs:247", "frameDidChange:", "v@:@".to_string()),
            ("view.rs:744", "insertBackTab:", "v@:@".to_string()),
        ],
        "the set of rows no protocol or class declares changed"
    );
    // Rows whose protocol this host does not register: exactly the allowance.
    assert_host_lacking_is_the_allowance(&host_lacking);
}

/// `@?` is implemented correctly: a declared method taking
/// [`aterm_objc::BlockPtr`] registers what `-[NSString
/// enumerateLinesUsingBlock:]` does (`v24@0:8@?16`, offset-free), while
/// `*mut c_void` registers `^v`.
#[test]
fn the_block_encoding_is_what_foundation_itself_emits() {
    let ns_string = class(c"NSString");
    // SAFETY: `NSString` is a live, immortal Foundation class.
    let foundation = unsafe { method_types(ns_string, sel!(enumerateLinesUsingBlock:)) }
        .expect("Foundation implements enumerateLinesUsingBlock:");
    assert_eq!(
        strip_method_offsets(&foundation),
        "v@:@?",
        "clang's own encoding for a block argument"
    );

    let cls = BlockProbe::class();
    // SAFETY: `cls` is the class `class()` just registered.
    let ours = unsafe { method_types(cls, sel!(takesABlock:)) }.expect("registered");
    assert_eq!(ours, "v@:@?", "and ours is the same string");

    // The counterexample, side by side: the pre-`BlockPtr` spelling.
    // SAFETY: as above.
    let old = unsafe { method_types(cls, sel!(takesAnOpaquePointer:)) }.expect("registered");
    assert_eq!(old, "v@:^v");
    assert_ne!(ours, old, "if these agreed, `BlockPtr` would be decoration");
    assert_eq!(<aterm_objc::BlockPtr as Encode>::ENCODING, "@?");
}

/// The set of rows whose registered encoding differs from the runtime's
/// authority is empty.
///
/// `checked == 70` keeps it from passing vacuously.
/// [`the_ported_signatures_encode_to_the_authority`] re-derives the
/// `window_delegate.rs` rows from compiled signatures; for the rest this
/// table's transcription is the only reading here, and
/// `objc_live_class_audit.rs` reads the registered classes.
#[test]
fn no_declared_row_disagrees_with_the_runtimes_own_authority() {
    assert!(
        load_appkit(),
        "AppKit must load for this census to mean anything"
    );
    let mut disagreements = Vec::new();
    let mut checked = 0_usize;
    let mut host_lacking = Vec::new();
    for row in ROWS {
        let authority = match authority_encoding(row) {
            Reading::Declared(authority) => authority,
            Reading::NoAuthority(_) => continue,
            Reading::HostLacksProtocol(name) => {
                host_lacking.push((row.site, row.sel, name));
                continue;
            }
        };
        checked += 1;
        let ours = expand(row.winit);
        if ours != authority {
            disagreements.push((row.site, row.sel, ours, authority));
        }
    }
    assert_host_lacking_is_the_allowance(&host_lacking);
    assert_eq!(
        checked + host_lacking.len(),
        69,
        "sixty-nine of the seventy-one have an authority — all sixty-nine checked on a host \
         whose AppKit registers NSApplicationDelegate, sixty-six where it does not"
    );

    let named: Vec<String> = disagreements
        .iter()
        .map(|(site, s, ours, auth)| format!("{site} {s}: winit={ours} authority={auth}"))
        .collect();
    assert_eq!(
        named,
        Vec::<String>::new(),
        "a declared row disagrees with the runtime's own authority; the last \
         time this set was non-empty it was `draggingEntered:` and it took the \
         x86_64 codegen below to show why that mattered"
    );

    // `draggingEntered:` once returned a one-byte `BOOL` where AppKit reads an
    // eight-byte `NSDragOperation`.
    assert_eq!(size_of::<Bool>(), 1);
    assert_eq!(size_of::<usize>(), 8);
    assert_ne!(
        Bool::ENCODING,
        "Q",
        "if BOOL and NSUInteger ever encoded alike the P3 row would have been \
         harmless and this whole census pointless"
    );
}

/// Every protocol a row names that this host does not register is on
/// [`HOST_MAY_LACK`], and every name on the allowance is a real authority in
/// the table.
#[test]
fn the_host_allowance_is_exactly_what_this_host_lacks() {
    assert!(load_appkit(), "AppKit must load for this to mean anything");
    assert!(
        !class(c"NSApplication").is_null(),
        "AppKit's classes are present, so an absent protocol is the host's doing"
    );
    let mut names: Vec<&str> = ROWS
        .iter()
        .filter_map(|r| match r.authority {
            Authority::Proto(n) => Some(n),
            _ => None,
        })
        .collect();
    names.sort_unstable();
    names.dedup();
    let absent: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| !host_registers(n))
        .collect();
    for n in &absent {
        assert!(
            HOST_MAY_LACK.contains(n),
            "{n} is absent from this host's AppKit and not on the allowance"
        );
    }
    for n in HOST_MAY_LACK {
        assert!(
            names.contains(n),
            "{n} is on the allowance but is no row's authority"
        );
    }
    eprintln!("winit_seam: this host's AppKit lacks {absent:?} of the authorities {names:?}");
}

/// The two rows next to it really ARE `BOOL`, which is what makes the one row
/// a defect rather than a pattern.
#[test]
fn the_other_two_dragging_rows_are_genuinely_bool() {
    assert!(
        load_appkit(),
        "AppKit must load for this census to mean anything"
    );
    let p = protocol(c"NSDraggingDestination");
    assert!(!p.is_null());
    for s in [sel!(prepareForDragOperation:), sel!(performDragOperation:)] {
        // SAFETY: `p` is a live protocol object.
        let t = unsafe { protocol_method_types(p, s, true) }.expect("declared");
        assert_eq!(
            strip_method_offsets(&t),
            format!("{}@:@", Bool::ENCODING),
            "{s:?} is a BOOL row"
        );
    }
    // SAFETY: as above.
    let entered =
        unsafe { protocol_method_types(p, sel!(draggingEntered:), true) }.expect("declared");
    assert_eq!(strip_method_offsets(&entered), "Q@:@");
}

declare_class! {
    /// Two spellings of the same pointer, so the census can show the encoding
    /// they produce side by side.
    struct BlockProbe: NSObject {
        const NAME: &str = "ATermW3BlockProbe";
        type Ivars = ();

        /// The right spelling: `"@?"`.
        @sel(takesABlock:)
        fn takes_a_block(&self, _b: aterm_objc::BlockPtr) {}

        /// The spelling a port would reach for without [`aterm_objc::BlockPtr`]
        /// — `"^v"`, an opaque `void *`.
        @sel(takesAnOpaquePointer:)
        fn takes_an_opaque_pointer(&self, _p: *mut c_void) {}
    }
}

// ------------------------------------------------- the ported signatures

aterm_objc::declare_class! {
    /// A mirror of `window_delegate.rs`'s declared methods: the same selectors
    /// at the same Rust argument and return types.
    ///
    /// `WinitWindowDelegate` itself lives in `vendor/winit` (which depends on
    /// this crate) and cannot be instantiated here. The mirror checks that the
    /// Rust types produce the right encodings, not that `window_delegate.rs`
    /// writes those types; the live-class auditor covers that.
    struct PortedShapes: NSObject {
        const NAME: &str = "ATermW3PortedShapes";
        type Ivars = ();

        @sel(windowShouldClose:)
        fn window_should_close(&self, _sender: aterm_objc::Id) -> Bool { Bool::NO }

        @sel(windowWillClose:)
        fn window_will_close(&self, _sender: aterm_objc::Id) {}

        @sel(windowDidResize:)
        fn window_did_resize(&self, _sender: aterm_objc::Id) {}

        @sel(windowWillStartLiveResize:)
        fn window_will_start_live_resize(&self, _sender: aterm_objc::Id) {}

        @sel(windowDidEndLiveResize:)
        fn window_did_end_live_resize(&self, _sender: aterm_objc::Id) {}

        @sel(windowDidMove:)
        fn window_did_move(&self, _sender: aterm_objc::Id) {}

        @sel(windowDidChangeBackingProperties:)
        fn window_did_change_backing_properties(&self, _sender: aterm_objc::Id) {}

        @sel(windowDidBecomeKey:)
        fn window_did_become_key(&self, _sender: aterm_objc::Id) {}

        @sel(windowDidResignKey:)
        fn window_did_resign_key(&self, _sender: aterm_objc::Id) {}

        @sel(windowWillEnterFullScreen:)
        fn window_will_enter_fullscreen(&self, _sender: aterm_objc::Id) {}

        @sel(windowWillExitFullScreen:)
        fn window_will_exit_fullscreen(&self, _sender: aterm_objc::Id) {}

        /// The one `NSUInteger` row, and the only one whose argument is not an
        /// object.
        @sel(window:willUseFullScreenPresentationOptions:)
        fn window_will_use_fullscreen_presentation_options(
            &self,
            _sender: aterm_objc::Id,
            proposed: usize,
        ) -> usize {
            proposed
        }

        @sel(windowDidEnterFullScreen:)
        fn window_did_enter_fullscreen(&self, _sender: aterm_objc::Id) {}

        @sel(windowDidExitFullScreen:)
        fn window_did_exit_fullscreen(&self, _sender: aterm_objc::Id) {}

        @sel(windowDidFailToEnterFullScreen:)
        fn window_did_fail_to_enter_fullscreen(&self, _sender: aterm_objc::Id) {}

        @sel(windowDidChangeOcclusionState:)
        fn window_did_change_occlusion_state(&self, _sender: aterm_objc::Id) {}

        @sel(windowDidChangeScreen:)
        fn window_did_change_screen(&self, _sender: aterm_objc::Id) {}

        /// THE P3 ROW, at its new type. `usize`, not `bool`.
        @sel(draggingEntered:)
        fn dragging_entered(&self, _sender: aterm_objc::Id) -> usize { 1 }

        @sel(prepareForDragOperation:)
        fn prepare_for_drag_operation(&self, _sender: aterm_objc::Id) -> Bool { Bool::YES }

        @sel(performDragOperation:)
        fn perform_drag_operation(&self, _sender: aterm_objc::Id) -> Bool { Bool::YES }

        @sel(concludeDragOperation:)
        fn conclude_drag_operation(&self, _sender: aterm_objc::Id) {}

        @sel(draggingExited:)
        fn dragging_exited(&self, _sender: aterm_objc::Id) {}

        /// The only row taking a bare pointer: `context:` is `void *`, encoded
        /// `"^v"` and NOT `"@"`.
        @sel(observeValueForKeyPath:ofObject:change:context:)
        fn observe_value(
            &self,
            _key_path: aterm_objc::Id,
            _object: aterm_objc::Id,
            _change: aterm_objc::Id,
            _context: *mut c_void,
        ) {}
    }
}

/// Every `window_delegate.rs` signature, declared on [`PortedShapes`], encodes
/// to what the runtime's authority declares (read off the registered mirror
/// class, not the table).
///
/// Covers only the rows the mirror declares; the expected count is derived
/// from the table. The other ported rows are read by
/// `crates/aterm-gui/examples/objc_live_class_audit.rs`, which inspects the
/// registered classes and is the better instrument.
#[test]
fn the_ported_signatures_encode_to_the_authority() {
    assert!(
        load_appkit(),
        "AppKit must load for this census to mean anything"
    );
    let cls = PortedShapes::class();
    let mut checked = 0_usize;
    let mirrored = |r: &&Row| r.site.starts_with("window_delegate.rs:");
    for row in ROWS.iter().filter(mirrored) {
        let s = sel_uncached(static_cstr(row.sel));
        // SAFETY: `cls` is the class `class()` just registered, and `s` is a
        // live selector.
        let registered = unsafe { method_types(cls, s) }
            .map(|t| strip_method_offsets(&t))
            .unwrap_or_else(|| panic!("{} is not registered on the mirror", row.sel));
        let Reading::Declared(authority) = authority_encoding(row) else {
            panic!("{} has no authority on this host", row.sel)
        };
        assert_eq!(
            registered, authority,
            "{} {}: the port's Rust signature encodes to {registered}, the \
             runtime declares {authority}",
            row.site, row.sel
        );
        // And the table agrees with both, so the transcription above is not
        // quietly drifting from the compiled truth.
        assert_eq!(expand(row.winit), registered, "{} table row", row.sel);
        checked += 1;
    }
    // Derived from the table: every row attributed to the mirrored file. A row
    // added to the table but not to `PortedShapes` panics in the lookup above.
    let expected = ROWS.iter().filter(mirrored).count();
    assert_eq!(checked, expected, "every mirrored row, none skipped");
    assert_eq!(
        expected, 23,
        "window_delegate.rs's 23 rows are what the mirror holds"
    );

    // The rows this test does not cover, as a number, so it cannot drift.
    let ported = ROWS.len();
    assert_eq!(ported, 71, "all 71 rows are ported; see the header");
    assert_eq!(
        ported - expected,
        48,
        "48 ported rows are checked against a REGISTERED class only by \
         crates/aterm-gui/examples/objc_live_class_audit.rs — if that gate ever \
         stops covering a class, this number is the size of the hole"
    );
}

// ------------------------------------ the ivar slot under a substituting init

/// A substituting initializer defeats the [`aterm_objc::IvarSlot`]
/// initialised flag for three of four kinds of survivor.
///
/// `window.rs` uses `alloc_ivars` because `-[NSWindow initWithContentRect:…]`
/// may release `self` and return another object. The flag catches a survivor
/// of the same class only; a superclass, sibling-subclass or unrelated
/// survivor is not caught.
///
/// The subject carries a `u8` (a 2-byte slot) so there is state to hand out
/// wrongly. Where the slot lands is a host fact: in `NSWindow`'s tail padding
/// on some macOS releases, at the end of the allocation on others. Three of
/// four survivors defeat the flag either way.
#[test]
fn a_substituting_initializer_defeats_the_initialized_flag() {
    assert!(
        load_appkit(),
        "AppKit must load: this is about NSWindow's instance layout"
    );

    // `+alloc`'s own allocator; no crate wrapper exists because nothing ships
    // an allocation without an initializer.
    unsafe extern "C" {
        fn class_createInstance(cls: aterm_objc::ClassPtr, extra: usize) -> aterm_objc::Id;
        fn class_getInstanceSize(cls: aterm_objc::ClassPtr) -> usize;
        /// `malloc`'s own view of the block, from libSystem: the allocation the
        /// read would have to stay inside.
        fn malloc_size(ptr: *const c_void) -> usize;
    }

    // Two classes with the SAME superclass and the SAME slot shape. The runtime
    // gives them the same offset, and that is the whole of case (c).
    let mut builder = aterm_objc::begin(c"NSWindow", c"ATermSeamSubstitutionSubject");
    builder.add_rust_ivar::<u8>();
    let subject = builder.register();
    let mut builder = aterm_objc::begin(c"NSWindow", c"ATermSeamSubstitutionSibling");
    builder.add_rust_ivar::<u8>();
    let sibling = builder.register();
    let off = subject.ivar_offset();
    assert_eq!(
        off,
        sibling.ivar_offset(),
        "same superclass and same slot shape put the slot at the same offset — \
         which is what makes a sibling's honest bytes readable as ours"
    );

    let window_size = unsafe { class_getInstanceSize(class(c"NSWindow")) };
    let object_size = unsafe { class_getInstanceSize(class(c"NSObject")) };
    let slot = size_of::<aterm_objc::IvarSlot<u8>>();
    // Where the slot lands is a host fact: in `NSWindow`'s tail padding
    // (inside a plain `NSWindow`'s allocation, so case (b) can read it) or at
    // the instance size with no padding (so case (b) is settled by arithmetic,
    // like (d)). "Inside" is measured against the malloc block, which malloc
    // rounds up past `class_getInstanceSize`.
    let superclass_instance = unsafe { class_createInstance(class(c"NSWindow"), 0) };
    let block = unsafe { malloc_size(superclass_instance.as_ptr()) };
    let in_padding = (off as usize) + slot <= block;
    assert!(
        in_padding || (off as usize) >= block,
        "the slot at {off} straddles the end of a plain NSWindow's {block}-byte block"
    );

    // Every instance is leaked: `-dealloc` on an uninitialised NSWindow is not
    // something to provoke.
    let slot_of = |obj: aterm_objc::Id| -> *mut aterm_objc::IvarSlot<u8> {
        // SAFETY: `obj` points at an instance of a class whose layout includes
        // `off`, asserted above for every class used here.
        unsafe { obj.as_ptr().cast::<u8>().offset(off).cast() }
    };
    let reads_a_value = |obj: aterm_objc::Id| -> Option<u8> {
        std::panic::catch_unwind(|| {
            // SAFETY: the slot is inside the instance's allocation and 1-aligned.
            *unsafe { aterm_objc::IvarSlot::<u8>::get(slot_of(obj).cast_const()) }
        })
        .ok()
    };
    // The named panic is the expected outcome twice below; silence libtest's
    // hook so the transcript stays readable.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    // (a) A survivor of the same class: the flag holds.
    let same_class = unsafe { class_createInstance(subject.class(), 0) };
    assert_eq!(
        reads_a_value(same_class),
        None,
        "a fresh instance of the same class is zero-filled, so `initialized` is \
         false and `get` panics by name — this is the case the corrected \
         sentence describes"
    );

    // (b) A plain `NSWindow`: panics only because its tail padding happens to
    // be zero, where it has padding; otherwise the slot is past the block and
    // no read is performed.
    if in_padding {
        assert_eq!(
            reads_a_value(superclass_instance),
            None,
            "NSWindow's tail padding is zero as allocated, so this panics — by \
             accident of what has not been written there yet"
        );
        // SAFETY: `off` is inside the plain NSWindow's malloc block
        // (`in_padding`, via `malloc_size`) and belongs to no ivar; writing one
        // padding byte is what a future framework ivar would do.
        unsafe { *superclass_instance.as_ptr().cast::<u8>().offset(off) = 1 };
        assert!(
            reads_a_value(superclass_instance).is_some(),
            "set that one padding byte and the flag reads TRUE and `get` hands out a \
             &T over padding — the panic in the line above was contingent on unused \
             bytes, not on this crate's zero-fill"
        );
    } else {
        assert!(
            (off as usize) >= block,
            "on this host the slot at {off} is past a plain NSWindow's {block}-byte block, \
             so this survivor is read OUT OF BOUNDS — defeated by a load the arithmetic \
             forbids, the shape of case (d)"
        );
        eprintln!(
            "winit_seam: NSWindow is {window_size} bytes on this host with no tail padding; \
             the slot at {off} is past its {block}-byte block, so case (b) is settled by \
             arithmetic rather than by a padding read"
        );
    }

    // (c) A sibling subclass whose own slot sits at the same offset: it
    // initialises its own ivar honestly, and we read that value.
    let sibling_instance = unsafe { class_createInstance(sibling.class(), 0) };
    // SAFETY: the sibling's slot is at its own registered offset, freshly
    // zeroed, and is the same `IvarSlot<u8>` shape.
    unsafe {
        aterm_objc::IvarSlot::<u8>::init(
            sibling_instance
                .as_ptr()
                .cast::<u8>()
                .offset(sibling.ivar_offset())
                .cast(),
            0x5a,
        );
    }
    assert_eq!(
        reads_a_value(sibling_instance),
        Some(0x5a),
        "the sibling's own honest byte is read as OUR `initialized` and OUR \
         value: `get` returns a &T over another class's bytes and nothing panics"
    );

    std::panic::set_hook(previous);

    // (d) An `NSObject`-sized survivor: the slot is outside its malloc block,
    // so no read is performed.
    let object_instance = unsafe { class_createInstance(class(c"NSObject"), 0) };
    let block = unsafe { malloc_size(object_instance.as_ptr()) };
    assert!(
        block >= object_size && (off as usize) >= block,
        "an NSObject is {object_size} bytes of instance in a {block}-byte block, \
         so reading the slot on one puts the load {} bytes past the end of the \
         allocation — out of bounds before any assertion can fire",
        off as usize - block
    );

    // The flag guards only the window between `+alloc` and the ivar store on one
    // instance; a class whose `Ivars` carry state must ensure its designated
    // initializer does not substitute.
}

// ------------------------------------------------- the swizzled framework row

/// `-[NSApplication sendEvent:]`'s live encoding matches the type `app.rs`
/// swizzles it with.
///
/// The expected string is derived by [`aterm_objc::MethodFn`] from the same
/// function-pointer type `SwizzleSite::install` checks at launch, and the
/// fork's own spelling of that type is read from `app.rs` and compared, so this
/// copy cannot drift.
#[test]
fn the_swizzled_row_still_encodes_the_way_app_rs_types_it() {
    use aterm_objc::{Id, MethodFn, Sel};

    assert!(load_appkit(), "AppKit must load for this to mean anything");

    /// `app.rs`'s `SendEvent`, spelled again.
    type SendEvent = unsafe extern "C-unwind" fn(Id, Sel, Id);

    // The fork really declares this type.
    let app_src =
        std::fs::read_to_string(fork_backend().join("app.rs")).expect("the fork is readable");
    assert!(
        app_src.contains(r#"type SendEvent = unsafe extern "C-unwind" fn(Id, Sel, Id);"#),
        "app.rs no longer types its swizzled implementation as \
         `unsafe extern \"C-unwind\" fn(Id, Sel, Id)`; this test's copy is stale and its \
         derivation proves nothing"
    );

    let derived = <SendEvent as MethodFn>::method_encoding();
    assert_eq!(derived, "v@:@", "the derivation itself moved");

    // SAFETY: `NSApplication` is a live class and `sendEvent:` an interned
    // selector; `method_types` answers `None` for an unimplemented one.
    let live = unsafe {
        aterm_objc::method_types(
            aterm_objc::class(c"NSApplication"),
            aterm_objc::sel_uncached(c"sendEvent:"),
        )
    }
    .map(|t| aterm_objc::strip_method_offsets(&t))
    .expect("-[NSApplication sendEvent:] is a real row");
    assert_eq!(
        live, derived,
        "AppKit's -sendEvent: encodes {live} and app.rs installs a {derived}; the \
         swizzle would be refused at launch, which is the right outcome and a \
         terrible way to find out"
    );
}

/// The fork swizzles exactly the rows this file checks: every
/// `SwizzleSite::install` call in the macOS backend is found in the source and
/// must match the checked set, so a new swizzle fails here with its file and
/// line.
#[test]
fn the_fork_patches_exactly_the_framework_rows_this_file_checks() {
    /// Every `(file, line, selector)` at which the fork installs a swizzle
    /// (`NAME.install(cls, sel!(x), f)`), read from the source.
    fn install_sites() -> Vec<(String, usize, String)> {
        let mut out = Vec::new();
        // `//` comments removed: this fork's own prose spells `.install(`
        // in the doc comment that explains it.
        for (file, stripped) in fork_sources() {
            let mut from = 0_usize;
            while let Some(at) = stripped[from..].find(".install(") {
                let start = from + at;
                from = start + ".install(".len();
                let line = stripped[..start].matches('\n').count() + 1;
                let rest = &stripped[from..];
                let sel = rest
                    .find("sel!(")
                    .and_then(|i| {
                        let tail = &rest[i + "sel!(".len()..];
                        tail.find(')').map(|e| tail[..e].trim().to_owned())
                    })
                    .unwrap_or_else(|| "<no sel! in the call>".to_owned());
                out.push((file.clone(), line, sel));
            }
        }
        out
    }

    /// The rows checked above, by the selector each patches.
    const CHECKED: &[&str] = &["sendEvent:"];

    let sites = install_sites();
    assert!(
        !sites.is_empty(),
        "no `SwizzleSite::install` call was found in the fork; this test is \
         not reading the source, and one that reads nothing passes"
    );
    let uncovered: Vec<String> = sites
        .iter()
        .filter(|(_, _, sel)| !CHECKED.contains(&sel.as_str()))
        .map(|(f, l, sel)| format!("{f}:{l}: {sel}"))
        .collect();
    assert!(
        uncovered.is_empty(),
        "{} swizzle site(s) in the fork patch a framework row that nothing in \
         this file checks the encoding of. A swizzle is the one thing in this \
         tree that replaces code Apple shipped, and the encoding comparison at \
         install is the only check that can notice Apple changing the \
         signature underneath it:\n  {}",
        uncovered.len(),
        uncovered.join("\n  ")
    );
    // Both ways, so a deleted site does not leave a check over nothing.
    let stale: Vec<&str> = CHECKED
        .iter()
        .filter(|c| !sites.iter().any(|(_, _, sel)| sel == *c))
        .copied()
        .collect();
    assert!(
        stale.is_empty(),
        "this file checks the encoding of {stale:?}, which the fork no longer \
         swizzles"
    );
}
