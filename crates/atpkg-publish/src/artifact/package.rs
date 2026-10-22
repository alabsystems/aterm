// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish pack-package <pkgdir> --build <YYYYMMDDNN>`: a prebuilt PACKAGE TREE
//! (a [`Shape::Package`] row: codex-alab, as `alab/package.sh` in its repo lays it), packed
//! WHOLE. No rebuild and no restrip: package.sh already built it with Trust, stripped it,
//! signed it ad hoc and smoke-tested it, and every byte it laid is what ships.
//!
//! Each step is a gate that refuses before anything later runs, and nothing is signed or
//! uploaded after a refusal:
//!
//! 1. the build id: given explicitly, inside the client's ALab store window
//!    (`atpkg::alab::in_store_window`: above the final legacy pin, below every vendor id);
//! 2. the package ([`read_package`]): an ALab package by the client's own marker
//!    (`codex-package.json` layout 1 whose entrypoint is `bin/<the exposed tool>`, and
//!    `alab-build.json`), for this target, whose two records name one version of the form
//!    `MAJOR.MINOR.0-dev.<count>+g<sha9>` with `<sha9>` the recorded source commit;
//! 3. the source ([`check_source`]): that commit is on `origin/main` of a checkout whose
//!    origin is the program's repo on the staging owner, and `<count>` is its
//!    `git rev-list --count`;
//! 4. the compiler ([`check_compiler`]): the store's admitted Trust (or `--toolchain`), held
//!    to what `alab-build.json` records it built with — its `-vV` byte for byte, its
//!    sysroot, the digest of its binaries and the id package.sh derives from them;
//! 5. the tree ([`check_tree`]): the arch gate over every object, then the client's ALab
//!    pack gate (`atpkg::install::check_alab_pack`: the marker, the shimmed entrypoint,
//!    `codesign --verify --strict` on every Mach-O, no other file executable);
//! 6. the payload lineage scan (`super::lineage::scan_tree`) against the admitted compiler;
//! 7. `codex-<build>.tar.zst` and `pkg-codex-<build>.toml`, signed with the machine key
//!    (`--key` names another, e.g. a scratch roster key for a local proof), then the client's
//!    whole stage of what was written ([`check_pack`]): the manifest verified under the
//!    roster, the archive's digest and `tree_root`, the gate again on the staged tree;
//! 8. only with `--upload`: the staging owner's `atpkg-<program>-<build>` release.
//!
//! [`verify_pinned`] is the same [`check_pack`] over a pack an index is about to pin
//! (`channel::index`): an ALab pin is signed only over a pack that verifies.

use std::path::{Path, PathBuf};

use super::pkgdoc::{self, Header, Kind, Row};
use super::tools::{self, HostTools, Tools};
use super::{arch, lineage_switch, pack, tarwrite, toolchain, upload};
use crate::api::Build;
use crate::channel::{Log, Scratch, download_retried};
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::host::Probe;
use crate::programs::{self, PackageDef, ProgramDef, Shape};
use crate::quad::VerifiedQuad;
use crate::triple::Triple;

/// Bound on a package record read.
const MAX_RECORD_BYTES: u64 = 64 * 1024;

/// The compiler `alab-build.json` says built the package (package.sh's `trusted_compiler`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompilerRecord {
    /// Its `rustc -vV`, verbatim.
    pub rustc_vv: String,
    /// The sysroot it ran from, as the compiler printed it.
    pub sysroot: String,
    /// [`binaries_sha256`] of that sysroot.
    pub binaries_sha256: String,
    /// [`compiler_id`] of the three.
    pub id: String,
}

/// A package tree, read and checked ([`read_package`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    /// Its root, canonical.
    pub root: PathBuf,
    /// The version both records name.
    pub version: String,
    /// The source commit (40 hex).
    pub source: String,
    /// The `<count>` of the version.
    pub count: u64,
    /// The compiler it was built with.
    pub compiler: CompilerRecord,
}

/// What a pack-package run is asked.
#[derive(Clone, Debug)]
pub struct PackageRequest {
    /// The program (a [`Shape::Package`] row).
    pub program: String,
    /// The package directory `alab/package.sh` laid.
    pub package: PathBuf,
    /// The build id (date form `YYYYMMDDNN`).
    pub build: Build,
    /// A checkout of the program's repo, whose `origin/main` must hold the source.
    pub source_repo: PathBuf,
    /// A Trust bin dir instead of the store's.
    pub toolchain: Option<PathBuf>,
    /// Where `<program>/` is staged.
    pub out: PathBuf,
    /// The signing key instead of the machine's (`~/.aterm/machine.key`).
    pub key: Option<PathBuf>,
    /// Upload to the staging owner.
    pub upload: bool,
}

