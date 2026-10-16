// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish pack-bundle <program> --sysroot <dir>`: a sysroot bundle (trust,
//! trust-mc), made self-contained at pack time.
//!
//! Its attestation rests on order: every byte mutation precedes relocation, every gate
//! runs over the final bytes, and the signed manifest describes exactly what was gated. The
//! order is a typestate (each phase consumes the previous stage), so a pass out of order
//! does not compile:
//!
//! ```text
//! Admitted   compiler lineage (and --hello-sysroot's) admitted; dirty-tree gate
//!  → Staged    sysroot copied (hard links kept); trust: closure frozen (super::closure)
//!  → Mutated   dev-source links pruned (any other link refused); foreign target
//!              sysroots pruned; fat Mach-O thinned; retired entrypoints stripped;
//!              one compiler (bin/rustc a hard link to bin/trustc); symbols stripped and
//!              re-signed once per hard-link group
//!  → Relocated `atpkg::relocate::relocate_stage` (fail-closed audit; donor roots only)
//!  → Gated     one-compiler gate; LINEAGE stamped; payload scan; arch gate over bin/;
//!              every exposed bin present; per-bin smoke; hello-world; tippy over a
//!              hello crate
//!  → Packed    tar + zstd, disk_installed, manifest (kind = sysroot-bundle,
//!              reloc = self-contained), signature, upload
//! ```
//!
//! Every phase reads the sysroot through the physical path admission resolved and leased,
//! never the request's spelling: a link like `~/.rustup/toolchains/trust` can be repointed
//! mid-pack, and the bytes staged must be the build that was admitted.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::lineage_switch::{self, Switch};
use super::pack::{self, Signer};
use super::pkgdoc::{self, Header, Kind, Row};
use super::tools::{self, HostTools, Run, Tools};
use super::{arch, closure, tarwrite, toolchain, upload};
use crate::api::{BundlePacker, BundleRequest, HelloGate, PackOutcome, Signing};
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::programs::{self, BundleDef, Hello, Shape};
use crate::triple::{Arch, Os, Triple};

/// The sysroot-bundle packer.
#[derive(Clone, Copy, Debug, Default)]
pub struct PackBundle;

impl BundlePacker for PackBundle {
    fn pack_bundle(&self, ctx: &Ctx, request: &BundleRequest) -> Result<PackOutcome> {
        pack_bundle_with(ctx, request, &HostTools)
    }
}

/// Stock-Rust entrypoints Trust's own surface gate forbids (`targo-trust`'s
/// `FORBIDDEN_TRUST_PUBLIC_BIN_NAMES`): stripped from a staged `bin/`, and refused in an
/// exposes list. `cargo`, `rustc` and `rustdoc` are required compatibility aliases and are
/// not here.
pub const RETIRED_BINS: &[&str] = &[
    "cargo-trust",
    "tcargo",
    "tcargo-trust",
    "tcargo-fmt",
    "rustfmt",
    "cargo-fmt",
    "cargo-clippy",
    "clippy-driver",
    "targo-clippy",
    "trust-clippy",
    "trust-clippy-driver",
    "rust-analyzer",
    "miri",
    "cargo-miri",
    "rust-gdb",
    "rust-gdbgui",
    "rust-lldb",
    "rust-windbg.cmd",
];

/// The `libexec/` counterpart of [`RETIRED_BINS`].
pub const RETIRED_LIBEXEC: &[&str] = &["rust-analyzer-proc-macro-srv"];

/// What a bundle's hello-world compiles with, and against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelloPlan {
    /// No hello, on purpose.
    Skip,
    /// The staged compiler against the staged sysroot (a working toolchain).
    Staged,
    /// The packed `compiler` against the Trust sysroot it was built from (trust-mc,
    /// whose own std is the panic=abort verification sysroot no hello links against).
    Trust {
        /// The sysroot.
        sysroot: PathBuf,
        /// The packed compiler, under `bin/`.
        compiler: String,
    },
}

/// The hello plan a program's table row and the request's gate make.
///
/// # Errors
///
/// A usage error for a Trust sysroot named for a bundle that proves its own std, a
/// trust-mc pack that names none, or one that is not an absolute sysroot.
pub fn hello_plan(def: &BundleDef, gate: &HelloGate) -> Result<HelloPlan> {
    match (def.hello, gate.sysroot.as_ref()) {
        (_, None) if !gate.enabled => Ok(HelloPlan::Skip),
        (Hello::StagedSysroot, None) => Ok(HelloPlan::Staged),
        (Hello::StagedSysroot, Some(_)) => Err(Error::usage(
            "--hello-sysroot is for a bundle whose own std does not link (trust-mc): this \
             bundle's hello proves its own staged sysroot",
        )),
        (Hello::TrustSysroot { .. }, None) => Err(Error::usage(
            "this bundle's hello compiles against the Trust sysroot its compiler was built \
             from: name it with --hello-sysroot (or skip the hello on purpose)",
        )),
        (Hello::TrustSysroot { compiler }, Some(sysroot)) => {
            if !sysroot.is_absolute() || !sysroot.join("lib/rustlib").is_dir() {
                return Err(Error::usage(format!(
                    "--hello-sysroot {} is not an absolute sysroot (no lib/rustlib/)",
                    sysroot.display()
                )));
            }
            Ok(HelloPlan::Trust {
                sysroot: sysroot.clone(),
                compiler: compiler.to_string(),
            })
        }
    }
}

/// The packaged version, derived from what is packed: the source checkout's
/// `release/trust-version.toml` `trust_product_version`, else the staged trustc's `-vV`
/// `trust:` line (never `release:`, the Rust compatibility number), else
/// `trust-mc-driver --version`'s last word.
///
/// # Errors
///
/// A refusal when nothing names one: a signed manifest is the wrong place to guess.
pub fn derive_version(tools: &dyn Tools, src_repo: &Path, sysroot: &Path) -> Result<String> {
    if let Ok(text) = std::fs::read_to_string(src_repo.join("release/trust-version.toml"))
        && let Some(v) = text.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == "trust_product_version")
                .then(|| v.trim().trim_matches('"').to_string())
                .filter(|v| !v.is_empty())
        })
    {
        return Ok(v);
    }
    let trustc = sysroot.join("bin/trustc");
    if trustc.is_file()
        && let Ok(text) = tools::version(tools, &trustc)
        && let Some(v) = text.lines().find_map(|l| l.strip_prefix("trust: "))
    {
        return Ok(v.trim().to_string());
    }
    let driver = sysroot.join("bin/trust-mc-driver");
    if driver.is_file()
        && let Ok(ran) = tools.run(&Run::new(&driver, ["--version"]).bounded())
        && ran.success()
        && let Some(v) = String::from_utf8_lossy(&ran.stdout)
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().last().map(str::to_string))
    {
        return Ok(v);
    }
    Err(Error::refused(format!(
        "cannot determine the version — expected trust_product_version in {}, a staged \
         bin/trustc answering -vV with a `trust:` line, or a bin/trust-mc-driver --version; \
         or name the version the program itself declares (--version)",
        src_repo.join("release/trust-version.toml").display()
    )))
}

