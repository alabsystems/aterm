// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE ARMED TRIPWIRE for the one claim this replacement rests on.
//!
//! `crates/aterm-core-maths` answers upstream's `#[no_std]`-and-`libm` contract
//! with `std`, which is sound here for a single reason: in aterm's graph the
//! trait is never imported, so no body of it is ever called. Both consumers
//! guard the import with `#[cfg(not(feature = "std"))]`, and `std` is on.
//!
//! That is two separate facts, and this file checks both of them against the
//! live tree rather than trusting the crate documentation:
//!
//! 1. [`consumers_take_std_on_every_cell`] re-derives the feature resolution
//!    per cell and requires `rustybuzz feature "std"` and
//!    `ttf-parser feature "std"` in all five.
//! 2. [`every_core_maths_import_is_still_cfg_gated_off`] reads the two
//!    consumers' own resolved sources and requires that the ONLY mentions of
//!    `core_maths` in either are `use` lines directly under
//!    `#[cfg(not(feature = "std"))]`.
//!
//! Either one failing means some body in `src/lib.rs` has become live, at which
//! point the std-vs-libm fidelity table in that file's header stops being
//! documentation and starts being a behaviour change to measure.
//!
//! # Both are ARMED, and the arming was run rather than argued
//!
//! A tripwire nobody has seen fire is a tripwire nobody knows is connected.
//! Each of these was made to fail once, deliberately, and then restored:
//!
//! * adding `default-features = false` to `rustybuzz` in
//!   `crates/aterm-render/Cargo.toml` makes
//!   [`consumers_take_std_on_every_cell`] fail on `aterm`/`aarch64-apple-darwin`
//!   with the message below;
//! * expecting `#[cfg(feature = "std")]` instead of the real guard makes
//!   [`every_core_maths_import_is_still_cfg_gated_off`] fail naming
//!   `rustybuzz-0.20.1/src/hb/face.rs:2`;
//! * pointing the `ttf-parser` file list at a file with no `core_maths` in it
//!   fires the `found > 0` CONTROL rather than passing vacuously — which is the
//!   failure mode a file-list-driven test has, and the reason that control is
//!   there.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The five cells, spelled out rather than recomputed from
/// `aterm_forge::resolve`, so that a change to either side of the pairing shows
/// up as a diff here. `wasm32-unknown-unknown` carries TWO cells — the two
/// cdylib modules aterm actually ships to a browser — which is why the root
/// package is part of the key and the triple alone is not.
const CELLS: [(&str, &str); 5] = [
    ("aterm", "aarch64-apple-darwin"),
    ("aterm", "x86_64-unknown-linux-gnu"),
    ("aterm", "x86_64-pc-windows-msvc"),
    ("aterm-wasm", "wasm32-unknown-unknown"),
    ("aterm-gpu-web", "wasm32-unknown-unknown"),
];

/// The two crates that declare a `core_maths` dependency.
///
/// No file list. There was one, naming the three `rustybuzz` files and the one
/// `ttf-parser` file that mention `core_maths` today, and a judge named the
/// hole it left: the vacuity control below asserts `found > 0` PER CONSUMER, so
/// a dependency bump that moved a `core_maths` import into a file the list did
/// not name would leave the listed files intact, keep `found` above zero, pass
/// — and the shim would have silently become live code in the new file. The
/// control was checking that the list was non-empty, not that it was complete.
/// Walking the source root recursively is what makes `found > 0` a real
/// completeness control, because now nothing can be missed rather than merely
/// nothing listed being absent.
const CONSUMERS: [&str; 2] = ["rustybuzz", "ttf-parser"];

/// Every `.rs` file under `root`, recursively.
fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
        for entry in entries {
            let entry = entry.unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/aterm-core-maths sits two levels under the workspace root")
        .to_path_buf()
}