/// What a pack-package run made.
#[derive(Clone, Debug)]
pub struct PackageOutcome {
    /// The program.
    pub program: String,
    /// The build id.
    pub build: Build,
    /// The package's version.
    pub version: String,
    /// The archive.
    pub asset: PathBuf,
    /// The manifest.
    pub manifest: PathBuf,
    /// Its signature.
    pub signature: PathBuf,
    /// Whether the staging release was written.
    pub uploaded: bool,
}

/// `program`'s package row: a [`Shape::Package`] row of an ALab-built agent program
/// ([`programs::alab_package`]) with one exposed tool and one target.
///
/// # Errors
///
/// A usage error naming what the program is instead.
pub fn package_row(program: &str) -> Result<(&'static ProgramDef, PackageDef, Triple)> {
    let def = programs::get(program)
        .ok_or_else(|| Error::usage(format!("`{program}` is not a program the channel pins")))?;
    let Shape::Package(p) = def.shape else {
        return Err(Error::usage(format!(
            "{program} is not a package row: a plain program packs with `pack`, a sysroot \
             bundle with `pack-bundle`"
        )));
    };
    if programs::alab_package(program).is_none() {
        return Err(Error::refused(format!(
            "{program}'s package row names repo {:?}, which the client does not route it from \
             (atpkg::alab) — a pin of it would reach nobody",
            def.repo
        )));
    }
    match (p.exposes, p.darwin) {
        ([_], [triple]) => Ok((def, p, *triple)),
        _ => Err(Error::refused(format!(
            "{program}'s package row must expose one tool for one target (its entrypoint is \
             bin/<tool>)"
        ))),
    }
}

/// Refuse a build id outside the client's ALab store window.
///
/// # Errors
///
/// The refusal, naming the window.
pub fn check_build(program: &str, build: Build) -> Result<()> {
    if atpkg::alab::in_store_window(build) {
        return Ok(());
    }
    Err(Error::refused(format!(
        "REFUSING {program} build {build}: an ALab build is numbered inside the client's ALab \
         store window ({} < build < {}; the date form YYYYMMDDNN) — outside it every client \
         reads the build as a legacy pin the vendor lane replaces, or as a vendor id",
        atpkg::alab::STORE_WINDOW.0,
        atpkg::alab::STORE_WINDOW.1
    )))
}

fn read_json(path: &Path) -> Result<aterm_json::Value> {
    let small = std::fs::symlink_metadata(path)
        .is_ok_and(|m| m.file_type().is_file() && m.len() <= MAX_RECORD_BYTES);
    let text = small
        .then(|| std::fs::read_to_string(path).ok())
        .flatten()
        .ok_or_else(|| {
            Error::refused(format!(
                "{} is not a regular UTF-8 record of at most {MAX_RECORD_BYTES} bytes",
                path.display()
            ))
        })?;
    aterm_json::from_str(&text)
        .map_err(|e| Error::refused(format!("{} is not JSON: {e}", path.display())))
}

fn text_of<'a>(v: &'a aterm_json::Value, key: &str, file: &str) -> Result<&'a str> {
    v.get(key)
        .and_then(aterm_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::refused(format!("{file} names no `{key}`")))
}

/// The `(count, sha9)` of a version `MAJOR.MINOR.0-dev.<count>+g<sha9>`.
#[must_use]
pub fn dev_version(version: &str) -> Option<(u64, &str)> {
    let (core, sha) = version.split_once("+g")?;
    let (line, count) = core.split_once("-dev.")?;
    let parts: Vec<&str> = line.split('.').collect();
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let line_ok = parts.len() == 3 && parts[..2].iter().all(|p| digits(p)) && parts[2] == "0";
    let sha_ok = sha.len() == 9 && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if !(line_ok && digits(count) && sha_ok) {
        return None;
    }
    Some((count.parse().ok()?, sha))
}

