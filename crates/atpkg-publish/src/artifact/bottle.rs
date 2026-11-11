// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish pack-bottle <program>`: stage, relocate, gate and sign an upstream bottle or
//! binary archive (`gh`, `git`) with no Rust compiler provenance block.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use atpkg::extract::{Container, ExtractOptions, SymlinkPolicy, extract_tree};

use super::pkgdoc::{self, BottleRow, Header, Kind};
use super::relocate::{self, Backend};
use super::tarwrite::{self, Compressor};
use super::{arch, bundle, pack, upload};
use crate::api::{Build, Signing};
use crate::channel::Scratch;
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::programs::{self, BottleDef, ProgramDef, Shape};
use crate::triple::{
    AARCH64_APPLE_DARWIN, AARCH64_UNKNOWN_LINUX_GNU, Triple, X86_64_APPLE_DARWIN,
    X86_64_UNKNOWN_LINUX_GNU,
};

mod git_text;
mod macos;
mod pinned;
pub(crate) mod reference;
pub mod reuse;
mod source;
pub mod upstream;

/// `pack-bottle`'s usage synopsis.
pub const SYNOPSIS: &str = "pack-bottle <program> (--archive <path> | --install-tree <dir>) --version <ver> --build <N> \
     [--triple T] [--strip-components K] [--dep-bottle PATH]… [--donor-root DIR]… \
     [--merge-from PKG] [--key PATH] [--out DIR] [--unsigned | --upload]";

/// Self-relocating `bin/git` wrapper written when `libexec/git-core/git` is staged.
const GIT_WRAPPER: &str = "#!/bin/sh\n\
__atpkg_bin_dir=$(CDPATH='' cd -- \"$(dirname -- \"$0\")\" && pwd -P) || exit 1\n\
__atpkg_root=$(dirname -- \"$__atpkg_bin_dir\")\n\
export ATERM_GIT_ROOT=\"$__atpkg_root\"\n\
export GIT_EXEC_PATH=\"${GIT_EXEC_PATH:-$__atpkg_root/libexec/git-core}\"\n\
if [ -d \"$__atpkg_root/share/git-core/templates\" ]; then\n\
  export GIT_TEMPLATE_DIR=\"${GIT_TEMPLATE_DIR:-$__atpkg_root/share/git-core/templates}\"\n\
fi\n\
exec \"$__atpkg_root/libexec/git-core/git\" \"$@\"\n";

/// Install inputs held to the archive packer's relocation and payload gates.
#[derive(Clone, Debug)]
pub enum BottleInput {
    /// An upstream install archive, extracted with the request's strip count.
    Archive(PathBuf),
    /// A producer-owned install tree, copied and held to the same relocation and payload gates.
    InstallTree(PathBuf),
}

/// What a `pack-bottle` run is asked.
#[derive(Clone, Debug)]
pub struct BottleRequest {
    /// The program (`gh` or `git`).
    pub program: String,
    /// Upstream archive or producer-built install tree.
    pub input: BottleInput,
    /// Upstream semantic version string (`2.x.y`).
    pub version: String,
    /// Monotonic channel build number (`>= 1`).
    pub build: Build,
    /// Target triple for this row.
    pub triple: Triple,
    /// Leading path components stripped on archive extraction (default `2` for `<name>/<ver>/…`).
    pub strip_components: u32,
    /// Dependency bottle archives unpacked into a scratch donor tree before relocation.
    pub dep_bottles: Vec<PathBuf>,
    /// Additional donor directories whose shared libraries may be vendored.
    pub donor_roots: Vec<PathBuf>,
    /// Existing manifest for the same `program@build` to merge another target row into.
    pub merge_from: Option<PathBuf>,
    /// Explicit machine signing key path overriding `ctx.identity.key`.
    pub key: Option<PathBuf>,
    /// Output directory (`<out>/<program>/` receives the packed assets).
    pub out: PathBuf,
    /// Sign with the machine key or write unsigned.
    pub signing: Signing,
    /// Upload the signed pack to the staging release.
    pub upload: bool,
}

/// What a `pack-bottle` run produced.
#[derive(Clone, Debug)]
pub struct BottleOutcome {
    /// The program.
    pub program: String,
    /// The build number.
    pub build: Build,
    /// The version string.
    pub version: String,
    /// The target triple packed.
    pub triple: Triple,
    /// The `.tar.zst` asset written.
    pub asset: PathBuf,
    /// The `pkg.toml` manifest written.
    pub manifest: PathBuf,
    /// The `pkg.toml.sig` signature written, when signed.
    pub signature: Option<PathBuf>,
    /// Whether the staging release was updated.
    pub uploaded: bool,
}

/// Resolve `program`'s [`Shape::Bottle`] table row and check `triple`.
///
/// # Errors
/// Usage error when `program` is unknown, not a bottle row, or does not target `triple`.
pub fn bottle_row(program: &str, triple: Triple) -> Result<(&'static ProgramDef, BottleDef)> {
    let def = programs::get(program)
        .ok_or_else(|| Error::usage(format!("`{program}` is not a program the channel pins")))?;
    let Shape::Bottle(b) = def.shape else {
        return Err(Error::usage(format!(
            "{program} is not a bottle row: plain programs pack with `pack`, sysroot bundles \
             with `pack-bundle`, ALab packages with `pack-package`"
        )));
    };
    if !b.darwin.contains(&triple) && !b.linux.contains(&triple) {
        return Err(Error::usage(format!(
            "{program} is not packed as a bottle on {triple} (targets: {})",
            b.darwin
                .iter()
                .chain(b.linux)
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok((def, b))
}

fn host_triple() -> Result<Triple> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok(AARCH64_APPLE_DARWIN)
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Ok(X86_64_APPLE_DARWIN)
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok(X86_64_UNKNOWN_LINUX_GNU)
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Ok(AARCH64_UNKNOWN_LINUX_GNU)
    } else {
        Err(Error::usage(
            "this host's triple is not a bottle target; pass --triple explicitly",
        ))
    }
}

fn check_version(version: &str) -> Result<&str> {
    let trimmed = version.trim();
    if trimmed.is_empty()
        || trimmed != version
        || trimmed
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '"' | '\\'))
    {
        return Err(Error::usage(format!(
            "invalid bottle --version `{version}`: expected a non-empty version token"
        )));
    }
    Ok(trimmed)
}

