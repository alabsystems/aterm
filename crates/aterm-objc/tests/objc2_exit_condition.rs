// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE `objc2` EXIT CONDITION, WITH EYES — the scope is derived, not remembered.
//!
//! # The finding this file exists to answer
//!
//! Both endgame metrics in `docs/THIRD_PARTY_ROAD_TO_ZERO.md` were scoped to
//! two directories: `crates/aterm-gui/src` and
//! `vendor/winit/src/platform_impl/macos`. `vendor/winit/src/platform/macos.rs`
//! is in NEITHER, is COMPILED ON macOS (`platform/mod.rs` gates it on
//! `macos_platform`), and carried a live `objc2::rc::Retained::as_ptr` call. So
//! a wave could have driven both numbers to zero and shipped a build that still
//! linked `objc2` for a line no instrument was looking at.
//!
//! **An exit condition that cannot see a live use is not an exit condition.**
//! Fixing the line would have left the hole; this file fixes the SCOPE, and
//! does it the only way that survives the next file someone adds:
//!
//! * The scope is **everything under `vendor/winit/src` that the macOS build
//!   compiles**, defined by SUBTRACTION — the whole tree minus the six per-OS
//!   slices — plus all of `crates/aterm-gui/src`. A new shared file, or a new
//!   macOS-gated extension module, is in scope the day it is written, with
//!   nobody remembering to add it.
//! * The subtraction list is the SAME six slices `aterm-census`'s
//!   `REVIEWED_VENDORED_CRATES` registers, and every path is existence-checked,
//!   so a rename fails this test instead of silently widening the blind spot.
//! * A second walk sweeps ALL of `crates/` and `vendor/winit/src` and requires
//!   every `objc2` code use it finds to be inside the scope or inside a
//!   declared slice. That is what makes the scope a claim rather than a hope.
//!
//! # THE RULE STRIPS `//`, AND A DOC FENCE IS BEHIND `//` — thirteenth pass
//!
//! The scope was widened to include `vendor/winit/src/platform/macos.rs` and
//! the `objc2::rc::Retained::as_ptr` at its line 506 was ported. Four hundred
//! and seventy lines ABOVE that, in the same file, sits an application-delegate
//! example whose fence is `#![cfg_attr(target_os = "macos", doc = "```")]` and
//! `doc = "```ignore"` only when NOT macOS — five doc lines importing six
//! family items (`objc2::rc::Retained`, `objc2::runtime::ProtocolObject`,
//! `objc2::{declare_class, …}`, `objc2_app_kit::{…}`, `objc2_foundation::{…}`).
//!
//! `code_idents` breaks on `//`, so the widened scope walks straight past it.
//! F5's shape, recurring inside F5's own fix.
//!
//! **Its severity is stated rather than inflated: it does not compile here.**
//! `winit` is a path dependency and NOT a workspace member, so
//! `cargo test --doc -p winit` answers *"package `winit` cannot be tested
//! because it requires dev-dependencies and is not a member of the workspace"*,
//! and `cargo test --workspace` never reaches it. So this is not a live
//! compiled use the way line 506 was. What it IS: authored to compile on macOS,
//! and the fork's own public documentation instructing a reader to use crates
//! the exit condition is about to delete. Removing the `objc2` row would leave
//! a doc example that cannot build, and nothing here would say so.
//!
//! So it is COUNTED, by `the_doc_fences_that_name_the_family_are_counted`,
//! which walks the same scope for family names inside doc comments and pins the
//! total — so it cannot grow silently and cannot be forgotten at the moment the
//! packages leave.
//!
//! # What this file does NOT see, stated because the last one did not
//!
//! The rule matches NAMES. `window_delegate.rs` passes this test and still
//! CONSUMES an `objc2` type at two lines — `monitor.ns_screen(mtm)` answers
//! `Option<Retained<NSScreen>>`, which reaches `seam::obj_of<T>` through a
//! generic parameter, so no `objc2` token appears in the file. A file "off the
//! list" can still change when its unported neighbours port. The two lines are
//! named at their sites and in the roadmap.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};

/// The four crate names the endgame is about, spelled so this file does not
/// match its own rule.
const FAMILY: &[&str] = &[
    concat!("objc", "2"),
    concat!("objc", "2_app_kit"),
    concat!("objc", "2_foundation"),
    concat!("block", "2"),
];

/// The `block2` member, which is a SEPARATE crate on the same exit condition
/// and is reported apart from the `objc2` family everywhere.
const BLOCK2: &str = concat!("block", "2");

/// The per-OS subtrees the macOS build does not compile.
///
/// Identical to the `platform_slices` list `aterm-census`'s
/// `REVIEWED_VENDORED_CRATES` registers for `winit`, and existence-checked here
/// for the same reason it is there: a stale path is a blind spot, not a
/// nuisance.
const NON_MACOS_SLICES: &[(&str, &[&str])] = &[
    (
        "linux",
        &[
            "src/platform_impl/linux",
            "src/platform/x11.rs",
            "src/platform/wayland.rs",
            "src/platform/startup_notify.rs",
        ],
    ),
    (
        "windows",
        &["src/platform_impl/windows", "src/platform/windows.rs"],
    ),
    ("web", &["src/platform_impl/web", "src/platform/web.rs"]),
    (
        "android",
        &["src/platform_impl/android", "src/platform/android.rs"],
    ),
    ("ios", &["src/platform_impl/ios", "src/platform/ios.rs"]),
    (
        "orbital",
        &["src/platform_impl/orbital", "src/platform/orbital.rs"],
    ),
];