fn is_commit(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Read and check the package at `dir` for `program` on `triple`: an ALab package by the
/// client's marker whose entrypoint is `bin/<tool>`, both records for `triple` and naming
/// one dev-form version whose sha is the recorded source commit's, and a full compiler
/// record.
///
/// # Errors
///
/// A refusal naming the first thing that does not hold.
pub fn read_package(dir: &Path, tool: &str, triple: Triple) -> Result<Package> {
    let root = std::fs::canonicalize(dir)
        .map_err(|e| Error::refused(format!("the package {} ({e})", dir.display())))?;
    if !atpkg::alab::is_alab_package(&root) {
        return Err(Error::refused(format!(
            "{} is not an ALab package: a {} of layout 1 whose entrypoint is a file in it, and \
             {} beside it (alab/package.sh lays both)",
            root.display(),
            atpkg::alab::PACKAGE_JSON,
            atpkg::alab::ALAB_BUILD_JSON
        )));
    }
    let entry = atpkg::alab::entrypoint_of(&root);
    let want = Path::new("bin").join(tool);
    if entry.as_deref() != Some(want.as_path()) {
        return Err(Error::refused(format!(
            "the package's entrypoint is {entry:?}, not {} — the shim of `{tool}` runs \
             bin/{tool}",
            want.display()
        )));
    }
    let pj = atpkg::alab::PACKAGE_JSON;
    let bj = atpkg::alab::ALAB_BUILD_JSON;
    let manifest = read_json(&root.join(pj))?;
    let build = read_json(&root.join(bj))?;
    let version = text_of(&manifest, "version", pj)?.to_string();
    for (v, file) in [(&manifest, pj), (&build, bj)] {
        let target = text_of(v, "target", file)?;
        if target != triple.as_str() {
            return Err(Error::refused(format!(
                "{file} is for {target}, not {triple}"
            )));
        }
    }
    let recorded = text_of(&build, "version", bj)?;
    if recorded != version {
        return Err(Error::refused(format!(
            "{pj} says version {version} but {bj} records {recorded} — one package, one version"
        )));
    }
    let source = text_of(&build, "source", bj)?.to_string();
    if !is_commit(&source) {
        return Err(Error::refused(format!(
            "{bj}'s source `{source}` is not a full commit sha"
        )));
    }
    let Some((count, sha9)) = dev_version(&version) else {
        return Err(Error::refused(format!(
            "version {version} is not MAJOR.MINOR.0-dev.<count>+g<sha9>: a bare stable version \
             arms the vendor's own updater, and the dev form is what keeps it off"
        )));
    };
    if !source.starts_with(sha9) {
        return Err(Error::refused(format!(
            "version {version} names commit {sha9}, but {bj} records source {source}"
        )));
    }
    let c = build
        .get("compiler")
        .ok_or_else(|| Error::refused(format!("{bj} records no compiler")))?;
    let field = |k: &str| text_of(c, k, &format!("{bj}'s compiler")).map(str::to_string);
    let compiler = CompilerRecord {
        rustc_vv: field("rustc_vV")?,
        sysroot: field("sysroot")?,
        binaries_sha256: field("binaries_sha256")?,
        id: field("id")?,
    };
    Ok(Package {
        root,
        version,
        source,
        count,
        compiler,
    })
}

/// Whether the remote URL `url` names `owner/repo` on GitHub (https or ssh, with or without
/// `.git`) or in a directory on this machine (a path or a `file://` URL: the operator's own
/// mirror). Another host is refused: `owner/repo` there is not ALab's.
#[must_use]
pub fn names_repo(url: &str, owner: &str, repo: &str) -> bool {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    let tail = format!("{owner}/{repo}");
    let Some(head) = url.strip_suffix(&tail) else {
        return false;
    };
    let local = |head: &str| head.starts_with('/') && head.ends_with('/');
    matches!(
        head,
        "https://github.com/" | "ssh://git@github.com/" | "git@github.com:"
    ) || local(head)
        || head.strip_prefix("file://").is_some_and(local)
}

/// The package's source commit is on `origin/main` of `repo_dir`, whose `origin` is
/// `owner/repo`, and the version's `<count>` is that commit's `git rev-list --count`.
/// Read only: nothing is fetched (a commit on a stale `origin/main` is on main still).
///
/// # Errors
///
/// A refusal naming what does not hold.
pub fn check_source(repo_dir: &Path, owner: &str, repo: &str, pkg: &Package) -> Result<()> {
    let url = crate::git::run(repo_dir, &["remote", "get-url", "origin"])?;
    if !names_repo(&url, owner, repo) {
        return Err(Error::refused(format!(
            "{}'s origin is {url}, not {owner}/{repo} — the source must be on ALab's main",
            repo_dir.display()
        )));
    }
    let on_main = crate::git::run(
        repo_dir,
        &[
            "merge-base",
            "--is-ancestor",
            &pkg.source,
            "refs/remotes/origin/main",
        ],
    );
    if let Err(e) = on_main {
        return Err(Error::refused(format!(
            "the package's source {} is not on {owner}/{repo} origin/main in {} ({e}) — only \
             what ALab ships is packed (fetch origin if it landed since)",
            pkg.source,
            repo_dir.display()
        )));
    }
    let count = crate::git::rev_count(repo_dir, &pkg.source)?;
    if count != pkg.count {
        return Err(Error::refused(format!(
            "version {} says <count> {}, but {} is commit {count} of its history",
            pkg.version, pkg.count, pkg.source
        )));
    }
    Ok(())
}

fn unhex(hex: &str) -> Option<Vec<u8>> {
    hex.len()
        .is_multiple_of(2)
        .then(|| {
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
                .collect()
        })
        .flatten()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The digest package.sh records for a sysroot's compiler binaries: SHA-256 over, for
/// `bin/trustc` (else `bin/rustc`) and then each `lib/librustc_driver-*.dylib` in name
/// order, the file's name, a NUL and the raw SHA-256 of its bytes.
///
/// # Errors
///
/// A refusal when the sysroot has no compiler, or a file does not read.
pub fn binaries_sha256(sysroot: &Path) -> Result<String> {
    let exe = ["trustc", "rustc"]
        .iter()
        .map(|n| sysroot.join("bin").join(n))
        .find(|p| p.is_file())
        .ok_or_else(|| Error::refused(format!("no trustc in {}/bin", sysroot.display())))?;
    let mut drivers: Vec<PathBuf> = std::fs::read_dir(sysroot.join("lib"))
        .map_err(|e| Error::io(format!("read {}/lib", sysroot.display()), &e))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n.starts_with("librustc_driver-") && n.ends_with(".dylib")
            })
        })
        .collect();
    drivers.sort();
    let mut h = aterm_digest::Sha256::new();
    for f in std::iter::once(exe).chain(drivers) {
        let digest = atpkg::sha256_file(&f)
            .ok()
            .and_then(|d| unhex(&d))
            .ok_or_else(|| Error::refused(format!("{} does not hash", f.display())))?;
        let name = f.file_name().map(|n| n.to_string_lossy().into_owned());
        h.update(name.unwrap_or_default().as_bytes());
        h.update(b"\0");
        h.update(&digest);
    }
    Ok(hex(&h.finalize()))
}

