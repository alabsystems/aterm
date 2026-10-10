// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `objc2` exit condition, with a derived scope.
//!
//! The objc2 family (`objc2`, `block2`, `objc2-app-kit`, `objc2-foundation`,
//! …) must not be used in code the macOS build compiles, and must not be
//! declared as a dependency.
//!
//! * The scope is everything under `vendor/winit/src` the macOS build
//!   compiles, defined by subtraction (the whole tree minus the per-OS
//!   slices), plus all of `crates/aterm-gui/src`, so a new file is in scope
//!   when it is written.
//! * The subtracted slices match `aterm-census`'s `REVIEWED_VENDORED_CRATES`
//!   and are existence-checked, so a rename fails instead of widening a blind
//!   spot.
//! * A second walk over all of `crates/` and `vendor/winit/src` requires every
//!   family use to be inside the scope or a declared slice.
//!
//! Limits: the rule strips `//` comments, so doc examples are not counted; and
//! it matches names, so a family type reaching a file through a generic
//! parameter is not seen.

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

/// The per-OS subtrees the macOS build does not compile. Identical to the
/// `platform_slices` `aterm-census`'s `REVIEWED_VENDORED_CRATES` registers for
/// `winit`, and existence-checked, since a stale path is a blind spot.
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

/// The recorded counts. Update them in the same commit that moves them, in
/// either direction.
mod recorded {
    /// Files in the macOS-compiled scope with an `objc2`-family code use,
    /// counting `block2`.
    pub const FILES_WITH_FAMILY: usize = 0;
    /// The same, `objc2` proper only.
    pub const FILES_WITH_OBJC2: usize = 0;
    /// …split by tree. Both are zero, so the family's manifest rows must be gone
    /// (see `the_manifest_rows_are_live_or_dead_exactly_as_the_file_counts_say`).
    pub const GUI_FILES: usize = 0;
    pub const WINIT_FILES: usize = 0;
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

/// Nothing outside the scope uses the family in code.
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

/// The code rule sees every spelling of an import, including a renaming
/// `use objc2 as oc;`, which has none of the punctuation the rule otherwise
/// keys on.
#[test]
fn the_code_rule_sees_a_renaming_import() {
    let family_seen = |l: &str| code_idents(l).iter().any(|n| FAMILY.contains(&n.as_str()));
    for line in [
        "use objc2::rc::Retained;",
        "extern crate objc2;",
        "use objc2::{rc::Retained, runtime::AnyObject};",
        "    let x = objc2::rc::autoreleasepool(|_| ());",
        "use objc2::rc::Retained as R;",
        // Renamed imports.
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

/// `(any family name, any name other than block2)` used in code.
///
/// Strips `//` comments and string literals (so a quoted call or a word in a
/// message does not count), then requires a family name followed by `::`, `;`,
/// `,` or `}`, or the `as` of a renaming import.
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
            // …or the `as` of a renaming import: `use objc2 as oc;` has none of
            // the four punctuation marks after the name.
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

/// A manifest with its comments stripped — the TOML twin of [`code_idents`], so
/// prose about a dependency (e.g. a retirement note naming
/// `objc2-app-kit/NSAccessibility`) is not read as the dependency.
///
/// `#` starts a comment only outside a string, so `"…"`/`'…'` data containing
/// `#` is kept and a trailing comment is removed.
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

/// Every dependency a manifest declares, as `(package, table)`, in any
/// spelling Cargo accepts.
///
/// Inline rows and `[…dependencies.NAME]` tables are both read:
///
/// ```toml
/// [target.'cfg(target_os = "macos")'.dependencies]
/// objc2-app-kit = { version = "0.2.2" }        # inline row
///
/// [target.'cfg(target_os = "macos")'.dependencies.objc2-app-kit]
/// version = "0.2.2"                            # table row — the FORK's own form
/// ```
///
/// Dotted spellings (`name.workspace = true`) count. Keys are unquoted, and a
/// `package = "…"` rename (inline, dotted `key.package`, or inside the table)
/// replaces the key, so the name reported is the package:
///
/// ```toml
/// [target.'cfg(target_os = "macos")'.dependencies]
/// appkit_bindings = { package = "objc2-app-kit", version = "0.2" }  # PLANT A
/// "objc2-app-kit" = { version = "0.2" }                             # PLANT B
/// ```
///
/// The table comes back with each name because the fork keeps legitimate
/// `objc2-foundation` / `objc2-ui-kit` rows under `cfg(target_os = "ios")`,
/// which no aterm build compiles. `aterm-gui` has no such exemption.
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
            // one rename. A row literally named `package` (a legal crate name)
            // has an empty field path, so it is never taken as a rename target.
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

/// The `package = "…"` rename inside an inline dependency value, if any.
/// `package` must be a whole key followed by `=`, so `default-features`, a
/// feature called `"package"`, or a path ending in `package` are left alone.
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

/// The manifest rule sees every spelling of a dependency: each row below
/// declares `objc2-app-kit`, differing only in how it is written (renamed via
/// `package =`, quoted key, table form, …). See [`declared_deps`].
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

    // It does not fire on prose, a feature list, a row that merely mentions the
    // name, or the near-miss `objc2`.
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

    // A renamed row under the ios cfg still comes back under the ios table.
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

/// The manifest rows agree with the recorded file counts.
///
/// With both [`recorded::GUI_FILES`] and [`recorded::WINIT_FILES`] at zero, no
/// family row may remain under `vendor/winit`'s macOS cfg or anywhere in
/// `aterm-gui`'s manifest, and `aterm-gui`'s `a11y-appkit` feature list may not
/// name the family.
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

    // The fork's rows, in any spelling but only under the macOS cfg (the ios
    // rows are legitimate).
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

    // With `WINIT_FILES` at zero, the fork's macOS rows must be absent.
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

    // `aterm-gui`'s rows. Any table counts: this crate has no target it does
    // not build.
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
    // All four present or all four absent: a partial retirement is a state
    // nothing is in, and the shape a careless re-add would take. The
    // `a11y-appkit` feature list travels with them (cargo refuses a feature
    // naming an absent dependency).
    assert!(
        gui_present.len() == gui_rows.len() || gui_present.is_empty(),
        "aterm-gui's four family rows are ONE decision and this tree has {} of \
         them: {gui_present:?}. Retire all four together, and the `a11y-appkit` \
         feature list with them",
        gui_present.len()
    );
    if gui_present.is_empty() {
        // A backstop: with the rows gone and the feature list left, cargo
        // refuses to load the workspace before any test runs. This arm catches
        // a feature list rewritten to name something else while still naming
        // the retired crate.
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