/// THE RECORDED COUNTS. Update them in the same commit that moves them, in
/// EITHER direction — a number that fell without being written down is how the
/// last scope went stale.
mod recorded {
    /// Files in the macOS-compiled scope with an `objc2`-family code use,
    /// counting `block2` (the documented rule).
    ///
    /// W9 phase 2: 15 -> 13. `cursor.rs` and `view.rs` are ported.
    /// Containment (main, 2026-09-05): 13 -> 12. `observer.rs`'s queued-closure
    /// block — its only family use, `block2` — became `aterm_objc::RcBlock`, and
    /// `block2` stopped being a direct dependency of the fork or of `aterm-gui`.
    ///
    /// W12: 13 -> 5, on its own branch, and the shape of the fall matters more
    /// than its size. `observer.rs` went first and took the count from 13 to 12
    /// WITHOUT moving `FILES_WITH_OBJC2`, because it was the one file in the
    /// scope whose only family name was `block2`. Then `menu.rs`, `monitor.rs`,
    /// `app.rs`, `app_state.rs`, `event_loop.rs`, `window.rs` and
    /// `aterm_objc_seam.rs` — which is ALL OF THEM. `WINIT_FILES` is zero.
    ///
    /// W13: 13 -> 8, on ITS own branch. `aterm-gui`'s last five are ported and
    /// its four rows retired. `GUI_FILES` is zero.
    ///
    /// THE MERGE, 2026-09-03: the two branches were cut from the same trunk
    /// and each recorded the other half unmoved (W12 wrote `GUI_FILES = 5`,
    /// W13 wrote `WINIT_FILES = 8`). Merged, both halves are zero, so every
    /// count below is zero — and the fork's four macOS rows, which
    /// `the_manifest_rows_are_live_or_dead_exactly_as_the_file_counts_say`
    /// holds to these counts, leave in the same commit.
    pub const FILES_WITH_FAMILY: usize = 0;
    /// The same, `objc2` proper only.
    pub const FILES_WITH_OBJC2: usize = 0;
    /// …split by tree.
    ///
    /// **Both halves are ZERO.** `aterm-gui`'s five (`alert_keys.rs`,
    /// `menu.rs`, `lib.rs`, `app_introspect.rs`, `appkit.rs`) went with W13;
    /// the fork's eight went with W12. What the merge can say that neither
    /// branch could: the `objc2`, `objc2-app-kit` and `objc2-foundation` rows
    /// under `cfg(target_os = "macos")` in `vendor/winit/Cargo.toml` are dead
    /// code AND dead features (`block2`'s row had already left with the
    /// containment, one wave earlier). W12 had
    /// measured them as live features — `aterm-gui` compiled only because the
    /// fork's `objc2-foundation` row enabled `NSThread`, `dispatch` and
    /// `NSEnumerator` for it — and held that in a test
    /// (`the_forks_objc2_rows_are_dead_code_but_live_features`) whose own
    /// failure message said to delete it the day `aterm-gui` stopped needing
    /// the hostage. W13's port is that day, and the test is gone with the rows.
    pub const GUI_FILES: usize = 0;
    pub const WINIT_FILES: usize = 0;
    /// The iOS slice, which no aterm target compiles. The retirement note that
    /// replaced the `objc2` row in `vendor/winit/Cargo.toml` cites these exact
    /// numbers.
    pub const IOS_FILES: usize = 9;
    pub const IOS_LINES: usize = 3_743;
    pub const IOS_FAMILY_FILES: usize = 8;
    pub const IOS_FAMILY_LINES: usize = 3_691;
    /// Family names inside RUNNING doc fences in the scope — invisible to the
    /// code rule by construction, because that rule strips `//`. See the note
    /// at the head of this file. Files, then lines.
    ///
    /// 1 file / 5 lines from the thirteenth pass until the W12 + W13 merge:
    /// `platform/macos.rs`'s application-delegate example, a live doctest on
    /// macOS. The packages it imports left the fork with the merge, so the
    /// example is fenced `ignore` on every platform now, with a note saying
    /// why — see `the_doc_fences_that_name_the_family_are_counted`, which
    /// pins that spelling so the fence cannot quietly go live again.
    pub const DOC_FENCE_FILES: usize = 0;
    pub const DOC_FENCE_LINES: usize = 0;
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// THE SCOPE: the macOS-compiled surface, by subtraction.
fn scope_files() -> Vec<PathBuf> {
    let repo = repo();
    let winit = repo.join("vendor/winit/src");
    let mut excluded: Vec<PathBuf> = Vec::new();
    for (label, paths) in NON_MACOS_SLICES {
        for p in *paths {
            let full = repo.join("vendor/winit").join(p);
            assert!(
                full.exists(),
                "the {label} slice path {p} is gone — the subtraction that \
                 defines the macOS scope is stale, and a stale subtraction \
                 WIDENS the blind spot silently"
            );
            excluded.push(full);
        }
    }

    let mut out: Vec<PathBuf> = rs_files(&winit)
        .into_iter()
        .filter(|f| !excluded.iter().any(|e| f.starts_with(e) || f == e))
        .collect();
    out.extend(rs_files(&repo.join("crates/aterm-gui/src")));
    out.sort();
    out
}

/// The scope is derived, and this is the case that proves it: the file that
/// hid.
#[test]
fn the_scope_sees_the_macos_gated_platform_extension() {
    let scope = scope_files();
    let repo = repo();
    for must in [
        "vendor/winit/src/platform/macos.rs",
        "vendor/winit/src/platform/mod.rs",
        "vendor/winit/src/platform_impl/macos/view.rs",
        "crates/aterm-gui/src/menu.rs",
    ] {
        let p = repo.join(must);
        assert!(scope.contains(&p), "{must} is not in the derived scope");
    }
    for must_not in [
        "vendor/winit/src/platform/ios.rs",
        "vendor/winit/src/platform_impl/ios/view.rs",
        "vendor/winit/src/platform_impl/windows/window.rs",
    ] {
        let p = repo.join(must_not);
        assert!(
            !scope.contains(&p),
            "{must_not} is a non-macOS slice and is in scope"
        );
    }
    assert!(
        scope.len() > 100,
        "the derived scope has only {} files",
        scope.len()
    );

    // `platform/macos.rs` is gated on `macos_platform` BY `platform/mod.rs`,
    // which is the fact the scope rests on. Read it rather than assume it.
    let mod_rs = std::fs::read_to_string(repo.join("vendor/winit/src/platform/mod.rs"))
        .expect("platform/mod.rs is readable");
    assert!(
        mod_rs.contains("#[cfg(any(macos_platform, docsrs))]\npub mod macos;"),
        "platform/mod.rs no longer gates `macos` on `macos_platform` — re-derive the scope"
    );
}

/// The file count that gates package removal, over the CORRECTED scope.
#[test]
fn the_family_file_count_is_what_is_recorded() {
    let repo = repo();
    let mut family = Vec::new();
    let mut objc2_only = Vec::new();
    for f in scope_files() {
        let (any, non_block2) = family_uses(&f);
        if any {
            family.push(f.clone());
        }
        if non_block2 {
            objc2_only.push(f);
        }
    }
    let rel = |v: &[PathBuf]| {
        v.iter()
            .map(|p| p.strip_prefix(&repo).unwrap_or(p).display().to_string())
            .collect::<Vec<_>>()
            .join("\n  ")
    };
    let gui = family
        .iter()
        .filter(|p| p.to_string_lossy().contains("aterm-gui"))
        .count();
    let winit = family.len() - gui;

    assert_eq!(
        family.len(),
        recorded::FILES_WITH_FAMILY,
        "files with a family code use moved to {} ({gui} gui, {winit} winit):\n  {}",
        family.len(),
        rel(&family)
    );
    assert_eq!(
        objc2_only.len(),
        recorded::FILES_WITH_OBJC2,
        "objc2-only count moved"
    );
    assert_eq!(gui, recorded::GUI_FILES, "the aterm-gui half moved");
    assert_eq!(winit, recorded::WINIT_FILES, "the winit half moved");
}

/// THE TEETH: nothing outside the scope may use the family in code.
///
/// This is the test the old scope could not have had, and the one that would
/// have caught `platform/macos.rs` the day it was written.
#[test]
fn every_family_use_in_the_tree_is_in_scope_or_in_a_declared_slice() {
    let repo = repo();
    let scope = scope_files();
    let mut slices: Vec<PathBuf> = Vec::new();
    for (_, paths) in NON_MACOS_SLICES {
        for p in *paths {
            slices.push(repo.join("vendor/winit").join(p));
        }
    }

    let mut stray = Vec::new();
    for root in ["crates", "vendor/winit/src"] {
        for f in rs_files(&repo.join(root)) {
            if !family_uses(&f).0 {
                continue;
            }
            if scope.contains(&f) || slices.iter().any(|s| f.starts_with(s) || &f == s) {
                continue;
            }
            stray.push(f.strip_prefix(&repo).unwrap_or(&f).display().to_string());
        }
    }
    assert!(
        stray.is_empty(),
        "a family code use is outside every declared scope — either it is \
         compiled (and the endgame metric cannot see it) or it belongs to a \
         slice nobody registered:\n  {}",
        stray.join("\n  ")
    );
}

/// What narrowing — and then retiring — the fork's macOS family rows cost,
/// checked.
///
/// The retirement note in `vendor/winit/Cargo.toml` cites these numbers in
/// prose. Prose goes stale; this does not.
#[test]
fn the_ios_slice_is_the_size_the_manifest_says_it_is() {
    let repo = repo();
    let mut files = Vec::new();
    for p in ["src/platform_impl/ios", "src/platform/ios.rs"] {
        let full = repo.join("vendor/winit").join(p);
        if full.is_dir() {
            files.extend(rs_files(&full));
        } else {
            files.push(full);
        }
    }
    let lines = |f: &PathBuf| {
        std::fs::read_to_string(f)
            .expect("readable")
            .lines()
            .count()
    };
    let total: usize = files.iter().map(lines).sum();
    let fam: Vec<&PathBuf> = files.iter().filter(|f| family_uses(f).0).collect();
    let fam_lines: usize = fam.iter().map(|f| lines(f)).sum();

    assert_eq!(
        files.len(),
        recorded::IOS_FILES,
        "the iOS slice's file count moved"
    );
    assert_eq!(
        total,
        recorded::IOS_LINES,
        "the iOS slice's line count moved"
    );
    assert_eq!(
        fam.len(),
        recorded::IOS_FAMILY_FILES,
        "the iOS family-file count moved"
    );
    assert_eq!(
        fam_lines,
        recorded::IOS_FAMILY_LINES,
        "the iOS family-line count moved"
    );

    // …and the rows themselves are GONE, or the whole argument above is
    // decoration. W9 phase 3 narrowed `objc2` and `block2` to `cfg(macos)` so
    // that an iOS backend nothing compiles could not hold them in the mac-arm
    // graph at zero macOS uses; the W12 + W13 merge reached zero and retired
    // all four. `the_manifest_rows_are_live_or_dead_exactly_as_the_file_counts_say`
    // holds the rows to the counts through `declared_deps`; this is the cheaper
    // textual spelling, kept so a re-added row is named by the test that
    // explains the narrowing that came first.
    let manifest = std::fs::read_to_string(repo.join("vendor/winit/Cargo.toml"))
        .expect("the fork's manifest is readable");
    // `block2` went one step further than narrowed, and first: its last macOS
    // use (`observer.rs`'s queued-closure block) became `aterm_objc::RcBlock`
    // with the containment and the row left then; `tests/send_prototype_census.rs`
    // refuses a new one. The other three left with the W12 + W13 merge.
    for name in [FAMILY[0], BLOCK2, "objc2-app-kit", "objc2-foundation"] {
        assert!(
            !manifest.contains(&format!(
                "[target.'cfg(target_os = \"macos\")'.dependencies.{name}]"
            )),
            "the fork's macOS `{name}` row is back — it was retired at the exit \
             condition, and nothing on a macOS-compiled path uses it"
        );
    }
}

/// The family names the CODE rule cannot see, because they are behind `//`.
///
/// A doc comment is not compiled by this workspace — `winit` is not a member,
/// so its doctests never run here — but it is the fork's own instruction to a
/// reader, and an exit condition that deletes a package while the
/// documentation still teaches that package is not finished. Counted so it
/// cannot grow, and so the number is in front of whoever removes the rows.
#[test]
fn the_doc_fences_that_name_the_family_are_counted() {
    let repo = repo();
    let mut hits: Vec<(String, usize, String)> = Vec::new();
    for f in scope_files() {
        let Ok(src) = std::fs::read_to_string(&f) else {
            continue;
        };
        // INSIDE A DOC CODE FENCE, and only there. A name in prose — this tree
        // has four, all of them notes ABOUT the port — is a description, not an
        // instruction, and counting it would make the number meaningless.
        //
        // The fence can be opened two ways, and `platform/macos.rs` uses the
        // second: a `//!`/`///` line whose body starts with a fence, or a
        // `#[doc = "```…"]` attribute. Where that attribute is `cfg_attr`'d,
        // this walk models THE macOS BUILD — the `target_os = "macos"` arm is
        // authoritative and the `not(...)` arm is skipped — because a scope
        // defined as "what macOS compiles" that then read the non-macOS arm
        // would cancel its own toggle and see nothing.
        let mut in_fence = false;
        let mut fence_runs = true;
        for (i, raw) in src.lines().enumerate() {
            let t = raw.trim_start();
            let attr_fence = t.contains("doc = \"```") && !t.contains("not(target_os");
            let doc_body = t.strip_prefix("//!").or_else(|| t.strip_prefix("///"));
            if attr_fence {
                let info = t.split("doc = \"```").nth(1).unwrap_or("");
                in_fence = !in_fence;
                fence_runs = !(info.starts_with("ignore") || info.starts_with("text"));
                continue;
            }
            let Some(body) = doc_body else { continue };
            if let Some(info) = body.trim_start().strip_prefix("```") {
                in_fence = !in_fence;
                fence_runs = !(info.starts_with("ignore") || info.starts_with("text"));
                continue;
            }
            if !in_fence || !fence_runs {
                continue;
            }
            // The same "followed by `::`/`;`/`,`/`}`" rule the code walk uses.
            if code_idents(body)
                .iter()
                .any(|n| FAMILY.contains(&n.as_str()))
            {
                hits.push((
                    f.strip_prefix(&repo).unwrap_or(&f).display().to_string(),
                    i + 1,
                    t.to_owned(),
                ));
            }
        }
    }
    let mut files: Vec<String> = hits.iter().map(|(f, _, _)| f.clone()).collect();
    files.sort();
    files.dedup();
    let listing = hits
        .iter()
        .map(|(f, l, t)| format!("{f}:{l}: {t}"))
        .collect::<Vec<_>>()
        .join("\n  ");
    assert_eq!(
        files.len(),
        recorded::DOC_FENCE_FILES,
        "the number of files teaching the family in doc comments moved:\n  {listing}"
    );
    assert_eq!(
        hits.len(),
        recorded::DOC_FENCE_LINES,
        "the number of doc lines teaching the family moved:\n  {listing}"
    );
    // The one that USED to count is `platform/macos.rs`'s application-delegate
    // example, whose fence was ENABLED on macOS through a `cfg_attr`. The
    // packages it imports left the fork at the W12 + W13 merge, so it is
    // `ignore`-fenced on every platform now and the walk above skips it by the
    // same rule it skips any `ignore` fence. Both halves are asserted — the
    // example is still there, and its fence is still the retired spelling — so
    // the zero above is a measurement of THAT file and not of a deleted one,
    // and a `cfg_attr` that quietly re-enables it fails here by name.
    let src =
        std::fs::read_to_string(repo.join("vendor/winit/src/platform/macos.rs")).expect("readable");
    assert!(
        src.contains("//! use objc2_app_kit::{NSApplication, NSApplicationDelegate};"),
        "the application-delegate example is gone from platform/macos.rs — re-derive this count"
    );
    assert!(
        !src.contains(r#"doc = "```")]"#),
        "the application-delegate example's fence is live again — the packages it \
         imports are not dependencies of this fork"
    );
    assert!(
        src.contains("//! ```ignore\n//! use objc2::rc::Retained;"),
        "the application-delegate example is no longer `ignore`-fenced — re-derive this count"
    );
}

/// THE RULE READS EVERY SPELLING OF AN IMPORT, not just the punctuated ones.
///
/// `code_idents` is the whole endgame metric: it decides which files count, and
/// through `every_family_use_in_the_tree_is_in_scope_or_in_a_declared_slice` it
/// decides whether anything is hiding. It matched a family name followed by
/// `::`, `;`, `,` or `}` — and a renaming import has none of those.
#[test]
fn the_code_rule_sees_a_renaming_import() {
    let family_seen = |l: &str| code_idents(l).iter().any(|n| FAMILY.contains(&n.as_str()));
    for line in [
        "use objc2::rc::Retained;",
        "extern crate objc2;",
        "use objc2::{rc::Retained, runtime::AnyObject};",
        "    let x = objc2::rc::autoreleasepool(|_| ());",
        "use objc2::rc::Retained as R;",
        // The three the rule could not see until pass 14.
        "use objc2 as oc;",
        "use objc2_app_kit as ak;",
        "extern crate objc2 as oc;",
    ] {
        assert!(family_seen(line), "the code rule cannot see {line:?}");
    }
    // …and it still does not fire on prose or on a quoted call, which is what
    // keeps the count meaningful.
    for line in [
        "// objc2::rc::Retained is what this replaced",
        "    assert!(msg.contains(\"objc2::rc::Retained\"));",
        "    let n = x as usize;",
    ] {
        assert!(!family_seen(line), "the code rule fired on {line:?}");
    }
}

/// Every `*.rs` under `dir`, recursively (empty if `dir` does not exist).
fn rs_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    out
}

/// `(any family name, any name other than block2)` used in CODE.
///
/// The documented rule: strip `//` comments AND string literals, then require
/// a family name followed by `::`, `;`, `,` or `}`. Stripping the literals is
/// load-bearing — it is what keeps a test assertion quoting a call, or a bare
/// word in a message, from counting as a port that has not happened.
fn family_uses(file: &Path) -> (bool, bool) {
    let Ok(src) = std::fs::read_to_string(file) else {
        return (false, false);
    };
    let (mut any, mut non_block2) = (false, false);
    for line in src.lines() {
        for name in code_idents(line) {
            if FAMILY.contains(&name.as_str()) {
                any = true;
                if name != BLOCK2 {
                    non_block2 = true;
                }
            }
        }
    }
    (any, non_block2)
}

/// The identifiers on `line`, outside comments and string literals, that are
/// followed by `::`, `;`, `,` or `}`.
fn code_idents(line: &str) -> Vec<String> {
    let b: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == '/' && i + 1 < b.len() && b[i + 1] == '/' {
            break;
        }
        if c == '"' {
            i += 1;
            while i < b.len() && b[i] != '"' {
                i += if b[i] == '\\' { 2 } else { 1 };
            }
            i += 1;
            continue;
        }
        if c.is_alphanumeric() || c == '_' {
            let start = i;
            while i < b.len() && (b[i].is_alphanumeric() || b[i] == '_') {
                i += 1;
            }
            let mut j = i;
            while j < b.len() && b[j].is_whitespace() {
                j += 1;
            }
            // …OR the `as` of a RENAMING import. `use objc2 as oc;` and
            // `use objc2_app_kit as ak;` put the family name in code with none
            // of the four punctuation marks after it, so the fourteenth pass
            // measured them slipping through this rule — and with them the
            // TEETH test, which shares this function, so a renamed import
            // anywhere in `crates/` was invisible too.
            //
            // Severity stated rather than inflated: nothing in the tree renames
            // a crate root, and the packages cannot actually leave while any
            // code uses them, because the build would fail. What was wrong is
            // that the number deciding when they MAY leave could not see one
            // way of writing the thing it counts.
            let as_kw =
                b[j..].starts_with(&['a', 's']) && b.get(j + 2).is_none_or(|c| c.is_whitespace());
            let follows = if j + 1 < b.len() && b[j] == ':' && b[j + 1] == ':' {
                true
            } else {
                as_kw || matches!(b.get(j), Some(';' | ',' | '}'))
            };
            if follows {
                out.push(b[start..i].iter().collect());
            }
            continue;
        }
        i += 1;
    }
    out
}