/// The id package.sh derives for a compiler: the first 16 hex of SHA-256 over its `-vV`,
/// sysroot and [`binaries_sha256`], NUL-separated.
#[must_use]
pub fn compiler_id(rustc_vv: &str, sysroot: &str, binaries: &str) -> String {
    let d = aterm_digest::Sha256::digest(format!("{rustc_vv}\0{sysroot}\0{binaries}").as_bytes());
    hex(&d)[..16].to_string()
}

/// The package was built by the compiler this pack admitted: `alab-build.json`'s record
/// names `compiler`'s own root as its sysroot, its `-vV` (`vv`) byte for byte, the digest
/// of that root's binaries, and the id derived from the three.
///
/// # Errors
///
/// A refusal naming the first difference.
pub fn check_compiler(
    record: &CompilerRecord,
    compiler: &atpkg::lineage::Compiler,
    vv: &str,
) -> Result<()> {
    let root = &compiler.content.root;
    let recorded = std::fs::canonicalize(&record.sysroot).ok();
    if recorded.as_deref() != Some(root.as_path()) {
        return Err(Error::refused(format!(
            "the package was built with the compiler at {}, not the admitted Trust at {} — \
             build it with the store's admitted toolchain (aterm pkg which trustc)",
            record.sysroot,
            root.display()
        )));
    }
    if record.rustc_vv != vv {
        return Err(Error::refused(format!(
            "the package's compiler said `{}`, the admitted one says `{}`",
            record.rustc_vv.lines().next().unwrap_or_default(),
            vv.lines().next().unwrap_or_default()
        )));
    }
    let bins = binaries_sha256(root)?;
    if record.binaries_sha256 != bins {
        return Err(Error::refused(format!(
            "the package's compiler binaries hashed to {}, the admitted Trust's hash to {bins} \
             — another build of the compiler at the same path",
            record.binaries_sha256
        )));
    }
    let id = compiler_id(&record.rustc_vv, &record.sysroot, &bins);
    if record.id != id {
        return Err(Error::refused(format!(
            "the package's compiler id {} is not the {id} its own record derives",
            record.id
        )));
    }
    Ok(())
}