/// Everything a bundle pack carries from its admission to its manifest.
struct Bundle<'a> {
    ctx: &'a Ctx,
    req: &'a BundleRequest,
    tools: &'a dyn Tools,
    def: BundleDef,
    repo: &'static str,
    /// The sysroot admitted and leased, resolved physically: every later phase reads
    /// this, never `req.sysroot`.
    sysroot: PathBuf,
    triple: Triple,
    build: u64,
    version: String,
    switch: Switch,
    /// Whether the bundle ships (and so is admitted by) its own bin/trustc.
    own_compiler: bool,
    proven: bool,
    hello: HelloPlan,
    base: Option<Vec<u8>>,
    signer: Option<Signer>,
    _lease: toolchain::Held,
}

/// Phase 1: everything that can be decided before a byte is copied, decided.
pub struct Admitted<'a>(Bundle<'a>);
/// Phase 2: the sysroot copied (and trust's closure frozen) into a stage of our own.
pub struct Staged<'a> {
    b: Bundle<'a>,
    stage: aterm_tempfile::TempDir,
}
/// Phase 3: every byte mutation done.
pub struct Mutated<'a> {
    b: Bundle<'a>,
    stage: aterm_tempfile::TempDir,
}
/// Phase 4: relocated, self-contained by the relocator's fail-closed audit.
pub struct Relocated<'a> {
    b: Bundle<'a>,
    stage: aterm_tempfile::TempDir,
}
/// Phase 5: every gate passed over the final bytes; the scan's provenance in hand.
pub struct Gated<'a> {
    b: Bundle<'a>,
    stage: aterm_tempfile::TempDir,
    provenance: atpkg::manifest::Provenance,
}

fn root_of(stage: &aterm_tempfile::TempDir) -> PathBuf {
    stage.path().join("root")
}

fn home_of(stage: &aterm_tempfile::TempDir) -> PathBuf {
    stage.path().join("smoke-home")
}

/// The packed compiler a bundle names its triple and its hello by: the program's own
/// (trust-mc's `bin/trust-mc-compiler`), else `bin/rustc`, else `bin/trustc` — the first
/// that is there.
fn packed_compiler(def: &BundleDef, root: &Path) -> Option<PathBuf> {
    let own = match def.hello {
        Hello::TrustSysroot { compiler } => Some(compiler),
        Hello::StagedSysroot => None,
    };
    own.into_iter()
        .chain(["rustc", "trustc"])
        .map(|n| root.join("bin").join(n))
        .find(|p| atpkg::toolchain::is_executable_file(p))
}

/// Admit a bundle request: every decision that needs no copy.
///
/// # Errors
///
/// A usage error for a malformed request; a refusal for a compiler, a hello sysroot, a
/// checkout or a key that does not pass.
pub fn admit<'a>(
    ctx: &'a Ctx,
    req: &'a BundleRequest,
    tools: &'a dyn Tools,
) -> Result<Admitted<'a>> {
    let program = &req.program;
    let pdef = programs::get(program)
        .ok_or_else(|| Error::usage(format!("`{program}` is not a program the channel pins")))?;
    let Shape::Bundle(def) = pdef.shape else {
        return Err(Error::usage(format!(
            "{program} is not a sysroot bundle: a plain program packs with `pack`"
        )));
    };
    if req.signing == Signing::Unsigned && req.upload {
        return Err(Error::usage(
            "--unsigned and --upload together make no sense — nothing unsigned may publish",
        ));
    }
    // Resolved once: everything below reads this physical path.
    let sysroot = std::fs::canonicalize(&req.sysroot)
        .ok()
        .filter(|s| s.join("bin").is_dir())
        .ok_or_else(|| {
            Error::refused(format!(
                "{} has no bin/ (not a sysroot)",
                ctx.shown(&req.sysroot)
            ))
        })?;
    let hello = match hello_plan(&def, &req.hello)? {
        // Pinned physically for the same reason as the sysroot: what is admitted below is
        // what the scan and the hello read.
        HelloPlan::Trust { sysroot, compiler } => HelloPlan::Trust {
            sysroot: std::fs::canonicalize(&sysroot)
                .map_err(|e| Error::io(format!("resolve {}", sysroot.display()), &e))?,
            compiler,
        },
        plan => plan,
    };
    let base = req
        .merge_from
        .as_ref()
        .map(|p| std::fs::read(p).map_err(|e| Error::io(format!("read {}", ctx.shown(p)), &e)))
        .transpose()?;
    let lease = toolchain::lease(
        ctx,
        &sysroot,
        &format!("atpkg-publish pack-bundle {program}"),
    )?;
    let trust_repo = lineage_switch::default_trust_repo(ctx);
    let switch = lineage_switch::read(ctx, &trust_repo)?;
    pack::say_switch(ctx, &trust_repo, &switch);

    // The hello sysroot must be an admitted Trust toolchain and the very compiler the
    // packed one was built from, or the hello proves nothing about it.
    let mut hello_compiler = None;
    if let HelloPlan::Trust {
        sysroot: hs,
        compiler,
    } = &hello
    {
        let c = toolchain::admit(tools, &hs.join("bin"), &switch).map_err(|e| {
            Error::refused(format!(
                "--hello-sysroot {} is not an admitted Trust toolchain, so a hello against it \
                 proves nothing about Trust: {e}",
                hs.display()
            ))
        })?;
        let packed = sysroot.join("bin").join(compiler);
        let packed_commit = tools::version(tools, &packed).ok().and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("commit-hash: ").map(str::to_string))
        });
        if packed_commit.as_deref() != Some(c.content.version.commit.as_str()) {
            return Err(Error::refused(format!(
                "--hello-sysroot's compiler is {}, but the packed {} reports {:?} — the hello \
                 must run against the Trust sysroot the packed compiler was built from",
                c.content.version.commit,
                packed.display(),
                packed_commit
            )));
        }
        hello_compiler = Some(c);
    }
    let own_compiler = atpkg::toolchain::is_executable_file(&sysroot.join("bin/trustc"));
    let admitting = if own_compiler {
        toolchain::admit(tools, &sysroot.join("bin"), &switch)?
    } else if let Some(c) = hello_compiler {
        c
    } else {
        return Err(Error::refused(format!(
            "{program} ships no bin/trustc and names no --hello-sysroot — nothing says which \
             Trust compiler built this bundle, so nothing can admit its bytes"
        )));
    };
    let proven = admitting.lineage != atpkg::lineage::LineageProof::Unproven;

    let triple = match req.triple {
        Some(t) => t,
        None => packed_compiler(&def, &sysroot)
            .ok_or_else(|| Error::refused("no staged compiler names the target: pass --triple"))
            .and_then(|c| toolchain::host_of(tools, &c))?,
    };
    let build = match req.build {
        Some(b) => b,
        None => crate::git::rev_count(&req.src_repo, "HEAD")?,
    };
    let dirty = crate::git::run(&req.src_repo, &["rev-parse", "--is-inside-work-tree"]).is_ok()
        && !crate::git::is_clean(&req.src_repo)?;
    if dirty && !req.allow_dirty {
        return Err(Error::refused(format!(
            "{} is a DIRTY checkout — it would masquerade as build {build}'s clean commit. \
             Commit, or pass --allow-dirty to stamp it (a dirty pack is never uploaded)",
            ctx.shown(&req.src_repo)
        )));
    }
    if dirty && req.upload {
        return Err(Error::refused(
            "a dirty pack is never uploaded (the provenance gate)",
        ));
    }
    let mut version = match &req.version {
        Some(v) => v.clone(),
        None => derive_version(tools, &req.src_repo, &sysroot)?,
    };
    if dirty {
        version.push_str("-dirty");
    }
    let signer = match req.signing {
        Signing::MachineKey => Some(Signer::load(ctx)?),
        Signing::Unsigned => None,
    };
    std::fs::create_dir_all(&req.out)
        .map_err(|e| Error::io(format!("mkdir {}", ctx.shown(&req.out)), &e))?;
    eprintln!(
        "atpkg-publish: {program} build={build} version={version} triple={triple} — compiler \
         admitted ({}, lineage {})",
        admitting.content.version.commit,
        if proven { "proven" } else { "unproven" }
    );
    Ok(Admitted(Bundle {
        ctx,
        req,
        tools,
        def,
        repo: pdef.repo,
        sysroot,
        triple,
        build,
        version,
        switch,
        own_compiler,
        proven,
        hello,
        base,
        signer,
        _lease: lease,
    }))
}

