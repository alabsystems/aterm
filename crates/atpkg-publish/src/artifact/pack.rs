// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish pack <program>`: one plain program build, for one target.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::lineage_switch::{self, Switch};
use super::pkgdoc::{self, Header, Kind, Row};
use super::tools::{HostTools, Run, RunError, Tools};
use super::{arch, package, sign, tarwrite, toolchain, upload};
use crate::api::{Build, PackOutcome, PackRequest, Packer, Signing};
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::host::ReleaseHost;
use crate::identity::MachineKey;
use crate::programs::{self, Harness, Shape};
use crate::triple::{Os, Triple};

mod diagnostics;

/// The plain-program packer.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pack;

impl Packer for Pack {
    /// Every pack is compressed in this process by aterm-zstd ([`tarwrite::Zstd`]): this
    /// machine needs no compressor of its own.
    fn can_pack(&self) -> std::result::Result<(), String> {
        Ok(())
    }

    fn pack(&self, ctx: &Ctx, request: &PackRequest) -> Result<PackOutcome> {
        pack_with(ctx, request, &HostTools)
    }

    fn pack_bottle(
        &self,
        ctx: &Ctx,
        request: &super::bottle::upstream::Request,
    ) -> Result<super::bottle::BottleOutcome> {
        super::bottle::upstream::pack(ctx, request)
    }

    fn can_pack_bottle(&self, program: &str, triple: Triple) -> std::result::Result<(), String> {
        super::bottle::upstream::can_build(program, triple).map_err(|error| error.to_string())
    }

    fn reuse_bottle(
        &self,
        ctx: &Ctx,
        request: &super::bottle::reuse::Request,
    ) -> Result<super::bottle::BottleOutcome> {
        super::bottle::reuse::prepare(ctx, request)
    }
}

/// How long a pack waits for another publish of the same program to finish.
pub const LOCK_WAIT: Duration = Duration::from_secs(300);

/// The lock every pack of `program` holds for its whole run: its stage is rebuilt from scratch and its release tag mutated, and two runs of one program often use different `out` dirs, so the lock is keyed on the program.
/// # Errors
/// A refusal when another publish holds it past `wait`.
pub fn pack_lock(ctx: &Ctx, program: &str, wait: Duration) -> Result<aterm_update_core::FileLock> {
    let dir = ctx.state_dir.join("locks");
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(format!("mkdir {}", dir.display()), &e))?;
    let path = dir.join(format!("pack-{program}.lock"));
    aterm_update_core::FileLock::acquire_within(&path, wait).map_err(|e| {
        if e.kind() == std::io::ErrorKind::TimedOut {
            Error::refused(format!(
                "could not take the pack-{program} lock within {} s — another publish of \
                 {program} is running",
                wait.as_secs()
            ))
        } else {
            Error::io(format!("lock {}", ctx.shown(&path)), &e)
        }
    })
}

/// The distribution CPU floor a target is built for: the oldest CPU any supported OS runs on, never the builder's (`target-cpu=native` faults on an older user's machine, and cross-built it is refused outright).
#[must_use]
pub fn cpu_floor(triple: Triple) -> Option<&'static str> {
    match (triple.os(), triple.arch()) {
        (Os::Darwin | Os::Linux, crate::triple::Arch::X86_64) => Some("x86-64-v2"),
        (Os::Darwin, crate::triple::Arch::Aarch64) => Some("apple-m1"),
        _ => None,
    }
}

/// The CPU override retained for explicit cross builds. Native builds append
/// their floor to Cargo's target configuration instead of replacing policy.
#[must_use]
pub fn rustflags(triple: Triple) -> String {
    cpu_floor(triple).map_or_else(String::new, |cpu| format!("-C target-cpu={cpu}"))
}

/// The variable a harness pack's build reads its own version from (D46): the pack sets it to the
/// version it signs (`MAJOR.MINOR.0+g<sha9>`, [`dev_version`]), so the binary's `--version` and
/// its claim say what the manifest says. Every pack's build has an inherited value removed
/// first ([`SCRUBBED_ENV`]).
pub const HARNESS_VERSION_ENV: &str = "ALAB_HARNESS_VERSION";

/// Inherited variables a pack removes so the compiler, repository policy, CPU
/// floor and harness version come from the admitted build inputs.
pub const SCRUBBED_ENV: &[&str] = &[
    HARNESS_VERSION_ENV,
    "RUSTFLAGS",
    "RUSTDOCFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_ENCODED_RUSTDOCFLAGS",
    "CARGO_BUILD_RUSTFLAGS",
    "CARGO_BUILD_RUSTDOCFLAGS",
    "CARGO_BUILD_TARGET",
    "TRUSTFLAGS",
    "CARGO_ENCODED_TRUSTFLAGS",
    "CARGO_BUILD_RUSTC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    "RUSTUP_TOOLCHAIN",
];

/// The one thing a caller may add to a pack's build environment: a cross linker, `CARGO_TARGET_<TRIPLE>_LINKER`.
/// # Errors
/// A usage error naming the first other key.
pub fn check_build_env(env: &[(String, String)]) -> Result<()> {
    for (k, _) in env {
        let linker = k
            .strip_prefix("CARGO_TARGET_")
            .and_then(|t| t.strip_suffix("_LINKER"))
            .is_some_and(|t| {
                !t.is_empty()
                    && t.bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            });
        if !linker {
            return Err(Error::usage(format!(
                "a pack's build environment takes a cross linker only \
                 (CARGO_TARGET_<TRIPLE>_LINKER), not `{k}`: the compiler and the CPU floor \
                 are the pack's own"
            )));
        }
    }
    Ok(())
}

/// What a checkout's `targo metadata` declares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    /// Every workspace member's manifest path.
    pub manifests: BTreeSet<String>,
    /// The program package's `metadata.atpkg` recipe, else the workspace recipe
    /// (with the members' exposed-list fallback): the shimmed tools.
    pub expose: Vec<String>,
    /// `bundle`: shipped, not shimmed.
    pub bundle: Vec<String>,
    /// `build-args`.
    pub build_args: Vec<String>,
    /// `bundle-only`: a sysroot program this lane cannot describe.
    pub bundle_only: bool,
    /// The program's own crate version (else the first member's, else `0.0.0`).
    pub version: String,
    /// The version of the package named like the program, when there is one: the only
    /// version a harness row reads ([`held_to_harness`]).
    pub own_version: Option<String>,
}