/// The arch gate over every object in `dir` (each must be `triple`'s; a fat Mach-O passes
/// with that slice), then the client's ALab pack gate. Returns how many objects were read.
///
/// # Errors
///
/// The first refusal.
pub fn check_tree(dir: &Path, triple: Triple, exposes: &[String]) -> Result<usize> {
    let files = super::relocate::walk_files(dir)
        .map_err(|e| Error::refused(format!("walk {}: {e}", dir.display())))?;
    let objects: Vec<PathBuf> = files
        .into_iter()
        .map(|rel| dir.join(rel))
        .filter(|p| arch::read(p).is_some())
        .collect();
    let n = arch::gate(triple, &objects)?;
    atpkg::install::check_alab_pack(dir, exposes).map_err(|e| {
        Error::refused(format!(
            "the client's ALab pack gate refuses {}: {e}",
            dir.display()
        ))
    })?;
    Ok(n)
}

/// The client's whole stage of one pack: `pkg` (already verified under the roster) is
/// `def`'s build `build`, exposes exactly the row's tools, carries a row for each of the
/// row's targets and no other, and each row's archive (`archive_of(asset)`) stages through
/// `atpkg::verify_and_stage` — digest, extract, `tree_root` — with the ALab pack gate run
/// on the staged tree, whose package names the manifest's version.
///
/// # Errors
///
/// The first refusal.
pub fn check_pack(
    def: &ProgramDef,
    row: &PackageDef,
    build: Build,
    pkg: &atpkg::PkgManifest,
    archive_of: &dyn Fn(&str) -> Result<PathBuf>,
) -> Result<()> {
    let p = def.name;
    if pkg.program != p || pkg.build_number != build {
        return Err(Error::refused(format!(
            "the manifest is {}@{}, not {p}@{build}",
            pkg.program, pkg.build_number
        )));
    }
    let mut exposes: Vec<&str> = pkg.exposes.iter().map(String::as_str).collect();
    exposes.sort_unstable();
    let mut want: Vec<&str> = row.exposes.to_vec();
    want.sort_unstable();
    if exposes != want {
        return Err(Error::refused(format!(
            "{p}@{build} shims [{}], not [{}]",
            exposes.join(", "),
            want.join(", ")
        )));
    }
    if let Some(stray) = pkg
        .artifacts
        .iter()
        .find(|a| !row.darwin.iter().any(|t| t.as_str() == a.target))
    {
        return Err(Error::refused(format!(
            "{p}@{build} carries a row for {}, a target ALab does not build {p} for — there its \
             client follows the vendor",
            stray.target
        )));
    }
    for triple in row.darwin {
        let artifact = pkg
            .artifacts
            .iter()
            .find(|a| a.target == triple.as_str())
            .ok_or_else(|| Error::refused(format!("{p}@{build} has no {triple} row")))?;
        if artifact.kind != Kind::Binary.as_str() {
            return Err(Error::refused(format!(
                "{p}@{build}'s {triple} row is a {}, not a binary package tree",
                artifact.kind
            )));
        }
        let archive = archive_of(&artifact.asset)?;
        let scratch = Scratch::new("pack-check")?;
        let build_dir = scratch.path().join("store").join(p).join(build.to_string());
        let exposes = pkg.exposes.clone();
        let gate = move |tree: &Path| atpkg::install::check_alab_pack(tree, &exposes);
        let hooks = atpkg::install::StageHooks {
            pre_swap: Some(&gate),
            sidecars: None,
            aliases: &[],
        };
        atpkg::verify_and_stage(artifact, &archive, &build_dir, &hooks).map_err(|e| {
            Error::refused(format!(
                "the client's stage refuses {p}@{build} for {triple}: {e}"
            ))
        })?;
        let staged = atpkg::alab::package_version(&build_dir);
        if staged.as_deref() != Some(pkg.version.as_str()) {
            return Err(Error::refused(format!(
                "{p}@{build}'s manifest says version {} but its package says {staged:?}",
                pkg.version
            )));
        }
    }
    Ok(())
}