/// Copy `src` into `dst`: directories made, symlinks copied as symlinks, regular files
/// copied with their modes, and a hard-link group copied once and linked.
///
/// # Errors
///
/// An I/O failure, or a special file.
pub fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    let mut seen: BTreeMap<(u64, u64), PathBuf> = BTreeMap::new();
    copy_into(src, dst, &mut seen)
}

fn copy_into(src: &Path, dst: &Path, seen: &mut BTreeMap<(u64, u64), PathBuf>) -> Result<()> {
    std::fs::create_dir_all(dst).map_err(|e| Error::io(format!("mkdir {}", dst.display()), &e))?;
    let mut entries: Vec<_> = std::fs::read_dir(src)
        .map_err(|e| Error::io(format!("read {}", src.display()), &e))?
        .flatten()
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for e in entries {
        let (from, to) = (e.path(), dst.join(e.file_name()));
        let meta = std::fs::symlink_metadata(&from)
            .map_err(|err| Error::io(format!("stat {}", from.display()), &err))?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            let target = std::fs::read_link(&from)
                .map_err(|err| Error::io(format!("readlink {}", from.display()), &err))?;
            symlink(&target, &to)?;
        } else if ft.is_dir() {
            copy_into(&from, &to, seen)?;
        } else if ft.is_file() {
            if let Some(key) = inode(&meta)
                && let Some(first) = seen.get(&key)
            {
                std::fs::hard_link(first, &to)
                    .map_err(|err| Error::io(format!("link {}", to.display()), &err))?;
                continue;
            }
            std::fs::copy(&from, &to)
                .map_err(|err| Error::io(format!("copy {}", from.display()), &err))?;
            if let Some(key) = inode(&meta) {
                seen.insert(key, to);
            }
        } else {
            return Err(Error::refused(format!(
                "{} is a special file, not a toolchain member",
                from.display()
            )));
        }
    }
    Ok(())
}

fn symlink(target: &Path, link: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
            .map_err(|e| Error::io(format!("symlink {}", link.display()), &e))
    }
    #[cfg(not(unix))]
    {
        let _ = target;
        Err(Error::refused(format!(
            "{} is a symlink, which this host cannot stage",
            link.display()
        )))
    }
}

