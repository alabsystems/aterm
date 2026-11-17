// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish pack-tree <dir> --program <tree> --tree-key <hex> --build N`: a build tree
//! another project's build reads whole (a [`Shape::Tree`] row: Trust's LLVM build tree, its
//! stage-0 seed), packed WHOLE under the key that names what it was built from
//! (`docs/DESIGN-disk-custody-2026-11-12.md` §5).
//!
//! The key is bound to what it names: `--key-inputs` is the text the key is the SHA-256 of
//! (DC-16c's reader writes it), refused unless it hashes to `--tree-key`, refused for a tree
//! whose completion stamp ([`atpkg::keyed_tree::KeyedProgram::stamp`], the LLVM build's
//! `.llvm-stamp`) it does not name, and signed into the row, so every client checks the key
//! against it and anyone can audit what a key covered.
//!
//! The tree is cloned into a scratch snapshot (so a tree that moves under the pack cannot
//! split the archive from its record; a clone where the volume makes one, a copy elsewhere), its
//! in-tree symlinks materialized (a signed tree is symlink-free: a link to a file becomes a hard
//! link to it, which the archive carries as one body and the client lays as one inode), and the
//! snapshot written as one `.tar.zst`, held to the release host's per-asset bound before it is
//! signed. The manifest's one row is `kind = "keyed-tree"` with the key and its inputs inside the
//! signed bytes and no compiler record (no first-party crate is compiled into a tree). Before
//! anything is uploaded the client's own install of the pair is run in scratch ([`check_pack`]):
//! the signed manifest parsed under the roster, the archive held to its digest and tree root,
//! and the tree laid under its key by `atpkg::keyed_tree::install`, which refuses any other key.

use std::path::{Path, PathBuf};

use super::pkgdoc::{self, Header, TreeRow};
use super::tarwrite::{self, Compressor};
use super::{bottle, bundle, pack, upload};
use crate::api::{Build, Signing};
use crate::channel::Scratch;
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::programs::{self, ProgramDef, Shape, TreeDef};
use crate::triple::Triple;

/// What a pack-tree run is asked.
#[derive(Clone, Debug)]
pub struct TreeRequest {
    /// The program (a [`Shape::Tree`] row).
    pub program: String,
    /// The tree to pack whole.
    pub tree: PathBuf,
    /// The key it is installed and asked for under (64 lowercase hex).
    pub key: String,
    /// The text the key is the SHA-256 of: what the tree was built from.
    pub key_inputs: PathBuf,
    /// A display version (`llvm-20.1.8`, the stage-0 seed's release).
    pub version: String,
    /// The build number its release is tagged with (`atpkg-<program>-<build>`).
    pub build: Build,
    /// The target the tree was built on.
    pub triple: Triple,
    /// Where `<program>/` is staged.
    pub out: PathBuf,
    /// The signing key instead of the machine's (`~/.aterm/release-ring/machine.key`).
    pub signing_key: Option<PathBuf>,
    /// Sign now, or stage unsigned for a signing ceremony.
    pub signing: Signing,
    /// Upload to the staging owner.
    pub upload: bool,
}

/// What a pack-tree run made.
#[derive(Clone, Debug)]
pub struct TreeOutcome {
    /// The program.
    pub program: String,
    /// The key.
    pub key: String,
    /// The build number.
    pub build: Build,
    /// The archive.
    pub asset: PathBuf,
    /// The manifest.
    pub manifest: PathBuf,
    /// Its signature, when signed.
    pub signature: Option<PathBuf>,
    /// Whether the staging release was written.
    pub uploaded: bool,
}

