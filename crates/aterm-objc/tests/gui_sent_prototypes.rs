// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! W13 — the census of the selectors `aterm-gui` SENDS, which nothing had.
//!
//! # The gap this closes
//!
//! `winit_sent_prototypes.rs` is the census of the FORK's sends. It exists
//! because W8 moved 177 binding calls off `objc2-app-kit` by hand and one of
//! them — `-requestUserAttention:`, `q24@0:8Q16`, a SIGNED return over an
//! UNSIGNED argument — was written with the wrong prototype. Nothing caught it:
//! both are 64-bit words on both Apple ABIs, the only values passed were 0 and
//! 10, every test passed and every pixel was identical.
//!
//! W13 did the same thing to `aterm-gui`, and there was no such file. The
//! wave's own record says the encodings were read from the runtime with
//! `method_getTypeEncoding` before a line was written — but a reading that is
//! not a test is a reading that happened once, on one machine, on one SDK, and
//! that no later edit is held to.
//!
//! The hazard is not hypothetical here; it is spelled out in this very port. In
//! `app_introspect.rs::read_native_chrome` two adjacent enumerated properties
//! are read at DIFFERENT widths — `-[NSWindow toolbarStyle]` is `NSInteger`
//! (`q`) and `-[NSToolbar displayMode]` is `NSUInteger` (`Q`) — and the port's
//! own comment calls them "a pair that would silently agree on every value
//! either happens to hold". So would
//! `-bitmapImageRepByConvertingToColorSpace:renderingIntent:` (`…@16q24`) and
//! `-representationUsingType:properties:` (`…Q16@24`), whose two helpers in
//! `aterm-objc`'s `send.rs` are documented as "exactly the pair the campaign's
//! census exists to keep apart" — while no census covered either of them.
//!
//! This file is that census, and it reports the runtime's answer.
//!
//! # Scope, stated so it cannot be mistaken for more
//!
//! [`SCOPE`] is the three files whose entire Objective-C surface W13 authored:
//! `alert_keys.rs`, `app_introspect.rs` and `lib.rs`'s paste sheet. Every
//! `sel!` site in them is walked and must have a row. `menu.rs`'s other
//! selectors, and the rest of `crates/aterm-gui/src`, are EARLIER waves' work
//! and are NOT covered — that is the remaining half of this crate's census debt
//! and it is counted by [`the_scope_is_a_named_fraction_of_the_crate`] rather
//! than left as an impression.
//!
//! # Non-vacuity — the four ways this could fail to be a test
//!
//! * No AppKit — [`load_appkit`] returns false and every test PANICS rather
//!   than passing with nothing consulted.
//! * A selector that does not resolve — a finding, never a skip. All 40 resolve
//!   on this SDK; there is no exception list, and adding one would have to argue
//!   for itself the way `winit_sent_prototypes.rs`'s four do.
//! * A shape vocabulary that accepts anything — [`token`] PANICS on a name it
//!   does not know, and [`the_expected_shapes_are_not_all_the_same`] requires
//!   the table to spell more than a handful of distinct encodings.
//! * A walk that reads nothing — [`every_sent_selector_in_scope_has_a_row`]
//!   requires all three files to exist and the walk to find sites in each.
//!
//! And the expectation is DERIVED FROM THE HELPER NAME, never copied from the
//! SDK: a transcribed string is a second guess, and two guesses that agree are
//! still a guess.

#![cfg(target_os = "macos")]

use std::ffi::{CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};

use aterm_objc::{class, sel_uncached, strip_method_offsets};

unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// `RTLD_LAZY`.
const RTLD_LAZY: c_int = 0x1;
/// `RTLD_DEFAULT`, as the pointer value dlfcn.h defines it on Darwin.
const RTLD_DEFAULT: *mut c_void = (-2_isize) as *mut c_void;