fn strings(v: Option<&aterm_json::Value>) -> Vec<String> {
    v.and_then(aterm_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn version_of(p: &aterm_json::Value) -> Option<&str> {
    p.get("version").and_then(aterm_json::Value::as_str)
}

fn package_recipe(recipe: &aterm_json::Value, program: &str) -> Result<Metadata> {
    if recipe.as_object().is_none() {
        return Err(Error::refused(format!(
            "{program}'s package.metadata.atpkg must be a table"
        )));
    }
    let own_strings = |key: &str| -> Result<Vec<String>> {
        let Some(value) = recipe.get(key) else {
            return Ok(Vec::new());
        };
        let values = value.as_array().ok_or_else(|| {
            Error::refused(format!(
                "{program}'s package.metadata.atpkg {key} must be a string array"
            ))
        })?;
        values
            .iter()
            .map(|s| {
                s.as_str().map(str::to_string).ok_or_else(|| {
                    Error::refused(format!(
                        "{program}'s package.metadata.atpkg {key} must be a string array"
                    ))
                })
            })
            .collect()
    };
    let bundle_only = match recipe.get("bundle-only") {
        None => false,
        Some(value) => value.as_bool().ok_or_else(|| {
            Error::refused(format!(
                "{program}'s package.metadata.atpkg bundle-only must be a boolean"
            ))
        })?,
    };
    let expose = own_strings("expose")?;
    if expose.is_empty() && !bundle_only {
        return Err(Error::refused(format!(
            "{program}'s package.metadata.atpkg must name a nonempty expose list"
        )));
    }
    Ok(Metadata {
        expose,
        bundle: own_strings("bundle")?,
        build_args: own_strings("build-args")?,
        bundle_only,
        ..Metadata::default()
    })
}

/// Read `targo metadata --no-deps --format-version 1`'s JSON.
/// # Errors
/// A refusal when it is not that JSON, or the program's own recipe is malformed
/// or names no exposed tool and is not bundle-only.
pub fn parse_metadata(json: &[u8], program: &str) -> Result<Metadata> {
    let v: aterm_json::Value = aterm_json::from_slice(json)
        .map_err(|e| Error::refused(format!("`targo metadata` printed no JSON: {e}")))?;
    let packages = v
        .get("packages")
        .and_then(aterm_json::Value::as_array)
        .ok_or_else(|| Error::refused("`targo metadata` lists no packages"))?;
    let ws = v.get("metadata").and_then(|m| m.get("atpkg"));
    let own = packages
        .iter()
        .find(|p| p.get("name").and_then(aterm_json::Value::as_str) == Some(program));
    let recipe = own
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("atpkg"));
    let member = |key: &str| -> Vec<String> {
        packages
            .iter()
            .flat_map(|p| {
                strings(
                    p.get("metadata")
                        .and_then(|m| m.get("atpkg"))
                        .and_then(|a| a.get(key)),
                )
            })
            .collect()
    };
    let declared = |key: &str| {
        let w = strings(ws.and_then(|a| a.get(key)));
        if w.is_empty() { member(key) } else { w }
    };
    let workspace_bundle_only = ws
        .and_then(|a| a.get("bundle-only"))
        .and_then(aterm_json::Value::as_bool)
        .unwrap_or(false)
        || packages.iter().any(|p| {
            p.get("metadata")
                .and_then(|m| m.get("atpkg"))
                .and_then(|a| a.get("bundle-only"))
                .and_then(aterm_json::Value::as_bool)
                == Some(true)
        });
    let selected = if let Some(recipe) = recipe {
        package_recipe(recipe, program)?
    } else {
        Metadata {
            expose: declared("expose"),
            bundle: declared("bundle"),
            build_args: strings(ws.and_then(|a| a.get("build-args"))),
            bundle_only: workspace_bundle_only,
            ..Metadata::default()
        }
    };
    let own_version = own.and_then(version_of);
    let version = own_version
        .or_else(|| packages.first().and_then(version_of))
        .unwrap_or("0.0.0")
        .to_string();
    let own_version = own_version.map(str::to_string);
    Ok(Metadata {
        manifests: packages
            .iter()
            .filter_map(|p| p.get("manifest_path").and_then(aterm_json::Value::as_str))
            .map(str::to_string)
            .collect(),
        version,
        own_version,
        ..selected
    })
}

/// Whether `v` is a historical `MAJOR.MINOR.0` or calendar `1.YYYYMMDD.N` release line,
/// without pre-release or build metadata.
#[must_use]
pub fn is_release_line(v: &str) -> bool {
    aterm_types::release_version::ReleaseVersion::parse(v).is_some()
}

/// `meta` held to a harness row's rule ([`Harness`]): its `expose` is the row's tool, or
/// becomes it when the repo declares none; nothing is bundled; the program-named package is
/// at a release line ([`is_release_line`]); and the build is `--locked`.
/// # Errors
/// A refusal naming the first rule `meta` breaks.
pub fn held_to_harness(program: &str, h: Harness, mut meta: Metadata) -> Result<Metadata> {
    if !meta.bundle.is_empty() {
        return Err(Error::refused(format!(
            "{program} is a harness: it ships its one tool `{}`, and its repo's \
             [workspace.metadata.atpkg] bundles {} besides",
            h.tool,
            meta.bundle.join(" ")
        )));
    }
    if !meta.expose.is_empty() && meta.expose != [h.tool] {
        return Err(Error::refused(format!(
            "{program}'s [workspace.metadata.atpkg] expose names {}, and its harness row \
             (programs::PROGRAMS) ships exactly `{}` — one of them is wrong; fix it before \
             anything is built",
            meta.expose.join(" "),
            h.tool
        )));
    }
    meta.expose = vec![h.tool.to_string()];
    let Some(own) = meta.own_version.as_deref() else {
        return Err(Error::refused(format!(
            "{program} has no package named {program}: a harness's version is its own \
             package's [workspace.package] version, never another member's"
        )));
    };
    if !is_release_line(own) {
        return Err(Error::refused(format!(
            "{program}'s version `{own}` is not MAJOR.MINOR.0 or calendar 1.YYYYMMDD.N (and a \
             dev build's `+g<sha>` is the pack's to stamp, never the manifest's)"
        )));
    }
    if !meta.build_args.iter().any(|a| a == "--locked") {
        meta.build_args.push("--locked".to_string());
    }
    Ok(meta)
}

/// A harness pack's version: the release line `own` with the dev build's `+g<sha>`, where
/// `sha` is `git rev-parse --short=9` of the commit built.
/// # Errors
/// A refusal when `own` is not a release line or `sha` is not 9..=40 lowercase hex.
pub fn dev_version(own: &str, sha: &str) -> Result<String> {
    if !is_release_line(own) {
        return Err(Error::refused(format!(
            "`{own}` is not a historical MAJOR.MINOR.0 or calendar 1.YYYYMMDD.N"
        )));
    }
    if !(9..=40).contains(&sha.len())
        || !sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(Error::refused(format!(
            "`{sha}` is not a short commit sha (9 to 40 lowercase hex)"
        )));
    }
    Ok(format!("{own}+g{sha}"))
}