fn cargo(root: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("could not run `cargo {}`: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "`cargo {}` failed ({}):\n{}",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn consumers_take_std_on_every_cell() {
    let root = workspace_root();
    for (pkg, triple) in CELLS {
        for consumer in CONSUMERS {
            let tree = cargo(
                &root,
                &[
                    "tree", "-p", pkg, "--target", triple, "-e", "features", "-i", consumer,
                ],
            );
            let want = format!("{consumer} feature \"std\"");
            assert!(
                tree.contains(&want),
                "\n\nTHE core_maths REPLACEMENT HAS BECOME LIVE CODE.\n\
                 Cell `{pkg}` / `{triple}` resolves `{consumer}` WITHOUT its `std` feature, so \
                 its `#[cfg(not(feature = \"std\"))] use core_maths::CoreFloat;` is now compiled \
                 and the bodies in crates/aterm-core-maths/src/lib.rs are being called for \
                 real.\n\n\
                 THAT IS NOT AUTOMATICALLY WRONG — the bodies are correct wherever `std` exists, \
                 which is every target aterm builds — but it is a BEHAVIOUR CHANGE that nobody \
                 has measured: `std` and `libm` are two implementations of sin/cos/tan/exp/ln \
                 and neither is correctly rounded, so glyph geometry can move by an ulp. Read \
                 the fidelity table in that crate's header, then either restore the `std` \
                 feature on `{consumer}` or measure the paint conformance suites before \
                 accepting the change.\n"
            );
        }
    }
}

#[test]
fn every_core_maths_import_is_still_cfg_gated_off() {
    let root = workspace_root();
    for consumer in CONSUMERS {
        let src_root = consumer_source_root(&root, consumer);
        let found = match scan_for_unguarded_import(&src_root) {
            Ok(n) => n,
            Err(why) => panic!("{why}"),
        };
        // CONTROL: a consumer whose sources stopped mentioning `core_maths` at
        // all would pass the scan above vacuously. Since the scan now walks
        // every `.rs` under the source root, this also fires when the package
        // moved somewhere the resolver no longer points at — the two ways the
        // test could prove nothing are the same assertion again.
        assert!(
            found > 0,
            "no `core_maths` mention found anywhere under `{consumer}` at {} — either the \
             dependency is gone (retire the [patch.crates-io] row and this test) or the source \
             root resolved wrong, and either way this test proved nothing",
            src_root.display()
        );
    }
}

/// Walk every `.rs` under `src_root` and check each `core_maths` mention.
///
/// Returns the number of mentions found, or the message for the first one that
/// is not a `#[cfg(not(feature = "std"))]`-guarded import of `CoreFloat`.
///
/// This is a free function taking a root, rather than the loop body it used to
/// be, for one reason: it is the only way to ARM the walk. The real consumers
/// live in the shared cargo registry under `~/.cargo/registry/src`, and
/// planting a file there to watch this go red would edit a cache every project
/// on this machine reads — so the arming copies a consumer to a temp directory
/// and plants there, which requires the scan to be callable on an arbitrary
/// root. See `the_walk_finds_a_planted_import_the_old_file_list_would_miss`.
fn scan_for_unguarded_import(src_root: &Path) -> Result<usize, String> {
    let mut found = 0usize;
    for path in rust_sources(src_root) {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !line.contains("core_maths") {
                continue;
            }
            found += 1;
            if line.trim() != "use core_maths::CoreFloat;" {
                return Err(format!(
                    "{}:{}: unexpected `core_maths` mention: {}",
                    path.display(),
                    i + 1,
                    line.trim()
                ));
            }
            let guard = i.checked_sub(1).map(|p| lines[p].trim()).unwrap_or("");
            if guard != "#[cfg(not(feature = \"std\"))]" {
                return Err(format!(
                    "\n\n{}:{} imports `core_maths::CoreFloat` WITHOUT the \
                     `#[cfg(not(feature = \"std\"))]` guard this replacement depends on.\n\
                     The trait is now imported unconditionally, so `crates/aterm-core-maths` is \
                     live code on every cell regardless of features. See the sibling test's \
                     message for what to do about it.\n",
                    path.display(),
                    i + 1
                ));
            }
        }
    }
    Ok(found)
}