/// Load AppKit into this test process, once.
fn load_appkit() -> bool {
    use std::sync::OnceLock;
    static LOADED: OnceLock<bool> = OnceLock::new();
    *LOADED.get_or_init(|| {
        let path = CString::new("/System/Library/Frameworks/AppKit.framework/AppKit")
            .expect("no interior NUL");
        // SAFETY: `dlopen` with a valid C path and `RTLD_LAZY`; the handle is
        // deliberately never closed — AppKit is process-lifetime once loaded.
        let handle = unsafe { dlopen(path.as_ptr(), RTLD_LAZY) };
        !handle.is_null()
    })
}

/// The files whose whole Objective-C surface W13 wrote. Relative to the repo.
const SCOPE: &[&str] = &[
    "crates/aterm-gui/src/alert_keys.rs",
    "crates/aterm-gui/src/app_introspect.rs",
    "crates/aterm-gui/src/lib.rs",
];

/// The Objective-C type encoding a helper's name implies, as
/// `<return><self><_cmd><args…>`.
fn shape_of(helper: &str) -> String {
    let rest = helper.strip_prefix("send").expect("a send helper");
    let mut parts = rest.split('_').filter(|p| !p.is_empty());
    let ret = parts.next().expect("a return token");
    let mut out = String::from(token(ret));
    out.push_str("@:");
    for a in parts {
        out.push_str(token(a));
    }
    out
}

/// One name from a helper's `send_a_b_c` spelling, as its encoding.
fn token(t: &str) -> &'static str {
    match t {
        "v" => "v",
        "id" => "@",
        "bool" => "B",
        "isize" => "q",
        "usize" => "Q",
        // The UNSIGNED short. `-[NSEvent keyCode]` is `unsigned short`, and it
        // is the only narrow return either half of the modal path reads.
        "u16" => "S",
        // A `void *` the receiver WRITES through — `-getBytes:length:`. `^v`
        // and not the const `r^v`, which is the distinction
        // `aterm-objc`'s `send_v_ptr_usize` doc sits on.
        "ptr" => "^v",
        // A BLOCK, which is `@?` and NOT `@`. The two rows that carry one are
        // the only sites in this scope with no typed helper — see
        // [`HAND_WRITTEN`].
        "block" => "@?",
        "rect" => "{CGRect={CGPoint=dd}{CGSize=dd}}",
        other => panic!(
            "unknown helper token {other:?} — add it to `token` rather than \
             letting the census accept a shape it cannot spell"
        ),
    }
}

/// INSTANCE selectors: receiver class, selector, and the `aterm-objc` helper
/// the site calls.
const ROWS: &[(&str, &str, &str)] = &[
    // ---- NSEvent: the modal key interceptor's reads (`alert_keys.rs`) ----
    ("NSEvent", "window", "send_id"),
    ("NSEvent", "charactersIgnoringModifiers", "send_id"),
    ("NSEvent", "modifierFlags", "send_usize"),
    ("NSEvent", "keyCode", "send_u16"),
    // ---- NSWindow ----
    ("NSWindow", "attachedSheet", "send_id"),
    ("NSWindow", "isVisible", "send_bool"),
    ("NSWindow", "windowNumber", "send_isize"),
    ("NSWindow", "toolbar", "send_id"),
    // THE SIGNED HALF of the pair `read_native_chrome` reads side by side.
    ("NSWindow", "toolbarStyle", "send_isize"),
    ("NSWindow", "standardWindowButton:", "send_id_usize"),
    ("NSWindow", "convertRectToBacking:", "send_rect_rect"),
    // ---- NSView ----
    ("NSView", "window", "send_id"),
    ("NSView", "superview", "send_id"),
    ("NSView", "bounds", "send_rect"),
    ("NSView", "convertRect:toView:", "send_rect_rect_id"),
    (
        "NSView",
        "bitmapImageRepForCachingDisplayInRect:",
        "send_id_rect",
    ),
    (
        "NSView",
        "cacheDisplayInRect:toBitmapImageRep:",
        "send_v_rect_id",
    ),
    // ---- NSButton: what the interceptor clicks ----
    ("NSButton", "performClick:", "send_v_id"),
    // ---- NSToolbar / NSToolbarItem ----
    ("NSToolbar", "items", "send_id"),
    // THE UNSIGNED HALF of that pair, one line from its twin above.
    ("NSToolbar", "displayMode", "send_usize"),
    ("NSToolbarItem", "itemIdentifier", "send_id"),
    ("NSToolbarItem", "label", "send_id"),
    // ---- NSArray: the enumeration `objc2`'s iterators used to hide ----
    ("NSArray", "count", "send_usize"),
    ("NSArray", "objectAtIndex:", "send_id_usize"),
    // ---- The menu bar, read back ----
    ("NSApplication", "mainMenu", "send_id"),
    ("NSMenu", "itemArray", "send_id"),
    ("NSMenuItem", "title", "send_id"),
    ("NSMenuItem", "submenu", "send_id"),
    // ---- NSBitmapImageRep: the chrome capture. THE OTHER NEAR-TWIN PAIR:
    // the rendering intent is a SIGNED NSInteger and the file type is an
    // UNSIGNED NSUInteger, in the same six lines of the same function.
    (
        "NSBitmapImageRep",
        "bitmapImageRepByConvertingToColorSpace:renderingIntent:",
        "send_id_id_isize",
    ),
    (
        "NSBitmapImageRep",
        "representationUsingType:properties:",
        "send_id_usize_id",
    ),
    // ---- NSData: the PNG the capture reads back ----
    ("NSData", "length", "send_usize"),
    ("NSData", "getBytes:length:", "send_v_ptr_usize"),
    // ---- NSAlert: the modal subsystem proper ----
    ("NSAlert", "setMessageText:", "send_v_id"),
    ("NSAlert", "setInformativeText:", "send_v_id"),
    ("NSAlert", "addButtonWithTitle:", "send_id_id"),
    ("NSAlert", "window", "send_id"),
    ("NSAlert", "runModal", "send_isize"),
];

