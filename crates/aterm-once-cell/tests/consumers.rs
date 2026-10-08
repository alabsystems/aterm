// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tripwires for the claims `crates/aterm-once-cell` rests on:
//!
//! 1. [`every_once_cell_path_in_the_graph_is_an_item_this_shim_provides`] —
//!    each `once_cell::…` path in a consumer's source names an item this crate has.
//! 2. [`no_consumer_reaches_for_an_item_this_shim_omits`] — the omitted upstream
//!    items stay unreferenced.
//! 3. [`the_consumer_set_is_still_the_one_this_shim_was_measured_against`] — the
//!    per-cell parent set, re-derived from the live resolve, is unchanged.
//! 4. [`the_dead_consumers_are_still_dead`] — the three `cfg`-dead imports stay dead.
//! 5. [`no_cell_enables_a_feature_this_shim_ignores`] — no cell turns on an
//!    upstream feature that would change upstream's implementation.
//!
//! No file list is hardcoded: each consumer's source root comes from `cargo
//! metadata` and is walked recursively, and the consumer set comes from `cargo
//! tree`, so a new file, module or consumer is read by construction.
//!
//! Each tripwire was armed by a plant that compiled, ran and failed:
//! `PROVIDED` without `("sync", "Lazy")` (names `criterion-0.5.1/src/lib.rs:88`),
//! `OMITTED` with `"OnceBox"` (names `ahash-0.8.12/src/random_state.rs:111`),
//! `EXPECTED_PARENTS` without `"x11rb"`, the `rustls` guard flipped to
//! `#[cfg(feature = "std")]`, and `INERT_FEATURES` with `"race"`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The five cells, spelled out (not taken from `aterm_forge::resolve`) so a
/// change shows up as a diff here. `wasm32-unknown-unknown` carries two cells,
/// so the root package is part of the key.
const CELLS: [(&str, &str); 5] = [
    ("aterm", "aarch64-apple-darwin"),
    ("aterm", "x86_64-unknown-linux-gnu"),
    ("aterm", "x86_64-pc-windows-msvc"),
    ("aterm-wasm", "wasm32-unknown-unknown"),
    ("aterm-gpu-web", "wasm32-unknown-unknown"),
];

/// Short cell names, matching `targo --unverified forge`'s.
const CELL_NAMES: [&str; 5] = ["mac-arm", "linux", "win", "wasm-cpu", "wasm-gpu"];

/// Every `module::Type` this shim exports. A `once_cell::…` path found in a
/// consumer's source must reduce to one of these.
const PROVIDED: [(&str, &str); 5] = [
    ("sync", "OnceCell"),
    ("sync", "Lazy"),
    ("unsync", "OnceCell"),
    ("unsync", "Lazy"),
    ("race", "OnceBox"),
];

/// Upstream types this shim omits (divergence 5 in `src/lib.rs`). The omitted
/// methods (`wait`, `get_unchecked`) are not scanned: a lexical scan cannot
/// follow them, and using one is a compile error.
const OMITTED: [&str; 3] = ["OnceNonZeroUsize", "OnceBool", "OnceRef"];

/// Upstream feature names that this shim accepts and IGNORES, and which would
/// change upstream's implementation if a consumer ever turned one on.
const INERT_FEATURES: [&str; 4] = [
    "critical-section",
    "atomic-polyfill",
    "parking_lot",
    "portable-atomic",
];

/// The direct parents of `once_cell` on each cell, as measured when this shim
/// was written. Re-derived from the live resolve by
/// [`the_consumer_set_is_still_the_one_this_shim_was_measured_against`].
const EXPECTED_PARENTS: [(&str, &[&str]); 5] = [
    ("mac-arm", &["rustls"]),
    (
        "linux",
        &[
            "ahash",
            "naga",
            "read-fonts",
            "rustls",
            "wayland-sys",
            "wgpu-core",
            "x11-dl",
            "x11rb",
            "xkbcommon-dl",
        ],
    ),
    (
        "win",
        &["naga", "read-fonts", "rustls", "wgpu-core", "wgpu-hal"],
    ),
    ("wasm-cpu", &["wasm-bindgen"]),
    (
        "wasm-gpu",
        &[
            "js-sys",
            "naga",
            "wasm-bindgen",
            "wasm-bindgen-futures",
            "wgpu-core",
        ],
    ),
];

