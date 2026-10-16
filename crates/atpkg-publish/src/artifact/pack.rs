// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish pack <program>`: one plain program build, for one target.
//!
//! The pipeline, each step a gate that refuses before anything later runs:
//!
//! 1. the Trust toolchain: resolved once to a physical build dir, leased for the whole
//!    pack, and admitted by content and lineage ([`super::toolchain`]) before anything is
//!    built;
//! 2. `targo metadata` → the program's `[workspace.metadata.atpkg]` `expose` / `bundle`
//!    / `build-args`; a `bundle-only` program is refused (that lane is
//!    [`super::bundle`]); no declaration → the loud name-token fallback;
//! 3. `targo --unverified build --release --target <triple>` with the distribution
//!    CPU floor (`x86-64-v2` / `apple-m1`, never `target-cpu=native`) and the seal's own
//!    `trustc`, set after a caller's cross linker, with every inherited variable that
//!    outranks them removed; every declared bin must have built;
//! 4. the arch gate over every binary ([`super::arch`]);
//! 5. stage `bin/` (stripped);
//! 6. the payload lineage scan (`atpkg::lineage::scan_tree`), whose record is the row's
//!    `[artifact.provenance]`;
//! 7. tar in-process, zstd by the CLI, read back through the client decoder
//!    ([`super::tarwrite`]), `disk_installed`;
//! 8. the manifest ([`super::pkgdoc`]): fresh, or this target's row appended to
//!    `merge_from`'s bytes verbatim; signed with the machine key unless staged unsigned (an
//!    `unproven` row is signed or staged only while the receipt switch reads dormant), the
//!    pair verified with the client's rule and parsed back against what was packed before
//!    either file is written;
//! 9. upload to the staging owner (refused when the public owner already serves this tag
//!    with different bytes: a public pack is immutable).

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::lineage_switch::{self, Switch};
use super::pkgdoc::{self, Header, Kind, Row};
use super::tools::{HostTools, Run, Tools};
use super::{arch, sign, tarwrite, toolchain, upload};
use crate::api::{Build, PackOutcome, PackRequest, Packer, Signing};
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::identity::MachineKey;
use crate::programs::{self, Shape};
use crate::triple::{Os, Triple};

/// The plain-program packer.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pack;

impl Packer for Pack {
    /// Every pack is compressed by the zstd CLI ([`tarwrite::COMPRESSOR`]).
    fn can_pack(&self) -> std::result::Result<(), String> {
        tarwrite::compressor_on_path().map(|_| ())
    }

    fn pack(&self, ctx: &Ctx, request: &PackRequest) -> Result<PackOutcome> {
        pack_with(ctx, request, &HostTools)
    }
}

/// How long a pack waits for another publish of the same program to finish.
pub const LOCK_WAIT: Duration = Duration::from_secs(300);

/// The lock every pack of `program` holds for its whole run: its stage is rebuilt from
/// scratch and its release tag mutated, and two runs of one program often use different
/// `out` dirs, so the lock is keyed on the program.
///
/// # Errors
///
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

/// The distribution CPU floor a target is built for: the oldest CPU any supported OS
/// runs on, never the builder's (`target-cpu=native` faults on an older user's machine,
/// and cross-built it is refused outright). `None` where the compiler's default for the
/// triple already is the floor.
#[must_use]
pub fn cpu_floor(triple: Triple) -> Option<&'static str> {
    match (triple.os(), triple.arch()) {
        // macOS 11 runs on Ivy Bridge (no AVX2): x86-64-v2, never v3. Linux has no OS
        // floor to reason from, so the silicon's SSE4.2/POPCNT era is the floor there too.
        (Os::Darwin | Os::Linux, crate::triple::Arch::X86_64) => Some("x86-64-v2"),
        // The oldest Apple Silicon there is.
        (Os::Darwin, crate::triple::Arch::Aarch64) => Some("apple-m1"),
        // aarch64 Linux/Windows: the default is armv8-a; x86_64 Windows: the default
        // x86-64 baseline.
        _ => None,
    }
}

