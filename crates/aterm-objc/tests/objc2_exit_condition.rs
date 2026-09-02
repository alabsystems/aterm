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
    pub const FILES_WITH_FAMILY: usize = 13;
    /// The same, `objc2` proper only.
    pub const FILES_WITH_OBJC2: usize = 12;
    /// …split by tree.
    ///
    /// The `gui` half has not moved since W7 and the `winit` half has moved
    /// every wave since, which is the shape of the remaining work rather than
    /// an accident: `aterm-gui`'s five are the crate's own AppKit surface and
    /// each needs a decision, while the fork's are a backend being ported file
    /// by file.
    pub const GUI_FILES: usize = 5;
    pub const WINIT_FILES: usize = 8;
    /// The iOS slice, which no aterm target compiles and which the `objc2` row
    /// in `vendor/winit/Cargo.toml` cites by these exact numbers.
    pub const IOS_FILES: usize = 9;
    pub const IOS_LINES: usize = 3_743;
    pub const IOS_FAMILY_FILES: usize = 8;
    pub const IOS_FAMILY_LINES: usize = 3_691;
    /// Family names inside DOC COMMENTS in the scope — invisible to the code
    /// rule by construction, because that rule strips `//`. See the note at the
    /// head of this file. Files, then lines.
    pub const DOC_FENCE_FILES: usize = 1;
    pub const DOC_FENCE_LINES: usize = 5;
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

/// What the narrowed `objc2` row in `vendor/winit/Cargo.toml` costs, checked.
///
/// The row cites these numbers in prose. Prose goes stale; this does not.
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

    // …and the manifest row must actually be narrowed, or the whole argument
    // above is decoration.
    let manifest = std::fs::read_to_string(repo.join("vendor/winit/Cargo.toml"))
        .expect("the fork's manifest is readable");
    for name in [FAMILY[0], BLOCK2] {
        assert!(
            manifest.contains(&format!(
                "[target.'cfg(target_os = \"macos\")'.dependencies.{name}]"
            )),
            "{name} is not gated on macOS alone — at zero macOS uses it would \
             stay in the mac-arm graph, held by an iOS backend nothing compiles"
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
    // The one that exists is `platform/macos.rs`'s application-delegate
    // example, and its fence is ENABLED on macOS. Both halves are asserted, so
    // a future reader cannot mistake it for a `text` block.
    assert!(
        files[0].ends_with("vendor/winit/src/platform/macos.rs"),
        "the counted doc fence moved to {}",
        files[0]
    );
    let src =
        std::fs::read_to_string(repo.join("vendor/winit/src/platform/macos.rs")).expect("readable");
    assert!(
        src.contains(r#"#![cfg_attr(target_os = "macos", doc = "```")]"#),
        "the fence is no longer macOS-enabled — re-derive this count"
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