fn inode(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        (meta.nlink() > 1).then(|| (meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

fn same_inode(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        match (std::fs::metadata(a), std::fs::metadata(b)) {
            (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        false
    }
}

/// Every path under `root` (no symlink followed), relative, sorted, with its type.
fn walk(root: &Path) -> Result<Vec<(PathBuf, std::fs::FileType)>> {
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let dir = root.join(&rel);
        for e in std::fs::read_dir(&dir)
            .map_err(|e| Error::io(format!("read {}", dir.display()), &e))?
            .flatten()
        {
            let child = rel.join(e.file_name());
            let ft = e
                .file_type()
                .map_err(|err| Error::io(format!("stat {}", child.display()), &err))?;
            if ft.is_dir() {
                stack.push(child.clone());
            }
            out.push((child, ft));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

impl<'a> Admitted<'a> {
    /// Copy the sysroot into a stage of this pack's own (hard links kept, the sysroot
    /// itself never written) and, for trust, freeze its rustc-dev + rust-src closure.
    ///
    /// # Errors
    ///
    /// An I/O failure, or the closure's refusal.
    pub fn stage(self) -> Result<Staged<'a>> {
        let b = self.0;
        // The tippy gate changes cwd to its hello crate. Resolve the stage's
        // parent first so its executable, HOME and PATH still name this copy.
        let out = std::fs::canonicalize(&b.req.out)
            .map_err(|e| Error::io(format!("resolve {}", b.ctx.shown(&b.req.out)), &e))?;
        let stage = aterm_tempfile::Builder::new()
            .prefix(&format!(".atpkg-bundle-{}-", b.req.program))
            .tempdir_in(&out)
            .map_err(|e| Error::io(format!("create a stage in {}", b.ctx.shown(&b.req.out)), &e))?;
        std::fs::create_dir_all(home_of(&stage))
            .map_err(|e| Error::io("create the smoke home", &e))?;
        copy_tree(&b.sysroot, &root_of(&stage))?;
        if b.def.closure {
            let probe = |p: &Path| tools::version(b.tools, p);
            let commit = closure::prepare(&b.sysroot, &root_of(&stage), b.triple, &probe)?;
            eprintln!(
                "atpkg-publish: frozen rustc-dev + rust-src closure for {commit} ({})",
                b.triple
            );
        }
        Ok(Staged { b, stage })
    }
}

/// Prune the dev-source links a sysroot carries (`lib/rustlib/{rustc-src,src}/…`, which
/// would dangle on a user box) and refuse any other symlink: the signed tree is
/// symlink-free by contract. Returns what was pruned.
///
/// # Errors
///
/// A refusal naming an unexpected link.
pub fn prune_links(root: &Path) -> Result<Vec<String>> {
    let mut pruned = Vec::new();
    for (rel, ft) in walk(root)? {
        if !ft.is_symlink() {
            continue;
        }
        let s = rel.to_string_lossy().into_owned();
        if s.starts_with("lib/rustlib/rustc-src/") || s.starts_with("lib/rustlib/src/") {
            std::fs::remove_file(root.join(&rel)).map_err(|e| Error::io(format!("rm {s}"), &e))?;
            pruned.push(s);
        } else {
            return Err(Error::refused(format!(
                "unexpected symlink in the staged sysroot: {s} — the signed tree is \
                 symlink-free by contract; teach the staging step its story if it is \
                 load-bearing"
            )));
        }
    }
    for husk in ["lib/rustlib/rustc-src", "lib/rustlib/src"] {
        let _ = std::fs::remove_dir(root.join(husk));
    }
    Ok(pruned)
}

/// Prune every triple-shaped `lib/rustlib/<dir>` that is not `triple` or kept: a
/// publisher's cross-target std is its state, not this asset's payload. Returns what was
/// pruned.
///
/// # Errors
///
/// An I/O failure.
pub fn prune_foreign_targets(root: &Path, triple: Triple, keep: &[String]) -> Result<Vec<String>> {
    let rustlib = root.join("lib/rustlib");
    let mut pruned = Vec::new();
    let Ok(entries) = std::fs::read_dir(&rustlib) else {
        return Ok(pruned);
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        let triple_shaped = name.matches('-').count() >= 2;
        if !triple_shaped || name == triple.as_str() || keep.contains(&name) {
            continue;
        }
        std::fs::remove_dir_all(rustlib.join(&name))
            .map_err(|e| Error::io(format!("rm lib/rustlib/{name}"), &e))?;
        pruned.push(name);
    }
    Ok(pruned)
}

/// A trust bundle carries a std for every target it keeps, not just a directory.
///
/// # Errors
///
/// A refusal naming a kept target with no `libstd-*.rlib`.
pub fn require_target_std(root: &Path) -> Result<()> {
    let rustlib = root.join("lib/rustlib");
    for e in std::fs::read_dir(&rustlib).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.matches('-').count() < 2 || !e.path().is_dir() {
            continue;
        }
        let has_std = std::fs::read_dir(e.path().join("lib")).is_ok_and(|rd| {
            rd.flatten().any(|f| {
                let n = f.file_name().to_string_lossy().into_owned();
                n.starts_with("libstd-") && n.ends_with(".rlib")
            })
        });
        if !has_std {
            return Err(Error::refused(format!(
                "lib/rustlib/{name} carries no libstd-*.rlib: the bundle would name a target it \
                 cannot compile for"
            )));
        }
    }
    Ok(())
}

/// Thin every fat Mach-O to `triple`'s slice, written through its inode (a hard-linked
/// member keeps its group). A fat file with no slice for the target is refused. Returns
/// the files thinned.
///
/// # Errors
///
/// A refusal for a file with no such slice; the tool's failure.
pub fn thin_fat(tools: &dyn Tools, root: &Path, triple: Triple) -> Result<Vec<String>> {
    let mut thinned = Vec::new();
    if triple.os() != Os::Darwin {
        return Ok(thinned);
    }
    let (_, want) = arch::expect(triple);
    let lipo_arch = match triple.arch() {
        Arch::Aarch64 => "arm64",
        Arch::X86_64 => "x86_64",
    };
    for rel in atpkg::relocate::walk_files(root).map_err(Error::cannot_run)? {
        let path = root.join(&rel);
        let Some(obj) = arch::read(&path) else {
            continue;
        };
        if obj.format != arch::Format::MachO || obj.cpus.len() < 2 {
            continue;
        }
        if !obj.cpus.contains(&want) {
            return Err(Error::refused(format!(
                "fat Mach-O {} has no {lipo_arch} slice ([{obj}]) — a {triple} asset cannot \
                 honestly carry it; fix the staged sysroot",
                rel.display()
            )));
        }
        let tmp = path.with_extension("atpkg-thin");
        tools
            .thin(&path, lipo_arch, &tmp)
            .map_err(|e| Error::refused(format!("lipo -thin {}: {e}", rel.display())))?;
        let thin = std::fs::read(&tmp).map_err(|e| Error::io("read the thinned slice", &e))?;
        let _ = std::fs::remove_file(&tmp);
        std::fs::write(&path, thin)
            .map_err(|e| Error::io(format!("write {}", rel.display()), &e))?;
        thinned.push(rel.to_string_lossy().into_owned());
    }
    Ok(thinned)
}

/// Strip the retired stock-Rust entrypoints Trust's surface gate forbids. A retired name
/// in `exposes` is a contradiction, not a file to clean up: it would ship a shim whose
/// target this very step deleted. Returns what was stripped.
///
/// # Errors
///
/// A refusal naming the exposed retired name.
pub fn strip_retired(root: &Path, exposes: &[&str]) -> Result<Vec<String>> {
    if let Some(name) = exposes.iter().find(|e| RETIRED_BINS.contains(e)) {
        return Err(Error::refused(format!(
            "the exposes list names '{name}', a retired public entrypoint trust's own surface \
             gate forbids — canonical names only"
        )));
    }
    let mut stripped = Vec::new();
    for (dir, names) in [("bin", RETIRED_BINS), ("libexec", RETIRED_LIBEXEC)] {
        for name in names {
            let p = root.join(dir).join(name);
            if std::fs::symlink_metadata(&p).is_ok() {
                std::fs::remove_file(&p).map_err(|e| Error::io(format!("rm {dir}/{name}"), &e))?;
                stripped.push(format!("{dir}/{name}"));
            }
        }
    }
    Ok(stripped)
}

fn first_difference(a: &[u8], b: &[u8]) -> String {
    match a.iter().zip(b).position(|(x, y)| x != y) {
        Some(i) => format!("first difference at byte {}", i + 1),
        None => "one is a prefix of the other".into(),
    }
}

/// What the one-compiler step found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OneCompiler {
    /// The bundle does not ship both names.
    NotApplicable,
    /// Already one inode.
    AlreadyLinked,
    /// Byte-identical copies, now one inode.
    LinkedCopy,
    /// The same program under two code signatures, now one inode.
    LinkedResigned,
}

/// Before any pass signs the tree, make `bin/rustc` a hard link to `bin/trustc` when the
/// two are byte-identical or the same program under two code signatures. Tippy refuses a
/// rustc that is not the selected trustc, and every later pass is hard-link aware, so the
/// compiler is signed once.
///
/// # Errors
///
/// A refusal for two different programs.
pub fn one_compiler(tools: &dyn Tools, root: &Path) -> Result<OneCompiler> {
    let (r, t) = (root.join("bin/rustc"), root.join("bin/trustc"));
    let regular = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_file());
    if !regular(&r) || !regular(&t) {
        return Ok(OneCompiler::NotApplicable);
    }
    if same_inode(&r, &t) {
        return Ok(OneCompiler::AlreadyLinked);
    }
    let (rb, tb) = (
        std::fs::read(&r).map_err(|e| Error::io("read bin/rustc", &e))?,
        std::fs::read(&t).map_err(|e| Error::io("read bin/trustc", &e))?,
    );
    let how = if rb == tb {
        OneCompiler::LinkedCopy
    } else if tools.same_unsigned(&r, &t).map_err(Error::refused)? {
        OneCompiler::LinkedResigned
    } else {
        return Err(Error::refused(format!(
            "bin/rustc and bin/trustc are different programs ({}, and still different with \
             both code signatures removed); a Trust bundle's rustc must BE its trustc — fix \
             the staged sysroot",
            first_difference(&rb, &tb)
        )));
    };
    std::fs::remove_file(&r).map_err(|e| Error::io("rm bin/rustc", &e))?;
    std::fs::hard_link(&t, &r).map_err(|e| Error::io("link bin/rustc", &e))?;
    Ok(how)
}

/// The one-compiler gate over the final bytes: when both ship, `bin/rustc` and
/// `bin/trustc` are byte-identical regular files (a hard link or a copy; the client hashes
/// them alike).
///
/// # Errors
///
/// A refusal for a link or two different files.
pub fn compiler_gate(root: &Path) -> Result<OneCompiler> {
    let (r, t) = (root.join("bin/rustc"), root.join("bin/trustc"));
    let present = |p: &Path| std::fs::symlink_metadata(p).is_ok();
    if !present(&r) || !present(&t) {
        return Ok(OneCompiler::NotApplicable);
    }
    let regular = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_file());
    if !regular(&r) || !regular(&t) {
        return Err(Error::refused(
            "one-compiler gate FAIL — bin/rustc and bin/trustc must both be regular files \
             (tippy refuses a link)",
        ));
    }
    if same_inode(&r, &t) {
        return Ok(OneCompiler::AlreadyLinked);
    }
    let (rb, tb) = (
        std::fs::read(&r).map_err(|e| Error::io("read bin/rustc", &e))?,
        std::fs::read(&t).map_err(|e| Error::io("read bin/trustc", &e))?,
    );
    if rb == tb {
        Ok(OneCompiler::LinkedCopy)
    } else {
        Err(Error::refused(format!(
            "one-compiler gate FAIL — bin/rustc and bin/trustc are not byte-identical ({}); \
             tippy refuses a rustc that is not the selected trustc",
            first_difference(&rb, &tb)
        )))
    }
}

/// Strip debug stabs and local symbols off every staged Mach-O (`strip -x -S`, exported
/// globals kept) and re-sign it ad hoc, once per hard-link group, re-linking the group's
/// other names: a per-file strip would sever the group and ship the driver twice.
/// rlib/rmeta/.a/.o stay untouched (compilation reads their symbols). Returns
/// `(stripped, relinked)`.
///
/// # Errors
///
/// The first strip or re-sign failure: a Mach-O the tools cannot process is one this
/// lane does not understand.
pub fn strip_symbols(tools: &dyn Tools, root: &Path) -> Result<(usize, usize)> {
    let mut groups: BTreeMap<(u64, u64), Vec<PathBuf>> = BTreeMap::new();
    let mut solo = 0u64;
    for rel in atpkg::relocate::walk_files(root).map_err(Error::cannot_run)? {
        let skip = rel
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e, "rlib" | "rmeta" | "a" | "o"));
        let path = root.join(&rel);
        if skip || !arch::read(&path).is_some_and(|o| o.format == arch::Format::MachO) {
            continue;
        }
        let meta = std::fs::metadata(&path).map_err(|e| Error::io("stat", &e))?;
        let key = inode(&meta).unwrap_or_else(|| {
            solo += 1;
            (u64::MAX, solo)
        });
        groups.entry(key).or_default().push(path);
    }
    let (mut stripped, mut relinked) = (0, 0);
    for members in groups.values() {
        let first = &members[0];
        tools
            .strip(first, true)
            .and_then(|()| tools.resign(first))
            .map_err(|e| {
                Error::refused(format!(
                    "strip -x -S / re-sign FAILED on {}: {e}",
                    first.strip_prefix(root).unwrap_or(first).display()
                ))
            })?;
        stripped += 1;
        for other in &members[1..] {
            if !same_inode(first, other) {
                std::fs::remove_file(other).map_err(|e| Error::io("relink", &e))?;
                std::fs::hard_link(first, other).map_err(|e| Error::io("relink", &e))?;
            }
            relinked += 1;
        }
    }
    Ok((stripped, relinked))
}