/// The three consumers whose `once_cell` import sits under a `cfg` that is off
/// in every cell: the crate name, the guard line that must precede the import,
/// and the feature this crate must keep resolving WITH for that guard to stay
/// false.
///
/// `naga`'s guard is not a feature test but a `cfg_aliases!` name its build
/// script defines as `no_std: { not(std) }` with
/// `std: { any(test, feature = "wgsl-in", feature = "stderr", feature = "fs") }`
/// — so `wgsl-in` being on is what keeps the import dead.
const DEAD_CONSUMERS: [(&str, &str, &str); 3] = [
    ("rustls", "#[cfg(not(feature = \"std\"))]", "std"),
    ("read-fonts", "#[cfg(not(feature = \"std\"))]", "std"),
    ("naga", "#[cfg(no_std)]", "wgsl-in"),
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/aterm-once-cell sits two levels under the workspace root")
        .to_path_buf()
}

/// `cargo <args>` in `root`, run at most once per `(root, args)` per process.
///
/// Every answer is a pure function of the lockfile and manifests, and cargo
/// serialises each call on its package-cache lock, so repeats are pure wall
/// time. A failed run is not cached: it panics in the test that asked.
fn cargo(root: &Path, args: &[&str]) -> String {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex, OnceLock};
    type Slots = BTreeMap<(PathBuf, Vec<String>), Arc<OnceLock<String>>>;
    static SLOTS: OnceLock<Mutex<Slots>> = OnceLock::new();
    let key = (
        root.to_path_buf(),
        args.iter().map(|a| (*a).to_string()).collect::<Vec<_>>(),
    );
    let slot = SLOTS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(key)
        .or_default()
        .clone();
    slot.get_or_init(|| cargo_uncached(root, args)).clone()
}

fn cargo_uncached(root: &Path, args: &[&str]) -> String {
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

/// One `once_cell::…` path found in a consumer's source.
#[derive(Debug)]
struct Mention {
    file: PathBuf,
    line_no: usize,
    line: String,
    /// The `module::Item` pairs this line names, or the reason it names none.
    path: Parsed,
}

/// Every package that links `once_cell`, as `(name, source root)`, from the
/// live resolve.
///
/// From `cargo tree`, not a manifest scan: `rustix 1.1.4` declares a
/// `cfg(windows)` dev-dependency on `once_cell` it never uses and that is in no
/// graph. The union covers the five cells plus the host dev graph, which
/// `cargo test` builds (`criterion`, `tempfile`).
fn consumers(root: &Path) -> Vec<(String, PathBuf)> {
    let mut names: BTreeSet<String> = BTreeSet::new();
    for (pkg, triple) in CELLS {
        names.extend(direct_parents(root, pkg, triple));
    }
    names.extend(host_dev_parents(root));
    names
        .into_iter()
        .map(|n| {
            let dir = source_root(root, &n);
            (n, dir)
        })
        .collect()
}

/// The direct parents of `once_cell` in the host graph WITH dev edges — the
/// graph `cargo test` actually compiles.
fn host_dev_parents(root: &Path) -> BTreeSet<String> {
    let tree = cargo(
        root,
        &[
            "tree",
            "-e",
            "normal,dev",
            "-i",
            "once_cell",
            "--depth",
            "1",
        ],
    );
    parse_tree_children(&tree)
}

/// The directory holding a resolved third-party package's sources, from
/// `cargo metadata`'s `manifest_path` for it.
///
/// Matched by directory name (`<name>-<version>`), not by scanning forward
/// from a `"name":"<name>"` key: that key also appears in every dependant's
/// dependency list, so a forward scan finds the wrong `manifest_path`.
fn source_root(root: &Path, name: &str) -> PathBuf {
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
        // `wasm-bindgen-futures` when looking for `wasm-bindgen`, so the
        // character after it must start a version.
        let Some(tail) = base.strip_prefix(&prefix) else {
            continue;
        };
        if !tail.starts_with(|c: char| c.is_ascii_digit()) || hits.contains(&dir.to_path_buf()) {
            continue;
        }
        hits.push(dir.to_path_buf());
    }
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one resolved `{name}` package, found {hits:?} — if the graph now \
         carries two versions, this test must be told which one links `once_cell`"
    );
    hits.remove(0)
}

/// Every `.rs` file under `dir`, recursively, so a call site that moves to a
/// new module is still read.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Every mention of the string `once_cell` in a consumer's whole source tree,
/// with the `module::Type` path parsed out where there is one.
fn mentions(src_root: &Path) -> Vec<Mention> {
    let mut out = Vec::new();
    for file in rust_files(src_root) {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if !line.contains("once_cell") {
                continue;
            }
            out.push(Mention {
                file: file.clone(),
                line_no: i + 1,
                line: line.trim().to_string(),
                path: parse_path(line),
            });
        }
    }
    out
}