/// The pack an index is about to pin for `def` at `build`, verified as a client would stage
/// it ([`check_pack`]) under `quad` (the index being signed): from `pack_dir` when given (a
/// local proof), else the public owner's download host, else the staging owner.
///
/// # Errors
///
/// A refusal when the pack cannot be read or does not verify.
pub fn verify_pinned(
    ctx: &Ctx,
    quad: &VerifiedQuad,
    def: &ProgramDef,
    build: Build,
    pack_dir: Option<&Path>,
    log: &mut Log,
) -> Result<()> {
    let Shape::Package(row) = def.shape else {
        return Err(Error::refused(format!("{} is not a package row", def.name)));
    };
    let p = def.name;
    let tag = upload::tag(p, build);
    let mf = pkgdoc::manifest_name(p, build);
    let sig = pkgdoc::sig_name(p, build);
    let scratch = Scratch::new("alab-pin")?;
    let (dir, from) = if let Some(d) = pack_dir {
        (d.to_path_buf(), ctx.shown(d))
    } else {
        fetch_pair(ctx, def.repo, &tag, &mf, &sig, scratch.path())?
    };
    let read = |name: &str| {
        std::fs::read(dir.join(name))
            .map_err(|e| Error::refused(format!("{tag}: {name} does not read from {from} ({e})")))
    };
    let pkg = quad.verify_pkg(&read(&mf)?, &read(&sig)?)?;
    let archive_of = |asset: &str| -> Result<PathBuf> {
        let path = dir.join(asset);
        if pack_dir.is_none() && !path.is_file() {
            fetch_asset(ctx, def.repo, &tag, asset, &pkg, &dir)?;
        }
        Ok(path)
    };
    check_pack(def, &row, build, &pkg, &archive_of)?;
    log.say(format!(
        "{p}@{build} ({}) verifies as every client stages it: the signed manifest under this \
         index's roster, the archive's digest and tree_root, the ALab pack gate (from {from})",
        pkg.version
    ));
    Ok(())
}

/// The manifest pair of `tag`, from the public owner's download host, else the staging
/// owner, into `dir`: `(dir, where it came from)`.
fn fetch_pair(
    ctx: &Ctx,
    repo: &str,
    tag: &str,
    mf: &str,
    sig: &str,
    dir: &Path,
) -> Result<(PathBuf, String)> {
    let public = ctx.public_slug(repo);
    let host = &*ctx.hosts.channel;
    let cap = atpkg::net::MANIFEST_CAP;
    if host.fetch_public(&public, tag, mf, &dir.join(mf), cap) == Probe::Present
        && host.fetch_public(&public, tag, sig, &dir.join(sig), cap) == Probe::Present
    {
        return Ok((dir.to_path_buf(), public.to_string()));
    }
    let staging = ctx.staging_slug(repo);
    download_retried(&*ctx.hosts.staging, &staging, tag, &[mf, sig], dir).map_err(|e| {
        Error::refused(format!(
            "{tag}: the signed manifest pair is on neither {public} nor {staging} ({e}) — \
             pack-package --upload stages it first"
        ))
    })?;
    Ok((dir.to_path_buf(), staging.to_string()))
}

/// One asset of `tag` into `dir`, from where the pair came: the public download host when
/// it serves it (at most the row's signed size), else the staging owner.
fn fetch_asset(
    ctx: &Ctx,
    repo: &str,
    tag: &str,
    asset: &str,
    pkg: &atpkg::PkgManifest,
    dir: &Path,
) -> Result<()> {
    let cap = pkg
        .artifacts
        .iter()
        .find(|a| a.asset == asset)
        .map_or(0, |a| a.size);
    let public = ctx.public_slug(repo);
    if ctx
        .hosts
        .channel
        .fetch_public(&public, tag, asset, &dir.join(asset), cap)
        == Probe::Present
    {
        return Ok(());
    }
    let staging = ctx.staging_slug(repo);
    download_retried(&*ctx.hosts.staging, &staging, tag, &[asset], dir).map_err(|e| {
        Error::refused(format!(
            "{tag}: {asset} is on neither {public} nor {staging} ({e})"
        ))
    })
}