impl<'a> Staged<'a> {
    /// Every byte mutation, in order, before relocation and before any gate.
    ///
    /// # Errors
    ///
    /// The first refusal.
    pub fn mutate(self) -> Result<Mutated<'a>> {
        let Staged { b, stage } = self;
        let root = root_of(&stage);
        for p in prune_links(&root)? {
            eprintln!("atpkg-publish: pruned dev-source symlink {p}");
        }
        for t in prune_foreign_targets(&root, b.triple, &b.req.keep_targets)? {
            eprintln!(
                "atpkg-publish: pruned foreign target sysroot lib/rustlib/{t} (publisher \
                 state, not {} payload)",
                b.triple
            );
        }
        if b.def.closure {
            require_target_std(&root)?;
        }
        for f in thin_fat(b.tools, &root, b.triple)? {
            eprintln!("atpkg-publish: thinned fat Mach-O {f}");
        }
        for f in strip_retired(&root, b.def.exposes)? {
            eprintln!("atpkg-publish: stripped retired public entrypoint {f}");
        }
        let one = one_compiler(b.tools, &root)?;
        eprintln!("atpkg-publish: one compiler — {one:?}");
        if b.req.strip {
            let (s, l) = strip_symbols(b.tools, &root)?;
            eprintln!(
                "atpkg-publish: strip pass — {s} Mach-O(s) stripped, {l} hard-link member(s) re-linked"
            );
        }
        Ok(Mutated { b, stage })
    }
}

