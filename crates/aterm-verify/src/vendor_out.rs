// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify vendor-out --into <dir> [--check]`: byte-identical copies of the crates another
//! repository builds from aterm's sources ([`CRATES`]), written under `<dir>/vendor/`.
//!
//! The screen reader and agent name table are projections. alab-policy comes from the receiving
//! repository's independent whole tagged copy at `vendor/alab-harness`, never a flattened relay.
//! Each projected crate is read from the folder aterm's root `[workspace.dependencies]` points
//! it at (where every member that inherits it builds it from), else `crates/<crate>`; its copy is
//! `vendor/<crate>/`:
//! - the files its library's build reads with debug assertions on and without `cfg(test)`, byte
//!   for byte: each module file it compiles and each file an `include_str!` or `include_bytes!`
//!   there embeds, as the census reader follows them ([`crate::own_tests::closure`]), so no
//!   `*_tests.rs` file and nothing only a test reads;
//! - a `Cargo.toml` that builds it alone: the workspace keys it inherits spelled out, the
//!   workspace's lints inlined, each dependency a path to another projection or canonical
//!   alab-policy, and the library its one target, whose tests and doctests stay beside the files
//!   they read: in aterm, or for a vendored
//!   crate in the repository aterm vendors it from. Its workspace is
//!   the one cargo builds it in, the nearest `[workspace]` manifest at or above its folder:
//!   aterm's root for a member, and for a crate aterm vendors from another repository (under a
//!   folder the root excludes) that repository's own root, whose version and lints are its own.
//!
//! A crate whose build depends on a crate outside the set and canonical alab-policy is refused,
//! as is one whose manifest says something this writer does not carry over (a build script,
//! features, a registry or git dependency) and one whose build reads a file outside its own folder.
//!
//! `vendor/VENDORED` lists the projections under `vendor/` as `<fnv1a64> <path>` (the FNV-1a 64
//! digest of its bytes in 16 lowercase hex digits, then its path from `vendor/`), in path order,
//! and carries an [`aterm_seal`] line of itself: the receiving repository's check finds a hand
//! edit of a copy, or of the list, without aterm. The canonical subtree and its adjacent pin
//! are separately owned: the target's index and working files must hold that pin before either
//! a check or a write, and neither a prior seal nor new output may claim their paths. Source
//! and target pins are independent; upstream's `tools/check-pin` checks each tag's identity.
//!
//! Writing replaces whatever is at a path it writes, and removes a file under `vendor/` only where
//! the sealed list there names it, as one an earlier run wrote; any other file it would not write
//! (a stray), outside the validated canonical owner, stops the run before anything changes.
//! `--check` writes nothing and names each projection file that differs from what a write would
//! leave, is missing, is extra or is a stray.
//! The aterm commit a copy is made from belongs in the receiving repository's commit message,
//! never in a file here.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use aterm_seal as seal;
use aterm_toml::{Table, Value};

use crate::exit;
use crate::own_tests;

/// The projections vendor-out copies: the screen reader and its agent name table.
pub const CRATES: [&str; 2] = ["aterm-agent-names", "aterm-phase"];

const CANONICAL: &str = "alab-harness";
const CANONICAL_DIR: &str = "vendor/alab-harness";
const CANONICAL_PIN: &str = "vendor/alab-harness.pin";
const POLICY: &str = "alab-policy";

/// The folder of `--into` the copies go in.
pub const VENDOR: &str = "vendor";

/// The list of the copies' digests, in [`VENDOR`].
pub const MANIFEST: &str = "VENDORED";

/// The command its seal line names.
const GENERATOR: &str = "targo --unverified verify vendor-out";

/// `verify vendor-out`'s usage.
pub const USAGE: &str = "targo --unverified verify vendor-out --into <dir> [--check]

Write byte-identical projections of aterm-agent-names and aterm-phase into
<dir>/vendor/<crate>/ for another repository to build from. Each crate is read where
aterm's workspace points it. The receiving repository must already carry its own
whole tagged vendor/alab-harness plus vendor/alab-harness.pin, from alab-harness's
tools/pin. alab-policy dependencies point into that copy; no flattened policy is
written. Source and target pins may differ. Both check their index and working files;
alab-harness's tools/check-pin separately checks each pin against its upstream tag.
Nothing is read until each source pinned copy holds to its pin (the index holds
the pinned tree and nothing under it is unstaged, untracked or ignored; else exit 1,
without git exit 3). The target pin must hold before checking or writing projections.
The copy holds the files each library's build reads with debug
assertions on (no test file, nothing only a test reads), a standalone Cargo.toml
(the version, edition and lints of the workspace it builds in spelled out, path
dependencies between projections or into canonical alab-policy, no tests), and
vendor/VENDORED, each projected file's FNV-1a
64 digest, sealed. A crate
that depends on one outside these owners is refused. Neither old seals nor new
output may claim the canonical subtree or pin. A write replaces each file at a path
it writes, whoever made it, and removes a file it no longer writes only where the
sealed list there names it (including obsolete flattened alab-policy files). Any
other file outside the validated canonical owner (a stray) stops the write
before anything changes.

  --check   write nothing; name each file that differs, is missing, is extra (a
            write removes it) or is a stray (a write refuses until it is moved)