/// A manifest with its COMMENTS STRIPPED — the TOML twin of [`code_idents`].
///
/// # Why this exists: the guard was armed at a spelling, not at the code
///
/// W13 phase 2 retired `aterm-gui`'s four rows and wrote, in the comment that
/// replaces them, what they used to name — including the literal
/// `objc2-app-kit/NSAccessibility`, because a retirement note that cannot say
/// what was retired is not a note. The backstop below asked
/// `gui.contains("objc2-app-kit/NSAccessibility")` over the RAW FILE, so the
/// documentation of the removal read as the removal not having happened, and
/// the test failed on a correct tree.
///
/// That is this campaign's recurring defect one more time: a guard whose
/// subject is the TEXT rather than the CODE. A feature list and a sentence
/// about a feature list are different things, and only the first can affect a
/// build. So the subject is now the code: comments go, then the question is
/// asked. Both arms of the backstop use it, so neither can be satisfied — or
/// broken — by prose.
///
/// Comments are stripped the way TOML actually defines them: `#` starts one
/// only OUTSIDE a string, so a `#` inside `"…"` or `'…'` is data. A naive
/// line-wise `starts_with('#')` would miss a trailing comment, and a naive
/// "cut at the first `#`" would corrupt any row whose value contains one.
fn manifest_code(manifest: &str) -> String {
    let mut out = String::with_capacity(manifest.len());
    for line in manifest.lines() {
        let mut quote: Option<char> = None;
        let mut end = line.len();
        for (i, c) in line.char_indices() {
            match (quote, c) {
                (None, '"' | '\'') => quote = Some(c),
                (Some(q), c) if c == q => quote = None,
                (None, '#') => {
                    end = i;
                    break;
                }
                _ => {}
            }
        }
        out.push_str(line[..end].trim_end());
        out.push('\n');
    }
    out
}