impl<'a> Mutated<'a> {
    /// Pack-time relocation: libraries vendored only from declared donor roots, each
    /// admitted as a Trust toolchain; any leftover machine-local reference is a refusal.
    ///
    /// # Errors
    ///
    /// A donor that is no Trust toolchain, or the relocator's refusal.
    pub fn relocate(self) -> Result<Relocated<'a>> {
        let Mutated { b, stage } = self;
        let mut donors = Vec::new();
        for d in &b.req.donor_roots {
            let root = std::fs::canonicalize(d)
                .map_err(|e| Error::io(format!("donor root {}", d.display()), &e))?;
            toolchain::admit(b.tools, &root.join("bin"), &b.switch).map_err(|e| {
                Error::refused(format!("donor root {} REFUSED — {e}", root.display()))
            })?;
            donors.push(root);
        }
        let report = b
            .tools
            .relocate(&root_of(&stage), &donors)
            .map_err(|e| Error::refused(format!("atpkg relocate: {e}")))?;
        report
            .require_self_contained()
            .map_err(|e| Error::refused(format!("atpkg relocate: {e}")))?;
        Ok(Relocated { b, stage })
    }
}

/// Every exposed bin is a file of the staged `bin/` — checked whether or not the smoke
/// runs: a manifest is never signed shimming a tool its payload lacks.
///
/// # Errors
///
/// A refusal naming every missing tool.
pub fn require_exposed(root: &Path, exposes: &[&str]) -> Result<()> {
    let missing: Vec<&str> = exposes
        .iter()
        .copied()
        .filter(|t| !root.join("bin").join(t).is_file())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(Error::refused(format!(
            "exposed bin(s) not in the staged sysroot: {} — a shim would point at nothing",
            missing.join(" ")
        )))
    }
}

/// Run one exposed bin under a hidden HOME with stdin closed: a loader error, or a bin
/// that never executes on any probe, is a refusal; a usage error for an unknown flag is
/// fine (it ran).
///
/// # Errors
///
/// A refusal naming the tool and why.
pub fn smoke(tools: &dyn Tools, root: &Path, home: &Path, tool: &str) -> Result<()> {
    const LOADER: [&str; 7] = [
        "dyld",
        "library not loaded",
        "image not found",
        "symbol not found",
        "cannot open shared object",
        "error while loading shared libraries",
        "no such file",
    ];
    let bin = root.join("bin").join(tool);
    if !bin.is_file() {
        return Err(Error::refused(format!(
            "exposed bin '{tool}' is not in the staged sysroot"
        )));
    }
    let mut ran_once = false;
    for probe in ["--version", "--help", "-h"] {
        let ran = tools
            .run(&hidden(Run::new(&bin, [probe]).bounded(), root, home))
            .map_err(|e| Error::refused(format!("smoke FAIL — {tool}: {e}")))?;
        let text = ran.text();
        if let Some(line) = text
            .lines()
            .find(|l| LOADER.iter().any(|w| l.to_ascii_lowercase().contains(w)))
        {
            return Err(Error::refused(format!(
                "smoke FAIL — {tool}: loader error on '{probe}': {}",
                line.trim()
            )));
        }
        ran_once |= ran.executed();
    }
    if ran_once {
        Ok(())
    } else {
        Err(Error::refused(format!(
            "smoke FAIL — {tool}: never executed cleanly on any probe (exec failure, crash or \
             signal death) — a bin that will not run on the build box will not run on a user \
             box"
        )))
    }
}

/// `run` under `env -i HOME=<home> TMPDIR=<home> PATH=<root>/bin:/usr/bin:/bin`.
fn hidden(mut run: Run, root: &Path, home: &Path) -> Run {
    let mut path = root.join("bin").into_os_string();
    path.push(":/usr/bin:/bin");
    run.clear_env = true;
    run.env = vec![
        ("HOME".into(), home.into()),
        ("TMPDIR".into(), home.into()),
        ("PATH".into(), path),
    ];
    run
}

/// The hello-world gate: the packed compiler compiles and runs a hello against `sysroot`
/// — strict first, then with verification off (a Trust compiler verifies by default and
/// may refuse an unproven `hello.rs`: a policy refusal, not a broken toolchain). Returns
/// the mode that proved it.
///
/// # Errors
///
/// A refusal when it compiles in neither mode, or the program prints the wrong thing.
pub fn hello(
    tools: &dyn Tools,
    root: &Path,
    home: &Path,
    compiler: &Path,
    sysroot: &Path,
) -> Result<&'static str> {
    let src = home.join("hello.rs");
    let exe = home.join("hello");
    std::fs::write(&src, "fn main(){ println!(\"hello, atpkg sysroot\"); }\n")
        .map_err(|e| Error::io("write hello.rs", &e))?;
    let what = || {
        format!(
            "{} against {}",
            compiler.file_name().unwrap_or_default().to_string_lossy(),
            sysroot.display()
        )
    };
    let mut last = String::new();
    for (mode, flag) in [
        ("strict", None),
        ("-Ztrust-verify=off", Some("-Ztrust-verify=off")),
    ] {
        let mut args: Vec<OsString> = vec!["--sysroot".into(), sysroot.into()];
        if let Some(f) = flag {
            args.push(f.into());
        }
        args.extend([
            "-o".into(),
            exe.clone().into_os_string(),
            src.clone().into_os_string(),
        ]);
        let mut run = hidden(Run::new(compiler, args), root, home);
        if let Some(f) = flag {
            run.env.push(("RUSTFLAGS".into(), f.into()));
        }
        let ran = tools
            .run(&run)
            .map_err(|e| Error::refused(format!("hello-world FAIL — {}: {e}", what())))?;
        if !ran.success() {
            last = String::from_utf8_lossy(&ran.stderr).into_owned();
            continue;
        }
        let out = tools
            .run(&hidden(
                Run::new(&exe, Vec::<OsString>::new()).bounded(),
                root,
                home,
            ))
            .map_err(|e| Error::refused(format!("hello-world FAIL — the hello: {e}")))?;
        return if out.text().contains("hello, atpkg sysroot") {
            Ok(mode)
        } else {
            Err(Error::refused(format!(
                "hello-world FAIL — the compiled hello printed {:?}",
                out.text()
            )))
        };
    }
    let tail: Vec<&str> = last.lines().collect();
    Err(Error::refused(format!(
        "hello-world FAIL — {} could not compile (strict AND verify-off):\n{}",
        what(),
        tail[tail.len().saturating_sub(5)..].join("\n")
    )))
}