/// The `RUSTFLAGS` a pack's build sets — always set, so a repo's own target rustflags
/// (its `target-cpu=native`) are replaced, never inherited.
#[must_use]
pub fn rustflags(triple: Triple) -> String {
    cpu_floor(triple).map_or_else(String::new, |cpu| format!("-C target-cpu={cpu}"))
}

/// Inherited variables a pack's build removes: each outranks, wraps or reroutes the
/// `RUSTC` / `RUSTFLAGS` the pack sets (`CARGO_ENCODED_RUSTFLAGS` beats `RUSTFLAGS`, a
/// wrapper runs whatever it likes, `RUSTUP_TOOLCHAIN` reroutes a proxy).
pub const SCRUBBED_ENV: &[&str] = &[
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_BUILD_RUSTFLAGS",
    "CARGO_BUILD_RUSTC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    "RUSTUP_TOOLCHAIN",
];

/// The one thing a caller may add to a pack's build environment: a cross linker,
/// `CARGO_TARGET_<TRIPLE>_LINKER`. Anything else could replace the seal's compiler or
/// the distribution CPU floor, which the pack alone decides.
///
/// # Errors
///
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
    /// `[workspace.metadata.atpkg] expose` (else the members' union): the shimmed tools.
    pub expose: Vec<String>,
    /// `bundle`: shipped, not shimmed.
    pub bundle: Vec<String>,
    /// `build-args`.
    pub build_args: Vec<String>,
    /// `bundle-only`: a sysroot program this lane cannot describe.
    pub bundle_only: bool,
    /// The program's own crate version (else the first member's, else `0.0.0`).
    pub version: String,
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

/// Read `targo metadata --no-deps --format-version 1`'s JSON.
///
/// # Errors
///
/// A refusal when it is not that JSON.
pub fn parse_metadata(json: &[u8], program: &str) -> Result<Metadata> {
    let v: aterm_json::Value = aterm_json::from_slice(json)
        .map_err(|e| Error::refused(format!("`targo metadata` printed no JSON: {e}")))?;
    let packages = v
        .get("packages")
        .and_then(aterm_json::Value::as_array)
        .ok_or_else(|| Error::refused("`targo metadata` lists no packages"))?;
    let ws = v.get("metadata").and_then(|m| m.get("atpkg"));
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
    let bundle_only = ws
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
    let version = packages
        .iter()
        .find(|p| p.get("name").and_then(aterm_json::Value::as_str) == Some(program))
        .and_then(version_of)
        .or_else(|| packages.first().and_then(version_of))
        .unwrap_or("0.0.0")
        .to_string();
    Ok(Metadata {
        manifests: packages
            .iter()
            .filter_map(|p| p.get("manifest_path").and_then(aterm_json::Value::as_str))
            .map(str::to_string)
            .collect(),
        expose: declared("expose"),
        bundle: declared("bundle"),
        build_args: strings(ws.and_then(|a| a.get("build-args"))),
        bundle_only,
        version,
    })
}