/// CLASS selectors — checked against the METAclass, which is a different
/// method list and the reason this is its own table.
const CLASS_ROWS: &[(&str, &str, &str)] = &[
    ("NSEvent", "removeMonitor:", "send_v_id"),
    ("NSApplication", "sharedApplication", "send_id"),
    ("NSColorSpace", "sRGBColorSpace", "send_id"),
    ("NSDictionary", "new", "send_id"),
    ("NSAlert", "new", "send_id"),
];

/// The two sites with NO typed helper, because their last argument is a BLOCK.
///
/// `aterm-objc` has no `send_*` for a block argument: a block is `@?` to the
/// runtime and `*mut c_void` to Rust, so both sites cast a raw
/// `aterm_objc::msg()` function pointer instead. That makes them the LEAST
/// checked sends in the crate — nothing about them is typed — and therefore the
/// ones a census is worth the most on. The shape name is spelled in the same
/// `send_<ret>_<args>` grammar as a real helper so [`shape_of`] derives the
/// expectation the same way; the site is named so the row cannot drift from it.
const HAND_WRITTEN: &[(&str, &str, &str, bool, &str)] = &[
    (
        "NSEvent",
        "addLocalMonitorForEventsMatchingMask:handler:",
        "send_id_usize_block",
        true,
        "crates/aterm-gui/src/alert_keys.rs — watch_alert_keys",
    ),
    (
        "NSAlert",
        "beginSheetModalForWindow:completionHandler:",
        "send_v_id_block",
        false,
        "crates/aterm-gui/src/lib.rs — present_multiline_paste_sheet",
    ),
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn leak_cstr(s: &str) -> &'static std::ffi::CStr {
    Box::leak(CString::new(s).expect("no interior NUL").into_boxed_c_str())
}

/// The runtime's own encoding for a selector, offsets stripped.
fn live_encoding(cls: &str, name: &str, class_method: bool) -> Option<String> {
    let c = class(leak_cstr(cls));
    let s = sel_uncached(leak_cstr(name));
    let types = if class_method {
        // SAFETY: `c` is a live registered class and `s` an interned selector;
        // `class_getClassMethod` tolerates one the metaclass does not implement.
        unsafe { aterm_objc::method_types(aterm_objc::class_of(c.as_id()), s) }
    } else {
        // SAFETY: as above, for the instance side.
        unsafe { aterm_objc::method_types(c, s) }
    };
    types.map(|t| strip_method_offsets(&t))
}