/// The clean main commit a harness build names, shared by local and packed builds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarnessIdentity {
    /// Full lowercase source commit, never a version-prefix guess.
    pub source: String,
    /// The source commit's complete-history commit count.
    pub build_number: Build,
    /// The harness's own release line with its source build stamp.
    pub version: String,
}

impl HarnessIdentity {
    /// Recheck the exact source after a build, without substituting a newer main.
    /// # Errors
    /// Git fails, the checkout or its commit count changed, or any build input is uncommitted.
    pub fn recheck(&self, checkout: &Path) -> Result<()> {
        if crate::git::head(checkout)? != self.source
            || crate::git::rev_count(checkout, "HEAD")? != self.build_number
            || crate::git::is_pristine(checkout)?.is_some()
        {
            return Err(Error::refused(
                "the harness source changed during the build; refusing its artifact",
            ));
        }
        Ok(())
    }
}

/// A harness build's checked metadata and identity, from `checkout` at `build`: refused unless
/// the checkout is pristine ([`crate::git::is_pristine`]), its `HEAD` is on `main` of
/// `<owner>/<repo>` (the staging owner's repo) as `host` serves it, as codex's package source
/// is ([`package::check_on_main`]), and `build` is its `HEAD` commit count, so the `+g<sha>`
/// and the build number both name a commit of the repo's main. A count alone can name several
/// commits main holds (a merged branch's commit, or a former tip a later merge moved off the
/// first-parent chain, repeats one): what holds a build number to one commit is this version,
/// which names it, and [`super::upload::publish`], which never stages a second version under a
/// build staging or the public owner already holds (D51).
/// # Errors
/// Invalid harness metadata, an unclean tree, a missing or foreign main, incomplete history,
/// an invalid full source SHA, a requested build count mismatch, or a concurrent source change.
pub fn harness_identity(
    host: &dyn ReleaseHost,
    program: &str,
    h: Harness,
    meta: Metadata,
    checkout: &Path,
    build: Option<Build>,
    source: (&str, &str),
) -> Result<(Metadata, HarnessIdentity)> {
    let meta = held_to_harness(program, h, meta)?;
    let own = meta.own_version.clone().unwrap_or_default();
    if let Some(stray) = crate::git::is_pristine(checkout)? {
        return Err(Error::refused(format!(
            "{program}: the checkout {} has changes or untracked files ({stray}) — a harness \
             version is minted from a clean tree or not at all (pack from a fresh clone)",
            checkout.display()
        )));
    }
    let (owner, repo) = source;
    let head = crate::git::head(checkout)?;
    if head.len() != 40 || !head.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(Error::refused(
            "a harness needs its full lowercase source SHA",
        ));
    }
    package::check_on_main(
        host,
        checkout,
        owner,
        repo,
        &format!("{program}'s HEAD"),
        &head,
    )?;
    let count = crate::git::rev_count(checkout, "HEAD")?;
    if let Some(build) = build.filter(|build| *build != count) {
        return Err(Error::refused(format!(
            "{program}: build {build} is not the checkout's HEAD (it counts {count} commits) — \
             a harness's build number and its +g<sha> name one commit"
        )));
    }
    let sha = crate::git::run(checkout, &["rev-parse", "--short=9", "HEAD"])?;
    if !head.starts_with(&sha) {
        return Err(Error::refused(
            "the harness source changed during the build; refusing its artifact",
        ));
    }
    let identity = HarnessIdentity {
        source: head,
        build_number: count,
        version: dev_version(&own, &sha)?,
    };
    identity.recheck(checkout)?;
    Ok((meta, identity))
}

/// The workspace members' freshly built bin executables, from a `--message-format=json` stream: sorted, each once.
#[must_use]
pub fn built_executables(stream: &[u8], meta: &Metadata) -> Vec<PathBuf> {
    let mut out: BTreeSet<PathBuf> = BTreeSet::new();
    for line in String::from_utf8_lossy(stream).lines() {
        if !line.contains("\"compiler-artifact\"") {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        if v.get("reason").and_then(aterm_json::Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let is_bin = v
            .get("target")
            .and_then(|t| t.get("kind"))
            .and_then(aterm_json::Value::as_array)
            .is_some_and(|k| k.iter().any(|s| s.as_str() == Some("bin")));
        let not_test = v
            .get("profile")
            .and_then(|p| p.get("test"))
            .and_then(aterm_json::Value::as_bool)
            != Some(true);
        let member = v
            .get("manifest_path")
            .and_then(aterm_json::Value::as_str)
            .is_some_and(|m| meta.manifests.contains(m));
        if is_bin
            && not_test
            && member
            && let Some(exe) = v.get("executable").and_then(aterm_json::Value::as_str)
        {
            out.insert(PathBuf::from(exe));
        }
    }
    out.into_iter().collect()
}

/// How the exposed set was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExposeMode {
    /// The repo's own `expose` declaration — authoritative.
    Declared,
    /// No declaration: the name-token fallback, said loudly.
    Heuristic,
}

/// What a pack ships.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    /// How the exposed set was chosen.
    pub mode: ExposeMode,
    /// `(built executable, name under bin/)`, shipped.
    pub shipped: Vec<(PathBuf, String)>,
    /// The tools shimmed.
    pub exposes: Vec<String>,
    /// Built and dropped.
    pub dropped: Vec<String>,
}

/// Name tokens that mark a bin as internal when a repo declares nothing.
const INTERNAL_TOKENS: &[&str] = &[
    "bench",
    "benches",
    "fixture",
    "fixtures",
    "driver",
    "drivers",
    "matrix",
    "table",
    "probe",
    "replay",
    "demo",
    "example",
    "examples",
    "sandbox",
    "scratch",
    "playground",
    "harness",
    "testbin",
    "testgen",
    "mock",
    "stub",
    "test",
    "tests",
];