/// The workspace members' freshly built bin executables, from a `--message-format=json`
/// stream: sorted, each once.
#[must_use]
pub fn built_executables(stream: &[u8], meta: &Metadata) -> Vec<PathBuf> {
    let mut out: BTreeSet<PathBuf> = BTreeSet::new();
    for line in String::from_utf8_lossy(stream).lines() {
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let is_bin = v
            .get("target")
            .and_then(|t| t.get("kind"))
            .and_then(aterm_json::Value::as_array)
            .is_some_and(|k| k.iter().any(|s| s.as_str() == Some("bin")));
        let member = v
            .get("manifest_path")
            .and_then(aterm_json::Value::as_str)
            .is_some_and(|m| meta.manifests.contains(m));
        if v.get("reason").and_then(aterm_json::Value::as_str) == Some("compiler-artifact")
            && is_bin
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
    let lower = name.to_ascii_lowercase();
    let tokens: Vec<&str> = lower.split(['-', '_']).collect();
    tokens.iter().any(|t| INTERNAL_TOKENS.contains(t))
        || (tokens.len() > 1 && (tokens[0] == "gen" || tokens[tokens.len() - 1] == "gen"))
}

/// Choose what ships and what is shimmed: the declared `expose` ∪ `bundle` (every name of
/// which must have built), else every bin the name-token fallback keeps.
///
/// # Errors
///
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
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Run `targo metadata` in the checkout through the resolved driver (no `--unverified`:
/// targo takes the lane flag on compilation commands only).
fn metadata(
    tools: &dyn Tools,
    tc: &toolchain::Toolchain,
    checkout: &Path,
    program: &str,
) -> Result<Metadata> {
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
    parse_metadata(&ran.stdout, program)
}

/// The build, with the lane flag, the target, the CPU floor and the seal's own compiler.
fn build(
    tools: &dyn Tools,
    tc: &toolchain::Toolchain,
    request: &PackRequest,
    meta: &Metadata,
    stage_dir: &Path,
) -> Result<Vec<PathBuf>> {
    let mut args: Vec<OsString> = vec![
        "--unverified".into(),
        "build".into(),
        "--release".into(),
        "--target".into(),
        request.triple.as_str().into(),
    ];
    args.extend(meta.build_args.iter().map(OsString::from));
    args.push("--message-format=json".into());
    let mut run = Run::new(tc.targo(), args);
    run.cwd = Some(request.checkout.clone());
    run.env_remove = SCRUBBED_ENV.iter().map(|k| (*k).to_string()).collect();
    run.env.extend(
        request
            .env
            .iter()
            .map(|(k, v)| (k.clone(), OsString::from(v))),
    );
    // Last, so nothing the caller passed can stand in for them.
    run.env.push(("RUSTC".into(), tc.trustc().into()));
    run.env
        .push(("RUSTFLAGS".into(), rustflags(request.triple).into()));
    let ran = tools
        .run(&run)
        .map_err(|e| Error::cannot_run(format!("the build: {e}")))?;
    let log = stage_dir.join("build.stderr");
    let _ = std::fs::write(&log, &ran.stderr);
    if !ran.success() {
        return Err(Error::refused(format!(
            "the build FAILED (see {}):\n{}",
            log.display(),
            tail(&ran.stderr, 5)
        )));
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

/// The stage `out/<program>` is rebuilt from scratch, so it must not hold the checkout
/// the pack builds (`--out` set to the checkout's parent would delete it).
///
/// # Errors
///
/// A refusal when it does.
pub fn check_stage_dir(ctx: &Ctx, stage_dir: &Path, checkout: &Path) -> Result<()> {
    let checkout = std::fs::canonicalize(checkout)
        .map_err(|e| Error::io(format!("resolve {}", ctx.shown(checkout)), &e))?;
    let parent = stage_dir.parent().unwrap_or(stage_dir);
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

/// The policy note every pack prints: why the receipt is not enforced, or why it is
/// required when nothing could read the switch (and what to fetch where).
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

/// The scan of a staged payload against the admitted compiler; its record is the row's
/// provenance.
///
/// # Errors
///
/// A refusal listing every finding: nothing is tarred, hashed or signed then.
pub fn scan(
    stage_root: &Path,
    compiler: &atpkg::lineage::Compiler,
) -> Result<atpkg::manifest::Provenance> {
    let report = atpkg::lineage::scan_tree(stage_root, &compiler.content.version.commit)
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

/// What the signing half of a pack holds from its start: the key and the roster generation
/// every signature is verified under ([`sign::authority`]). Loaded before anything is
/// built, so a pack that cannot sign stops in seconds.
pub struct Signer {
    key: MachineKey,
    authority: sign::Authority,
}

impl Signer {
    /// Load the machine key and elect the roster.
    ///
    /// # Errors
    ///
    /// No key, or no roster that verifies.
    pub fn load(ctx: &Ctx) -> Result<Self> {
        Ok(Self {
            key: MachineKey::load(ctx, &ctx.identity.key)?,
            authority: sign::authority(ctx)?,
        })
    }
}

/// Sign the manifest's exact bytes when a signer is given (re-asking the switch for an
/// `unproven` row first), verify the pair and hold the parsed document to what was
/// packed, and only then write the manifest and its signature.
///
/// # Errors
///
/// The unproven gate's, the signer's or the self-check's refusal; neither file is
/// written after one, and a signature an earlier run left is removed (it covers other
/// bytes).
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
    std::fs::write(pkg, doc.bytes())
        .map_err(|e| Error::io(format!("write {}", pkg.display()), &e))?;
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
///
/// # Errors
///
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
    // Read before the stage is reset: the base may live under `out`.
    let base = request
        .merge_from
        .as_ref()
        .map(|p| std::fs::read(p).map_err(|e| Error::io(format!("read {}", ctx.shown(p)), &e)))
        .transpose()?;

    let tc = toolchain::resolve(ctx, request.toolchain.as_deref())?;
    let _lease = toolchain::lease(ctx, &tc.bin, &format!("atpkg-publish pack {program}"))?;
    let trust_repo = lineage_switch::default_trust_repo(ctx);
    let switch = lineage_switch::read(ctx, &trust_repo)?;
    say_switch(ctx, &trust_repo, &switch);
    let compiler = toolchain::admit(tools, &tc.bin, &switch)?;
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
    let exes = build(tools, &tc, request, &meta, &stage_dir)?;
    let gated = arch::gate(request.triple, &exes)?;
    eprintln!(
        "atpkg-publish: arch gate OK — {gated} binary(ies) are {} as claimed ({})",
        arch::expect(request.triple).0,
        request.triple
    );
    let sel = select(&meta, &exes, request.triple)?;
    if sel.mode == ExposeMode::Heuristic {
        eprintln!(
            "atpkg-publish: WARNING — {program} has no [workspace.metadata.atpkg] expose list; \
             using the UNCURATED name-token fallback (add the declaration to fix)"
        );
    }
    for (exe, name) in &sel.shipped {
        let dest = stage_root.join("bin").join(name);
        std::fs::copy(exe, &dest).map_err(|e| Error::io(format!("stage {name}"), &e))?;
        // Best effort: a foreign-target object a host strip cannot read ships unstripped.
        let _ = tools.strip(&dest, false);
    }

    let provenance = scan(&stage_root, &compiler)?;
    let asset = stage_dir.join(pkgdoc::asset_name(program, build_number, request.triple));
    let packed = tarwrite::write(&stage_root, &asset)?;
    let header = Header {
        program: program.clone(),
        version: meta.version.clone(),
        build: build_number,
        exposes: sel.exposes.clone(),
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
    let pkg = stage_dir.join(pkgdoc::file_name(program, build_number));
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

/// `atpkg-publish pack <program> --checkout DIR [--triple T] [--build N] [--merge-from
/// PKG] [--toolchain BIN] [--out DIR] [--trust-repo DIR] [--unsigned] [--upload]`.
///
/// # Errors
///
/// A usage error for a malformed command line; the pack's refusals.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let mut ctx = ctx.clone();
    let request = parse_args(&mut ctx, args, &HostTools)?;
    let ctx = &ctx;
    if request.upload {
        super::upload::token_preflight(ctx)?;
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

/// The command line as a [`PackRequest`]; `--trust-repo` moves where `ctx` reads the
/// receipt switch. With no `--triple` the target is the host the pack's own compiler
/// names (`trustc -vV`), so the label and the compiler agree.
///
/// # Errors
///
/// A usage error naming what is wrong; the toolchain's refusal when the triple must be
/// read from it.
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
                build = Some(
                    value()?
                        .parse::<Build>()
                        .map_err(|_| Error::usage("--build takes a decimal build number"))?,
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
    })
}