/// EVERY DEPENDENCY A MANIFEST DECLARES, IN EITHER SPELLING, with the table it
/// was declared in.
///
/// # The blind spot this closes, which was measured before it was fixed
///
/// The row check used to be `line.starts_with("{name} = ")` for `aterm-gui` and
/// `manifest.contains("[target.…dependencies.{name}]")` for the fork — that is,
/// each half knew exactly ONE of Cargo's two spellings, and each knew the other
/// half's. Cargo accepts both everywhere:
///
/// ```toml
/// [target.'cfg(target_os = "macos")'.dependencies]
/// objc2-app-kit = { version = "0.2.2" }        # inline row
///
/// [target.'cfg(target_os = "macos")'.dependencies.objc2-app-kit]
/// version = "0.2.2"                            # table row — the FORK's own form
/// ```
///
/// So `aterm-gui` could re-acquire the whole family in the fork's spelling and
/// the guard would answer "retired". PLANTED AND MEASURED before this helper
/// existed: adding the table form above to `crates/aterm-gui/Cargo.toml` left
/// `the_manifest_rows_are_live_or_dead_exactly_as_the_file_counts_say` GREEN
/// while `cargo tree -p aterm-gui` showed `objc2-app-kit v0.2.2` live in the
/// graph. The guard was armed at a spelling of its own rule; its subject is the
/// declared dependency now, however it is written.
///
/// The table CONTEXT comes back with each name because it is load-bearing in
/// one direction: the fork legitimately keeps `objc2-foundation` and
/// `objc2-ui-kit` rows under `cfg(target_os = "ios")`, a target no aterm build
/// compiles, and those must NOT read as macOS rows. `aterm-gui` has no such
/// exemption — a family dependency in ANY of its tables is a live dependency.
///
/// Dotted spellings (`name.workspace = true`, `name.version = "…"`) count too.
///
/// # THE NAME IT REPORTS IS THE PACKAGE, NOT THE KEY — sixteenth pass
///
/// Two further spellings were PLANTED into `crates/aterm-gui/Cargo.toml` and
/// MEASURED. For both, `cargo tree -i -p objc2-app-kit --target
/// aarch64-apple-darwin -e normal` printed `aterm-gui` as a DIRECT parent again
/// — the exact edge W13 phase 2 retired — while
/// `the_manifest_rows_are_live_or_dead_exactly_as_the_file_counts_say` reported
/// `ok. 1 passed`:
///
/// ```toml
/// [target.'cfg(target_os = "macos")'.dependencies]
/// appkit_bindings = { package = "objc2-app-kit", version = "0.2" }  # PLANT A
/// "objc2-app-kit" = { version = "0.2" }                             # PLANT B
/// ```
///
/// PLANT A is the manifest twin of the defect
/// [`the_code_rule_sees_a_renaming_import`] closed on the code side, and it is
/// worse here than there: `package = "…"` is a Cargo RENAME, whose entire
/// purpose is that the key stops being the package name, so a guard keyed on
/// the key cannot see the dependency at all. The file already knew renaming was
/// this campaign's defect shape — it had simply never asked the question of a
/// manifest. PLANT B is quoting, which TOML permits on any key including one
/// that needs none, and which `rsplit_once('.')` and `split_once(['=', '.'])`
/// both hand back with the quote characters still attached.
///
/// So this reports the PACKAGE each row resolves to: keys are unquoted, and a
/// `package = "…"` field — in an inline value, in the dotted `key.package`
/// form, or as a row inside a `[…dependencies.key]` table — REPLACES the key.
/// The subject is the dependency, not the spelling of its name.
///
/// The table CONTEXT still comes back with each name, for the `ios` exemption
/// above.
fn declared_deps(manifest: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut table = String::new();
    // Where in `out` the row a `[…dependencies.NAME]` header opened lives, so a
    // `package = "…"` row inside that table can rename it.
    let mut table_row: Option<usize> = None;
    for line in manifest_code(manifest).lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            table = header.to_string();
            table_row = None;
            // `[…dependencies.NAME]` — the row IS the table.
            if let Some((prefix, name)) = header.rsplit_once('.')
                && prefix.ends_with("dependencies")
            {
                table_row = Some(out.len());
                out.push((prefix.to_string(), unquote_key(name).to_string()));
            }
            continue;
        }
        // A `package = "…"` row inside a `[…dependencies.NAME]` table renames it.
        if let Some(i) = table_row
            && let Some((key, value)) = line.split_once('=')
            && unquote_key(key) == "package"
            && let Some(real) = first_string(value)
        {
            out[i].1 = real.to_string();
            continue;
        }
        // `[…dependencies]` table: `NAME = …`, `NAME.field = …`.
        if table.ends_with("dependencies")
            && let Some((path, value)) = line.split_once('=')
        {
            let mut segments = path.split('.');
            let Some(name) = segments.next().map(unquote_key).filter(|n| !n.is_empty()) else {
                continue;
            };
            let field: Vec<&str> = segments.map(unquote_key).collect();
            // `NAME.package = "real"` and `NAME = { package = "real", … }` are
            // one rename written two ways. Anything else keeps the key: a row
            // literally named `package` (a legal crate name) has an empty field
            // path, so its own value is never mistaken for a rename target.
            let renamed = if field == ["package"] {
                first_string(value)
            } else if field.is_empty() {
                inline_rename(value)
            } else {
                None
            };
            out.push((table.clone(), renamed.unwrap_or(name).to_string()));
        }
    }
    out
}