fn normalize_lexical(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(c) => out.push(c),
        }
    }
    Some(out)
}

fn collect_symlinks(root: &Path) -> Result<Vec<PathBuf>> {
    let mut links = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| Error::io(format!("read {}", dir.display()), &e))?;
        for entry in entries {
            let entry = entry.map_err(|e| Error::io(format!("read {}", dir.display()), &e))?;
            let ft = entry
                .file_type()
                .map_err(|e| Error::io(format!("stat {}", entry.path().display()), &e))?;
            let path = entry.path();
            if ft.is_symlink() {
                links.push(path);
            } else if ft.is_dir() {
                stack.push(path);
            }
        }
    }
    links.sort();
    Ok(links)
}

/// Materialize every in-root symlink under `root` into a hard link (for regular files) or a copied
/// directory tree, refusing broken or escaping symlinks so [`tarwrite::write`] receives a
/// symlink-free stage.
///
/// # Errors
/// Refusal when a symlink escapes `root`, loops, or names a missing/special target; I/O failure.
pub fn materialize_symlinks(root: &Path) -> Result<()> {
    let canon_root = std::fs::canonicalize(root)
        .map_err(|e| Error::io(format!("canonicalize {}", root.display()), &e))?;
    for _pass in 0..8 {
        let links = collect_symlinks(&canon_root)?;
        if links.is_empty() {
            return Ok(());
        }
        for link in links {
            let mut cur = link.clone();
            let mut hops = 0usize;
            let (target_path, meta) = loop {
                if hops >= 16 {
                    return Err(Error::refused(format!(
                        "symlink loop in bottle stage at {}",
                        link.display()
                    )));
                }
                let meta = std::fs::symlink_metadata(&cur).map_err(|_| {
                    Error::refused(format!(
                        "dangling symlink in bottle stage: {} -> {}",
                        link.display(),
                        cur.display()
                    ))
                })?;
                if !meta.file_type().is_symlink() {
                    break (cur, meta);
                }
                let raw = std::fs::read_link(&cur)
                    .map_err(|e| Error::io(format!("readlink {}", cur.display()), &e))?;
                if raw.is_absolute() {
                    return Err(Error::refused(format!(
                        "absolute symlink in bottle stage: {} -> {}",
                        cur.display(),
                        raw.display()
                    )));
                }
                let joined = cur.parent().unwrap_or(&canon_root).join(&raw);
                let Some(norm) = normalize_lexical(&joined) else {
                    return Err(Error::refused(format!(
                        "escaping symlink in bottle stage: {} -> {}",
                        cur.display(),
                        raw.display()
                    )));
                };
                if !norm.starts_with(&canon_root) {
                    return Err(Error::refused(format!(
                        "symlink escapes bottle stage: {} -> {}",
                        cur.display(),
                        raw.display()
                    )));
                }
                cur = norm;
                hops += 1;
            };
            std::fs::remove_file(&link)
                .map_err(|e| Error::io(format!("rm symlink {}", link.display()), &e))?;
            if meta.is_file() {
                std::fs::hard_link(&target_path, &link).map_err(|e| {
                    Error::io(
                        format!("hard-link {} -> {}", link.display(), target_path.display()),
                        &e,
                    )
                })?;
            } else if meta.is_dir() {
                bundle::copy_tree(&target_path, &link)?;
            } else {
                return Err(Error::refused(format!(
                    "symlink {} targets special file {}",
                    link.display(),
                    target_path.display()
                )));
            }
        }
    }
    Err(Error::refused(
        "nested directory symlinks in bottle stage exceeded 8 materialization passes",
    ))
}

fn replace_bytes(haystack: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    if needle.is_empty() {
        return haystack.to_vec();
    }
    let mut out = Vec::with_capacity(haystack.len());
    let mut rest = haystack;
    while let Some(pos) = rest.windows(needle.len()).position(|w| w == needle) {
        out.extend_from_slice(&rest[..pos]);
        out.extend_from_slice(replacement);
        rest = &rest[pos + needle.len()..];
    }
    out.extend_from_slice(rest);
    out
}