/// Every row in the census, as `(class, selector, helper, is_class_method)`.
fn all_rows() -> Vec<(&'static str, &'static str, &'static str, bool)> {
    let mut out: Vec<_> = ROWS.iter().map(|r| (r.0, r.1, r.2, false)).collect();
    out.extend(CLASS_ROWS.iter().map(|r| (r.0, r.1, r.2, true)));
    out.extend(HAND_WRITTEN.iter().map(|r| (r.0, r.1, r.2, r.3)));
    out
}

#[test]
fn every_sent_selector_encodes_the_way_its_helper_spells_it() {
    assert!(
        load_appkit(),
        "AppKit is not present: this census has no authority to consult"
    );

    let mut checked = 0_usize;
    let mut findings = Vec::new();
    for (cls, name, helper, is_class) in all_rows() {
        let want = shape_of(helper);
        match live_encoding(cls, name, is_class) {
            Some(got) if got == want => checked += 1,
            Some(got) => findings.push(format!(
                "{cls} {name}: the site sends {helper} = {want:?}, runtime says {got:?}"
            )),
            None => findings.push(format!(
                "{cls} {name}: selector does not resolve — a census that cannot \
                 ask is not a census"
            )),
        }
    }
    assert!(
        findings.is_empty(),
        "{} disagreement(s):\n  {}",
        findings.len(),
        findings.join("\n  ")
    );
    assert_eq!(
        checked,
        all_rows().len(),
        "not every row was consulted, which is how a census passes by reading nothing"
    );
    assert!(
        checked >= 40,
        "only {checked} rows: this scope has 40 distinct selectors and the \
         table has shrunk without the scope shrinking"
    );
}

/// Every `sel!` site in [`SCOPE`] has a row. Both directions.
#[test]
fn every_sent_selector_in_scope_has_a_row() {
    let repo = repo();
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut per_file = Vec::new();
    for rel in SCOPE {
        let path = repo.join(rel);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{rel} is unreadable ({e}) — the scope is stale"));
        let before = seen.len();
        for (i, line) in src.lines().enumerate() {
            // Comments are stripped so PROSE naming a selector is not a site,
            // which is the same rule the exit-condition guard learned twice.
            let code = line.split("//").next().unwrap_or("");
            let mut rest = code;
            while let Some(at) = rest.find("sel!(") {
                rest = &rest[at + "sel!(".len()..];
                let Some(end) = rest.find(')') else { break };
                let name = rest[..end].trim();
                rest = &rest[end..];
                if name.is_empty() || name.starts_with('$') {
                    continue; // `sel!($name)` — a macro_rules parameter.
                }
                seen.push((name.to_owned(), format!("{rel}:{}", i + 1)));
            }
        }
        per_file.push((rel, seen.len() - before));
    }
    for (rel, n) in &per_file {
        assert!(
            *n > 0,
            "{rel} contributed no `sel!` site — a walk that reads nothing passes"
        );
    }

    let covered: Vec<&str> = all_rows().iter().map(|r| r.1).collect();
    let mut uncovered: Vec<String> = seen
        .iter()
        .filter(|(name, _)| !covered.contains(&name.as_str()))
        .map(|(name, at)| format!("{at}: {name}"))
        .collect();
    uncovered.sort();
    uncovered.dedup();
    assert!(
        uncovered.is_empty(),
        "{} selector(s) sent in scope have no census row:\n  {}",
        uncovered.len(),
        uncovered.join("\n  ")
    );

    // …and BOTH WAYS: a row naming a selector nothing in scope sends is a row
    // that has outlived its site, and it would pre-approve a future send of it.
    //
    // THIS RULE WAS PLANTED AGAINST AND FAILED ONCE, and the weaker version is
    // recorded because the lesson is the file's whole subject. The first
    // spelling asked "does ANY file in `crates/aterm-gui/src` send this?", so
    // that `-[NSAlert runModal]` — sent from `menu.rs::confirm`, which W13 also
    // wrote and which is not in scope — could keep its row. Plant D4 added
    // `("NSWindow", "makeKeyAndOrderFront:", "send_v_id")`, a selector this
    // census has nothing to do with, and the test PASSED: a crate-wide rule
    // pre-approves all 320 selectors the crate sends. So the exception is now
    // NAMED rather than inferred from a wide set — [`OUT_OF_SCOPE_ROWS`], one
    // entry, with its file and its reason, checked to spell the selector.
    let out_of_scope: Vec<&str> = OUT_OF_SCOPE_ROWS.iter().map(|r| r.0).collect();
    let stale: Vec<&str> = covered
        .iter()
        .copied()
        .filter(|n| !seen.iter().any(|(name, _)| name == n) && !out_of_scope.contains(n))
        .collect();
    assert!(
        stale.is_empty(),
        "these census rows name selectors no site in scope sends, and no \
         `OUT_OF_SCOPE_ROWS` entry argues for them: {stale:?}"
    );
}