/// Whether the name-token fallback drops `name` as an internal bin.
#[must_use]
pub fn looks_internal(name: &str) -> bool {
    let mut first_gen = false;
    let mut last_gen = false;
    let mut count = 0usize;
    for t in name.split(['-', '_']) {
        if INTERNAL_TOKENS.iter().any(|w| t.eq_ignore_ascii_case(w)) {
            return true;
        }
        let is_gen = t.eq_ignore_ascii_case("gen");
        if count == 0 {
            first_gen = is_gen;
        }
        last_gen = is_gen;
        count += 1;
    }
    count > 1 && (first_gen || last_gen)
}

/// Choose what ships and what is shimmed: the declared `expose` ∪ `bundle` (every name of which must have built), else every bin the name-token fallback keeps.
/// # Errors
/// A refusal for a declared bin that did not build, or an empty exposed set.
pub fn select(meta: &Metadata, exes: &[PathBuf], triple: Triple) -> Result<Selection> {
    let name_of = |p: &Path| {
        let file = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match (triple.os(), file.strip_suffix(".exe")) {
            (Os::Windows, Some(stem)) => (stem.to_string(), file),
            _ => (file.clone(), file),
        }
    };
    let declared = !meta.expose.is_empty();
    let wanted: BTreeSet<&str> = meta
        .expose
        .iter()
        .chain(&meta.bundle)
        .map(String::as_str)
        .collect();
    let mut sel = Selection {
        mode: if declared {
            ExposeMode::Declared
        } else {
            ExposeMode::Heuristic
        },
        shipped: Vec::new(),
        exposes: Vec::new(),
        dropped: Vec::new(),
    };
    let mut built = BTreeSet::new();
    for exe in exes {
        let (name, file) = name_of(exe);
        built.insert(name.clone());
        let (ship, expose) = if declared {
            (wanted.contains(name.as_str()), meta.expose.contains(&name))
        } else {
            let keep = !looks_internal(&name);
            (keep, keep)
        };
        if !ship {
            sel.dropped.push(name);
            continue;
        }
        if expose {
            sel.exposes.push(name);
        }
        sel.shipped.push((exe.clone(), file));
    }
    if declared {
        let missing: Vec<&str> = wanted
            .iter()
            .copied()
            .filter(|w| !built.contains(*w))
            .collect();
        if !missing.is_empty() {
            return Err(Error::refused(format!(
                "declared expose/bundle bin(s) not built: {} (fix build-args in \
                 [workspace.metadata.atpkg], or the bin name)",
                missing.join(" ")
            )));
        }
    }
    if sel.exposes.is_empty() {
        return Err(Error::refused(format!(
            "the exposed set is empty (dropped: {})",
            sel.dropped.join(" ")
        )));
    }
    Ok(sel)
}

fn tail(bytes: &[u8], n: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut lines: Vec<&str> = text.lines().rev().take(n).collect();
    lines.reverse();
    lines.join("\n")
}

/// Run `targo metadata` in the checkout through the resolved driver (no `--unverified`:
/// targo takes the lane flag on compilation commands only).
fn metadata(
    tools: &dyn Tools,
    tc: &toolchain::Toolchain,
    checkout: &Path,
    program: &str,
) -> Result<Metadata> {
    parse_metadata(&metadata_bytes(tools, tc, checkout)?, program)
}

fn metadata_bytes(
    tools: &dyn Tools,
    tc: &toolchain::Toolchain,
    checkout: &Path,
) -> Result<Vec<u8>> {
    let mut run = Run::new(
        tc.targo(),
        ["metadata", "--no-deps", "--format-version", "1"],
    );
    run.cwd = Some(checkout.to_path_buf());
    run.env.push(("RUSTC".into(), tc.trustc().into()));
    let ran = tools
        .run(&run)
        .map_err(|e| Error::cannot_run(format!("`targo metadata`: {e}")))?;
    if !ran.success() {
        return Err(Error::refused(format!(
            "`targo metadata` failed in {}:\n{}",
            checkout.display(),
            tail(&ran.stderr, 5)
        )));
    }
    Ok(ran.stdout)
}

/// `targo metadata --no-deps --format-version 1` of `checkout`, as the toolchain a pack of it
/// would build with prints it: what [`parse_metadata`] and [`super::qualify::parse`] read. The
/// toolchain is leased for the read.
/// # Errors
/// No resolvable toolchain, a `targo metadata` that could not run, or its refusal.
pub fn metadata_json(ctx: &Ctx, tools: &dyn Tools, checkout: &Path) -> Result<Vec<u8>> {
    let tc = toolchain::resolve(ctx, None)?;
    let _lease = toolchain::lease(ctx, &tc.bin, "atpkg-publish qualify (metadata)")?;
    metadata_bytes(tools, &tc, checkout)
}

/// Remove ambient compiler-policy and routing replacements from both config
/// discovery and compilation, so they observe the same repository inputs.
fn build_env_remove(triple: Triple) -> Vec<String> {
    let mut removed: Vec<_> = SCRUBBED_ENV
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    let target = triple.as_str().replace('-', "_").to_ascii_uppercase();
    for prefix in ["TARGET", "HOST"] {
        for suffix in ["RUSTFLAGS", "RUSTDOCFLAGS"] {
            removed.push(format!("CARGO_{prefix}_{target}_{suffix}"));
        }
    }
    removed.extend([
        "CARGO_HOST_RUSTFLAGS".into(),
        "CARGO_HOST_RUSTDOCFLAGS".into(),
    ]);
    removed
}

/// Keep the compiler defaults previously selected by an empty cross override
/// explicit when repository flags are preserved on a native build.
fn native_cpu_floor(triple: Triple) -> &'static str {
    cpu_floor(triple).unwrap_or(match triple.arch() {
        crate::triple::Arch::Aarch64 => "generic",
        crate::triple::Arch::X86_64 => "x86-64",
    })
}

fn nonempty_flags(value: &aterm_json::Value) -> Result<bool> {
    if let Some(values) = value.as_array() {
        if values.iter().all(|v| v.as_str().is_some()) {
            return Ok(!values.is_empty());
        }
    } else if let Some(text) = value.as_str() {
        return Ok(!text.trim().is_empty());
    }
    Err(Error::refused(
        "Cargo rustflags must be strings or string arrays",
    ))
}