Name the aterm commit in the receiving repository's commit message.

Exit: 0 written (--check: the copy matches), 1 refused (--check: it does not),
2 usage, 3 could not run.
";

/// What vendor-out writes: each file's bytes, by its path from `vendor/`.
pub type Copy = BTreeMap<String, Vec<u8>>;

/// How a file under `vendor/` stands against what vendor-out would write ([`drift`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Drift {
    /// It is there, and its bytes (or its kind: not a regular file) are not what vendor-out
    /// writes.
    Differs(String),
    /// Vendor-out writes it, and it is not there.
    Missing(String),
    /// It is there, vendor-out does not write it, and the sealed [`MANIFEST`] there names it as
    /// one an earlier run wrote: a write removes it.
    Extra(String),
    /// It is there, vendor-out does not write it, and no sealed [`MANIFEST`] there names it: a
    /// write refuses, changing nothing, until it is moved out.
    Stray(String),
}

impl Drift {
    /// The path from `vendor/` it names.
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::Differs(path) | Self::Missing(path) | Self::Extra(path) | Self::Stray(path) => {
                path
            }
        }
    }

    /// The word a report leads it with.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Differs(_) => "differs",
            Self::Missing(_) => "missing",
            Self::Extra(_) => "extra",
            Self::Stray(_) => "stray",
        }
    }
}

/// Run `verify vendor-out` with the arguments after `vendor-out`, for the aterm checkout `cwd` is
/// in.
#[must_use]
pub fn run(args: &[String], cwd: &Path) -> i32 {
    let mut into: Option<&str> = None;
    let mut check = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return exit::PASS;
            }
            "--check" => check = true,
            "--into" => match it.next() {
                Some(dir) if !dir.is_empty() && !dir.starts_with('-') => into = Some(dir),
                _ => return usage("--into needs a directory"),
            },
            other => match other.strip_prefix("--into=") {
                Some(dir) if !dir.is_empty() => into = Some(dir),
                _ => return usage(&format!("unknown argument `{other}`")),
            },
        }
    }
    let Some(into) = into else {
        return usage("--into <dir> is required: the receiving repository's checkout");
    };
    let into = cwd.join(into);
    if !into.is_dir() {
        return usage(&format!("--into {}: no such directory", into.display()));
    }
    let Some(root) = crate::checkout_root(cwd) else {
        eprintln!(
            "verify vendor-out: COULD NOT RUN — {} is not inside a git checkout. Fix: run it \
             from the aterm checkout",
            cwd.display()
        );
        return exit::COULD_NOT_RUN;
    };
    if same_dir(&into, &root) {
        return usage(&format!(
            "--into {} is the aterm checkout itself; name the receiving repository's checkout",
            into.display()
        ));
    }
    let want = match pinned_source(&root, &CRATES) {
        Ok(want) => want,
        Err((code, line)) => {
            eprintln!("verify vendor-out: {line}");
            return code;
        }
    };
    let target = match Target::checked(&into) {
        Ok(target) => target,
        Err((code, line)) => {
            eprintln!("verify vendor-out: {line}");
            return code;
        }
    };
    let shown = &target.vendor;
    if check {
        let found = match drift(&target, &want) {
            Ok(found) => found,
            Err((code, why)) => {
                let label = if code == exit::COULD_NOT_RUN {
                    "COULD NOT RUN"
                } else {
                    "REFUSED"
                };
                eprintln!("verify vendor-out: {label} — {why}");
                return code;
            }
        };
        for item in &found {
            println!("{} {VENDOR}/{}", item.word(), item.path());
        }
        println!("{}", verdict(shown, &found, want.len()));
        if found.is_empty() {
            exit::PASS
        } else {
            exit::FAILED
        }
    } else {
        match write(&target, &want) {
            Ok(changed) => {
                println!(
                    "verify vendor-out: {} holds {} files ({} written, {} removed)",
                    shown.display(),
                    want.len(),
                    changed.written,
                    changed.removed
                );
                exit::PASS
            }
            Err((code, why)) => {
                let label = if code == exit::COULD_NOT_RUN {
                    "COULD NOT RUN"
                } else {
                    "REFUSED"
                };
                eprintln!("verify vendor-out: {label} — {why}");
                code
            }
        }
    }
}

fn usage(why: &str) -> i32 {
    eprintln!("verify vendor-out: {why} (verify vendor-out --help)");
    exit::USAGE
}