/// The rows kept for a site OUTSIDE [`SCOPE`], each argued for individually.
///
/// A census row with no site is dead weight that pre-approves a future send. A
/// row whose site is real but sits in a file this census does not walk is a
/// different thing, and the difference has to be written down or it becomes a
/// hole — see the plant recorded above.
const OUT_OF_SCOPE_ROWS: &[(&str, &str, &str)] = &[(
    "runModal",
    "crates/aterm-gui/src/menu.rs",
    "`menu::confirm` is the modal subsystem's app-modal half and W13 wrote its \
     send, but `menu.rs` also carries 49 selectors from W1-W12 that this file \
     does not cover, so the file is not in SCOPE and this one row is.",
)];

/// Each out-of-scope exception really names a live site, and there are few.
#[test]
fn the_out_of_scope_rows_argue_for_themselves() {
    let repo = repo();
    for (name, file, reason) in OUT_OF_SCOPE_ROWS {
        let src = std::fs::read_to_string(repo.join(file))
            .unwrap_or_else(|e| panic!("{file} is unreadable ({e})"));
        assert!(
            selectors_in(&src).contains(*name),
            "the exception for {name} names {file}, which no longer sends it"
        );
        assert!(
            reason.len() >= 80,
            "the exception for {name} must say why, not just that"
        );
        assert!(
            all_rows().iter().any(|r| r.1 == *name),
            "{name} is excused but has no census row to excuse"
        );
    }
    assert_eq!(
        OUT_OF_SCOPE_ROWS.len(),
        1,
        "a row kept for a site this census does not walk must argue for itself \
         individually, and the list is not a place to park the backlog"
    );
}