fn inode_key(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Some((meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

const MAX_BOTTLE_TEXT_BYTES: u64 = 64 * 1024 * 1024;

/// Rewrite Homebrew interpreter placeholders in NUL-free regular files inside `stage` once per
/// hard-link inode.
///
/// # Errors
/// I/O failure reading or updating a text file.
pub fn rewrite_bottle_text(stage: &Path) -> Result<()> {
    let files = relocate::walk_files(stage)
        .map_err(|e| Error::refused(format!("walk {}: {e}", stage.display())))?;
    let mut seen: BTreeSet<(u64, u64)> = BTreeSet::new();
    let pairs: &[(&[u8], &[u8])] = &[
        (b"@@HOMEBREW_PERL@@", b"/usr/bin/perl"),
        (b"@@HOMEBREW_PREFIX@@/bin/bash", b"/bin/bash"),
        (b"@@HOMEBREW_PREFIX@@/bin/sh", b"/bin/sh"),
    ];
    for rel in files {
        let path = stage.join(&rel);
        let (file, meta) = crate::identity::open_regular_nonblock(&path)
            .map_err(|e| Error::io(format!("stat {}", path.display()), &e))?;
        if let Some(key) = inode_key(&meta)
            && !seen.insert(key)
        {
            continue;
        }
        if meta.len() > MAX_BOTTLE_TEXT_BYTES {
            use std::io::Read as _;
            let mut prefix = Vec::new();
            (&file)
                .take(64 * 1024)
                .read_to_end(&mut prefix)
                .map_err(|error| Error::io(format!("read {} prefix", path.display()), &error))?;
            // A NUL already proves this is not a rewriteable text file. Large
            // compiler libraries need no full read; ambiguous large files still
            // reach the same bounded-reader refusal below.
            if prefix.contains(&0) {
                continue;
            }
        }
        let bytes = crate::identity::read_regular_file_bytes(file, &meta, MAX_BOTTLE_TEXT_BYTES)
            .map_err(|e| Error::io(format!("read {}", path.display()), &e))?;
        if bytes.contains(&0) || !bytes.windows(10).any(|w| w == b"@@HOMEBREW") {
            continue;
        }
        let mut updated = bytes;
        let mut changed = false;
        for (old, new) in pairs {
            if updated.windows(old.len()).any(|w| w == *old) {
                let replaced = replace_bytes(&updated, old, new);
                if replaced != updated {
                    changed = true;
                    updated = replaced;
                }
            }
        }
        if changed {
            use std::io::Write as _;
            let perms = meta.permissions();
            #[cfg(unix)]
            {
                let _ = crate::identity::set_regular_mode(&path, 0o644);
            }
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
            }
            let mut f = opts
                .open(&path)
                .map_err(|e| Error::io(format!("open {}", path.display()), &e))?;
            if !f
                .metadata()
                .map_err(|e| Error::io(format!("stat {}", path.display()), &e))?
                .is_file()
            {
                return Err(Error::refused(format!(
                    "bottle: {} is not a regular file",
                    path.display()
                )));
            }
            f.set_len(0)
                .and_then(|()| f.write_all(&updated))
                .map_err(|e| Error::io(format!("write {}", path.display()), &e))?;
            let _ = f.set_permissions(perms);
        }
    }
    Ok(())
}

fn is_shared_lib_name(name: &str) -> bool {
    name.ends_with(".dylib") || name.contains(".so.") || name.ends_with(".so")
}

fn basename_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn collect_donor_libs(donor_roots: &[PathBuf]) -> Result<BTreeMap<String, PathBuf>> {
    let mut libs = BTreeMap::new();
    for root in donor_roots {
        if !root.is_dir() {
            continue;
        }
        let files = relocate::walk_files(root)
            .map_err(|e| Error::refused(format!("walk donor {}: {e}", root.display())))?;
        for rel in files {
            if let Some(name) = rel.file_name().and_then(|n| n.to_str())
                && is_shared_lib_name(name)
            {
                libs.entry(name.to_string())
                    .or_insert_with(|| root.join(&rel));
            }
        }
    }
    Ok(libs)
}

fn preserve_donor_licenses(stage: &Path, donors: &[PathBuf]) -> Result<()> {
    for (index, root) in donors.iter().enumerate() {
        for relative in relocate::walk_files(root).map_err(Error::refused)? {
            let name = relative
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !["copying", "copyright", "license", "licence", "notice"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
            {
                continue;
            }
            let destination = stage
                .join("share/licenses/upstream")
                .join(index.to_string())
                .join(&relative);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| Error::io("create donor license directory", &error))?;
            }
            crate::identity::copy_regular_file(&root.join(&relative), &destination)
                .map_err(|error| Error::io("preserve upstream donor license", &error))?;
        }
    }
    Ok(())
}

fn preserve_gnu_git_trust(stage: &Path, donors: &[PathBuf]) -> Result<()> {
    let mut bundle = None;
    for root in donors {
        for relative in relocate::walk_files(root).map_err(Error::refused)? {
            if !relative.ends_with("share/ca-certificates/cacert.pem") {
                continue;
            }
            let bytes = crate::identity::read_regular_bytes(&root.join(relative), 4 * 1024 * 1024)
                .map_err(|error| Error::io("read declared certificate bundle", &error))?;
            if !bytes
                .windows(b"-----BEGIN CERTIFICATE-----".len())
                .any(|part| part == b"-----BEGIN CERTIFICATE-----")
            {
                return Err(Error::refused(
                    "declared ca-certificates bundle contains no PEM certificates",
                ));
            }
            if bundle.as_ref().is_some_and(|previous| previous != &bytes) {
                return Err(Error::refused(
                    "declared donors contain different ca-certificates bundles",
                ));
            }
            bundle = Some(bytes);
        }
    }
    let bundle = bundle.ok_or_else(|| {
        Error::refused("GNU Git needs the declared ca-certificates bottle for portable HTTPS trust")
    })?;
    std::fs::create_dir_all(stage.join("share/ca-certificates"))
        .and_then(|()| std::fs::create_dir_all(stage.join("etc")))
        .map_err(|error| Error::io("create GNU Git trust directories", &error))?;
    std::fs::write(stage.join("share/ca-certificates/cacert.pem"), bundle)
        .and_then(|()| std::fs::write(stage.join("etc/gitconfig"),
            b"[http]\n\tsslCAInfo = %(prefix)/share/ca-certificates/cacert.pem\n[include]\n\tpath = /etc/gitconfig\n"))
        .map_err(|error| Error::io("write GNU Git default trust configuration", &error))
}

fn relink_group(primary: &Path, aliases: &[PathBuf]) -> Result<()> {
    let primary_key = std::fs::symlink_metadata(primary)
        .ok()
        .and_then(|m| inode_key(&m));
    for alias in aliases {
        let alias_key = std::fs::symlink_metadata(alias)
            .ok()
            .and_then(|m| inode_key(&m));
        if primary_key.is_some() && primary_key == alias_key {
            continue;
        }
        super::relink::replace(primary, alias)
            .map_err(|e| Error::io(format!("relink {}", alias.display()), &e))?;
    }
    Ok(())
}

/// Resolve `@@HOMEBREW_*@@` load-command ids, dependencies and rpaths in `stage` through `backend`,
/// vendoring any needed shared libraries from `donor_roots` into `lib/atpkg-vendored/`.
///
/// # Errors
/// Refusal when a `@@HOMEBREW_*@@` dependency is missing from both `stage` and `donor_roots`, or
/// when a load-command rewrite fails.
pub fn rewrite_bottle_objects(
    stage: &Path,
    donor_roots: &[PathBuf],
    triple: Triple,
    backend: &dyn Backend,
) -> Result<Vec<String>> {
    let donor_libs = collect_donor_libs(donor_roots)?;
    let vendored_dir = stage.join(relocate::VENDOR_REL);
    let files = relocate::walk_files(stage)
        .map_err(|e| Error::refused(format!("walk {}: {e}", stage.display())))?;

    let mut stage_libs: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut vendored: BTreeSet<String> = BTreeSet::new();
    let mut group_first: BTreeMap<(u64, u64), PathBuf> = BTreeMap::new();
    let mut aliases: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    let mut queue: Vec<PathBuf> = Vec::new();

    for rel in &files {
        let path = stage.join(rel);
        if let Some(name) = rel.file_name().and_then(|n| n.to_str())
            && is_shared_lib_name(name)
        {
            stage_libs
                .entry(name.to_string())
                .or_insert_with(|| path.parent().unwrap_or(stage).to_path_buf());
        }
        if !backend.is_native_object(&path) {
            continue;
        }
        if let Ok(meta) = std::fs::symlink_metadata(&path)
            && meta.is_file()
            && meta.len() > 0
            && let Some(key) = inode_key(&meta)
        {
            match group_first.entry(key) {
                std::collections::btree_map::Entry::Occupied(first) => {
                    aliases.entry(first.get().clone()).or_default().push(path);
                    continue;
                }
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert(path.clone());
                }
            }
        }
        queue.push(path);
    }

    let mut processed: BTreeSet<PathBuf> = BTreeSet::new();
    while let Some(obj) = queue.pop() {
        if !processed.insert(obj.clone()) {
            continue;
        }
        let refs = backend.read_refs(&obj).map_err(Error::refused)?;
        let group = aliases.get(&obj).map_or(&[][..], Vec::as_slice);
        let mut changed = false;

        if let Some(loader) = system_loader(triple)
            && let Some(interpreter) = backend.interpreter(&obj).map_err(Error::refused)?
        {
            if interpreter == "@@HOMEBREW_PREFIX@@/lib/ld.so" {
                #[cfg(unix)]
                crate::identity::set_regular_mode(&obj, 0o755)
                    .map_err(|error| Error::io("make GNU interpreter rewrite writable", &error))?;
                backend
                    .set_interpreter(&obj, loader)
                    .map_err(Error::refused)?;
                changed = true;
            } else if interpreter != loader {
                return Err(Error::refused(format!(
                    "{}: GNU bottle interpreter `{interpreter}` is not the target system loader `{loader}`",
                    obj.display()
                )));
            }
        }

        #[cfg(unix)]
        {
            if refs
                .id
                .as_deref()
                .is_some_and(|id| id.starts_with("@@HOMEBREW_"))
                || refs.needed.iter().any(|d| d.starts_with("@@HOMEBREW_"))
                || refs.rpaths.iter().any(|r| r.starts_with("@@HOMEBREW_"))
            {
                let _ = crate::identity::set_regular_mode(&obj, 0o755);
            }
        }

        if let Some(id) = refs.id.as_deref()
            && id.starts_with("@@HOMEBREW_")
        {
            backend
                .set_id_portable(&obj, basename_of(id))
                .map_err(Error::refused)?;
            changed = true;
        }

        let mut needed_lib_dirs: Vec<PathBuf> = Vec::new();
        for dep in &refs.needed {
            if !dep.starts_with("@@HOMEBREW_")
                && !(system_loader(triple).is_some() && donor_libs.contains_key(basename_of(dep)))
            {
                continue;
            }
            let base = basename_of(dep).to_string();
            let lib_dir = if let Some(existing_dir) = stage_libs.get(&base) {
                existing_dir.clone()
            } else {
                let src = donor_libs.get(&base).ok_or_else(|| {
                    Error::refused(format!(
                        "bottle dependency `{dep}` in {} names `{base}`, which is in neither \
                         the archive nor any --dep-bottle / --donor-root",
                        obj.strip_prefix(stage).unwrap_or(&obj).display()
                    ))
                })?;
                if atpkg::lineage::stock_location(src).is_some() {
                    return Err(Error::refused(format!(
                        "refusing to vendor `{base}` from stock location {}",
                        src.display()
                    )));
                }
                std::fs::create_dir_all(&vendored_dir)
                    .map_err(|e| Error::io(format!("mkdir {}", vendored_dir.display()), &e))?;
                let dest = vendored_dir.join(&base);
                crate::identity::copy_regular_file(src, &dest)
                    .map_err(|e| Error::io(format!("copy donor {}", src.display()), &e))?;
                #[cfg(unix)]
                {
                    let _ = crate::identity::set_regular_mode(&dest, 0o755);
                }
                stage_libs.insert(base.clone(), vendored_dir.clone());
                vendored.insert(base.clone());
                queue.push(dest);
                vendored_dir.clone()
            };
            if !needed_lib_dirs.contains(&lib_dir) {
                needed_lib_dirs.push(lib_dir);
            }
            backend
                .repoint_dep(&obj, dep, &base)
                .map_err(Error::refused)?;
            changed = true;
        }

        let hb_rpaths: Vec<String> = refs
            .rpaths
            .iter()
            .filter(|r| r.starts_with("@@HOMEBREW_"))
            .cloned()
            .collect();
        let mut keep: Vec<String> = refs
            .rpaths
            .iter()
            .filter(|r| !r.starts_with("@@HOMEBREW_"))
            .cloned()
            .collect();
        if !hb_rpaths.is_empty() && needed_lib_dirs.is_empty() {
            let fallback = if vendored_dir.is_dir() {
                vendored_dir.clone()
            } else if stage.join("lib").is_dir() {
                stage.join("lib")
            } else {
                vendored_dir.clone()
            };
            needed_lib_dirs.push(fallback);
        }
        if !hb_rpaths.is_empty() || !needed_lib_dirs.is_empty() {
            let mut dirs: Vec<&Path> = vec![obj.parent().unwrap_or(stage)];
            for alias in group {
                let d = alias.parent().unwrap_or(stage);
                if !dirs.contains(&d) {
                    dirs.push(d);
                }
            }
            let mut drop_list = hb_rpaths;
            let mut first = true;
            for dir in &dirs {
                for lib_dir in &needed_lib_dirs {
                    let rel = relocate::origin_relative(backend.origin_token(), dir, lib_dir);
                    if !first && keep.contains(&rel) && drop_list.is_empty() {
                        continue;
                    }
                    first = false;
                    backend
                        .fix_rpaths(&obj, &rel, &keep, &drop_list)
                        .map_err(Error::refused)?;
                    if !keep.contains(&rel) {
                        keep.push(rel);
                    }
                    drop_list.clear();
                    changed = true;
                }
            }
        }

        if changed {
            backend.resign(&obj, None).map_err(Error::refused)?;
            relink_group(&obj, group)?;
        }
    }
    Ok(vendored.into_iter().collect())
}