/// `--check`'s last line for the copy in `shown`, of which [`drift`] found `found`, where a write
/// leaves `files` files: that it matches, or how many files the lines above name and what makes it
/// current, the strays a write refuses over first.
#[must_use]
pub fn verdict(shown: &Path, found: &[Drift], files: usize) -> String {
    let count = |n: usize| {
        if n == 1 {
            "1 file".to_string()
        } else {
            format!("{n} files")
        }
    };
    let shown = shown.display();
    if found.is_empty() {
        return format!("verify vendor-out: {shown} matches ({})", count(files));
    }
    let first = if found.iter().any(|item| matches!(item, Drift::Stray(_))) {
        format!(
            "move each file named stray out of {VENDOR}/ (no earlier run wrote it, so a write \
             refuses while it is there), then "
        )
    } else {
        String::new()
    };
    format!(
        "verify vendor-out: {shown} does not match: {} named above; {first}rerun without --check \
         to rewrite it",
        count(found.len())
    )
}

/// What vendor-out copies from the checkout at `root`: [`vendored`] of `crates`, once every copy
/// aterm vendors pinned by tree id holds to its pin ([`aterm_forge::tree_pins`]: the index holds
/// the pinned tree and the working copy changes nothing under it). vendor-out reads the working
/// files and runs before verify's own pin check, so it applies the same source dependency
/// validation before projecting crates that compile against those dependencies.
///
/// # Errors
/// The exit code and line [`run`] stops with: a pin that does not hold (refused, `[OB-1]`), git
/// unable to judge one (could not run), or a crate [`vendored`] refuses.
fn pinned_source(root: &Path, crates: &[&str]) -> Result<Copy, (i32, String)> {
    match aterm_forge::tree_pins(root) {
        Ok(_) => {}
        Err(aterm_forge::TreePinError::Fail(why)) => {
            return Err((
                exit::FAILED,
                format!("REFUSED — [OB-1] {why}; nothing was copied"),
            ));
        }
        Err(aterm_forge::TreePinError::CouldNotRun(why)) => {
            return Err((exit::COULD_NOT_RUN, format!("COULD NOT RUN — [OB-1] {why}")));
        }
    }
    vendored(root, crates).map_err(|why| (exit::FAILED, format!("REFUSED — {why}")))
}

/// Whether `a` and `b` name one directory.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The copy of `crates` that vendor-out writes, [`MANIFEST`] included: each read from its
/// `source_dir` in the checkout at `root`, and resolved against its `own_workspace`.
///
/// # Errors
/// A crate this writer refuses (the module's rules), or a file that does not read.
pub fn vendored(root: &Path, crates: &[&str]) -> Result<Copy, String> {
    if crates.contains(&POLICY) {
        return Err(
            "alab-policy is owned by the receiving repository's canonical \
                    vendor/alab-harness pin, never a flattened projection"
                .to_string(),
        );
    }
    let top = toml_table(root, "Cargo.toml")?;
    let mut copy = Copy::new();
    for &name in crates {
        let dir = source_dir(name, &top)?;
        let manifest = toml_table(root, &format!("{dir}/Cargo.toml"))?;
        let workspace = own_workspace(root, &dir, &top)?;
        let cargo = standalone(name, &dir, &manifest, &workspace, crates)?;
        let lib = lib_path(name, &manifest)?;
        if root.join(&dir).join("build.rs").exists() {
            return Err(format!(
                "{name} has a build script ({dir}/build.rs), which a copy cannot carry"
            ));
        }
        copy.insert(format!("{name}/Cargo.toml"), cargo.into_bytes());
        for (path, bytes) in built_files(root, name, &dir, &lib)? {
            copy.insert(format!("{name}/{path}"), bytes);
        }
    }
    let list = manifest_text(&copy)?;
    copy.insert(MANIFEST.to_string(), list.into_bytes());
    Ok(copy)
}

/// The folder of the checkout the crate `name` is read from: the path the root manifest's
/// `[workspace.dependencies]` gives it, which every member that inherits it builds it from, else
/// `crates/<name>`.
///
/// # Errors
/// The root names it with no path (a registry or git crate, of which this checkout holds no
/// source), or with one that is not a folder inside the checkout.
fn source_dir(name: &str, top: &Table) -> Result<String, String> {
    let Some(entry) = top
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(|d| d.get(name))
    else {
        return Ok(format!("crates/{name}"));
    };
    let Some(path) = entry
        .as_table()
        .and_then(|spec| spec.get("path"))
        .and_then(Value::as_str)
    else {
        return Err(format!(
            "the root's [workspace.dependencies] takes {name} from no path, so this checkout \
             holds no source of it to copy"
        ));
    };
    inside(path).ok_or_else(|| {
        format!(
            "the root's [workspace.dependencies] points {name} at {path}, which is not a folder \
             inside this checkout"
        )
    })
}