/// What a line mentioning `once_cell::` resolves to.
#[derive(Debug, PartialEq)]
enum Parsed {
    /// Not a path at all: `once_cell` appears as a bare word, in a `docs.rs`
    /// URL (slashes, not `::`), or in a `#[cfg(feature = "once_cell")]`.
    NotAPath,
    /// One or more `module::Item` pairs. A braced group yields all of them.
    Items(Vec<(String, String)>),
    /// A `once_cell::` path that rebinds a module (`use once_cell::{sync, unsync};`,
    /// `use once_cell::sync::{self, Lazy};`, `use once_cell::sync as oc;`), so
    /// its types arrive on lines this scan cannot see. Reported, never skipped.
    Unresolvable,
}

/// Pull the `module::Item` pairs out of a line containing `once_cell::…`.
///
/// Only the first two segments after `once_cell::` are taken, so a trailing
/// method (`::once_cell::unsync::Lazy::force`, which `wasm-bindgen` writes)
/// reduces to the same pair as the import that introduced it. A `docs.rs` URL
/// spells the path with slashes and is not matched. Braced groups yield every
/// item, so `use once_cell::sync::{Lazy, OnceCell};` is read, not skipped.
fn parse_path(line: &str) -> Parsed {
    fn seg(s: &str) -> (String, &str) {
        let n = s
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(s.len());
        (s[..n].to_string(), &s[n..])
    }
    let Some(i) = line.find("once_cell::") else {
        return Parsed::NotAPath;
    };
    let rest = &line[i + "once_cell::".len()..];
    let (module, rest) = seg(rest);
    if module.is_empty() {
        // `once_cell::{sync, unsync}` — a module group, with no type named on
        // this line at all.
        return if rest.starts_with('{') {
            Parsed::Unresolvable
        } else {
            Parsed::NotAPath
        };
    }
    let Some(rest) = rest.strip_prefix("::") else {
        // `use once_cell::sync;` or `use once_cell::sync as oc;` — the types
        // arrive on later lines that do not contain the string `once_cell` at
        // all, so no lexical scan can follow them.
        return Parsed::Unresolvable;
    };
    if let Some(group) = rest.strip_prefix('{') {
        let group = group.split('}').next().unwrap_or(group);
        let mut out = Vec::new();
        let mut unresolvable = false;
        for part in group.split(',') {
            let (item, _) = seg(part.trim());
            if item.is_empty() {
                continue;
            }
            // `{self, Lazy}` re-binds the MODULE, whose later uses this scan
            // cannot see; the sibling items are still checked.
            if item == "self" {
                unresolvable = true;
            } else {
                out.push((module.clone(), item));
            }
        }
        if unresolvable || out.is_empty() {
            return Parsed::Unresolvable;
        }
        return Parsed::Items(out);
    }
    let (item, _) = seg(rest);
    if item.is_empty() {
        return Parsed::Unresolvable;
    }
    Parsed::Items(vec![(module, item)])
}

/// Every import form either resolves or reports itself unresolvable.
#[test]
fn parse_path_resolves_every_import_form_or_says_it_cannot() {
    let items = |v: &[(&str, &str)]| {
        Parsed::Items(
            v.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        )
    };
    assert_eq!(
        parse_path("use once_cell::sync::Lazy;"),
        items(&[("sync", "Lazy")])
    );
    assert_eq!(
        parse_path("    use once_cell::race::OnceBox;"),
        items(&[("race", "OnceBox")])
    );
    assert_eq!(
        parse_path("::once_cell::unsync::Lazy::force(&self.0 .0)"),
        items(&[("unsync", "Lazy")])
    );
    // Braced and module-rebinding forms.
    assert_eq!(
        parse_path("use once_cell::sync::{Lazy, OnceCell};"),
        items(&[("sync", "Lazy"), ("sync", "OnceCell")]),
        "a braced import must yield EVERY item, not none"
    );
    assert_eq!(
        parse_path("use once_cell::sync::{self, Lazy};"),
        Parsed::Unresolvable
    );
    assert_eq!(
        parse_path("use once_cell::{sync, unsync};"),
        Parsed::Unresolvable
    );
    assert_eq!(
        parse_path("use once_cell::sync as oc;"),
        Parsed::Unresolvable
    );
    // Prose and URLs stay quiet.
    assert_eq!(
        parse_path("/// [`OnceBox`]: https://docs.rs/once_cell/latest/once_cell/race/x.html"),
        Parsed::NotAPath
    );
    assert_eq!(
        parse_path("#[cfg(feature = \"once_cell\")]"),
        Parsed::NotAPath
    );
}