/// One layer of TOML key quoting removed — `"objc2-app-kit"` and
/// `'objc2-app-kit'` are the same key as `objc2-app-kit`.
fn unquote_key(key: &str) -> &str {
    let key = key.trim();
    for q in ['"', '\''] {
        if key.len() >= 2 && key.starts_with(q) && key.ends_with(q) {
            return key[1..key.len() - 1].trim();
        }
    }
    key
}

/// The first quoted string in `value`, unquoted.
fn first_string(value: &str) -> Option<&str> {
    let value = value.trim_start();
    for q in ['"', '\''] {
        if let Some(rest) = value.strip_prefix(q)
            && let Some(end) = rest.find(q)
        {
            return Some(&rest[..end]);
        }
    }
    None
}

/// The `package = "…"` rename inside an INLINE dependency value, if it has one.
///
/// `package` is required to be a whole key — preceded by a boundary and
/// followed by `=` — so `default-features`, a feature literally called
/// `"package"`, and a path ending in `package` are all left alone.
fn inline_rename(value: &str) -> Option<&str> {
    let mut rest = value;
    while let Some(i) = rest.find("package") {
        let boundary = rest[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != '-');
        let after = &rest[i + "package".len()..];
        if boundary
            && let Some(v) = after.trim_start().strip_prefix('=')
            && let Some(name) = first_string(v)
        {
            return Some(name);
        }
        rest = after;
    }
    None
}