/// `path`, relative to the checkout's root, as `/`-joined parts with `.` and `..` resolved; `None`
/// when it is absolute, names the root itself or leaves the checkout.
fn inside(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in Path::new(path).components() {
        match part {
            Component::Normal(part) => parts.push(part.to_str()?),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The workspace the crate in `dir` builds in, as cargo finds it, by its manifest's path and table:
/// the crate's own manifest where that has a `[workspace]` table, else the nearest manifest above
/// `dir`, up to the checkout's root (`top`), whose `[workspace]` takes the crate in
/// ([`takes_in`]): cargo passes over a root whose `exclude` covers the crate. That is the root's
/// for a member under `crates/`, and a vendored repository's own root for a crate inside a folder
/// the root excludes, so the crate inherits that repository's version and lints, never aterm's.
///
/// # Errors
/// A manifest on the way up does not read as TOML or has a `workspace` that is no table, or none
/// takes the crate in: cargo would then build it as a workspace of its own, where its `workspace =
/// true` keys have nothing to inherit from, so no copy of it builds as aterm builds it (a vendored
/// repository's copy that lost its own root manifest, say).
fn own_workspace(root: &Path, dir: &str, top: &Table) -> Result<(String, Table), String> {
    let mut passed = Vec::new();
    let mut folder = Some(dir);
    while let Some(at) = folder {
        let (manifest, table) = if at.is_empty() {
            ("Cargo.toml".to_string(), Some(top.clone()))
        } else {
            let manifest = format!("{at}/Cargo.toml");
            let table = if root.join(&manifest).is_file() {
                Some(toml_table(root, &manifest)?)
            } else {
                None
            };
            (manifest, table)
        };
        if let Some(table) = table
            && let Some(workspace) = table.get("workspace")
        {
            let Some(workspace) = workspace.as_table() else {
                return Err(format!("{manifest}'s workspace is not a table"));
            };
            if at == dir || takes_in(workspace, at, dir) {
                return Ok((manifest, table));
            }
            passed.push(manifest);
        }
        folder = (!at.is_empty()).then(|| at.rsplit_once('/').map_or("", |(parent, _)| parent));
    }
    Err(format!(
        "no workspace takes in {dir} ({} excludes it, and no other manifest at or above it has a \
         [workspace]), so cargo builds it as a workspace of its own and its `workspace = true` \
         keys have nothing to inherit: a vendored repository's copy needs its own root manifest",
        if passed.is_empty() {
            "no manifest".to_string()
        } else {
            passed.join(", ")
        }
    ))
}

/// Whether the `[workspace]` table `workspace`, in the manifest of the folder `at`, takes in the
/// crate in `dir` below it, as cargo judges a root it finds above a package: unless an `exclude`
/// entry covers the crate's manifest and no `members` entry names it, each entry a path from
/// `at` compared component by component (cargo compares `members` here literally, before any glob
/// is expanded).
fn takes_in(workspace: &Table, at: &str, dir: &str) -> bool {
    let manifest = format!("{dir}/Cargo.toml");
    let covers = |key: &str| {
        workspace
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|entry| {
                inside(&if at.is_empty() {
                    entry.to_string()
                } else {
                    format!("{at}/{entry}")
                })
            })
            .any(|path| Path::new(&manifest).starts_with(path))
    };
    !covers("exclude") || covers("members")
}

/// `relative` under `root`, read as a TOML table.
fn toml_table(root: &Path, relative: &str) -> Result<Table, String> {
    let text =
        crate::read_regular_text(root.join(relative)).map_err(|e| format!("{relative}: {e}"))?;
    match aterm_toml::from_str::<Value>(&text) {
        Ok(Value::Table(table)) => Ok(table),
        Ok(_) => Err(format!("{relative} is not a TOML table")),
        Err(e) => Err(format!("{relative}: {e}")),
    }
}

/// The files of the crate in `dir` that its library's build reads (the module's rules), by path
/// from `dir`.
fn built_files(
    root: &Path,
    name: &str,
    dir: &str,
    lib: &str,
) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let text = |path: &str| crate::read_regular_text(root.join(path)).ok();
    let read = own_tests::closure(&text, &format!("{dir}/{lib}"), dir, false).ok_or_else(|| {
        format!(
            "the census reader cannot settle what {name}'s library reads (a module path a \
             `cfg_attr` or a macro sets, an include whose path is no literal, or a file that \
             does not read): no copy is made of it"
        )
    })?;
    let prefix = format!("{dir}/");
    let mut files = BTreeMap::new();
    for path in read.compiled.keys().chain(&read.embedded) {
        let Some(within) = path.strip_prefix(&prefix) else {
            return Err(format!(
                "{name}'s library reads {path}, outside {dir}: a copy of the folder would not \
                 build"
            ));
        };
        let bytes = crate::read_regular(root.join(path)).map_err(|e| format!("{path}: {e}"))?;
        files.insert(within.to_string(), bytes);
    }
    Ok(files)
}

/// The library's root file, from the package's folder.
fn lib_path(name: &str, manifest: &Table) -> Result<String, String> {
    let lib = manifest.get("lib").and_then(Value::as_table);
    match lib.and_then(|lib| lib.get("path")) {
        None => Ok("src/lib.rs".to_string()),
        Some(Value::String(path)) => Ok(path.clone()),
        Some(_) => Err(format!("{name}'s [lib] path is not a string")),
    }
}

/// The keys of `[package]` a copy writes, in its order, each as the crate resolves it.
const PACKAGE_KEYS: [&str; 6] = [
    "name",
    "version",
    "edition",
    "license",
    "authors",
    "description",
];

/// The keys of `[package]` a copy leaves out: what only aterm's own tools, its own test targets or
/// publishing read. `repository` names the private development repository, which a copy that is
/// published must not.
const PACKAGE_DROPPED: [&str; 12] = [
    "repository",
    "metadata",
    "publish",
    "autotests",
    "autobins",
    "autoexamples",
    "autobenches",
    "readme",
    "homepage",
    "documentation",
    "keywords",
    "categories",
];

/// The top-level tables a copy leaves out: targets other than the library (the tests, benches,
/// binaries and examples stay in aterm) and what only they build with.
const TOP_DROPPED: [&str; 6] = [
    "dev-dependencies",
    "test",
    "bench",
    "bin",
    "example",
    "badges",
];

/// The crate `name`'s standalone `Cargo.toml`, from its own `manifest` in `dir` and its
/// `workspace` manifest ([`own_workspace`]: its path and table); `set` is every crate vendored
/// with it.
///
/// # Errors
/// Anything a copy cannot carry over (the module's rules).
fn standalone(
    name: &str,
    dir: &str,
    manifest: &Table,
    (from, workspace): &(String, Table),
    set: &[&str],
) -> Result<String, String> {
    let shared = workspace.get("workspace").and_then(Value::as_table);
    let refuse = |what: String| Err(format!("{name}: {what}, which a copy does not carry"));
    for key in manifest.keys() {
        let carried = matches!(
            key.as_str(),
            "package" | "lib" | "lints" | "dependencies" | "target"
        );
        if !carried && !TOP_DROPPED.contains(&key.as_str()) {
            let empty = manifest
                .get(key)
                .and_then(Value::as_table)
                .is_some_and(Table::is_empty);
            if !(empty && matches!(key.as_str(), "features" | "build-dependencies")) {
                return refuse(format!("[{key}]"));
            }
        }
    }
    let Some(package) = manifest.get("package").and_then(Value::as_table) else {
        return Err(format!("{name}'s Cargo.toml has no [package]"));
    };
    if package.get("name").and_then(Value::as_str) != Some(name) {
        return Err(format!(
            "{dir}/Cargo.toml names another package; the set names {name}, read from that folder"
        ));
    }
    for key in package.keys() {
        if !PACKAGE_KEYS.contains(&key.as_str()) && !PACKAGE_DROPPED.contains(&key.as_str()) {
            return refuse(format!("package.{key}"));
        }
    }
    let inherited = shared
        .and_then(|w| w.get("package"))
        .and_then(Value::as_table);
    let mut out = format!(
        "# SPDX-License-Identifier: Apache-2.0\n\
         # Copyright 2026 Andrew Yates\n\
         # Written by `{GENERATOR}` from aterm's {dir}/Cargo.toml: rerun it there, never\n\
         # edit this copy (../{MANIFEST} holds each file's digest).\n\
         \n[package]\n"
    );
    for key in PACKAGE_KEYS {
        let value = match package.get(key) {
            None => continue,
            Some(Value::Table(spec)) if spec.get("workspace") == Some(&Value::Boolean(true)) => {
                inherited.and_then(|p| p.get(key)).ok_or_else(|| {
                    format!(
                        "{name}: {key}.workspace = true, and its workspace ({from}) sets no {key}"
                    )
                })?
            }
            Some(value) => value,
        };
        let _ = writeln!(out, "{key} = {}", inline(value)?);
    }
    if package.get("edition").is_none() {
        return Err(format!(
            "{name} names no edition; a copy spells its edition out"
        ));
    }
    out.push_str(
        "publish = false\nautobins = false\nautoexamples = false\nautotests = false\n\
         autobenches = false\n",
    );
    let lib = manifest.get("lib").and_then(Value::as_table);
    if let Some(lib) = lib {
        for key in lib.keys() {
            if !matches!(
                key.as_str(),
                "path" | "name" | "test" | "doctest" | "bench" | "harness"
            ) {
                return refuse(format!("lib.{key}"));
            }
        }
    }
    // Its tests stay with the files they read: in aterm for a member, in the repository aterm
    // vendors it from (whose workspace it builds in) for a vendored crate.
    if from.as_str() == "Cargo.toml" {
        out.push_str("\n# The copy's one target. Its tests and doctests run in aterm, beside the files\n# they read.\n[lib]\n");
    } else {
        let _ = write!(
            out,
            "\n# The copy's one target. Its tests and doctests run in the repository aterm\n\
             # vendors it from (the workspace of aterm's {from}), beside the files they read.\n\
             [lib]\n"
        );
    }
    if let Some(lib_name) = lib.and_then(|lib| lib.get("name")) {
        let _ = writeln!(out, "name = {}", inline(lib_name)?);
    }
    let _ = writeln!(
        out,
        "path = {}\ntest = false\ndoctest = false\nbench = false",
        quoted(&lib_path(name, manifest)?)
    );
    let lints = match manifest.get("lints") {
        None => None,
        Some(Value::Table(lints)) if lints.get("workspace") == Some(&Value::Boolean(true)) => {
            if lints.len() > 1 {
                return refuse("[lints] beside workspace = true".to_string());
            }
            Some(
                shared
                    .and_then(|w| w.get("lints"))
                    .and_then(Value::as_table)
                    .ok_or_else(|| {
                        format!(
                            "{name}: lints.workspace = true, and its workspace ({from}) has no \
                             [workspace.lints]"
                        )
                    })?,
            )
        }
        Some(Value::Table(lints)) => Some(lints),
        Some(_) => return Err(format!("{name}'s [lints] is not a table")),
    };
    for (tool, levels) in lints.into_iter().flatten() {
        let Some(levels) = levels.as_table() else {
            return Err(format!("{name}'s lints.{tool} is not a table"));
        };
        let _ = writeln!(out, "\n[lints.{}]", key(tool));
        for (lint, level) in levels {
            let _ = writeln!(out, "{} = {}", key(lint), inline(level)?);
        }
    }
    let deps = |table: Option<&Value>, title: &str, out: &mut String| -> Result<(), String> {
        let Some(table) = table else {
            return Ok(());
        };
        let Some(table) = table.as_table() else {
            return Err(format!("{name}'s [{title}] is not a table"));
        };
        let _ = writeln!(out, "\n[{title}]");
        for (dep, spec) in table {
            let _ = writeln!(
                out,
                "{} = {}",
                key(dep),
                dependency(name, dep, spec, shared, set)?
            );
        }
        Ok(())
    };
    deps(manifest.get("dependencies"), "dependencies", &mut out)?;
    if let Some(targets) = manifest.get("target") {
        let Some(targets) = targets.as_table() else {
            return Err(format!("{name}'s [target] is not a table"));
        };
        for (cfg, tables) in targets {
            let Some(tables) = tables.as_table() else {
                return Err(format!("{name}'s target.{cfg} is not a table"));
            };
            for kind in tables.keys() {
                if kind != "dependencies" && kind != "dev-dependencies" {
                    return refuse(format!("target.{cfg}.{kind}"));
                }
            }
            let title = format!("target.{}.dependencies", key(cfg));
            deps(tables.get("dependencies"), &title, &mut out)?;
        }
    }
    Ok(out)
}

/// The copy's spelling of the dependency `dep` of `name` (`spec`, under its own workspace's
/// `shared` `[workspace]` where it says `workspace = true`): a path to the copy of a crate in
/// `set`, or canonical alab-policy in the receiving repository's independent pin.
///
/// # Errors
/// It is neither a crate of `set` nor canonical alab-policy, or no path dependency.
fn dependency(
    name: &str,
    dep: &str,
    spec: &Value,
    shared: Option<&Table>,
    set: &[&str],
) -> Result<String, String> {
    let outside = |package: &str| {
        Err(format!(
            "{name} depends on {package}, which is neither a projected crate ({}) nor canonical \
             alab-policy: a projection builds only from these owners",
            set.join(", ")
        ))
    };
    let Some(own) = spec.as_table() else {
        return outside(dep);
    };
    let mut merged = Table::new();
    if own.get("workspace") == Some(&Value::Boolean(true)) {
        match shared
            .and_then(|w| w.get("dependencies"))
            .and_then(|d| d.get(dep))
        {
            Some(Value::Table(from)) => {
                for (k, v) in from {
                    merged.insert(k.clone(), v.clone());
                }
            }
            Some(_) => return outside(dep),
            None => {
                return Err(format!(
                    "{name}: {dep}.workspace = true, and [workspace.dependencies] has no {dep}"
                ));
            }
        }
    }
    for (k, v) in own {
        match (k.as_str(), merged.get(k.as_str())) {
            ("workspace", _) => {}
            ("features", Some(Value::Array(from))) => {
                let mut all = from.clone();
                all.extend(v.as_array().cloned().unwrap_or_default());
                merged.insert(k.clone(), Value::Array(all));
            }
            _ => {
                merged.insert(k.clone(), v.clone());
            }
        }
    }
    let package = match merged.get("package") {
        None => dep,
        Some(Value::String(package)) => package.as_str(),
        Some(_) => return Err(format!("{name}'s dependency {dep} names no package")),
    };
    if merged.get("path").and_then(Value::as_str).is_none()
        || !(set.contains(&package) || package == POLICY)
    {
        return outside(package);
    }
    let path = if package == POLICY {
        format!("../{CANONICAL}/crates/{POLICY}")
    } else {
        format!("../{package}")
    };
    let mut out = format!("{{ path = {}", quoted(&path));
    if package != dep {
        let _ = write!(out, ", package = {}", quoted(package));
    }
    for (k, v) in &merged {
        match k.as_str() {
            "path" | "package" | "version" => {}
            "features" | "default-features" => {
                let _ = write!(out, ", {} = {}", key(k), inline(v)?);
            }
            other => {
                return Err(format!(
                    "{name}'s dependency {dep} sets {other}, which a copy does not carry"
                ));
            }
        }
    }
    out.push_str(" }");
    Ok(out)
}

/// `value` as one line of TOML.
fn inline(value: &Value) -> Result<String, String> {
    Ok(match value {
        Value::String(text) => quoted(text),
        Value::Integer(n) => n.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Array(items) => {
            let items: Result<Vec<String>, String> = items.iter().map(inline).collect();
            format!("[{}]", items?.join(", "))
        }
        Value::Table(table) => {
            let mut pairs = Vec::new();
            for (k, v) in table {
                pairs.push(format!("{} = {}", key(k), inline(v)?));
            }
            format!("{{ {} }}", pairs.join(", "))
        }
        Value::Float(_) | Value::Datetime(_) => {
            return Err(format!(
                "a manifest value this writer does not spell: {value:?}"
            ));
        }
    })
}

/// `text` as a TOML basic string.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `name` as a TOML key: bare where it may be, else quoted.
fn key(name: &str) -> String {
    if !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        name.to_string()
    } else {
        quoted(name)
    }
}