/// The `cargo tree` inverse-dependency roots of `once_cell` on one cell.
fn direct_parents(root: &Path, pkg: &str, triple: &str) -> BTreeSet<String> {
    let tree = cargo(
        root,
        &[
            "tree",
            "-p",
            pkg,
            "--target",
            triple,
            "-e",
            "normal",
            "-i",
            "once_cell",
            "--depth",
            "1",
        ],
    );
    parse_tree_children(&tree)
}

/// The package names on the child lines of a `cargo tree --depth 1` inverse
/// listing, skipping the root and the `[dev-dependencies]` annotation rows.
fn parse_tree_children(tree: &str) -> BTreeSet<String> {
    tree.lines()
        .skip(1) // the `once_cell v1.21.4 (…)` root itself
        .filter_map(|l| {
            let l = l.trim_start_matches(['\u{251c}', '\u{2514}', '\u{2500}', '\u{2502}', ' ']);
            let name = l.split_whitespace().next()?;
            // `cargo tree` prints an `[dev-dependencies]` label under the edge
            // it qualifies; it is not a package.
            (!name.is_empty() && !name.starts_with('[')).then(|| name.to_string())
        })
        .collect()
}

#[test]
fn every_once_cell_path_in_the_graph_is_an_item_this_shim_provides() {
    let root = workspace_root();
    let consumers = consumers(&root);
    assert!(
        !consumers.is_empty(),
        "no `once_cell` consumers resolved; this test checks nothing"
    );

    let mut checked = 0usize;
    for (name, src_root) in &consumers {
        let ms = mentions(src_root);
        // CONTROL, per consumer: a package that declares the dependency but
        // whose sources never mention it would pass the loop below vacuously.
        assert!(
            !ms.is_empty(),
            "`{name}` links `once_cell` but no source under {} mentions it",
            src_root.display()
        );
        for m in ms {
            let pairs = match &m.path {
                Parsed::NotAPath => continue, // a doc comment or a URL
                // Never skipped: for an import this scan cannot follow, "found
                // nothing" and "there is nothing" look alike.
                Parsed::Unresolvable => panic!(
                    "`{name}` imports `once_cell` in a form this scan cannot follow (a module \
                     rebind):\n{}:{}\n    {}\nRead its call sites by hand, then teach \
                     `parse_path` the form.",
                    m.file.display(),
                    m.line_no,
                    m.line
                ),
                Parsed::Items(pairs) => pairs,
            };
            for (module, item) in pairs {
                checked += 1;
                assert!(
                    PROVIDED.iter().any(|(pm, pi)| pm == module && pi == item),
                    "`{name}` uses `once_cell::{module}::{item}`, which this shim does not \
                     provide (it exports {PROVIDED:?}):\n{}:{}\n    {}\nAdd the item \
                     (`unsafe` is forbidden) or retire the `[patch.crates-io] once_cell` row.",
                    m.file.display(),
                    m.line_no,
                    m.line
                );
            }
        }
    }
    assert!(
        checked > 0,
        "{} consumers walked, 0 `once_cell::…` paths resolved; `parse_path` no longer matches",
        consumers.len()
    );
}

#[test]
fn no_consumer_reaches_for_an_item_this_shim_omits() {
    let root = workspace_root();
    let consumers = consumers(&root);
    assert!(!consumers.is_empty(), "no `once_cell` consumers resolved");
    for (name, src_root) in &consumers {
        for m in mentions(src_root) {
            for omitted in OMITTED {
                assert!(
                    !m.line.contains(omitted),
                    "`{name}` names `{omitted}`, which this shim omits (divergence 5 in \
                     src/lib.rs):\n{}:{}\n    {}\nAdd an `unsafe`-free equivalent.",
                    m.file.display(),
                    m.line_no,
                    m.line
                );
            }
        }
    }
}

#[test]
fn the_consumer_set_is_still_the_one_this_shim_was_measured_against() {
    let root = workspace_root();
    for (i, (pkg, triple)) in CELLS.iter().enumerate() {
        let cell = CELL_NAMES[i];
        let found = direct_parents(&root, pkg, triple);
        let (_, expected) = EXPECTED_PARENTS
            .iter()
            .find(|(c, _)| *c == cell)
            .unwrap_or_else(|| panic!("no EXPECTED_PARENTS row for `{cell}`"));
        let expected: BTreeSet<String> = expected.iter().map(|s| (*s).to_string()).collect();
        assert_eq!(
            found, expected,
            "`once_cell` parents on `{cell}` ({pkg} / {triple}) changed\n\
             found:    {found:?}\n\
             expected: {expected:?}\n\
             Census the new parent (module, methods, cfg guard), then update EXPECTED_PARENTS, \
             the liveness table in src/lib.rs, and DEAD_CONSUMERS if it is dead."
        );
    }
}