/// THE MANIFEST RULE READS EVERY SPELLING OF A DEPENDENCY — the twin of
/// [`the_code_rule_sees_a_renaming_import`], asked of the other half.
///
/// That test exists because a renaming IMPORT (`use objc2 as oc;`) slipped the
/// code rule. Nobody had asked the same question of the manifest rule, and the
/// answer was worse: a renaming DEPENDENCY
/// (`appkit = { package = "objc2-app-kit" }`) slipped it, and so did a merely
/// QUOTED key. Both were planted into `crates/aterm-gui/Cargo.toml`, and for
/// both `cargo tree -i -p objc2-app-kit` printed `aterm-gui` as a direct parent
/// while the rows test passed — see [`declared_deps`].
///
/// The subject here is the PACKAGE. Every row below declares `objc2-app-kit`;
/// they differ only in how it is written.
#[test]
fn the_manifest_rule_sees_a_renamed_dependency() {
    const MACOS: &str = "target.'cfg(target_os = \"macos\")'.dependencies";
    let names = |m: &str| -> Vec<String> { declared_deps(m).into_iter().map(|(_, n)| n).collect() };

    for (label, manifest) in [
        (
            "plain inline row",
            "[target.'cfg(target_os = \"macos\")'.dependencies]\n             objc2-app-kit = { version = \"0.2\" }\n",
        ),
        (
            "dotted row",
            "[target.'cfg(target_os = \"macos\")'.dependencies]\n             objc2-app-kit.workspace = true\n",
        ),
        (
            "table row — the fork's own form",
            "[target.'cfg(target_os = \"macos\")'.dependencies.objc2-app-kit]\n             version = \"0.2\"\n",
        ),
        (
            "PLANT B — double-quoted key",
            "[target.'cfg(target_os = \"macos\")'.dependencies]\n             \"objc2-app-kit\" = { version = \"0.2\" }\n",
        ),
        (
            "PLANT B — single-quoted key",
            "[target.'cfg(target_os = \"macos\")'.dependencies]\n             'objc2-app-kit' = { version = \"0.2\" }\n",
        ),
        (
            "PLANT B — quoted key in a table header",
            "[target.'cfg(target_os = \"macos\")'.dependencies.\"objc2-app-kit\"]\n             version = \"0.2\"\n",
        ),
        (
            "PLANT A — inline rename",
            "[target.'cfg(target_os = \"macos\")'.dependencies]\n             appkit_bindings = { package = \"objc2-app-kit\", version = \"0.2\" }\n",
        ),
        (
            "PLANT A — dotted rename",
            "[target.'cfg(target_os = \"macos\")'.dependencies]\n             appkit_bindings.package = \"objc2-app-kit\"\n             appkit_bindings.version = \"0.2\"\n",
        ),
        (
            "PLANT A — rename inside a dependency table",
            "[target.'cfg(target_os = \"macos\")'.dependencies.appkit_bindings]\n             package = \"objc2-app-kit\"\n             version = \"0.2\"\n",
        ),
    ] {
        let deps = declared_deps(manifest);
        assert!(
            deps.iter().any(|(t, n)| t == MACOS && n == "objc2-app-kit"),
            "the manifest rule cannot see {label}: it read {deps:?}"
        );
    }

    // …and it still does not fire on prose, on a feature list, on a row that
    // merely mentions the name, or on the near-miss `objc2` — which is what
    // keeps the classification meaningful in the other direction.
    for (label, manifest) in [
        (
            "a commented-out row",
            "[target.'cfg(target_os = \"macos\")'.dependencies]\n             # objc2-app-kit = { version = \"0.2\" }\n",
        ),
        (
            "a feature list naming a binding feature",
            "[features]\na11y-appkit = [\"objc2-app-kit/NSAccessibility\"]\n",
        ),
        (
            "the package's own name",
            "[package]\nname = \"objc2-app-kit\"\n",
        ),
        (
            "the shorter family name",
            "[target.'cfg(target_os = \"macos\")'.dependencies]\nobjc2 = \"0.5\"\n",
        ),
        (
            "a crate literally called `package`",
            "[dependencies]\npackage = \"1.0\"\n",
        ),
    ] {
        assert!(
            !names(manifest).contains(&"objc2-app-kit".to_string()),
            "the manifest rule fired on {label}"
        );
    }

    // The ios exemption survives the rewrite: a renamed row under the ios cfg
    // comes back under the IOS table, so it can still be told apart from a
    // macOS one.
    let ios = declared_deps(
        "[target.'cfg(target_os = \"ios\")'.dependencies]\n         uikit = { package = \"objc2-ui-kit\", version = \"0.2\" }\n",
    );
    assert_eq!(
        ios,
        vec![(
            "target.'cfg(target_os = \"ios\")'.dependencies".to_string(),
            "objc2-ui-kit".to_string()
        )],
        "the table context was lost, and with it the ios exemption"
    );
}