/// [`pack_package`] over explicit tools — the seam the tests drive.
///
/// # Errors
///
/// Any gate's refusal; nothing is signed or uploaded after one.
pub fn pack_package_with(
    ctx: &Ctx,
    request: &PackageRequest,
    tools: &dyn Tools,
) -> Result<PackageOutcome> {
    let program = request.program.as_str();
    let (def, row, triple) = package_row(program)?;
    let tool = row.exposes[0];
    check_build(program, request.build)?;
    let mut ctx = ctx.clone();
    if let Some(key) = &request.key {
        ctx.identity.key.clone_from(key);
    }
    let ctx = &ctx;
    // The key and the roster first: a pack that cannot sign stops in seconds.
    let signer = pack::Signer::load(ctx)?;

    // THE SNAPSHOT: every gate below and the archive read one private copy of the package
    // (a clone on APFS), so a `package.sh` run rewriting `<pkgdir>` meanwhile can change
    // nothing between what the gates passed and the bytes that are signed.
    let original = std::fs::canonicalize(&request.package)
        .map_err(|e| Error::refused(format!("the package {} ({e})", request.package.display())))?;
    let snapshot = Scratch::new("pack-package")?;
    super::bundle::copy_tree(&original, &snapshot.path().join("package"))?;
    let pkg = read_package(&snapshot.path().join("package"), tool, triple)?;
    eprintln!(
        "atpkg-publish: {program} {} (source {}) → build {}",
        pkg.version, pkg.source, request.build
    );
    check_source(&request.source_repo, &ctx.accounts.staging, def.repo, &pkg)?;

    let tc = toolchain::resolve(ctx, request.toolchain.as_deref())?;
    let _lease = toolchain::lease(
        ctx,
        &tc.bin,
        &format!("atpkg-publish pack-package {program}"),
    )?;
    let trust_repo = lineage_switch::default_trust_repo(ctx);
    let switch = lineage_switch::read(ctx, &trust_repo)?;
    pack::say_switch(ctx, &trust_repo, &switch);
    let compiler = toolchain::admit(tools, &tc.bin, &switch)?;
    let vv = tools::version(tools, &tc.trustc()).map_err(Error::refused)?;
    check_compiler(&pkg.compiler, &compiler, &vv)?;
    eprintln!(
        "atpkg-publish: compiler OK — built by the admitted Trust {} ({})",
        compiler.content.version.commit,
        ctx.shown(&compiler.content.root)
    );

    let exposes: Vec<String> = row.exposes.iter().map(|t| (*t).to_string()).collect();
    let objects = check_tree(&pkg.root, triple, &exposes)?;
    eprintln!(
        "atpkg-publish: tree OK — {objects} object(s) are {triple}; the client's ALab pack \
         gate passes (marker, bin/{tool}, codesign --verify --strict)"
    );
    let provenance = pack::scan(&pkg.root, &compiler)?;

    let _lock = pack::pack_lock(ctx, program, pack::LOCK_WAIT)?;
    let stage_dir = request.out.join(program);
    pack::check_stage_dir(ctx, &stage_dir, &original)?;
    let parent = stage_dir.parent().unwrap_or(&stage_dir);
    if std::fs::canonicalize(parent).is_ok_and(|p| p.starts_with(&original)) {
        return Err(Error::refused(format!(
            "--out {} is inside the package {} — the archive would pack itself",
            ctx.shown(&request.out),
            ctx.shown(&original)
        )));
    }
    if std::fs::symlink_metadata(&stage_dir).is_ok() {
        std::fs::remove_dir_all(&stage_dir)
            .map_err(|e| Error::io(format!("rm {}", ctx.shown(&stage_dir)), &e))?;
    }
    std::fs::create_dir_all(&stage_dir)
        .map_err(|e| Error::io(format!("mkdir {}", ctx.shown(&stage_dir)), &e))?;
    let asset_name = pkgdoc::asset_name(program, request.build, triple);
    let asset = stage_dir.join(&asset_name);
    let packed = tarwrite::write(&pkg.root, &asset)?;
    let header = Header {
        program: program.to_string(),
        version: pkg.version.clone(),
        build: request.build,
        exposes: exposes.clone(),
    };
    let row_doc = Row {
        triple,
        kind: Kind::Binary,
        asset: asset_name,
        sha256: packed.sha256.clone(),
        tree_root: packed.tree_root.clone(),
        size: packed.size,
        disk_installed: packed.disk_installed,
        provenance,
    };
    let doc = pkgdoc::fresh(&header, &row_doc)?;
    let manifest = stage_dir.join(pkgdoc::manifest_name(program, request.build));
    let signature = pack::write_manifest(ctx, &doc, &header, &row_doc, &manifest, Some(&signer))?
        .ok_or_else(|| Error::refused("the manifest was not signed"))?;

    // What a client stages from these exact files.
    let read = |p: &Path| std::fs::read(p).map_err(|e| Error::io(ctx.shown(p), &e));
    let parsed = signer
        .authority()
        .verify_pkg(&read(&manifest)?, &read(&signature)?)?;
    let archive_of = |name: &str| Ok(stage_dir.join(name));
    check_pack(def, &row, request.build, &parsed, &archive_of)?;
    eprintln!(
        "atpkg-publish: self-check OK — the client verifies the signed manifest and stages \
         the archive through the ALab pack gate"
    );

    let mut uploaded = false;
    if request.upload {
        upload::preflight(
            &asset,
            &row_doc.sha256,
            &[manifest.clone(), signature.clone()],
        )?;
        let notes = format!(
            "atpkg pack of {program} {} ({triple}, source {}). Signed pkg manifest + tarball.",
            pkg.version, pkg.source
        );
        uploaded = upload::publish(
            ctx,
            def.repo,
            &upload::tag(program, request.build),
            &manifest,
            &[asset.clone(), manifest.clone(), signature.clone()],
            &notes,
        )? == upload::Uploaded::Staged;
    }
    Ok(PackageOutcome {
        program: program.to_string(),
        build: request.build,
        version: pkg.version,
        asset,
        manifest,
        signature,
        uploaded,
    })
}