/// [`MANIFEST`]'s text for `copy` (which does not hold it yet): its seal line, a line on what
/// each line says, and `<fnv1a64> <path>` per file in path order, sealed.
///
/// # Errors
/// A path that would break a line of the list (a newline in it).
fn manifest_text(copy: &Copy) -> Result<String, String> {
    let mut text = seal::line("#", GENERATOR);
    text.push_str(
        "\n# One line per projected file: its FNV-1a 64 digest, then its path from here.\n\
         # alab-harness/ and alab-harness.pin are separately owned by their canonical tree pin.\n",
    );
    for (path, bytes) in copy {
        if path.contains(['\n', '\r']) {
            return Err(format!("a vendored path holds a line break: {path:?}"));
        }
        let _ = writeln!(text, "{:016x} {path}", aterm_hash::fnv1a_64(bytes));
    }
    seal::stamp(&text)
}

/// A destination whose independently owned canonical copy has been checked. Only this checked
/// target reaches the projection reader/writer; neither operation touches the canonical owner.
struct Target {
    vendor: PathBuf,
}

impl Target {
    fn checked(root: &Path) -> Result<Self, (i32, String)> {
        match aterm_forge::tree_pin(root, CANONICAL_DIR, CANONICAL_PIN) {
            Ok(_) => Ok(Self {
                vendor: root.join(VENDOR),
            }),
            Err(aterm_forge::TreePinError::Fail(why)) => Err((
                exit::FAILED,
                format!("REFUSED — target [OB-1] {why}; nothing was copied"),
            )),
            Err(aterm_forge::TreePinError::CouldNotRun(why)) => Err((
                exit::COULD_NOT_RUN,
                format!("COULD NOT RUN — target [OB-1] {why}"),
            )),
        }
    }
}