/// Array overrides carry only the suffix: replaying the resolved array would
/// duplicate Cargo's existing merged arguments. A scalar must instead replace
/// its old scalar with the same text plus the floor.
fn floor_config(key: &str, original: Option<&aterm_json::Value>, cpu: &str) -> Result<OsString> {
    let value = match original.and_then(aterm_json::Value::as_str) {
        Some(text) => aterm_json::to_string(&format!("{text} -C target-cpu={cpu}")),
        None => {
            if let Some(value) = original {
                nonempty_flags(value)?;
            }
            aterm_json::to_string(&["-C", &format!("target-cpu={cpu}")])
        }
    }
    .map_err(|_| Error::refused("could not encode the Cargo CPU floor"))?;
    Ok(format!("{key}={value}").into())
}

fn native_floor_args(
    tools: &dyn Tools,
    tc: &toolchain::Toolchain,
    request: &PackRequest,
    meta: &Metadata,
) -> Result<Vec<OsString>> {
    let mut args: Vec<OsString> = [
        "-Z",
        "unstable-options",
        "config",
        "get",
        "--format",
        "json",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    let mut declared = meta.build_args.iter();
    while let Some(arg) = declared.next() {
        if arg == "--" {
            break;
        }
        if arg == "--target" {
            if declared.next().map(String::as_str) != Some(request.triple.as_str()) {
                return Err(Error::refused(
                    "native pack metadata selects a different target",
                ));
            }
        } else if let Some(target) = arg.strip_prefix("--target=") {
            if target != request.triple.as_str() {
                return Err(Error::refused(
                    "native pack metadata selects a different target",
                ));
            }
        } else if matches!(arg.as_str(), "--config" | "-Z" | "-C") {
            let value = declared
                .next()
                .ok_or_else(|| Error::refused("missing Cargo configuration argument"))?;
            args.extend([arg.into(), value.into()]);
        } else if arg.starts_with("--config=") || arg.starts_with("-Z") || arg.starts_with("-C") {
            args.push(arg.into());
        }
    }
    let mut query = Run::new(tc.targo(), args);
    query.cwd = Some(request.checkout.clone());
    query.env_remove = build_env_remove(request.triple);
    query.env.extend(
        request
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.into())),
    );
    query.env.push(("RUSTC".into(), tc.trustc().into()));
    // A bounded build bounds the configuration read it starts with.
    query.deadline = request.build_within;
    let ran = tools
        .run(&query)
        .map_err(|e| Error::cannot_run(format!("read Cargo configuration: {e}")))?;
    if !ran.success() {
        return Err(Error::refused(format!(
            "could not resolve native Cargo policy (exit {:?})",
            ran.code
        )));
    }
    // The complete configuration may contain unrelated credentials. Keep it
    // only in memory; never include it in diagnostics, receipts or argv.
    let config: aterm_json::Value = aterm_json::from_slice(&ran.stdout)
        .map_err(|_| Error::refused("Cargo configuration did not return JSON"))?;
    if config.as_object().is_none() {
        return Err(Error::refused("Cargo configuration did not return a table"));
    }
    if let Some(target) = config.get("build").and_then(|v| v.get("target")) {
        let matches = target.as_str().map_or_else(
            || {
                target.as_array().is_some_and(|targets| {
                    !targets.is_empty()
                        && targets
                            .iter()
                            .all(|t| t.as_str() == Some(request.triple.as_str()))
                })
            },
            |target| target == request.triple.as_str(),
        );
        if !matches {
            return Err(Error::refused(
                "native pack build.target selects a different target",
            ));
        }
    }
    let cpu = native_cpu_floor(request.triple);
    let mut overrides = Vec::new();
    if let Some(targets) = config.get("target").and_then(aterm_json::Value::as_object) {
        for (target, table) in targets {
            if let Some(flags) = table.get("rustflags") {
                // Turning an empty target segment into a nonempty one would
                // hide build.rustflags, which Cargo uses only as a fallback.
                if nonempty_flags(flags)? {
                    let quoted = aterm_json::to_string(target)
                        .map_err(|_| Error::refused("could not encode Cargo target"))?;
                    overrides.push(floor_config(
                        &format!("target.{quoted}.rustflags"),
                        Some(flags),
                        cpu,
                    )?);
                }
            }
        }
    }
    overrides.push(floor_config(
        "build.rustflags",
        config.get("build").and_then(|v| v.get("rustflags")),
        cpu,
    )?);
    let host = config.get("host");
    // Cargo selects an existing specific host table as a whole, even when it
    // only declares a linker. Do not invent one and shadow default host policy.
    let (key, flags) = match host.and_then(|h| h.get(request.triple.as_str())) {
        Some(specific) => {
            let quoted = aterm_json::to_string(request.triple.as_str())
                .map_err(|_| Error::refused("could not encode Cargo host"))?;
            (
                format!("host.{quoted}.rustflags"),
                specific.get("rustflags"),
            )
        }
        None => (
            "host.rustflags".into(),
            host.and_then(|h| h.get("rustflags")),
        ),
    };
    overrides.push(floor_config(&key, flags, cpu)?);
    Ok(overrides
        .into_iter()
        .flat_map(|value| ["--config".into(), value])
        .collect())
}