/// THE TABLE IS PINNED TO THE CODE, not merely to the runtime.
///
/// [`every_sent_selector_encodes_the_way_its_helper_spells_it`] asks the
/// runtime whether the HELPER NAMED IN THE TABLE is right. It cannot see a site
/// that calls a DIFFERENT helper than its row claims — the table would keep
/// agreeing with AppKit while the code disagreed with the table, and the census
/// would be a third guess rather than a check. This asks the source.
///
/// PLANTED AND MEASURED, and the measurement is more precise than the claim a
/// reader would have accepted. Flipping `-displayMode`'s helper in
/// `app_introspect.rs` does NOT compile — its four `match` arms are
/// `NS_TOOLBAR_DISPLAY_MODE_*` constants typed `usize`, so `rustc` objects
/// first, and this test is not what catches that site. Flipping the event's
/// `-modifierFlags` read in `alert_keys.rs` (`decide`, the `send_usize` whose
/// result is `as u64` on the next character), DOES:
///
/// ```text
/// $ cargo check -p aterm-gui --lib     # send_usize -> send_isize
///     Finished `dev` profile in 14.45s
/// $ cargo test -p aterm-objc --test gui_sent_prototypes
///     alert_keys.rs:258: the site sends modifierFlags with send_isize,
///     the census says ["send_usize"]
/// ```
///
/// (The instance selector is spelled without its receiver here on purpose:
/// `tools/grep_guard.sh` B9a bans the pasteable name of the WindowServer-backed
/// CLASS-level flags query tree-wide, and the two share a suffix.)
///
/// So the guarantee this test adds is exactly the one the type system does not
/// give: wherever a send's result is cast, widened or handed straight on, the
/// signedness is checked by nothing else at all — which is the shape of the
/// `-requestUserAttention:` defect, whose value range also made it invisible.
#[test]
fn the_table_names_the_helper_each_site_actually_calls() {
    let repo = repo();
    let mut pairs: Vec<(String, String, String)> = Vec::new();
    for rel in SCOPE {
        let src = std::fs::read_to_string(repo.join(rel)).expect("a scope file is readable");
        // Comments stripped first, so a SAFETY note naming a helper beside a
        // selector is not a call site.
        let code: String = src
            .lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        let bytes = code.as_bytes();
        let mut i = 0;
        while let Some(at) = code[i..].find("send_") {
            let start = i + at;
            // A `send_…` that is part of a longer identifier is not a call.
            let is_start = start == 0 || !is_ident(bytes[start - 1] as char);
            let Some(open_rel) = code[start..].find('(') else {
                break;
            };
            let open = start + open_rel;
            let helper = &code[start..open];
            i = start + "send_".len();
            if !is_start || !helper.chars().all(is_ident) {
                continue;
            }
            // Balance to the closing paren; the call's arguments are inside.
            let mut depth = 1_i32;
            let mut j = open + 1;
            while j < bytes.len() && depth > 0 {
                match bytes[j] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            let body = &code[open + 1..j.saturating_sub(1)];
            // The selector is the argument AFTER the receiver, so the FIRST
            // `sel!` in the body is this call's own; a later one belongs to a
            // nested call passed as a further argument.
            let Some(sat) = body.find("sel!(") else {
                continue;
            };
            let rest = &body[sat + "sel!(".len()..];
            let Some(end) = rest.find(')') else { continue };
            let name = rest[..end].trim();
            if name.is_empty() || name.starts_with('$') {
                continue;
            }
            let line = code[..start].matches('\n').count() + 1;
            pairs.push((helper.to_owned(), name.to_owned(), format!("{rel}:{line}")));
        }
    }
    assert!(
        pairs.len() >= 45,
        "the source walk found only {} (helper, selector) pair(s) — it is not \
         reading the ports, and a check that reads nothing passes",
        pairs.len()
    );

    let mut findings = Vec::new();
    for (helper, name, at) in &pairs {
        let rows: Vec<&str> = all_rows()
            .iter()
            .filter(|r| r.1 == name)
            .map(|r| r.2)
            .collect();
        if rows.is_empty() {
            continue; // reported by `every_sent_selector_in_scope_has_a_row`
        }
        if !rows.contains(&helper.as_str()) {
            findings.push(format!(
                "{at}: the site sends {name} with {helper}, the census says {rows:?}"
            ));
        }
    }
    assert!(
        findings.is_empty(),
        "{} site(s) call a helper their census row does not name:\n  {}",
        findings.len(),
        findings.join("\n  ")
    );
}

/// A character Rust allows inside an identifier.
fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Every distinct selector spelled by a `sel!` site under `dir`, comments
/// stripped.
fn selectors_under(dir: &Path) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if !p.extension().is_some_and(|x| x == "rs") {
                continue;
            }
            if let Ok(src) = std::fs::read_to_string(&p) {
                out.extend(selectors_in(&src));
            }
        }
    }
    out
}

/// Every distinct selector one source file spells, comments stripped.
fn selectors_in(src: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for line in src.lines() {
        let code = line.split("//").next().unwrap_or("");
        let mut rest = code;
        while let Some(at) = rest.find("sel!(") {
            rest = &rest[at + "sel!(".len()..];
            let Some(end) = rest.find(')') else { break };
            let name = rest[..end].trim().to_owned();
            rest = &rest[end..];
            if !name.is_empty() && !name.starts_with('$') {
                out.insert(name);
            }
        }
    }
    out
}