/// `pack-package`'s synopsis.
pub const SYNOPSIS: &str = "pack-package <pkgdir> --build YYYYMMDDNN [--program codex] \
     [--source-repo DIR] [--toolchain BIN] [--trust-repo DIR] [--key PATH] [--out DIR] [--upload]";

/// The command line as a [`PackageRequest`]; `--trust-repo` moves where `ctx` reads the
/// receipt switch. `--program` defaults to the one package row, `--source-repo` to
/// `~/<repo>`, `--out` to `<state>/pack-package`.
///
/// # Errors
///
/// A usage error naming what is wrong.
pub fn parse_args(ctx: &mut Ctx, args: &[String]) -> Result<PackageRequest> {
    let usage = || Error::usage(SYNOPSIS);
    let mut package = None;
    let mut program = None;
    let mut build = None;
    let mut source_repo = None;
    let mut toolchain = None;
    let mut out = None;
    let mut key = None;
    let mut upload = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(usage);
        match a.as_str() {
            "--program" => program = Some(value()?),
            "--build" => {
                build =
                    Some(value()?.parse::<Build>().map_err(|_| {
                        Error::usage("--build takes a decimal build id (YYYYMMDDNN)")
                    })?);
            }
            "--source-repo" => source_repo = Some(PathBuf::from(value()?)),
            "--toolchain" => toolchain = Some(PathBuf::from(value()?)),
            "--trust-repo" => ctx.trust_repo = PathBuf::from(value()?),
            "--key" => key = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--upload" => upload = true,
            s if s.starts_with('-') => {
                return Err(Error::usage(format!("pack-package: unknown flag `{s}`")));
            }
            s if package.is_none() => package = Some(PathBuf::from(s)),
            _ => return Err(usage()),
        }
    }
    let package = package.ok_or_else(usage)?;
    let build = build.ok_or_else(|| {
        Error::usage(
            "pack-package names its --build explicitly (YYYYMMDDNN, above the head's pin): a \
             package has no build number of its own",
        )
    })?;
    let program = match program {
        Some(p) => p,
        None => {
            let mut rows = programs::PROGRAMS
                .iter()
                .filter(|d| matches!(d.shape, Shape::Package(_)));
            match (rows.next(), rows.next()) {
                (Some(d), None) => d.name.to_string(),
                _ => return Err(Error::usage("pack-package needs --program")),
            }
        }
    };
    let repo = programs::get(&program).map_or(program.as_str(), |d| d.repo);
    Ok(PackageRequest {
        source_repo: source_repo.unwrap_or_else(|| ctx.home.join(repo)),
        program,
        package,
        build,
        toolchain,
        out: out.unwrap_or_else(|| ctx.state_dir.join("pack-package")),
        key,
        upload,
    })
}

/// `atpkg-publish pack-package …` ([`parse_args`]).
///
/// # Errors
///
/// A usage error for a malformed command line; the pack's refusals.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let mut ctx = ctx.clone();
    let request = parse_args(&mut ctx, args)?;
    let ctx = &ctx;
    if request.upload {
        upload::token_preflight(ctx)?;
    }
    let o = pack_package_with(ctx, &request, &HostTools)?;
    println!(
        "atpkg-publish: packed {}@{} ({}) → {} (signed){}",
        o.program,
        o.build,
        o.version,
        ctx.shown(&o.manifest),
        if o.uploaded {
            ", uploaded to staging"
        } else {
            ""
        }
    );
    Ok(())
}