/// Build with the admitted compiler and distribution floor. Native builds keep
/// repository flags: append floor suffixes to Cargo's resolved target, fallback
/// and selected host policy. An explicit native target or RUSTFLAGS replaces it.
/// Cross builds retain their explicit target and existing flag policy.
fn build(
    tools: &dyn Tools,
    tc: &toolchain::Toolchain,
    request: &PackRequest,
    meta: &Metadata,
    stage_dir: &Path,
    harness_version: Option<&str>,
    compiler_host: &str,
) -> Result<Vec<PathBuf>> {
    let native = compiler_host == request.triple.as_str();
    let mut args: Vec<OsString> = vec!["--unverified".into(), "build".into(), "--release".into()];
    if !native {
        args.extend(["--target".into(), request.triple.as_str().into()]);
    }
    args.extend(meta.build_args.iter().map(OsString::from));
    if native {
        args.extend(native_floor_args(tools, tc, request, meta)?);
    }
    args.push("--message-format=json".into());
    let mut run = Run::new(tc.targo(), args);
    run.cwd = Some(request.checkout.clone());
    run.env_remove = build_env_remove(request.triple);
    run.env.extend(
        request
            .env
            .iter()
            .map(|(k, v)| (k.clone(), OsString::from(v))),
    );
    run.env.push(("RUSTC".into(), tc.trustc().into()));
    if !native {
        run.env
            .push(("RUSTFLAGS".into(), rustflags(request.triple).into()));
    }
    if let Some(v) = harness_version {
        run.env.push((HARNESS_VERSION_ENV.into(), v.into()));
    }
    let ran = match request.build_within {
        None => tools
            .run(&run)
            .map_err(|e| Error::cannot_run(format!("the build: {e}")))?,
        Some(within) => {
            // A bounded collector keeps a stream's tail, and the build's JSON messages name
            // every executable: the bounded build is the one whose stdout must be complete.
            run.deadline = Some(within);
            match tools.run_complete(&run, aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES) {
                Ok(ran) => ran,
                Err(RunError::TimedOut) => {
                    return Err(Error::refused(format!(
                        "the build of {} did not finish within its bound of {} s and was ended",
                        request.program,
                        within.as_secs()
                    )));
                }
                Err(e @ RunError::Spawn(_)) => {
                    return Err(Error::cannot_run(format!("the build: {e}")));
                }
                // The build ran and left no usable result (its messages were cut short or
                // outgrew what a pack reads): a defect of this build, not of the machine.
                Err(e @ RunError::AfterStart(_)) => {
                    return Err(Error::refused(format!(
                        "the build of {}: {e}",
                        request.program
                    )));
                }
            }
        }
    };
    if !ran.success() {
        return Err(Error::refused(
            diagnostics::Failure::capture(&run, &ran).refusal(stage_dir),
        ));
    }
    let log = stage_dir.join("build.stderr");
    let _ = std::fs::remove_file(&log);
    {
        use std::io::Write as _;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let _ = opts.open(&log).and_then(|mut f| f.write_all(&ran.stderr));
    }
    let exes = built_executables(&ran.stdout, meta);
    if exes.is_empty() {
        return Err(Error::refused(format!(
            "the build of {} produced no workspace bin",
            request.program
        )));
    }
    Ok(exes)
}

/// The stage `out/<program>` is rebuilt from scratch, so it must not hold the checkout the pack builds (`--out` set to the checkout's parent would delete it).
/// # Errors
/// A refusal when it does.
pub fn check_stage_dir(ctx: &Ctx, stage_dir: &Path, checkout: &Path) -> Result<()> {
    let checkout = std::fs::canonicalize(checkout)
        .map_err(|e| Error::io(format!("resolve {}", ctx.shown(checkout)), &e))?;
    let parent = stage_dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|e| Error::io(format!("mkdir {}", ctx.shown(parent)), &e))?;
    let parent = std::fs::canonicalize(parent)
        .map_err(|e| Error::io(format!("resolve {}", ctx.shown(parent)), &e))?;
    let stage = stage_dir
        .file_name()
        .map_or(parent.clone(), |n| parent.join(n));
    if checkout.starts_with(&stage) {
        return Err(Error::refused(format!(
            "the stage {} would be rebuilt from scratch, and it holds the checkout {} —              choose an --out that does not contain the checkout",
            ctx.shown(&stage),
            ctx.shown(&checkout)
        )));
    }
    Ok(())
}

fn reset_dir(dir: &Path) -> Result<()> {
    if std::fs::symlink_metadata(dir).is_ok() {
        std::fs::remove_dir_all(dir).map_err(|e| Error::io(format!("rm {}", dir.display()), &e))?;
    }
    std::fs::create_dir_all(dir).map_err(|e| Error::io(format!("mkdir {}", dir.display()), &e))
}

/// The policy note every pack prints: why the receipt is not enforced, or why it is required when nothing could read the switch (and what to fetch where).
pub fn say_switch(ctx: &Ctx, trust_repo: &Path, switch: &Switch) {
    match switch {
        Switch::Dormant => eprintln!(
            "atpkg-publish: lineage receipt NOT ENFORCED — {}'s origin main carries no minted \
             re-pin yet (its seed ledger has no [release.mint]); the compiler is still \
             admitted by content and every payload is still scanned, and a receipt that is \
             present is checked in full",
            ctx.shown(trust_repo)
        ),
        Switch::Unknown(why) => eprintln!(
            "atpkg-publish: lineage receipt REQUIRED — nothing shows the receipt switch is \
             still dormant: {why}"
        ),
        Switch::Live => {}
    }
}

/// The scan of a staged payload against the admitted compiler; its record is the row's provenance.
/// # Errors
/// A refusal listing every finding: nothing is tarred, hashed or signed then.
pub fn scan(
    stage_root: &Path,
    compiler: &atpkg::lineage::Compiler,
) -> Result<atpkg::manifest::Provenance> {
    let report = super::lineage::scan_tree(stage_root, &compiler.content.version.commit)
        .map_err(|e| Error::cannot_run(format!("the payload scan: {e}")))?;
    if !report.findings.is_empty() {
        return Err(Error::refused(format!(
            "REFUSING to sign — the staged payload carries another compiler's evidence \
             (these bytes were not built by the admitted compiler {}):\n  {}",
            compiler.content.version.commit,
            report
                .findings
                .iter()
                .map(atpkg::lineage::Finding::describe)
                .collect::<Vec<_>>()
                .join("\n  ")
        )));
    }
    Ok(pkgdoc::provenance(compiler, report.objects))
}

/// What the signing half of a pack holds from its start: the key and the roster generation every signature is verified under ([`sign::elected`], once per run).
pub struct Signer {
    key: MachineKey,
    authority: std::sync::Arc<sign::Authority>,
}

impl Signer {
    /// Load the machine key and elect the roster.
    /// # Errors
    /// No key, or no roster that verifies.
    pub fn load(ctx: &Ctx) -> Result<Self> {
        Ok(Self {
            key: MachineKey::load(ctx, &ctx.identity.key)?,
            authority: sign::elected(ctx)?,
        })
    }

    /// The roster generation its signatures are verified under.
    #[must_use]
    pub fn authority(&self) -> &sign::Authority {
        &self.authority
    }
}