/// WHICH MANIFEST ROWS ARE DEAD CODE, DECIDED BY THE FILE COUNTS ABOVE.
///
/// # The claim this exists to refute, and the measurement that refutes it
///
/// W13 was planned against a premise: "the fork's CODE is at zero, so its four
/// macOS family rows are dead code", and "deleting the fork's rows alone leaves
/// winit compiling and breaks `aterm-gui` with five errors". **Both halves are
/// backwards**, and [`recorded::WINIT_FILES`] is why: eight files of the
/// compiled macOS backend still use the family. MEASURED, by deleting the four
/// rows and asking the compiler:
///
/// ```text
/// $ cargo check -p winit          # with the fork's 4 macOS family rows deleted
/// 27 errors: unresolved import `objc2_foundation` (7), `objc2_app_kit` (6),
///            `objc2` (3), `block2` (1); cannot find module `objc2` (9), `block2` (1)
/// ```
///
/// The other direction, measured the same way after W13's port:
///
/// ```text
/// $ cargo check -p aterm-gui --all-targets   # with aterm-gui's 4 rows AND the
///                                            # `a11y-appkit` feature list deleted
/// exit 0
/// $ cargo forge survey --cell mac-arm
/// mac-arm  116  69  47  563,759  24,865      # byte-identical to before
/// ```
///
/// So the rows fall into two classes and this test is the classification:
///
/// * **`aterm-gui`'s four are DEAD CODE** — `GUI_FILES` is 0, and the crate
///   compiles without them. Retiring them costs nothing and BUYS nothing on its
///   own: with them gone, `cargo tree -i` shows every one of the six family
///   packages held by `vendor/winit` alone, so the package count, the LOC and
///   the unsafe-token count do not move by one.
/// * **The fork's four are LIVE** — `WINIT_FILES` is 8. The `objc2` row in
///   `vendor/winit/Cargo.toml` says this itself ("`objc2` leaves only when every
///   file in this backend has stopped using it"), and it is the row's own words
///   that this test holds it to.
///
/// **The package set moves when the FORK's eight files port, and at no earlier
/// commit.** That is a port, not a manifest edit, and writing it down here is
/// what stops the next wave from planning against the premise this one did.
///
/// # What phase 2 then did, and what it was worth
///
/// It retired `aterm-gui`'s four rows and emptied the `a11y-appkit` feature
/// list, so this test now takes its `gui_present.is_empty()` arm. The survey
/// across all five cells was byte-identical over that commit — 0 packages,
/// 0 LOC, 0 unsafe tokens — exactly as the classification above predicted, and
/// `cargo tree -i` now answers `winit` alone for all four family packages.
/// The value is that the fork is the SOLE remaining parent: there is no longer
/// a second row anywhere that could be mistaken for the thing holding them.
///
/// The doc fence in `vendor/winit/src/platform/macos.rs` was deliberately NOT
/// removed with them, and `the_doc_fences_that_name_the_family_are_counted`
/// still counts its 5 lines. It teaches a reader to write an
/// `NSApplicationDelegate` against `objc2` — crates the FORK still depends on
/// and still compiles against. Deleting accurate documentation ahead of the
/// retirement it belongs to would have been cosmetic churn that also made the
/// count lie about where the campaign stands.
#[test]
fn the_manifest_rows_are_live_or_dead_exactly_as_the_file_counts_say() {
    let repo = repo();
    // BOTH manifests are read as CODE, not as text — see `manifest_code`. The
    // prose in either one may name any row it likes; only a live row counts.
    let fork = manifest_code(
        &std::fs::read_to_string(repo.join("vendor/winit/Cargo.toml"))
            .expect("the fork's manifest is readable"),
    );
    let gui = manifest_code(
        &std::fs::read_to_string(repo.join("crates/aterm-gui/Cargo.toml"))
            .expect("aterm-gui's manifest is readable"),
    );

    // The fork's four rows. Read in EITHER spelling (see `declared_deps`) but
    // ONLY under the macOS cfg: the fork keeps legitimate `objc2-foundation`
    // and `objc2-ui-kit` rows under `cfg(target_os = "ios")`, and counting one
    // of those as a macOS row would report this exit condition as further from
    // done than it is.
    let fork_rows = [FAMILY[0], BLOCK2, "objc2-app-kit", "objc2-foundation"];
    let fork_macos = "target.'cfg(target_os = \"macos\")'.dependencies";
    let fork_deps = declared_deps(&fork);
    let present: Vec<&str> = fork_rows
        .iter()
        .copied()
        .filter(|name| {
            fork_deps
                .iter()
                .any(|(table, dep)| table == fork_macos && dep == name)
        })
        .collect();

    // THE LOAD-BEARING ARM IS GONE WITH THE ROWS. While `WINIT_FILES` was
    // above zero this asserted all four rows PRESENT ("deleting them was
    // measured at 27 compile errors in `winit` itself"); the W12 + W13 merge
    // took the count to zero, so the arm that pinned the rows in place would
    // now be dead code guarding a constant — which is exactly what clippy
    // says of `WINIT_FILES > 0`. Like the aterm-gui half below, this half is
    // written for a ported fork, and says so rather than branching on it.
    assert_eq!(
        recorded::WINIT_FILES,
        0,
        "this test's fork half is written for a ported backend; re-derive it"
    );
    assert!(
        present.is_empty(),
        "the fork's macOS backend is at zero family uses, so these rows are \
         dead code and owe their retirement: {present:?}"
    );

    // `aterm-gui`'s four. ANY table counts here — unlike the fork, this crate
    // has no target it does not build, so a family dependency in any of its
    // dependency tables, in either spelling, is a live dependency.
    let gui_rows = ["objc2-app-kit", "objc2-foundation", FAMILY[0], BLOCK2];
    let gui_deps = declared_deps(&gui);
    let gui_present: Vec<&str> = gui_rows
        .iter()
        .copied()
        .filter(|name| gui_deps.iter().any(|(_, dep)| dep == name))
        .collect();
    assert_eq!(
        recorded::GUI_FILES,
        0,
        "this test's aterm-gui half is written for a ported crate; re-derive it"
    );
    // W13 phase 2 RETIRED all four, so the live tree takes the empty arm. What
    // is pinned is still the PAIRING rather than "they must be gone", because
    // the property that matters survives in both directions: all four present
    // or all four absent. They are one decision, and a partial retirement
    // leaves a manifest claiming a mixed state nothing is in — which is also
    // the shape a careless re-add would take, so the rule keeps its teeth after
    // the retirement it was written before.
    // The `a11y-appkit` feature list travels with them for the same reason —
    // it names `objc2-app-kit/*` features, and cargo refuses a feature list that
    // points at an absent dependency ("feature `a11y-appkit` includes
    // `objc2-app-kit/NSAccessibility`, but `objc2-app-kit` is not a dependency"),
    // measured.
    assert!(
        gui_present.len() == gui_rows.len() || gui_present.is_empty(),
        "aterm-gui's four family rows are ONE decision and this tree has {} of \
         them: {gui_present:?}. Retire all four together, and the `a11y-appkit` \
         feature list with them",
        gui_present.len()
    );
    if gui_present.is_empty() {
        // A BACKSTOP, AND IT IS NOT THE CATCHER. Measured: with the four rows
        // deleted and the feature list left, cargo refuses to load the workspace
        // at all ("feature `a11y-appkit` includes `objc2-app-kit/NSAccessibility`,
        // but `objc2-app-kit` is not a dependency"), so no test in this file ever
        // runs to say it. This arm exists for the shape cargo would NOT catch —
        // a feature list rewritten to name something else while still teaching
        // the retired crate — and the claim about which instrument catches what
        // is written down rather than assumed, because a guard credited with a
        // catch it does not make is how the last two passes' defects survived.
        assert!(
            !gui.contains("objc2-app-kit/NSAccessibility"),
            "the rows are retired but the `a11y-appkit` feature list still names \
             `objc2-app-kit` features; cargo refuses that manifest"
        );
    } else {
        assert!(
            gui.contains("objc2-app-kit/NSAccessibility"),
            "the `a11y-appkit` feature list lost its `objc2-app-kit` features \
             while the rows are still here — re-derive which of the two moved"
        );
    }
}