/// Whether the item on line `idx` is compiled only when `guard` holds.
///
/// Two shapes. `rustls` and `naga` put the attribute directly on the `use`:
///
/// ```text
/// #[cfg(not(feature = "std"))]
/// use once_cell::race::OnceBox;
/// ```
///
/// `read-fonts` puts it on the enclosing module and leaves the `use` bare:
///
/// ```text
/// #[cfg(not(feature = "std"))]
/// mod once_impl {
///     use once_cell::race::OnceBox;
/// ```
///
/// So this also walks enclosing blocks, tracking the attribute (if any) that
/// preceded each `{`. Brace counting is lexical; a `{` in a string or comment
/// can only make it report "not guarded", a loud failure, never a silent pass.
fn guarded_by(lines: &[&str], idx: usize, guard: &str) -> bool {
    // Shape one: the attribute sits directly above the item.
    if idx > 0 && lines[idx - 1].trim() == guard {
        return true;
    }
    // Shape two: an enclosing block carries it.
    let mut stack: Vec<Option<String>> = Vec::new();
    let mut pending: Option<String> = None;
    for (i, raw) in lines.iter().enumerate() {
        if i == idx {
            return stack.iter().any(|a| a.as_deref() == Some(guard));
        }
        let t = raw.trim();
        if t.starts_with("#[") {
            pending = Some(t.to_string());
            continue;
        }
        if t.is_empty() || t.starts_with("//") {
            continue;
        }
        let opens = t.matches('{').count();
        let closes = t.matches('}').count();
        for _ in 0..opens {
            stack.push(pending.take());
        }
        for _ in 0..closes {
            stack.pop();
        }
        pending = None;
    }
    false
}

#[test]
fn the_dead_consumers_are_still_dead() {
    let root = workspace_root();
    let consumers = consumers(&root);

    // The import is still under the recorded guard.
    for (name, guard, _) in DEAD_CONSUMERS {
        let (_, src_root) = consumers
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("`{name}` is no longer a resolved `once_cell` consumer"));
        let mut guarded = 0usize;
        for file in rust_files(src_root) {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                // Only the `use` that imports the type, not doc comments about it.
                if !line.contains("once_cell::") || !line.trim_start().starts_with("use ") {
                    continue;
                }
                guarded += 1;
                assert!(
                    guarded_by(&lines, i, guard),
                    "{}:{} imports `once_cell` without the recorded `{guard}`, so `{name}` is \
                     now live code:\n    {}\nRe-check the contracts in src/lib.rs and update \
                     its liveness table.",
                    file.display(),
                    i + 1,
                    line.trim()
                );
            }
        }
        assert!(
            guarded > 0,
            "no `use once_cell::…` line under {}; dead consumer `{name}` was not checked",
            src_root.display()
        );
    }

    // The feature that keeps each guard false still resolves on every cell.
    for (i, (pkg, triple)) in CELLS.iter().enumerate() {
        let cell = CELL_NAMES[i];
        let parents = direct_parents(&root, pkg, triple);
        for (name, guard, feature) in DEAD_CONSUMERS {
            if !parents.contains(name) {
                continue; // not on this cell
            }
            let tree = cargo(
                &root,
                &[
                    "tree", "-p", pkg, "--target", triple, "-e", "features", "-i", name,
                ],
            );
            let want = format!("{name} feature \"{feature}\"");
            assert!(
                tree.contains(&want),
                "`{name}` on `{cell}` ({pkg} / {triple}) resolves without feature `{feature}`, so \
                 its `{guard}` import is live. Check the liveness table and divergences in \
                 crates/aterm-once-cell/src/lib.rs before accepting it."
            );
        }
    }
}

#[test]
fn no_cell_enables_a_feature_this_shim_ignores() {
    let root = workspace_root();
    for (i, (pkg, triple)) in CELLS.iter().enumerate() {
        let cell = CELL_NAMES[i];
        let tree = cargo(
            &root,
            &[
                "tree",
                "-p",
                pkg,
                "--target",
                triple,
                "-e",
                "features",
                "-i",
                "once_cell",
            ],
        );
        for feature in INERT_FEATURES {
            let bad = format!("once_cell feature \"{feature}\"");
            assert!(
                !tree.contains(&bad),
                "cell `{cell}` ({pkg} / {triple}) enables `once_cell/{feature}`, which this shim \
                 ignores; implement it or turn it off."
            );
        }
    }
}