fn system_loader(triple: Triple) -> Option<&'static str> {
    match triple {
        X86_64_UNKNOWN_LINUX_GNU => Some("/lib64/ld-linux-x86-64.so.2"),
        AARCH64_UNKNOWN_LINUX_GNU => Some("/lib/ld-linux-aarch64.so.1"),
        _ => None,
    }
}

/// Install the self-relocating `bin/git` wrapper when `program == "git"` and `libexec/git-core` is
/// present, unlinking `bin/git` first so a hard-linked `libexec/git-core/git` binary inode is
/// never truncated in place.
///
/// # Errors
/// I/O failure or missing `git` binary when `libexec/git-core` exists.
pub fn install_git_wrapper(program: &str, stage: &Path) -> Result<bool> {
    if program != "git" {
        return Ok(false);
    }
    let git_core = stage.join("libexec/git-core");
    if !git_core.is_dir() {
        return Ok(false);
    }
    let bin_dir = stage.join("bin");
    let bin_git = bin_dir.join("git");
    let core_git = git_core.join("git");
    if !core_git.is_file() {
        if !bin_git.is_file() {
            return Err(Error::refused(
                "git bottle has libexec/git-core/ without either libexec/git-core/git or bin/git",
            ));
        }
        std::fs::hard_link(&bin_git, &core_git)
            .map_err(|e| Error::io(format!("link {}", core_git.display()), &e))?;
    }
    std::fs::create_dir_all(&bin_dir)
        .map_err(|e| Error::io(format!("mkdir {}", bin_dir.display()), &e))?;
    if std::fs::symlink_metadata(&bin_git).is_ok() {
        std::fs::remove_file(&bin_git)
            .map_err(|e| Error::io(format!("rm {}", bin_git.display()), &e))?;
    }
    {
        use std::io::Write as _;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o755)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let mut f = opts
            .open(&bin_git)
            .map_err(|e| Error::io(format!("open {}", bin_git.display()), &e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            f.set_permissions(std::fs::Permissions::from_mode(0o755))
                .map_err(|e| Error::io(format!("chmod {}", bin_git.display()), &e))?;
        }
        f.write_all(GIT_WRAPPER.as_bytes())
            .map_err(|e| Error::io(format!("write {}", bin_git.display()), &e))?;
    }
    Ok(true)
}