/// Whether a vendor-relative path names either canonical-owned artifact, after resolving dots.
fn protected(path: &str) -> bool {
    inside(path).is_some_and(|path| {
        path == CANONICAL || path.starts_with("alab-harness/") || path == "alab-harness.pin"
    })
}

fn disjoint<'a>(paths: impl IntoIterator<Item = &'a String>) -> Result<(), (i32, String)> {
    for path in paths {
        if protected(path) {
            return Err((
                exit::FAILED,
                format!(
                    "projection ownership overlaps canonical vendor/{path}: neither output nor \
                     a sealed {MANIFEST} may claim the canonical subtree or pin"
                ),
            ));
        }
    }
    Ok(())
}

/// The paths a sealed [`MANIFEST`] in `vendor` lists (itself among them): the files an earlier
/// run wrote. Empty where the list is missing or its seal does not hold.
fn written_before(vendor: &Path) -> BTreeSet<String> {
    let Ok(text) = crate::read_regular_text(vendor.join(MANIFEST)) else {
        return BTreeSet::new();
    };
    if !matches!(seal::check(&text), seal::Seal::Holds { .. }) {
        return BTreeSet::new();
    }
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once(' ').map(|(_, path)| path.to_string()))
        .chain(std::iter::once(MANIFEST.to_string()))
        .collect()
}