/// The tippy gate: when the bundle ships `bin/tippy`, it lints a hello crate once as a
/// user would (`tippy --offline`, hidden HOME, staged `bin/` first on PATH). The smoke
/// cannot stand in: `tippy --version` exits 0 before tippy looks at its siblings.
///
/// # Errors
///
/// A refusal quoting tippy's `setup error` (else its last line), the stage root removed.
pub fn tippy_gate(tools: &dyn Tools, root: &Path, home: &Path) -> Result<bool> {
    let tippy = root.join("bin/tippy");
    if !tippy.is_file() {
        return Ok(false);
    }
    let krate = home.join("tippy-hello");
    let _ = std::fs::remove_dir_all(&krate);
    std::fs::create_dir_all(krate.join("src"))
        .map_err(|e| Error::io("create the hello crate", &e))?;
    std::fs::write(
        krate.join("Cargo.toml"),
        "[package]\nname = \"tippy-hello\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .map_err(|e| Error::io("write the hello crate", &e))?;
    std::fs::write(
        krate.join("src/main.rs"),
        "fn main() { println!(\"hello, atpkg sysroot\"); }\n",
    )
    .map_err(|e| Error::io("write the hello crate", &e))?;
    let mut run = hidden(Run::new(&tippy, ["--offline"]), root, home);
    run.cwd = Some(krate);
    let ran = tools
        .run(&run)
        .map_err(|e| Error::refused(format!("tippy gate FAIL — {e}")))?;
    if ran.success() {
        return Ok(true);
    }
    let text = ran.text();
    let reason = text
        .lines()
        .find(|l| l.contains("setup error"))
        .or_else(|| text.lines().rfind(|l| !l.trim().is_empty()))
        .unwrap_or("no output");
    let prefix = format!("{}/", root.display());
    Err(Error::refused(format!(
        "tippy gate FAIL (exit {}) — {}",
        ran.code
            .map_or_else(|| "signal".to_string(), |c| c.to_string()),
        reason.replace(&prefix, "")
    )))
}

impl<'a> Relocated<'a> {
    /// Every gate, over the final bytes.
    ///
    /// # Errors
    ///
    /// The first refusal.
    pub fn gate(self) -> Result<Gated<'a>> {
        let Relocated { b, stage } = self;
        let root = root_of(&stage);
        let home = home_of(&stage);
        compiler_gate(&root)?;
        let probe = |p: &Path| tools::version(b.tools, p);
        // LINEAGE is stamped inside the signed tree over the shipped digests, after the
        // last byte-changing pass. An unproven source records nothing.
        if b.own_compiler && b.proven {
            atpkg::lineage::stamp_with(&root, &b.sysroot.join("bin"), &probe).map_err(|r| {
                Error::refused(format!(
                    "cannot stamp LINEAGE over the staged compiler:\n  {}",
                    r.join("\n  ")
                ))
            })?;
        }
        let scan_bin = match (&b.hello, b.own_compiler) {
            (_, true) => root.join("bin"),
            (HelloPlan::Trust { sysroot, .. }, false) => sysroot.join("bin"),
            _ => root.join("bin"),
        };
        let scan_compiler = toolchain::admit(b.tools, &scan_bin, &b.switch)?;
        let provenance = pack::scan(&root, &scan_compiler)?;
        let tree = arch::gate_tree(b.triple, &root.join("bin"))?;
        eprintln!(
            "atpkg-publish: arch gate OK — {} object(s) in bin/ ({} script(s) named, not \
             gated)",
            tree.objects,
            tree.scripts.len()
        );
        for script in &tree.scripts {
            eprintln!("atpkg-publish:   not gated (a #! script): {script}");
        }
        require_exposed(&root, b.def.exposes)?;
        if b.req.smoke {
            for tool in b.def.exposes {
                smoke(b.tools, &root, &home, tool)?;
            }
        }
        match &b.hello {
            _ if !b.req.hello.enabled => {
                eprintln!("atpkg-publish: hello-world skipped on purpose");
            }
            HelloPlan::Skip => {
                eprintln!("atpkg-publish: hello-world skipped on purpose");
            }
            HelloPlan::Staged => {
                let compiler = packed_compiler(&b.def, &root).ok_or_else(|| {
                    Error::refused(
                        "hello-world FAIL — no rustc-like compiler in the staged bin/, so the \
                         gate could not be attempted (skip it on purpose with no hello)",
                    )
                })?;
                hello(b.tools, &root, &home, &compiler, &root)?;
            }
            HelloPlan::Trust { sysroot, compiler } => {
                hello(
                    b.tools,
                    &root,
                    &home,
                    &root.join("bin").join(compiler),
                    sysroot,
                )?;
            }
        }
        if b.req.smoke {
            tippy_gate(b.tools, &root, &home)?;
        }
        Ok(Gated {
            b,
            stage,
            provenance,
        })
    }
}