/// Removes `git-credential-osxkeychain` from a staged git bottle. A bottle's binaries are signed
/// ad hoc, so each build's helper has an identity of its own that no keychain item's access list
/// names: a lookup raises macOS's consent dialog and git waits on it, which the owner ruled must
/// not happen (2026-11-03; `docs/DESIGN-init-system-2026-10-20.md` §1.18). Git asked for a helper
/// it lacks says so and goes on.
///
/// # Errors
/// I/O failure removing a helper that is present.
fn drop_keychain_helper(stage: &Path) -> Result<()> {
    for dir in ["libexec/git-core", "bin"] {
        let helper = stage.join(dir).join("git-credential-osxkeychain");
        match std::fs::remove_file(&helper) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::io(format!("rm {}", helper.display()), &e)),
        }
    }
    Ok(())
}

fn check_stage_tree(
    program: &str,
    stage: &Path,
    triple: Triple,
    exposes: &[&str],
    backend: &dyn Backend,
) -> Result<usize> {
    for tool in exposes {
        let entry = stage.join("bin").join(tool);
        if !atpkg::toolchain::is_executable_file(&entry) {
            return Err(Error::refused(format!(
                "staged bottle lacks executable bin/{tool} ({})",
                entry.display()
            )));
        }
    }
    let files = relocate::walk_files(stage)
        .map_err(|e| Error::refused(format!("walk {}: {e}", stage.display())))?;
    let mut leftover_placeholder = None;
    let mut objects = Vec::new();
    for rel in files {
        let path = stage.join(&rel);
        if let Ok(bytes) = crate::identity::read_regular_bytes(&path, 64 * 1024 * 1024)
            && !bytes.contains(&0)
            && bytes.windows(10).any(|w| w == b"@@HOMEBREW")
        {
            leftover_placeholder = Some(rel.display().to_string());
            break;
        }
        if arch::read(&path).is_some() {
            if matches!(program, "git" | "openssl-signing") {
                let bytes = crate::identity::read_regular_bytes(&path, 64 * 1024 * 1024)
                    .map_err(|error| Error::io(format!("read {}", path.display()), &error))?;
                macos::check(&bytes, program)
                    .map_err(|why| Error::refused(format!("{}: {why}", rel.display())))?;
            }
            if backend.is_native_object(&path) {
                if let Some(loader) = system_loader(triple)
                    && let Some(actual) = backend.interpreter(&path).map_err(Error::refused)?
                    && actual != loader
                {
                    return Err(Error::refused(format!(
                        "{}: GNU Git interpreter is `{actual}`, expected `{loader}`",
                        rel.display()
                    )));
                }
                let refs = backend.read_refs(&path).map_err(Error::refused)?;
                let has_hb = refs
                    .id
                    .as_deref()
                    .is_some_and(|id| id.contains("@@HOMEBREW"))
                    || refs.needed.iter().any(|d| d.contains("@@HOMEBREW"))
                    || refs.rpaths.iter().any(|r| r.contains("@@HOMEBREW"));
                if has_hb {
                    leftover_placeholder = Some(rel.display().to_string());
                    break;
                }
            }
            objects.push(path);
        }
    }
    if let Some(rel) = leftover_placeholder {
        return Err(Error::refused(format!(
            "staged bottle still carries an unrewritten @@HOMEBREW_*@@ placeholder in {rel}"
        )));
    }
    Ok(arch::gate(triple, &objects)?)
}