/// The hand-written sites are named, and their names still point at a site.
#[test]
fn the_untyped_block_sends_name_their_sites() {
    let repo = repo();
    for (_, name, _, _, site) in HAND_WRITTEN {
        let (rel, _) = site.split_once(" — ").expect("`path — function` form");
        let src = std::fs::read_to_string(repo.join(rel))
            .unwrap_or_else(|e| panic!("{rel} is unreadable ({e})"));
        assert!(
            src.contains(name),
            "the hand-written row for {name} names {rel}, which no longer spells it"
        );
        // The claim that these have no typed helper is checked against
        // `aterm-objc` itself rather than remembered: a `send_*_block` helper
        // appearing there is the moment these rows stop being special.
        let send = std::fs::read_to_string(repo.join("crates/aterm-objc/src/send.rs"))
            .expect("send.rs is readable");
        assert!(
            !send.contains("_block("),
            "`aterm-objc` grew a block-taking send helper — these two sites \
             should use it, and this exception should go"
        );
    }
    assert_eq!(
        HAND_WRITTEN.len(),
        2,
        "an untyped `msg()` cast must argue for itself individually"
    );
}

/// `NSBeep` is a C FUNCTION, and the port binding it as one is checked here
/// rather than asserted in a comment.
///
/// `lib.rs` used to call `objc2_app_kit::NSBeep()`; W13 replaced it with
/// `appkit::beep`, a plain `extern "C"` call. If it were really a message the
/// symbol would not be in the dynamic symbol table at all, and a reader has no
/// way to tell those two apart by looking.
#[test]
fn nsbeep_is_a_c_function_not_a_message() {
    assert!(load_appkit(), "AppKit is not present");
    let name = CString::new("NSBeep").expect("no interior NUL");
    // SAFETY: `dlsym` with `RTLD_DEFAULT` and a valid C string; the result is
    // only compared against null.
    let sym = unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) };
    assert!(
        !sym.is_null(),
        "`NSBeep` is not an exported C symbol on this SDK — `appkit::beep` \
         cannot be a plain call"
    );
}

#[test]
fn the_expected_shapes_are_not_all_the_same() {
    let mut shapes: Vec<String> = all_rows().iter().map(|r| shape_of(r.2)).collect();
    shapes.sort();
    shapes.dedup();
    assert!(
        shapes.len() >= 12,
        "only {} distinct shapes: a census whose rows all spell one encoding \
         cannot catch a row that spells the wrong one",
        shapes.len()
    );
}

/// WHAT THIS CENSUS DOES NOT COVER, counted rather than described.
///
/// `crates/aterm-gui/src` sends far more selectors than [`SCOPE`] does. Those
/// belong to earlier waves (W1–W12), which ported them with the same hand-read
/// prototypes and left the same gap. The number is pinned here so the debt is a
/// figure someone can watch fall, not an impression — and so this file cannot
/// be mistaken for covering the crate.
#[test]
fn the_scope_is_a_named_fraction_of_the_crate() {
    let repo = repo();
    let crate_wide = selectors_under(&repo.join("crates/aterm-gui/src"));
    let mut in_scope = std::collections::BTreeSet::new();
    for rel in SCOPE {
        let src = std::fs::read_to_string(repo.join(rel)).expect("a scope file is readable");
        for line in src.lines() {
            let code = line.split("//").next().unwrap_or("");
            let mut rest = code;
            while let Some(at) = rest.find("sel!(") {
                rest = &rest[at + "sel!(".len()..];
                let Some(end) = rest.find(')') else { break };
                let name = rest[..end].trim().to_owned();
                rest = &rest[end..];
                if !name.is_empty() && !name.starts_with('$') {
                    in_scope.insert(name);
                }
            }
        }
    }
    assert_eq!(
        in_scope.len(),
        40,
        "the scope's distinct selector count moved to {} — re-derive the table",
        in_scope.len()
    );
    let uncovered = crate_wide.len() - in_scope.len();
    assert_eq!(
        uncovered, 280,
        "the UNCENSUSED remainder of `crates/aterm-gui/src` moved to {uncovered}. \
         That is not a failure — it is the number this file exists to make \
         visible. Update it in the commit that moves it, in either direction"
    );
}