impl Gated<'_> {
    /// Tar, sign and (maybe) upload what was gated.
    ///
    /// # Errors
    ///
    /// The manifest's, the signature's or the upload's refusal.
    pub fn pack(self) -> Result<PackOutcome> {
        let Gated {
            b,
            stage,
            provenance,
        } = self;
        let program = &b.req.program;
        // The same per-program lock as a plain pack: the asset, the manifest and the
        // release tag are shared with every other publish of this program.
        let _lock = pack::pack_lock(b.ctx, program, pack::LOCK_WAIT)?;
        let asset_name = pkgdoc::asset_name(program, b.build, b.triple);
        let asset = b.req.out.join(&asset_name);
        let packed = tarwrite::write(&root_of(&stage), &asset)?;
        drop(stage);
        let header = Header {
            program: program.clone(),
            version: b.version.clone(),
            build: b.build,
            exposes: b.def.exposes.iter().map(|s| (*s).to_string()).collect(),
        };
        let row = Row {
            triple: b.triple,
            kind: Kind::SysrootBundle,
            asset: asset_name,
            sha256: packed.sha256.clone(),
            tree_root: packed.tree_root.clone(),
            size: packed.size,
            disk_installed: packed.disk_installed,
            provenance,
        };
        let doc = match b.base.clone() {
            Some(bytes) => pkgdoc::merge(bytes, &header, &row)?,
            None => pkgdoc::fresh(&header, &row)?,
        };
        let pkg = b.req.out.join(pkgdoc::file_name(program, b.build));
        let signature = pack::write_manifest(b.ctx, &doc, &header, &row, &pkg, b.signer.as_ref())?;
        let mut uploaded = false;
        if b.req.upload {
            let sig = signature
                .clone()
                .ok_or_else(|| Error::refused("nothing unsigned may publish"))?;
            upload::preflight(&asset, &row.sha256, &[pkg.clone(), sig.clone()])?;
            uploaded = upload::publish(
                b.ctx,
                b.repo,
                &upload::tag(program, b.build),
                &pkg,
                &[asset.clone(), pkg.clone(), sig],
                &format!(
                    "atpkg sysroot bundle for {program} ({}, {}). Signed pkg + tarball.",
                    b.version, b.triple
                ),
            )? == upload::Uploaded::Staged;
        }
        Ok(PackOutcome {
            program: program.clone(),
            build: b.build,
            version: b.version.clone(),
            triple: b.triple,
            asset,
            manifest: pkg,
            signature,
            exposes: header.exposes,
            lineage: pkgdoc::lineage_of(&row.provenance)?,
            uploaded,
        })
    }
}

/// [`PackBundle`] over explicit tools — the seam the tests drive.
///
/// # Errors
///
/// Any phase's refusal; nothing is signed or uploaded after one.
pub fn pack_bundle_with(
    ctx: &Ctx,
    request: &BundleRequest,
    tools: &dyn Tools,
) -> Result<PackOutcome> {
    admit(ctx, request, tools)?
        .stage()?
        .mutate()?
        .relocate()?
        .gate()?
        .pack()
}

/// `atpkg-publish pack-bundle …`.
///
/// # Errors
///
/// A usage error for a malformed command line; the pack's refusals.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let mut ctx = ctx.clone();
    let request = parse_args(&mut ctx, args)?;
    let ctx = &ctx;
    if request.upload {
        super::upload::token_preflight(ctx)?;
    }
    let out = PackBundle.pack_bundle(ctx, &request)?;
    println!(
        "atpkg-publish: packed {}@{} ({}) for {} → {}{}{}",
        out.program,
        out.build,
        out.version,
        out.triple,
        ctx.shown(&out.manifest),
        if out.signature.is_some() {
            " (signed)"
        } else {
            " (UNSIGNED)"
        },
        if out.uploaded {
            ", uploaded to staging"
        } else {
            ""
        }
    );
    Ok(())
}

/// The command line as a [`BundleRequest`]; `--trust-repo` moves where `ctx` reads the
/// receipt switch. The source checkout defaults to `~/<program>`; the output to its
/// `dist/`.
///
/// # Errors
///
/// A usage error naming what is wrong.
pub fn parse_args(ctx: &mut Ctx, args: &[String]) -> Result<BundleRequest> {
    let usage = || {
        Error::usage(
            "pack-bundle <program> --sysroot <dir> [--src-repo DIR] [--build N] [--version V] \
             [--triple T] [--hello-sysroot DIR | --no-hello] [--donor-root DIR]… \
             [--keep-target T]… [--merge-from PKG] [--out DIR] [--trust-repo DIR] \
             [--allow-dirty] [--no-smoke] [--no-strip] [--unsigned] [--upload]",
        )
    };
    let mut program = None;
    let mut sysroot = None;
    let mut src_repo = None;
    let mut req_build = None;
    let mut version = None;
    let mut triple = None;
    let mut hello = HelloGate::default();
    let mut donor_roots = Vec::new();
    let mut keep_targets = Vec::new();
    let mut merge_from = None;
    let mut out = None;
    let (mut allow_dirty, mut smoke, mut strip) = (false, true, true);
    let mut signing = Signing::MachineKey;
    let mut upload = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(usage);
        match a.as_str() {
            "--sysroot" => sysroot = Some(PathBuf::from(value()?)),
            "--src-repo" => src_repo = Some(PathBuf::from(value()?)),
            "--build" => {
                req_build = Some(
                    value()?
                        .parse()
                        .map_err(|_| Error::usage("--build takes a decimal build number"))?,
                );
            }
            "--version" => version = Some(value()?),
            "--triple" => triple = Some(Triple::parse(&value()?)?),
            "--hello-sysroot" => hello.sysroot = Some(PathBuf::from(value()?)),
            "--no-hello" => hello.enabled = false,
            "--donor-root" => donor_roots.push(PathBuf::from(value()?)),
            "--keep-target" => keep_targets.push(value()?),
            "--merge-from" => merge_from = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--trust-repo" => ctx.trust_repo = PathBuf::from(value()?),
            "--allow-dirty" => allow_dirty = true,
            "--no-smoke" => smoke = false,
            "--no-strip" => strip = false,
            "--unsigned" => signing = Signing::Unsigned,
            "--upload" => upload = true,
            s if s.starts_with('-') => {
                return Err(Error::usage(format!("pack-bundle: unknown flag `{s}`")));
            }
            s if program.is_none() => program = Some(s.to_string()),
            _ => return Err(usage()),
        }
    }
    let program: String = program.ok_or_else(usage)?;
    let sysroot = sysroot.ok_or_else(usage)?;
    let src_repo = src_repo.unwrap_or_else(|| ctx.home.join(&program));
    let out = out.unwrap_or_else(|| src_repo.join("dist"));
    Ok(BundleRequest {
        program,
        sysroot,
        src_repo,
        build: req_build,
        version,
        triple,
        hello,
        donor_roots,
        keep_targets,
        allow_dirty,
        smoke,
        strip,
        merge_from,
        out,
        signing,
        upload,
    })
}