/// Pack a bottle archive using explicit relocation and compression backends (the seam tests drive).
///
/// # Errors
/// Any extraction, relocation, arch-gate, manifest, client self-check, or upload refusal.
pub fn pack_bottle_with(
    ctx: &Ctx,
    request: &BottleRequest,
    backend: &dyn Backend,
    compressor: &dyn Compressor,
) -> Result<BottleOutcome> {
    let program = request.program.as_str();
    let (def, bottle_def) = bottle_row(program, request.triple)?;
    let version = check_version(&request.version)?.to_string();
    if request.build == 0 {
        return Err(Error::usage("--build must be >= 1"));
    }
    if request.signing == Signing::Unsigned && request.upload {
        return Err(Error::usage(
            "--unsigned and --upload together make no sense — nothing unsigned may publish",
        ));
    }
    if matches!(bottle_def.origin, programs::BottleOrigin::Reference(_)) {
        reference::check_request(request)?;
    }
    let (input, directory) = match &request.input {
        BottleInput::Archive(path) => (path, false),
        BottleInput::InstallTree(path) => (path, true),
    };
    if !std::fs::symlink_metadata(input).is_ok_and(|metadata| {
        if directory {
            metadata.is_dir()
        } else {
            metadata.is_file()
        }
    }) {
        return Err(Error::refused(format!(
            "the package input {} is not a regular {}",
            ctx.shown(input),
            if directory { "install tree" } else { "archive" }
        )));
    }
    let base = request
        .merge_from
        .as_ref()
        .map(|p| {
            crate::identity::read_regular_bytes(p, crate::identity::MAX_META_BYTES)
                .map_err(|e| Error::io(format!("read {}", ctx.shown(p)), &e))
        })
        .transpose()?;

    let mut ctx = ctx.clone();
    if let Some(key) = &request.key {
        ctx.identity.key.clone_from(key);
    }
    let ctx = &ctx;
    let deferred_reference_signing = request.signing == Signing::MachineKey
        && matches!(bottle_def.origin, programs::BottleOrigin::Reference(_));
    let signer = match request.signing {
        Signing::MachineKey if deferred_reference_signing => None,
        Signing::MachineKey => Some(pack::Signer::load(ctx)?),
        Signing::Unsigned => None,
    };

    let scratch = Scratch::new("pack-bottle")?;
    let stage = scratch.path().join("stage");
    std::fs::create_dir_all(&stage)
        .map_err(|e| Error::io(format!("mkdir {}", stage.display()), &e))?;
    if directory {
        bundle::copy_tree(input, &stage)?;
    } else {
        extract_tree(
            input,
            &stage,
            archive_container(input)?,
            MAX_BOTTLE_EXTRACT_BYTES,
            ExtractOptions {
                strip_components: request.strip_components,
                symlinks: SymlinkPolicy::InRoot,
                allow_global_pax: true,
            },
        )
        .map_err(|e| Error::refused(format!("extract {}: {e}", ctx.shown(input))))?;
    }

    let mut all_donors = Vec::new();
    for (idx, dep_archive) in request.dep_bottles.iter().enumerate() {
        if !dep_archive.is_file() {
            return Err(Error::refused(format!(
                "--dep-bottle {} is not a regular file",
                ctx.shown(dep_archive)
            )));
        }
        let dep_dir = scratch.path().join(format!("dep-{idx}"));
        std::fs::create_dir_all(&dep_dir)
            .map_err(|e| Error::io(format!("mkdir {}", dep_dir.display()), &e))?;
        extract_tree(
            dep_archive,
            &dep_dir,
            archive_container(dep_archive)?,
            MAX_BOTTLE_EXTRACT_BYTES,
            ExtractOptions {
                strip_components: 0,
                symlinks: SymlinkPolicy::InRoot,
                allow_global_pax: true,
            },
        )
        .map_err(|e| Error::refused(format!("extract dep {}: {e}", ctx.shown(dep_archive))))?;
        materialize_symlinks(&dep_dir)?;
        all_donors.push(
            std::fs::canonicalize(&dep_dir)
                .map_err(|e| Error::io(format!("canonicalize {}", dep_dir.display()), &e))?,
        );
    }
    for root in &request.donor_roots {
        let canon = std::fs::canonicalize(root)
            .map_err(|e| Error::refused(format!("--donor-root {} ({e})", ctx.shown(root))))?;
        all_donors.push(canon);
    }

    materialize_symlinks(&stage)?;
    if let programs::BottleOrigin::Reference(recipe) = bottle_def.origin {
        reference::prepare(recipe, request, &stage)?;
    }
    preserve_donor_licenses(&stage, &all_donors)?;
    if program == "git" && system_loader(request.triple).is_some() {
        preserve_gnu_git_trust(&stage, &all_donors)?;
    }
    rewrite_bottle_text(&stage)?;
    if program == "git" {
        drop_keychain_helper(&stage)?;
        git_text::rewrite(&stage, &version)?;
    }
    let _wrapped = install_git_wrapper(program, &stage)?;
    let mut all_vendored = rewrite_bottle_objects(&stage, &all_donors, request.triple, backend)?;
    let report =
        relocate::relocate_with(&stage, backend, None, &all_donors).map_err(Error::refused)?;
    report.require_self_contained().map_err(Error::refused)?;
    for lib in &report.vendored {
        if !all_vendored.contains(lib) {
            all_vendored.push(lib.clone());
        }
    }
    let objects = check_stage_tree(program, &stage, request.triple, bottle_def.exposes, backend)?;
    eprintln!(
        "atpkg-publish: bottle stage OK — {program} {version} ({}) gated {objects} object(s), \
         vendored {} lib(s)",
        request.triple,
        all_vendored.len()
    );

    let _lock = pack::pack_lock(ctx, program, pack::LOCK_WAIT)?;
    let out_dir = request.out.join(program);
    if base.is_none() && std::fs::symlink_metadata(&out_dir).is_ok() {
        std::fs::remove_dir_all(&out_dir)
            .map_err(|e| Error::io(format!("rm {}", ctx.shown(&out_dir)), &e))?;
    }
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| Error::io(format!("mkdir {}", ctx.shown(&out_dir)), &e))?;

    let asset_name = pkgdoc::asset_name(program, request.build, request.triple);
    let asset = out_dir.join(&asset_name);
    let packed = tarwrite::write_with(&stage, &asset, compressor)?;
    let exposes: Vec<String> = bottle_def
        .exposes
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let header = Header {
        program: program.to_string(),
        version: version.clone(),
        build: request.build,
        exposes,
        shim_env: pinned::invocation_environment(&bottle_def, &version),
    };
    let row = BottleRow {
        triple: request.triple,
        kind: if system_loader(request.triple).is_some() {
            Kind::ElfBinary
        } else {
            Kind::Binary
        },
        asset: asset_name,
        sha256: packed.sha256.clone(),
        tree_root: packed.tree_root.clone(),
        size: packed.size,
        disk_installed: packed.disk_installed,
    };
    let doc = match base {
        Some(existing) => pkgdoc::merge_bottle(existing, &header, &row)?,
        None => pkgdoc::fresh_bottle(&header, &row)?,
    };
    let manifest = out_dir.join(pkgdoc::manifest_name(program, request.build));
    let mut signature =
        pack::write_bottle_manifest(ctx, &doc, &header, &row, &manifest, signer.as_ref())?;

    // Publication self-check: authenticate and inspect a scratch tree without marking it live.
    let manifest_bytes =
        crate::identity::read_regular_bytes(&manifest, crate::identity::MAX_META_BYTES)
            .map_err(|e| Error::io(ctx.shown(&manifest), &e))?;
    let parsed = if let (Some(s), Some(sig_path)) = (&signer, &signature) {
        let sig_bytes =
            crate::identity::read_regular_bytes(sig_path, crate::identity::MAX_META_BYTES)
                .map_err(|e| Error::io(ctx.shown(sig_path), &e))?;
        s.authority().verify_pkg(&manifest_bytes, &sig_bytes)?
    } else {
        let text = std::str::from_utf8(&manifest_bytes)
            .map_err(|e| Error::refused(format!("manifest UTF-8: {e}")))?;
        aterm_toml::from_str::<atpkg::PkgManifest>(text)
            .map_err(|e| Error::refused(format!("manifest parse: {e}")))?
    };
    pkgdoc::check_parsed_bottle(&parsed, &header, &row)?;
    let artifact = parsed
        .artifacts
        .iter()
        .find(|a| a.target == request.triple.as_str())
        .ok_or_else(|| Error::refused("missing target row in self-checked manifest"))?;
    let self_check = Scratch::new("bottle-check")?;
    let check_dir = self_check
        .path()
        .join("store")
        .join(program)
        .join(request.build.to_string());
    atpkg::verify_for_publication(
        artifact,
        &asset,
        &check_dir,
        &atpkg::install::StageHooks::NONE,
    )
    .map_err(|e| {
        Error::refused(format!(
            "publication verification refuses {program}@{} for {}: {e}",
            request.build, request.triple
        ))
    })?;

    if let programs::BottleOrigin::Reference(recipe) = bottle_def.origin {
        let native_controls = reference::qualify(recipe, request, &check_dir)?;
        reference::qualification::write(
            recipe,
            request,
            &manifest,
            &manifest_bytes,
            artifact,
            native_controls,
        )?;
        if deferred_reference_signing {
            signature = super::sign::sign_all(ctx, std::slice::from_ref(&manifest))?
                .into_iter()
                .next();
        }
    }

    let mut uploaded = false;
    if request.upload {
        let sig = signature
            .as_ref()
            .ok_or_else(|| Error::refused("cannot upload an unsigned manifest"))?;
        upload::preflight(&asset, &row.sha256, &[manifest.clone(), sig.clone()])?;
        let notes = format!(
            "atpkg bottle pack of {program} {version} ({}). Signed pkg manifest + self-contained tarball.",
            request.triple
        );
        uploaded = upload::publish(
            ctx,
            def.repo,
            &upload::tag(program, request.build),
            &manifest,
            &[asset.clone(), manifest.clone(), sig.clone()],
            &notes,
        )? == upload::Uploaded::Staged;
    }

    Ok(BottleOutcome {
        program: program.to_string(),
        build: request.build,
        version,
        triple: request.triple,
        asset,
        manifest,
        signature,
        uploaded,
    })
}