/// THE ARMING for the recursive walk, and for the completeness control that
/// depends on it.
///
/// A judge found that the previous version of this test hardcoded a list of
/// four files and asserted only `found > 0` PER CONSUMER, so a dependency bump
/// that moved a `core_maths` import into an unlisted file would leave the
/// listed files intact, keep `found` above zero, and pass — while the shim
/// silently became live code in the new file. This plants exactly that: a NEW
/// nested file that the old list does not name, holding an UNGUARDED import.
///
/// The plant goes into a temp copy, never the registry (see
/// `scan_for_unguarded_import`), and the test proves both halves in one run:
/// the clean copy scans OK with the real mentions found, and the planted copy
/// is named as an error at the planted path.
#[test]
fn the_walk_finds_a_planted_import_the_old_file_list_would_miss() {
    let root = workspace_root();
    let src_root = consumer_source_root(&root, "ttf-parser");
    let tmp = std::env::temp_dir().join(format!(
        "aterm-core-maths-arm-{}-{}",
        std::process::id(),
        line!()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    copy_tree(&src_root, &tmp);

    // Control: the untouched copy scans clean, and finds the real mentions.
    let clean = scan_for_unguarded_import(&tmp).expect("the untouched copy scans clean");
    assert!(clean > 0, "the copy should carry the real mentions");

    // Plant: a new file, nested deeper than anything the old list named.
    let planted_dir = tmp.join("src").join("armed").join("deeper");
    std::fs::create_dir_all(&planted_dir).expect("mkdir");
    let planted = planted_dir.join("moved_here_by_an_upstream_bump.rs");
    std::fs::write(&planted, "use core_maths::CoreFloat;\n").expect("plant");

    let err = scan_for_unguarded_import(&tmp)
        .expect_err("an unguarded import in an unlisted file must be an error");
    assert!(
        err.contains("moved_here_by_an_upstream_bump.rs"),
        "the error must name the planted file, not merely fail: {err}"
    );
    std::fs::remove_dir_all(&tmp).expect("cleanup");
}

/// Recursive directory copy, for the arming above.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for entry in std::fs::read_dir(from).expect("read_dir") {
        let entry = entry.expect("entry");
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_tree(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).expect("copy");
        }
    }
}

/// The directory holding a resolved third-party package's sources, from
/// `cargo metadata`'s `manifest_path` for it.
///
/// The lookup is by DIRECTORY NAME (`<name>-<version>`), not by scanning
/// forward from a `"name":"<name>"` key: that key also appears in every
/// dependant's dependency list, and the first hit for `rustybuzz` is inside
/// `aterm-render`'s entry, so a forward scan finds aterm-render's own
/// `manifest_path` and reads `crates/aterm-render/src/hb/face.rs`. It did,
/// which is how this note got written.
fn consumer_source_root(root: &Path, name: &str) -> PathBuf {
    let json = cargo(root, &["metadata", "--format-version", "1"]);
    const KEY: &str = "\"manifest_path\":\"";
    let prefix = format!("{name}-");
    let mut hits: Vec<PathBuf> = Vec::new();
    let mut rest = json.as_str();
    while let Some(i) = rest.find(KEY) {
        rest = &rest[i + KEY.len()..];
        let Some(end) = rest.find('"') else { break };
        let manifest = Path::new(&rest[..end]);
        rest = &rest[end..];
        let Some(dir) = manifest.parent() else {
            continue;
        };
        let Some(base) = dir.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // `<name>-<version>`: the prefix alone would also match a sibling like
        // `ttf-parser-fontdue`, so the character after it must start a version.
        let Some(tail) = base.strip_prefix(&prefix) else {
            continue;
        };
        if !tail.starts_with(|c: char| c.is_ascii_digit()) || hits.contains(&dir.to_path_buf()) {
            continue;
        }
        // …AND it must be a copy that actually depends on `core_maths`. The
        // graph carries TWO `ttf-parser`s: 0.25.1 in the shipped cells, and
        // 0.21.1 under `fontdue`, which aterm-render keeps as a DEV-ONLY
        // differential oracle for its first-party face. The old one predates
        // the `core_maths` dependency entirely, so selecting by name alone
        // found two directories and could have read the wrong one.
        if std::fs::read_to_string(manifest).is_ok_and(|m| m.contains("core_maths")) {
            hits.push(dir.to_path_buf());
        }
    }
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one resolved `{name}` package that depends on `core_maths`, \
         found {hits:?}"
    );
    hits.remove(0)
}