/// `program`'s tree row, for `triple`.
/// # Errors
/// A usage error naming what the program is instead, or a target the row does not pack.
pub fn tree_row(program: &str, triple: Triple) -> Result<(&'static ProgramDef, TreeDef)> {
    let def = programs::get(program)
        .ok_or_else(|| Error::usage(format!("`{program}` is not a program the table knows")))?;
    let Shape::Tree(row) = def.shape else {
        return Err(Error::usage(format!(
            "{program} is not a keyed tree: a plain program packs with `pack`, a sysroot \
             bundle with `pack-bundle`, a package tree with `pack-package`"
        )));
    };
    if !row.darwin.contains(&triple) {
        return Err(Error::refused(format!(
            "{program} is packed for {} only, not {triple}",
            row.darwin
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok((def, row))
}

/// Pack `request`'s tree with `compressor` (the seam the tests drive).
/// # Errors
/// The row, key, version or build refused; the snapshot, the archive, the signature, or the
/// client's own install of the pair failed. Nothing is uploaded after a refusal.
pub fn pack_tree_with(
    ctx: &Ctx,
    request: &TreeRequest,
    compressor: &dyn Compressor,
) -> Result<TreeOutcome> {
    let program = request.program.as_str();
    let (def, _row) = tree_row(program, request.triple)?;
    let key = atpkg::keyed_tree::TreeKey::parse(&request.key).ok_or_else(|| {
        Error::usage(format!(
            "--tree-key `{}` is not 64 lowercase hex (a SHA-256 over what the tree was built from)",
            request.key
        ))
    })?;
    if request.build == 0 {
        return Err(Error::usage("--build must be >= 1"));
    }
    if request.signing == Signing::Unsigned && request.upload {
        return Err(Error::usage(
            "--unsigned and --upload together make no sense — nothing unsigned may publish",
        ));
    }
    let mut ctx = ctx.clone();
    if let Some(path) = &request.signing_key {
        ctx.identity.key.clone_from(path);
        ctx.identity.store = None;
    }
    let ctx = &ctx;
    let signer = match request.signing {
        Signing::MachineKey => Some(pack::Signer::load(ctx)?),
        Signing::Unsigned => None,
    };

    let original = std::fs::canonicalize(&request.tree)
        .map_err(|e| Error::refused(format!("the tree {} ({e})", ctx.shown(&request.tree))))?;
    if !original.is_dir() {
        return Err(Error::refused(format!(
            "the tree {} is not a directory",
            ctx.shown(&original)
        )));
    }
    let key_inputs = read_key_inputs(ctx, &request.key_inputs, &key)?;
    check_stamp(ctx, program, &original, &key_inputs)?;

    let _lock = pack::pack_lock(ctx, program, pack::LOCK_WAIT)?;
    let stage_dir = request.out.join(program);
    let parent = stage_dir.parent().unwrap_or(&stage_dir);
    std::fs::create_dir_all(parent)
        .map_err(|e| Error::io(format!("mkdir {}", ctx.shown(parent)), &e))?;
    // Before the snapshot: an --out inside the tree would pack itself, and is refused before a
    // byte of the tree is read.
    if std::fs::canonicalize(parent).is_ok_and(|p| p.starts_with(&original)) {
        return Err(Error::refused(format!(
            "--out {} is inside the tree {} — the archive would pack itself",
            ctx.shown(&request.out),
            ctx.shown(&original)
        )));
    }
    let snapshot = Scratch::new("pack-tree")?;
    let stage = snapshot.path().join("tree");
    bundle::clone_tree(&original, &stage)?;
    bottle::materialize_symlinks(&stage)?;

    if std::fs::symlink_metadata(&stage_dir).is_ok() {
        std::fs::remove_dir_all(&stage_dir)
            .map_err(|e| Error::io(format!("rm {}", ctx.shown(&stage_dir)), &e))?;
    }
    std::fs::create_dir_all(&stage_dir)
        .map_err(|e| Error::io(format!("mkdir {}", ctx.shown(&stage_dir)), &e))?;
    let asset_name = pkgdoc::asset_name(program, request.build, request.triple);
    let asset = stage_dir.join(&asset_name);
    let packed = tarwrite::write_with(&stage, &asset, compressor)?;
    drop(snapshot);
    // The release host's per-asset bound, before anything is signed for these bytes.
    if packed.size > aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND {
        return Err(Error::refused(format!(
            "{} is {} bytes, over the release host's 2 GiB per-asset limit: nothing is signed",
            ctx.shown(&asset),
            packed.size
        )));
    }

    let header = Header {
        program: program.to_string(),
        version: request.version.clone(),
        build: request.build,
        exposes: Vec::new(),
        shim_env: Vec::new(),
        harness: None,
    };
    let row = TreeRow {
        triple: request.triple,
        key: key.to_string(),
        key_inputs,
        asset: asset_name,
        sha256: packed.sha256.clone(),
        tree_root: packed.tree_root.clone(),
        size: packed.size,
        disk_installed: packed.disk_installed,
    };
    let doc = pkgdoc::fresh_tree(&header, &row)?;
    let manifest = stage_dir.join(pkgdoc::manifest_name(program, request.build));
    let signature =
        pack::write_tree_manifest(ctx, &doc, &header, &row, &manifest, signer.as_ref())?;

    let manifest_bytes =
        crate::identity::read_regular_bytes(&manifest, crate::identity::MAX_META_BYTES)
            .map_err(|e| Error::io(ctx.shown(&manifest), &e))?;
    let parsed = if let (Some(s), Some(sig)) = (&signer, &signature) {
        let sig_bytes = crate::identity::read_regular_bytes(sig, crate::identity::MAX_META_BYTES)
            .map_err(|e| Error::io(ctx.shown(sig), &e))?;
        s.authority().verify_pkg(&manifest_bytes, &sig_bytes)?
    } else {
        let text = std::str::from_utf8(&manifest_bytes)
            .map_err(|e| Error::refused(format!("manifest UTF-8: {e}")))?;
        aterm_toml::from_str::<atpkg::PkgManifest>(text)
            .map_err(|e| Error::refused(format!("manifest parse: {e}")))?
    };
    pkgdoc::check_parsed_tree(&parsed, &header, &row)?;
    check_pack(def, &key, request.triple, &parsed, &asset)?;
    eprintln!(
        "atpkg-publish: self-check OK — the client installs the pack under key {key} and \
         refuses it under any other"
    );

    let mut uploaded = false;
    if request.upload {
        let signature = signature
            .clone()
            .ok_or_else(|| Error::refused("the manifest was not signed"))?;
        upload::preflight(&asset, &row.sha256, &[manifest.clone(), signature.clone()])?;
        let notes = format!(
            "{program} {} — a keyed build tree (key {key}), installed whole under its key and \
             handed out as copy-on-write clones (atpkg::keyed_tree)",
            request.version
        );
        uploaded = upload::publish(
            ctx,
            def.repo,
            &upload::tag(program, request.build),
            &manifest,
            &[asset.clone(), manifest.clone(), signature],
            &notes,
        )? == upload::Uploaded::Staged;
    }
    Ok(TreeOutcome {
        program: program.to_string(),
        key: key.to_string(),
        build: request.build,
        asset,
        manifest,
        signature,
        uploaded,
    })
}

/// The text at `path` the key is the SHA-256 of: at most [`atpkg::keyed_tree::KEY_INPUTS_MAX`]
/// bytes of UTF-8 that hash to `key`.
/// # Errors
/// An unreadable, oversized or non-UTF-8 file, or one that does not hash to `key`.
fn read_key_inputs(ctx: &Ctx, path: &Path, key: &atpkg::keyed_tree::TreeKey) -> Result<String> {
    let max = u64::try_from(atpkg::keyed_tree::KEY_INPUTS_MAX).unwrap_or(u64::MAX);
    let text = crate::identity::read_regular_text(path, max).map_err(|e| {
        Error::refused(format!(
            "--key-inputs {}: {e} (UTF-8 text of at most {max} bytes)",
            ctx.shown(path)
        ))
    })?;
    let got = atpkg::keyed_tree::TreeKey::of_inputs(text.as_bytes());
    if text.is_empty() || &got != key {
        return Err(Error::refused(format!(
            "--key-inputs {} hashes to {got}, not --tree-key {key}: a key is the SHA-256 of what \
             the tree was built from, and a tree is never signed for a key its inputs do not make",
            ctx.shown(path)
        )));
    }
    Ok(text)
}

/// For a program whose build writes a completion stamp at the tree's root
/// ([`atpkg::keyed_tree::KeyedProgram::stamp`]), the stamp must be there and `key_inputs` must
/// name its text: a tree is signed only for the inputs its own build recorded.
/// # Errors
/// A missing or unreadable stamp, or inputs that do not name it.
fn check_stamp(ctx: &Ctx, program: &str, tree: &Path, key_inputs: &str) -> Result<()> {
    let Some(stamp) = atpkg::keyed_tree::program(program).and_then(|p| p.stamp) else {
        return Ok(());
    };
    let at = tree.join(stamp);
    let max = u64::try_from(atpkg::keyed_tree::KEY_INPUTS_MAX).unwrap_or(u64::MAX);
    let text = crate::identity::read_regular_text(&at, max).map_err(|e| {
        Error::refused(format!(
            "the tree {} has no readable {stamp} ({e}): only a build that finished is packed",
            ctx.shown(tree)
        ))
    })?;
    let recorded = text.trim();
    if recorded.is_empty() || !key_inputs.contains(recorded) {
        return Err(Error::refused(format!(
            "--key-inputs does not name the tree's {stamp} ({recorded:?}): the key must cover \
             what the build that wrote this tree recorded it was built from"
        )));
    }
    Ok(())
}

/// The client's install of one tree pack, in a scratch store: `pkg` (already verified under the
/// roster) is `def`'s, its `triple` row installs from `archive` under `key` through
/// `atpkg::keyed_tree::install` — digest, extraction, tree root, the signed key — and the pack
/// is refused under a key that differs from `key` in its last digit.
/// # Errors
/// The first refusal.
pub fn check_pack(
    def: &ProgramDef,
    key: &atpkg::keyed_tree::TreeKey,
    triple: Triple,
    pkg: &atpkg::PkgManifest,
    archive: &Path,
) -> Result<()> {
    if pkg.program != def.name {
        return Err(Error::refused(format!(
            "the manifest is {}'s, not {}'s",
            pkg.program, def.name
        )));
    }
    let scratch = Scratch::new("pack-tree-check")?;
    let layout = atpkg::store::Layout {
        prefix: scratch.path().join("pkg"),
    };
    let refuse = |e: atpkg::keyed_tree::TreeError| {
        Error::refused(format!(
            "the client's install refuses {}@{} for {triple}: {e}",
            def.name, pkg.build_number
        ))
    };
    atpkg::keyed_tree::install(&layout, pkg, key, triple.as_str(), archive).map_err(refuse)?;
    let other = other_key(key);
    match atpkg::keyed_tree::install(&layout, pkg, &other, triple.as_str(), archive) {
        Err(atpkg::keyed_tree::TreeError::OtherKey { .. }) => Ok(()),
        Err(e) => Err(refuse(e)),
        Ok(_) => Err(Error::refused(format!(
            "the client installs {}@{} under key {other}, which it is not signed for",
            def.name, pkg.build_number
        ))),
    }
}

/// `key` with its last digit changed.
fn other_key(key: &atpkg::keyed_tree::TreeKey) -> atpkg::keyed_tree::TreeKey {
    let mut hex = key.as_str().to_string();
    let last = if hex.ends_with('0') { "1" } else { "0" };
    hex.replace_range(63.., last);
    atpkg::keyed_tree::TreeKey::parse(&hex).unwrap_or_else(|| key.clone())
}

/// `pack-tree`'s synopsis.
pub const SYNOPSIS: &str = "pack-tree <dir> --program llvm-tree|trust-stage0 --tree-key HEX \
     --key-inputs FILE --version V --build N [--triple T] [--key PATH] [--out DIR] \
     [--unsigned | --upload]";

/// The command line as a [`TreeRequest`].
/// # Errors
/// A usage error naming what is wrong.
pub fn parse_args(ctx: &Ctx, args: &[String]) -> Result<TreeRequest> {
    let usage = || Error::usage(SYNOPSIS);
    let mut tree = None;
    let mut program = None;
    let mut key = None;
    let mut key_inputs = None;
    let mut version = None;
    let mut build = None;
    let mut triple = programs::PIN_TRIPLE;
    let mut signing_key = None;
    let mut out = None;
    let mut signing = Signing::MachineKey;
    let mut upload = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(usage);
        match a.as_str() {
            "--program" => program = Some(value()?),
            "--tree-key" => key = Some(value()?),
            "--key-inputs" => key_inputs = Some(PathBuf::from(value()?)),
            "--version" => version = Some(value()?),
            "--build" => {
                let v = value()?;
                build = Some(
                    (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                        .then(|| v.parse::<Build>().ok())
                        .flatten()
                        .ok_or_else(|| Error::usage("--build takes a decimal build number"))?,
                );
            }
            "--triple" => triple = Triple::parse(&value()?)?,
            "--key" => signing_key = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--unsigned" => signing = Signing::Unsigned,
            "--upload" => upload = true,
            s if s.starts_with('-') => {
                return Err(Error::usage(format!("pack-tree: unknown flag `{s}`")));
            }
            s if tree.is_none() => tree = Some(PathBuf::from(s)),
            _ => return Err(usage()),
        }
    }
    Ok(TreeRequest {
        program: program.ok_or_else(|| Error::usage("pack-tree needs --program"))?,
        tree: tree.ok_or_else(usage)?,
        key: key.ok_or_else(|| {
            Error::usage("pack-tree needs --tree-key: a tree is named by what it was built from")
        })?,
        key_inputs: key_inputs.ok_or_else(|| {
            Error::usage("pack-tree needs --key-inputs: the text --tree-key is the SHA-256 of")
        })?,
        version: version.ok_or_else(|| Error::usage("pack-tree needs --version"))?,
        build: build.ok_or_else(|| Error::usage("pack-tree needs --build"))?,
        triple,
        out: out.unwrap_or_else(|| ctx.state_dir.join("pack-tree")),
        signing_key,
        signing,
        upload,
    })
}

/// `atpkg-publish pack-tree …` ([`parse_args`]).
/// # Errors
/// A usage error for a malformed command line; the pack's refusals.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let request = parse_args(ctx, args)?;
    if request.upload {
        upload::program_preflight(ctx, &request.program)?;
    }
    let o = pack_tree_with(ctx, &request, &tarwrite::Zstd)?;
    println!(
        "atpkg-publish: packed {}@{} (key {}) → {}{}",
        o.program,
        o.build,
        o.key,
        ctx.shown(&o.manifest),
        match (&o.signature, o.uploaded) {
            (None, _) => " (UNSIGNED)",
            (Some(_), true) => " (signed), uploaded to staging",
            (Some(_), false) => " (signed)",
        }
    );
    Ok(())
}