const MAX_BOTTLE_EXTRACT_BYTES: u64 = 1024 * 1024 * 1024;

fn archive_container(path: &Path) -> Result<Container> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let ends_with_ignore_case = |ext: &str| {
        name.len() >= ext.len()
            && name
                .get(name.len() - ext.len()..)
                .is_some_and(|s| s.eq_ignore_ascii_case(ext))
    };
    if ends_with_ignore_case(".tar.zst") {
        Ok(Container::TarZst)
    } else if ends_with_ignore_case(".tar.gz") || ends_with_ignore_case(".tgz") {
        Ok(Container::TarGz)
    } else if ends_with_ignore_case(".tar") {
        Ok(Container::Tar)
    } else if ends_with_ignore_case(".zip") {
        Ok(Container::Zip)
    } else {
        Err(Error::usage(format!(
            "unsupported bottle archive extension on {} (expected .tar.zst, .tar.gz, .tgz, .tar, or .zip)",
            path.display()
        )))
    }
}

/// Parse `pack-bottle` CLI arguments into a [`BottleRequest`].
///
/// # Errors
/// Usage error naming what is missing or malformed.
pub fn parse_args(ctx: &Ctx, args: &[String]) -> Result<BottleRequest> {
    let usage = || Error::usage(SYNOPSIS);
    let mut program = None;
    let mut input = None;
    let mut version = None;
    let mut build = None;
    let mut triple = None;
    let mut strip_components = 2u32;
    let mut dep_bottles = Vec::new();
    let mut donor_roots = Vec::new();
    let mut merge_from = None;
    let mut key = None;
    let mut out = None;
    let mut signing = Signing::MachineKey;
    let mut upload = false;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(usage);
        match arg.as_str() {
            "--archive" | "--install-tree" => {
                if input.is_some() {
                    return Err(Error::usage(
                        "pack-bottle takes exactly one --archive or --install-tree",
                    ));
                }
                let path = PathBuf::from(value()?);
                input = Some(if arg == "--archive" {
                    BottleInput::Archive(path)
                } else {
                    BottleInput::InstallTree(path)
                });
            }
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
            "--triple" => triple = Some(Triple::parse(&value()?)?),
            "--strip-components" => {
                let v = value()?;
                strip_components = (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| v.parse::<u32>().ok())
                    .flatten()
                    .ok_or_else(|| {
                        Error::usage("--strip-components takes a non-negative integer")
                    })?;
            }
            "--dep-bottle" => dep_bottles.push(PathBuf::from(value()?)),
            "--donor-root" => donor_roots.push(PathBuf::from(value()?)),
            "--merge-from" => merge_from = Some(PathBuf::from(value()?)),
            "--key" => key = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--unsigned" => signing = Signing::Unsigned,
            "--upload" => upload = true,
            s if s.starts_with('-') => {
                return Err(Error::usage(format!("pack-bottle: unknown flag `{s}`")));
            }
            s if program.is_none() => program = Some(s.to_string()),
            _ => return Err(usage()),
        }
    }

    let program = program.ok_or_else(usage)?;
    let input = input.ok_or_else(usage)?;
    let version = version.ok_or_else(usage)?;
    let build = build.ok_or_else(usage)?;
    let triple = match triple {
        Some(t) => t,
        None => host_triple()?,
    };
    let out = out.unwrap_or_else(|| ctx.state_dir.join("pack-bottle"));

    Ok(BottleRequest {
        program,
        input,
        version,
        build,
        triple,
        strip_components,
        dep_bottles,
        donor_roots,
        merge_from,
        key,
        out,
        signing,
        upload,
    })
}

/// `atpkg-publish pack-bottle …` CLI entry point.
///
/// # Errors
/// Usage error for malformed flags, missing host relocation backend, or any pack gate refusal.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let request = parse_args(ctx, args)?;
    if request.upload {
        upload::program_preflight(ctx, &request.program)?;
    }
    let backend = relocate::backend_for_host().map_err(Error::cannot_run)?;
    let outcome = pack_bottle_with(ctx, &request, backend.as_ref(), &tarwrite::Zstd)?;
    println!(
        "atpkg-publish: packed bottle {}@{} ({}) for {} → {}{}{}",
        outcome.program,
        outcome.build,
        outcome.version,
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

#[cfg(test)]
#[path = "bottle_tests.rs"]
mod tests;