/// Every entry under `vendor` but its folders, by path from it, each with whether it is a regular
/// file; empty where `vendor` does not exist.
///
/// # Errors
/// A folder that does not list.
fn entries(target: &Target) -> Result<BTreeMap<String, bool>, String> {
    let vendor = &target.vendor;
    let mut found = BTreeMap::new();
    if std::fs::symlink_metadata(vendor).is_err() {
        return Ok(found);
    }
    let mut folders = vec![(vendor.to_path_buf(), String::new())];
    while let Some((folder, prefix)) = folders.pop() {
        let listed =
            std::fs::read_dir(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
        for entry in listed {
            let entry = entry.map_err(|e| format!("{}: {e}", folder.display()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = format!("{prefix}{name}");
            if protected(&path) {
                continue;
            }
            let kind = entry
                .file_type()
                .map_err(|e| format!("{}: {e}", entry.path().display()))?;
            if kind.is_dir() {
                folders.push((entry.path(), format!("{path}/")));
            } else {
                found.insert(path, kind.is_file());
            }
        }
    }
    Ok(found)
}

/// Each projection file under the checked `target` that differs from `want`, is missing from it,
/// is extra (the sealed [`MANIFEST`] there names it) or is a stray (none names it), in path order.
///
/// # Errors
/// Overlapping canonical ownership, or a folder or file that does not read.
fn drift(target: &Target, want: &Copy) -> Result<Vec<Drift>, (i32, String)> {
    let vendor = &target.vendor;
    let ours = written_before(vendor);
    disjoint(want.keys())?;
    disjoint(&ours)?;
    let have = entries(target).map_err(|why| (exit::COULD_NOT_RUN, why))?;
    let mut found = Vec::new();
    for (path, bytes) in want {
        match have.get(path) {
            None => found.push(Drift::Missing(path.clone())),
            Some(false) => found.push(Drift::Differs(path.clone())),
            Some(true) => {
                let now = crate::read_regular(vendor.join(path))
                    .map_err(|e| (exit::COULD_NOT_RUN, format!("{VENDOR}/{path}: {e}")))?;
                if now != *bytes {
                    found.push(Drift::Differs(path.clone()));
                }
            }
        }
    }
    found.extend(
        have.keys()
            .filter(|path| !want.contains_key(*path))
            .map(|path| {
                if ours.contains(path) {
                    Drift::Extra(path.clone())
                } else {
                    Drift::Stray(path.clone())
                }
            }),
    );
    found.sort_by(|a, b| a.path().cmp(b.path()));
    Ok(found)
}

/// What [`write`] changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Written {
    /// Files written: missing, or not as vendor-out writes them.
    pub written: usize,
    /// Files removed: ones an earlier run wrote that this one does not.
    pub removed: usize,
}

/// Make the checked target's projections hold exactly `want`: remove the files an earlier run
/// wrote that `want` does not hold ([`Drift::Extra`]), then write each file that is missing or
/// differs, replacing whatever is at that path. Canonical artifacts are never changed.
///
/// # Errors
/// A [`Drift::Stray`], a file under `vendor` that `want` does not hold and the sealed list there
/// does not name, with nothing changed; or a write or removal that failed, which leaves the copy
/// part written.
fn write(target: &Target, want: &Copy) -> Result<Written, (i32, String)> {
    let vendor = &target.vendor;
    let found = drift(target, want)?;
    let strangers: Vec<&str> = found
        .iter()
        .filter_map(|item| match item {
            Drift::Stray(path) => Some(path.as_str()),
            _ => None,
        })
        .collect();
    if !strangers.is_empty() {
        return Err((
            exit::FAILED,
            format!(
                "{} holds {} that no earlier run wrote (the sealed {MANIFEST} there names none of \
             them); move them out of {VENDOR}/ or remove them, then rerun. Nothing was written",
                vendor.display(),
                strangers.join(", ")
            ),
        ));
    }
    let mut changed = Written {
        written: 0,
        removed: 0,
    };
    for item in &found {
        if let Drift::Extra(path) = item {
            let at = vendor.join(path);
            std::fs::remove_file(&at)
                .map_err(|e| (exit::FAILED, format!("{}: {e}", at.display())))?;
            changed.removed += 1;
            let mut folder = at.parent();
            while let Some(dir) = folder.filter(|dir| *dir != vendor) {
                if std::fs::remove_dir(dir).is_err() {
                    break;
                }
                folder = dir.parent();
            }
        }
    }
    for item in &found {
        let (Drift::Differs(path) | Drift::Missing(path)) = item else {
            continue;
        };
        let at = vendor.join(path);
        if let Some(dir) = at.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| (exit::FAILED, format!("{}: {e}", dir.display())))?;
        }
        if matches!(std::fs::symlink_metadata(&at), Ok(meta) if !meta.is_file()) {
            std::fs::remove_file(&at)
                .map_err(|e| (exit::FAILED, format!("{}: {e}", at.display())))?;
        }
        aterm_uds::atomic_file::write_flushed(&at, &want[path], 0o644, None)
            .map_err(|e| (exit::FAILED, format!("{}: {e}", at.display())))?;
        changed.written += 1;
    }
    Ok(changed)
}

#[cfg(test)]
#[path = "vendor_out_tests.rs"]
mod tests;