/// Sign the manifest's exact bytes when a signer is given (re-asking the switch for an `unproven` row first), verify the pair and hold the parsed document to what was packed, and only then write the manifest and its signature.
/// # Errors
/// The unproven gate's, the signer's or the self-check's refusal; neither file is written after one, and a signature an earlier run left is removed (it covers other bytes).
pub fn write_manifest(
    ctx: &Ctx,
    doc: &pkgdoc::Doc,
    header: &Header,
    row: &Row,
    pkg: &Path,
    signer: Option<&Signer>,
) -> Result<Option<PathBuf>> {
    if pkgdoc::lineage_of(&row.provenance)? == crate::api::Lineage::Unproven {
        sign::unproven_gate(ctx, "this pack's provenance")?;
    }
    let _ = std::fs::remove_file(sign::sig_path(pkg));
    let sig = match signer {
        Some(s) => {
            let (sig, parsed) = sign::sign_checked(&s.key, doc.bytes(), &s.authority)?;
            pkgdoc::check_parsed(&parsed, header, row)?;
            Some(sig)
        }
        None => None,
    };
    crate::lane::report::write_atomic_bytes(pkg, doc.bytes())?;
    let Some(sig) = sig else {
        eprintln!(
            "atpkg-publish: wrote {} UNSIGNED — no client accepts it until a rostered \
             machine runs `atpkg-publish sign` over these exact bytes",
            ctx.shown(pkg)
        );
        return Ok(None);
    };
    sign::write_sig(pkg, &sig).map(Some)
}

/// Sign a bottle manifest's exact bytes when a signer is given, verify the pair and hold the parsed document to what was packed, and only then write the manifest and its signature.
/// # Errors
/// The signer's or the self-check's refusal; neither file is written after one, and a signature an earlier run left is removed (it covers other bytes).
pub fn write_bottle_manifest(
    ctx: &Ctx,
    doc: &pkgdoc::Doc,
    header: &Header,
    row: &pkgdoc::BottleRow,
    pkg: &Path,
    signer: Option<&Signer>,
) -> Result<Option<PathBuf>> {
    let _ = std::fs::remove_file(sign::sig_path(pkg));
    let sig = match signer {
        Some(s) => {
            let (sig, parsed) = sign::sign_checked(&s.key, doc.bytes(), &s.authority)?;
            pkgdoc::check_parsed_bottle(&parsed, header, row)?;
            Some(sig)
        }
        None => None,
    };
    crate::lane::report::write_atomic_bytes(pkg, doc.bytes())?;
    let Some(sig) = sig else {
        eprintln!(
            "atpkg-publish: wrote {} UNSIGNED — no client accepts it until a rostered \
             machine runs `atpkg-publish sign` over these exact bytes",
            ctx.shown(pkg)
        );
        return Ok(None);
    };
    sign::write_sig(pkg, &sig).map(Some)
}

/// [`Pack`] over explicit tools — the seam the tests drive.
/// # Errors
/// Any gate's refusal; nothing is signed or uploaded after one.
pub fn pack_with(ctx: &Ctx, request: &PackRequest, tools: &dyn Tools) -> Result<PackOutcome> {
    let program = &request.program;
    let def = programs::get(program)
        .ok_or_else(|| Error::usage(format!("`{program}` is not a program the channel pins")))?;
    if !matches!(def.shape, Shape::Plain { .. }) {
        return Err(Error::usage(format!(
            "{program} is not a plain program: a sysroot bundle packs with `pack-bundle`"
        )));
    }
    if request.signing == Signing::Unsigned && request.upload {
        return Err(Error::usage(
            "--unsigned and --upload together make no sense — nothing unsigned may publish",
        ));
    }
    check_build_env(&request.env)?;
    let base = request
        .merge_from
        .as_ref()
        .map(|p| {
            crate::identity::read_regular_bytes(p, crate::identity::MAX_META_BYTES)
                .map_err(|e| Error::io(format!("read {}", ctx.shown(p)), &e))
        })
        .transpose()?;

    let tc = toolchain::resolve(ctx, request.toolchain.as_deref())?;
    let _lease = toolchain::lease(ctx, &tc.bin, &format!("atpkg-publish pack {program}"))?;
    let trust_repo = lineage_switch::default_trust_repo(ctx);
    let switch = lineage_switch::read(ctx, &trust_repo)?;
    say_switch(ctx, &trust_repo, &switch);
    let compiler = toolchain::admit_with_components(
        tools,
        &tc.bin,
        &switch,
        ctx.frontend_component.as_deref(),
        ctx.targo_component.as_deref(),
    )?;
    let signer = match request.signing {
        Signing::MachineKey => Some(Signer::load(ctx)?),
        Signing::Unsigned => None,
    };
    if !request.checkout.is_dir() {
        return Err(Error::refused(format!(
            "the checkout {} is not a directory",
            ctx.shown(&request.checkout)
        )));
    }
    let build_number: Build = match request.build {
        Some(b) => b,
        None => crate::git::rev_count(&request.checkout, "HEAD")?,
    };
    let _lock = pack_lock(ctx, program, LOCK_WAIT)?;
    let stage_dir = request.out.join(program);
    check_stage_dir(ctx, &stage_dir, &request.checkout)?;
    reset_dir(&stage_dir)?;
    let stage_root = stage_dir.join("root");
    std::fs::create_dir_all(stage_root.join("bin"))
        .map_err(|e| Error::io(format!("mkdir {}", stage_root.display()), &e))?;
    eprintln!(
        "atpkg-publish: {program} build={build_number} triple={} — building with {} \
         --unverified (RUSTC={})",
        request.triple,
        ctx.shown(&tc.targo()),
        ctx.shown(&tc.trustc())
    );

    let meta = metadata(tools, &tc, &request.checkout, program)?;
    if meta.bundle_only {
        return Err(Error::refused(format!(
            "{program} declares [workspace.metadata.atpkg] bundle-only — it ships a SYSROOT, \
             not an exposes set of standalone bins; packing it here would publish whatever \
             its `expose` names under the program's name and delete the real shims from every \
             updating user's PATH. Use `atpkg-publish pack-bundle {program}`"
        )));
    }
    let (meta, version, harness_id) = match def.harness() {
        Some(h) => {
            let (meta, identity) = harness_identity(
                ctx.hosts.staging.as_ref(),
                program,
                h,
                meta,
                &request.checkout,
                Some(build_number),
                (&ctx.accounts.staging, def.repo),
            )?;
            let version = identity.version.clone();
            (meta, version, Some(identity))
        }
        None => {
            let version = meta.version.clone();
            (meta, version, None)
        }
    };
    pkgdoc::check_exposes(program, &meta.expose)?;
    let harness_version = harness_id.as_ref().map(|id| id.version.as_str());
    let exes = build(
        tools,
        &tc,
        request,
        &meta,
        &stage_dir,
        harness_version,
        &compiler.content.version.host,
    )?;
    if let Some(identity) = &harness_id {
        identity.recheck(&request.checkout)?;
    }
    let gated = arch::gate(request.triple, &exes)?;
    eprintln!(
        "atpkg-publish: arch gate OK — {gated} binary(ies) are {} as claimed ({})",
        arch::expect(request.triple).0,
        request.triple
    );
    let sel = select(&meta, &exes, request.triple)?;
    pkgdoc::check_exposes(program, &sel.exposes)?;
    if sel.mode == ExposeMode::Heuristic {
        eprintln!(
            "atpkg-publish: WARNING — {program} has no [workspace.metadata.atpkg] expose list; \
             using the UNCURATED name-token fallback (add the declaration to fix)"
        );
    }
    for (exe, name) in &sel.shipped {
        let dest = stage_root.join("bin").join(name);
        crate::identity::copy_regular_file(exe, &dest)
            .map_err(|e| Error::io(format!("stage {name}"), &e))?;
        let _ = tools.strip(&dest, false);
    }

    let provenance = scan(&stage_root, &compiler)?;
    let asset = stage_dir.join(pkgdoc::asset_name(program, build_number, request.triple));
    let packed = tarwrite::write_with(&stage_root, &asset, tools.compressor())?;
    let header = Header {
        program: program.clone(),
        version,
        build: build_number,
        exposes: sel.exposes.clone(),
        shim_env: Vec::new(),
    };
    let row = Row {
        triple: request.triple,
        kind: Kind::Binary,
        asset: pkgdoc::asset_name(program, build_number, request.triple),
        sha256: packed.sha256.clone(),
        tree_root: packed.tree_root.clone(),
        size: packed.size,
        disk_installed: packed.disk_installed,
        provenance,
    };
    let doc = match base {
        Some(bytes) => pkgdoc::merge(bytes, &header, &row)?,
        None => pkgdoc::fresh(&header, &row)?,
    };
    if let Some(identity) = &harness_id {
        identity.recheck(&request.checkout)?;
    }
    let pkg = stage_dir.join(pkgdoc::manifest_name(program, build_number));
    let signature = write_manifest(ctx, &doc, &header, &row, &pkg, signer.as_ref())?;

    let mut uploaded = false;
    if request.upload {
        let sig = signature
            .clone()
            .ok_or_else(|| Error::refused("nothing unsigned may publish"))?;
        upload::preflight(&asset, &row.sha256, &[pkg.clone(), sig.clone()])?;
        let notes = format!(
            "atpkg pack of {program} ({}, {}). Signed pkg manifest + tarball.",
            header.version, request.triple
        );
        uploaded = upload::publish(
            ctx,
            def.repo,
            &upload::tag(program, build_number),
            &pkg,
            &[asset.clone(), pkg.clone(), sig],
            &notes,
        )? == upload::Uploaded::Staged;
    }
    Ok(PackOutcome {
        program: program.clone(),
        build: build_number,
        version: header.version,
        triple: request.triple,
        asset,
        manifest: pkg,
        signature,
        exposes: sel.exposes,
        lineage: pkgdoc::lineage_of(&row.provenance)?,
        uploaded,
    })
}

/// `atpkg-publish pack <program> --checkout DIR [--triple T] [--build N] [--merge-from PKG] [--toolchain BIN] [--out DIR] [--trust-repo DIR] [--unsigned] [--upload]`.
/// # Errors
/// A usage error for a malformed command line; the pack's refusals.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let mut ctx = ctx.clone();
    let request = parse_args(&mut ctx, args, &HostTools)?;
    let ctx = &ctx;
    if request.upload {
        super::upload::program_preflight(ctx, &request.program)?;
    }
    let outcome = Pack.pack(ctx, &request)?;
    println!(
        "atpkg-publish: packed {}@{} for {} → {}{}{}",
        outcome.program,
        outcome.build,
        outcome.triple,
        ctx.shown(&outcome.manifest),
        if outcome.signature.is_some() {
            " (signed)"
        } else {
            " (UNSIGNED)"
        },
        if outcome.uploaded {
            ", uploaded to staging"
        } else {
            ""
        }
    );
    Ok(())
}

/// The command line as a [`PackRequest`]; `--trust-repo` moves where `ctx` reads the receipt switch.
/// # Errors
/// A usage error naming what is wrong; the toolchain's refusal when the triple must be read from it.
pub fn parse_args(ctx: &mut Ctx, args: &[String], tools: &dyn Tools) -> Result<PackRequest> {
    let usage = || {
        Error::usage(
            "pack <program> --checkout <dir> [--triple T] [--build N] [--merge-from PKG] \
             [--toolchain BIN] [--out DIR] [--trust-repo DIR] [--unsigned] [--upload]",
        )
    };
    let mut program = None;
    let mut checkout = None;
    let mut triple = None;
    let mut build = None;
    let mut merge_from = None;
    let mut toolchain = None;
    let mut out = None;
    let mut signing = Signing::MachineKey;
    let mut upload = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(usage);
        match a.as_str() {
            "--checkout" => checkout = Some(PathBuf::from(value()?)),
            "--triple" => triple = Some(Triple::parse(&value()?)?),
            "--build" => {
                let v = value()?;
                build = Some(
                    (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                        .then(|| v.parse::<Build>().ok())
                        .flatten()
                        .ok_or_else(|| Error::usage("--build takes a decimal build number"))?,
                );
            }
            "--merge-from" => merge_from = Some(PathBuf::from(value()?)),
            "--toolchain" => toolchain = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--trust-repo" => ctx.trust_repo = PathBuf::from(value()?),
            "--unsigned" => signing = Signing::Unsigned,
            "--upload" => upload = true,
            s if s.starts_with('-') => {
                return Err(Error::usage(format!("pack: unknown flag `{s}`")));
            }
            s if program.is_none() => program = Some(s.to_string()),
            _ => return Err(usage()),
        }
    }
    let program = program.ok_or_else(usage)?;
    let checkout = checkout.ok_or_else(usage)?;
    let triple = match triple {
        Some(t) => t,
        None => {
            let tc = toolchain::resolve(ctx, toolchain.as_deref())?;
            toolchain::host_of(tools, &tc.trustc())?
        }
    };
    let out = out.unwrap_or_else(|| checkout.join("target/atpkg-pack"));
    Ok(PackRequest {
        program,
        checkout,
        triple,
        build,
        merge_from,
        toolchain,
        out,
        signing,
        upload,
        env: Vec::new(),
        build_within: None,
    })
}
