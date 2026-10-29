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

/// `pack-bottle`'s usage synopsis.
pub const SYNOPSIS: &str = "pack-bottle <program> --archive <path> --version <ver> --build <N> \
     [--triple T] [--strip-components K] [--dep-bottle PATH]… [--donor-root DIR]… \
     [--merge-from PKG] [--key PATH] [--out DIR] [--unsigned | --upload]";

/// Self-relocating `bin/git` wrapper written when `libexec/git-core/git` is staged.
const GIT_WRAPPER: &str = "#!/bin/sh\n\
__atpkg_bin_dir=$(CDPATH='' cd -- \"$(dirname -- \"$0\")\" && pwd -P) || exit 1\n\
__atpkg_root=$(dirname -- \"$__atpkg_bin_dir\")\n\
export GIT_EXEC_PATH=\"${GIT_EXEC_PATH:-$__atpkg_root/libexec/git-core}\"\n\
if [ -d \"$__atpkg_root/share/git-core/templates\" ]; then\n\
  export GIT_TEMPLATE_DIR=\"${GIT_TEMPLATE_DIR:-$__atpkg_root/share/git-core/templates}\"\n\
fi\n\
exec \"$__atpkg_root/libexec/git-core/git\" \"$@\"\n";

/// What a `pack-bottle` run is asked.
#[derive(Clone, Debug)]
pub struct BottleRequest {
    /// The program (`gh` or `git`).
    pub program: String,
    /// Upstream bottle `.tar.gz` / `.tar` / `.zip` archive.
    pub archive: PathBuf,
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
    if !b.darwin.contains(&triple) {
        return Err(Error::usage(format!(
            "{program} is not packed as a bottle on {triple} (targets: {})",
            b.darwin
                .iter()
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
            let target_path = loop {
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
                    break cur;
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
            let meta = std::fs::symlink_metadata(&target_path).map_err(|e| {
                Error::io(format!("stat symlink target {}", target_path.display()), &e)
            })?;
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
    let mut i = 0;
    while i < haystack.len() {
        if haystack[i..].starts_with(needle) {
            out.extend_from_slice(replacement);
            i += needle.len();
        } else {
            out.push(haystack[i]);
            i += 1;
        }
    }
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
        let meta = std::fs::metadata(&path)
            .map_err(|e| Error::io(format!("stat {}", path.display()), &e))?;
        if let Some(key) = inode_key(&meta)
            && !seen.insert(key)
        {
            continue;
        }
        let bytes =
            std::fs::read(&path).map_err(|e| Error::io(format!("read {}", path.display()), &e))?;
        if bytes.contains(&0) || !bytes.windows(10).any(|w| w == b"@@HOMEBREW") {
            continue;
        }
        let mut updated = bytes.clone();
        for (old, new) in pairs {
            updated = replace_bytes(&updated, old, new);
        }
        if updated != bytes {
            let perms = meta.permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
            }
            std::fs::write(&path, &updated)
                .map_err(|e| Error::io(format!("write {}", path.display()), &e))?;
            let _ = std::fs::set_permissions(&path, perms);
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

fn relink_group(primary: &Path, aliases: &[PathBuf]) -> Result<()> {
    for alias in aliases {
        std::fs::remove_file(alias)
            .map_err(|e| Error::io(format!("rm alias {}", alias.display()), &e))?;
        std::fs::hard_link(primary, alias)
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
        if let Ok(meta) = std::fs::metadata(&path)
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

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if refs
                .id
                .as_deref()
                .is_some_and(|id| id.starts_with("@@HOMEBREW_"))
                || refs.needed.iter().any(|d| d.starts_with("@@HOMEBREW_"))
                || refs.rpaths.iter().any(|r| r.starts_with("@@HOMEBREW_"))
            {
                let _ = std::fs::set_permissions(&obj, std::fs::Permissions::from_mode(0o755));
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
            if !dep.starts_with("@@HOMEBREW_") {
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
                std::fs::copy(src, &dest)
                    .map_err(|e| Error::io(format!("copy donor {}", src.display()), &e))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
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
    std::fs::write(&bin_git, GIT_WRAPPER.as_bytes())
        .map_err(|e| Error::io(format!("write {}", bin_git.display()), &e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&bin_git, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| Error::io(format!("chmod {}", bin_git.display()), &e))?;
    }
    Ok(true)
}

fn check_stage_tree(
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
        if let Ok(bytes) = std::fs::read(&path)
            && !bytes.contains(&0)
            && bytes.windows(10).any(|w| w == b"@@HOMEBREW")
        {
            leftover_placeholder = Some(rel.display().to_string());
            break;
        }
        if arch::read(&path).is_some() {
            if backend.is_native_object(&path) {
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
    if !request.archive.is_file() {
        return Err(Error::refused(format!(
            "the bottle archive {} is not a regular file",
            ctx.shown(&request.archive)
        )));
    }
    let base = request
        .merge_from
        .as_ref()
        .map(|p| std::fs::read(p).map_err(|e| Error::io(format!("read {}", ctx.shown(p)), &e)))
        .transpose()?;

    let mut ctx = ctx.clone();
    if let Some(key) = &request.key {
        ctx.identity.key.clone_from(key);
    }
    let ctx = &ctx;
    let signer = match request.signing {
        Signing::MachineKey => Some(pack::Signer::load(ctx)?),
        Signing::Unsigned => None,
    };

    let scratch = Scratch::new("pack-bottle")?;
    let stage = scratch.path().join("stage");
    std::fs::create_dir_all(&stage)
        .map_err(|e| Error::io(format!("mkdir {}", stage.display()), &e))?;
    extract_tree(
        &request.archive,
        &stage,
        archive_container(&request.archive)?,
        MAX_BOTTLE_EXTRACT_BYTES,
        ExtractOptions {
            strip_components: request.strip_components,
            symlinks: SymlinkPolicy::InRoot,
            allow_global_pax: true,
        },
    )
    .map_err(|e| Error::refused(format!("extract {}: {e}", ctx.shown(&request.archive))))?;

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
    rewrite_bottle_text(&stage)?;
    let _wrapped = install_git_wrapper(program, &stage)?;
    let mut all_vendored = rewrite_bottle_objects(&stage, &all_donors, backend)?;
    let report =
        relocate::relocate_with(&stage, backend, None, &all_donors).map_err(Error::refused)?;
    report.require_self_contained().map_err(Error::refused)?;
    for lib in &report.vendored {
        if !all_vendored.contains(lib) {
            all_vendored.push(lib.clone());
        }
    }
    let objects = check_stage_tree(&stage, request.triple, bottle_def.exposes, backend)?;
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
    };
    let row = BottleRow {
        triple: request.triple,
        kind: Kind::Binary,
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
    let signature =
        pack::write_bottle_manifest(ctx, &doc, &header, &row, &manifest, signer.as_ref())?;

    // Client self-check: parse (and signature-verify when signed) and stage through verify_and_stage.
    let manifest_bytes =
        std::fs::read(&manifest).map_err(|e| Error::io(ctx.shown(&manifest), &e))?;
    let parsed = if let (Some(s), Some(sig_path)) = (&signer, &signature) {
        let sig_bytes = std::fs::read(sig_path).map_err(|e| Error::io(ctx.shown(sig_path), &e))?;
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
    atpkg::verify_and_stage(
        artifact,
        &asset,
        &check_dir,
        &atpkg::install::StageHooks::NONE,
    )
    .map_err(|e| {
        Error::refused(format!(
            "the client's verify_and_stage refuses {program}@{} for {}: {e}",
            request.build, request.triple
        ))
    })?;

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
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Ok(Container::TarGz)
    } else if name.ends_with(".tar") {
        Ok(Container::Tar)
    } else if name.ends_with(".zip") {
        Ok(Container::Zip)
    } else {
        Err(Error::usage(format!(
            "unsupported bottle archive extension on {} (expected .tar.gz, .tgz, .tar, or .zip)",
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
    let mut archive = None;
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
            "--archive" => archive = Some(PathBuf::from(value()?)),
            "--version" => version = Some(value()?),
            "--build" => {
                build = Some(
                    value()?
                        .parse::<Build>()
                        .map_err(|_| Error::usage("--build takes a decimal build number"))?,
                );
            }
            "--triple" => triple = Some(Triple::parse(&value()?)?),
            "--strip-components" => {
                strip_components = value()?
                    .parse::<u32>()
                    .map_err(|_| Error::usage("--strip-components takes a non-negative integer"))?;
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
    let archive = archive.ok_or_else(usage)?;
    let version = version.ok_or_else(usage)?;
    let build = build.ok_or_else(usage)?;
    let triple = match triple {
        Some(t) => t,
        None => host_triple()?,
    };
    let out = out.unwrap_or_else(|| ctx.state_dir.join("pack-bottle"));

    Ok(BottleRequest {
        program,
        archive,
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
        upload::token_preflight(ctx)?;
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
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::artifact::relocate::ObjectRefs;
    use crate::artifact::tarwrite::RawFrames;

    #[derive(Default)]
    struct FakeBackend {
        refs: Mutex<BTreeMap<PathBuf, ObjectRefs>>,
    }

    impl FakeBackend {
        fn set(&self, path: &Path, refs: ObjectRefs) {
            self.refs
                .lock()
                .expect("lock")
                .insert(path.to_path_buf(), refs);
        }

        fn lookup(map: &BTreeMap<PathBuf, ObjectRefs>, path: &Path) -> ObjectRefs {
            let clone_refs = |r: &ObjectRefs| ObjectRefs {
                id: r.id.clone(),
                needed: r.needed.clone(),
                rpaths: r.rpaths.clone(),
            };
            if let Some(r) = map.get(path) {
                return clone_refs(r);
            }
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && let Some((_, r)) = map
                    .iter()
                    .find(|(k, _)| k.file_name().and_then(|n| n.to_str()) == Some(name))
            {
                return clone_refs(r);
            }
            ObjectRefs::default()
        }

        fn entry_mut<'a>(
            map: &'a mut BTreeMap<PathBuf, ObjectRefs>,
            path: &Path,
        ) -> &'a mut ObjectRefs {
            if !map.contains_key(path) {
                let seed = Self::lookup(map, path);
                map.insert(path.to_path_buf(), seed);
            }
            map.get_mut(path).expect("inserted")
        }
    }

    impl Backend for FakeBackend {
        fn is_native_object(&self, path: &Path) -> bool {
            arch::read(path).is_some()
        }

        fn read_refs(&self, path: &Path) -> std::result::Result<ObjectRefs, String> {
            let map = self.refs.lock().expect("lock");
            Ok(Self::lookup(&map, path))
        }

        fn set_id_portable(&self, path: &Path, basename: &str) -> std::result::Result<(), String> {
            let mut map = self.refs.lock().expect("lock");
            let entry = Self::entry_mut(&mut map, path);
            entry.id = Some(format!("@rpath/{basename}"));
            Ok(())
        }

        fn repoint_dep(
            &self,
            path: &Path,
            from: &str,
            basename: &str,
        ) -> std::result::Result<(), String> {
            let mut map = self.refs.lock().expect("lock");
            let entry = Self::entry_mut(&mut map, path);
            for dep in &mut entry.needed {
                if dep == from {
                    *dep = format!("@rpath/{basename}");
                }
            }
            Ok(())
        }

        fn fix_rpaths(
            &self,
            path: &Path,
            rel_origin: &str,
            keep: &[String],
            drop: &[String],
        ) -> std::result::Result<(), String> {
            let mut map = self.refs.lock().expect("lock");
            let entry = Self::entry_mut(&mut map, path);
            entry.rpaths.retain(|r| !drop.contains(r));
            for k in keep {
                if !entry.rpaths.contains(k) {
                    entry.rpaths.push(k.clone());
                }
            }
            if !entry.rpaths.iter().any(|r| r == rel_origin) {
                entry.rpaths.push(rel_origin.to_string());
            }
            Ok(())
        }

        fn resign(&self, _path: &Path, _sign_id: Option<&str>) -> std::result::Result<(), String> {
            Ok(())
        }

        fn origin_token(&self) -> &'static str {
            "@loader_path"
        }
    }

    fn thin_macho_bytes(cputype: u32) -> Vec<u8> {
        let mut bytes = vec![0u8; 32];
        bytes[0..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        bytes[4..8].copy_from_slice(&cputype.to_le_bytes());
        bytes
    }

    fn write_ustar_header(
        name: &str,
        size: u64,
        mode: u32,
        typeflag: u8,
        linkname: &str,
    ) -> [u8; 512] {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        let mode_s = format!("{mode:07o}\0");
        h[100..108].copy_from_slice(mode_s.as_bytes());
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        let size_s = format!("{size:011o}\0");
        h[124..136].copy_from_slice(size_s.as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        h[148..156].copy_from_slice(b"        ");
        h[156] = typeflag;
        if !linkname.is_empty() {
            h[157..157 + linkname.len()].copy_from_slice(linkname.as_bytes());
        }
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        let sum_s = format!("{sum:06o}\0 ");
        h[148..156].copy_from_slice(sum_s.as_bytes());
        h
    }

    enum TarEntry<'a> {
        Dir(&'a str),
        File(&'a str, &'a [u8], u32),
        Symlink(&'a str, &'a str),
    }

    fn build_tar(path: &Path, entries: &[TarEntry<'_>]) {
        let mut out = Vec::new();
        for e in entries {
            match *e {
                TarEntry::Dir(name) => {
                    out.extend_from_slice(&write_ustar_header(name, 0, 0o755, b'5', ""));
                }
                TarEntry::File(name, data, mode) => {
                    out.extend_from_slice(&write_ustar_header(
                        name,
                        data.len() as u64,
                        mode,
                        b'0',
                        "",
                    ));
                    out.extend_from_slice(data);
                    let rem = data.len() % 512;
                    if rem != 0 {
                        out.resize(out.len() + (512 - rem), 0);
                    }
                }
                TarEntry::Symlink(name, target) => {
                    out.extend_from_slice(&write_ustar_header(name, 0, 0o777, b'2', target));
                }
            }
        }
        out.resize(out.len() + 1024, 0);
        std::fs::write(path, out).expect("write tar");
    }

    #[test]
    fn bottle_row_admits_gh_and_git_on_darwin_and_refuses_others() {
        assert!(bottle_row("gh", AARCH64_APPLE_DARWIN).is_ok());
        assert!(bottle_row("git", X86_64_APPLE_DARWIN).is_ok());
        assert!(bottle_row("git", X86_64_UNKNOWN_LINUX_GNU).is_err());
        assert!(bottle_row("aterm", AARCH64_APPLE_DARWIN).is_err());
        assert!(bottle_row("no-such-program", AARCH64_APPLE_DARWIN).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn materialize_symlinks_turns_in_root_links_into_hard_links_and_refuses_escape() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let stage = dir.path().join("stage");
        std::fs::create_dir_all(stage.join("bin")).expect("mkdir");
        std::fs::create_dir_all(stage.join("libexec/git-core")).expect("mkdir");
        std::fs::write(stage.join("bin/git"), b"binary-bytes").expect("write");
        std::os::unix::fs::symlink("../../bin/git", stage.join("libexec/git-core/git"))
            .expect("symlink");

        materialize_symlinks(&stage).expect("in-root symlink materializes");
        let m1 = std::fs::symlink_metadata(stage.join("bin/git")).expect("stat bin/git");
        let m2 = std::fs::symlink_metadata(stage.join("libexec/git-core/git"))
            .expect("stat libexec/git-core/git");
        assert!(!m2.file_type().is_symlink());
        assert_eq!(
            inode_key(&m1),
            inode_key(&m2),
            "must share one hard-link inode"
        );

        // Negative control: an escaping symlink fails closed.
        let bad = dir.path().join("bad");
        std::fs::create_dir_all(&bad).expect("mkdir");
        std::fs::write(dir.path().join("outside"), b"secret").expect("write");
        std::os::unix::fs::symlink("../outside", bad.join("leak")).expect("symlink");
        assert!(materialize_symlinks(&bad).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn pack_bottle_packs_git_and_gh_merges_dual_darwin_and_refuses_missing_dep() {
        use std::sync::Arc;

        use crate::host::fake::FakeHost;
        use crate::testkit::Keys;

        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let keys = Keys::mint("m3", &[]);
        let host = Arc::new(FakeHost::new());
        let ctx = keys.ctx(tmp.path(), host.clone());
        keys.publish_quad(
            &host,
            &ctx.index_slug(),
            41,
            &keys.index_toml(41, &[("ay", "ay")], &[("ay", 9)]),
        );

        let arm64 = thin_macho_bytes(0x0100_000c);
        let x64 = thin_macho_bytes(0x0100_0007);

        let dep_tar = tmp.path().join("pcre2.bottle.tar");
        build_tar(
            &dep_tar,
            &[
                TarEntry::Dir("pcre2/10.44/lib/"),
                TarEntry::File("pcre2/10.44/lib/libpcre2-8.0.dylib", &arm64, 0o755),
            ],
        );

        let git_tar = tmp.path().join("git.arm64.bottle.tar");
        build_tar(
            &git_tar,
            &[
                TarEntry::Dir("git/2.51.0/bin/"),
                TarEntry::Dir("git/2.51.0/libexec/git-core/"),
                TarEntry::Dir("git/2.51.0/share/git-core/templates/"),
                TarEntry::File("git/2.51.0/bin/git", &arm64, 0o755),
                TarEntry::Symlink("git/2.51.0/libexec/git-core/git", "../../bin/git"),
                TarEntry::File(
                    "git/2.51.0/libexec/git-core/git-difftool",
                    b"#!@@HOMEBREW_PERL@@\nprint \"ok\\n\";\n",
                    0o755,
                ),
                TarEntry::File(
                    "git/2.51.0/share/git-core/templates/description",
                    b"Unnamed repository\n",
                    0o644,
                ),
            ],
        );

        let backend = FakeBackend::default();
        backend.set(
            Path::new("bin/git"),
            ObjectRefs {
                id: None,
                needed: vec![
                    "@@HOMEBREW_PREFIX@@/opt/pcre2/lib/libpcre2-8.0.dylib".to_string(),
                    "/usr/lib/libSystem.B.dylib".to_string(),
                ],
                rpaths: vec!["@@HOMEBREW_PREFIX@@/lib".to_string()],
            },
        );
        backend.set(
            Path::new("libpcre2-8.0.dylib"),
            ObjectRefs {
                id: Some("@@HOMEBREW_PREFIX@@/opt/pcre2/lib/libpcre2-8.0.dylib".to_string()),
                needed: vec!["/usr/lib/libSystem.B.dylib".to_string()],
                rpaths: Vec::new(),
            },
        );

        // Negative control: omitting `--dep-bottle` refuses fail-closed on unresolved bottle dep.
        let out_dir = tmp.path().join("out");
        let missing_dep_req = BottleRequest {
            program: "git".to_string(),
            archive: git_tar.clone(),
            version: "2.51.0".to_string(),
            build: 1,
            triple: AARCH64_APPLE_DARWIN,
            strip_components: 2,
            dep_bottles: Vec::new(),
            donor_roots: Vec::new(),
            merge_from: None,
            key: None,
            out: out_dir.clone(),
            signing: Signing::Unsigned,
            upload: false,
        };
        assert!(
            pack_bottle_with(&ctx, &missing_dep_req, &backend, &RawFrames::WHOLE).is_err(),
            "missing @@HOMEBREW_PREFIX@@ dep must fail closed"
        );

        // Positive: with `--dep-bottle`, `git` vendors `libpcre2-8.0.dylib`, writes the `bin/git`
        // wrapper without clobbering `libexec/git-core/git`, and stages cleanly via atpkg.
        let mut arm_req = missing_dep_req;
        arm_req.dep_bottles = vec![dep_tar];
        let arm_out = pack_bottle_with(&ctx, &arm_req, &backend, &RawFrames::WHOLE)
            .expect("arm64 git bottle packs");
        assert!(arm_out.asset.is_file());
        assert!(arm_out.manifest.is_file());

        // Merge an `x86_64-apple-darwin` row into the same `git@1` manifest.
        let git_x64_tar = tmp.path().join("git.x64.bottle.tar");
        build_tar(
            &git_x64_tar,
            &[
                TarEntry::Dir("git/2.51.0/bin/"),
                TarEntry::Dir("git/2.51.0/libexec/git-core/"),
                TarEntry::File("git/2.51.0/libexec/git-core/git", &x64, 0o755),
            ],
        );
        let x64_backend = FakeBackend::default();
        let merged_copy = tmp.path().join("git-1-base.pkg.toml");
        std::fs::copy(&arm_out.manifest, &merged_copy).expect("copy base manifest");
        let x64_req = BottleRequest {
            program: "git".to_string(),
            archive: git_x64_tar,
            version: "2.51.0".to_string(),
            build: 1,
            triple: X86_64_APPLE_DARWIN,
            strip_components: 2,
            dep_bottles: Vec::new(),
            donor_roots: Vec::new(),
            merge_from: Some(merged_copy),
            key: None,
            out: out_dir,
            signing: Signing::MachineKey,
            upload: false,
        };
        let merged = pack_bottle_with(&ctx, &x64_req, &x64_backend, &RawFrames::WHOLE)
            .expect("x64 git bottle merges and signs");
        assert!(merged.signature.as_ref().is_some_and(|p| p.is_file()));
        let text = std::fs::read_to_string(&merged.manifest).expect("read merged");
        let parsed: atpkg::PkgManifest = aterm_toml::from_str(&text).expect("parse merged");
        assert_eq!(parsed.artifacts.len(), 2);
        assert!(parsed.artifacts.iter().all(|a| a.provenance.is_none()));

        // Verify staged arm64 tree keeps Mach-O `libexec/git-core/git` and wrapper `bin/git`.
        let staged = tmp.path().join("staged-git");
        let arm_row = parsed
            .artifacts
            .iter()
            .find(|a| a.target == "aarch64-apple-darwin")
            .expect("arm64 row");
        atpkg::verify_and_stage(
            arm_row,
            &arm_out.asset,
            &staged,
            &atpkg::install::StageHooks::NONE,
        )
        .expect("verify_and_stage");
        let wrapper = std::fs::read_to_string(staged.join("bin/git")).expect("read wrapper");
        assert!(wrapper.starts_with("#!/bin/sh\n"));
        assert!(wrapper.contains("GIT_EXEC_PATH="));
        assert_eq!(
            std::fs::read(staged.join("libexec/git-core/git")).expect("read core git"),
            arm64,
            "wrapper write must not clobber hard-linked libexec/git-core/git binary"
        );
        let perl_script = std::fs::read_to_string(staged.join("libexec/git-core/git-difftool"))
            .expect("read script");
        assert!(perl_script.starts_with("#!/usr/bin/perl\n"));
        assert!(
            staged
                .join("lib/atpkg-vendored/libpcre2-8.0.dylib")
                .is_file()
        );
    }

    #[cfg(unix)]
    #[test]
    fn git_wrapper_runs_before_relocation_and_in_stage_libs_receive_rpath() {
        use std::sync::Arc;

        use crate::host::fake::FakeHost;
        use crate::testkit::Keys;

        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let keys = Keys::mint("m3", &[]);
        let host = Arc::new(FakeHost::new());
        let ctx = keys.ctx(tmp.path(), host);

        let arm64 = thin_macho_bytes(0x0100_000c);
        let dep_tar = tmp.path().join("gettext.bottle.tar");
        build_tar(
            &dep_tar,
            &[
                TarEntry::Dir("gettext/0.22.5/lib/"),
                TarEntry::File("gettext/0.22.5/lib/libintl.8.dylib", &arm64, 0o755),
            ],
        );

        // Archive has `bin/git`, `libexec/git-core/` (without `libexec/git-core/git`), and an
        // in-stage shared library `lib/libgit_helper.dylib`.
        let git_tar = tmp.path().join("git.bin_only.bottle.tar");
        build_tar(
            &git_tar,
            &[
                TarEntry::Dir("git/2.51.0/bin/"),
                TarEntry::Dir("git/2.51.0/lib/"),
                TarEntry::Dir("git/2.51.0/libexec/git-core/"),
                TarEntry::File("git/2.51.0/bin/git", &arm64, 0o755),
                TarEntry::File("git/2.51.0/lib/libgit_helper.dylib", &arm64, 0o755),
            ],
        );

        let backend = FakeBackend::default();
        backend.set(
            Path::new("git"),
            ObjectRefs {
                id: None,
                needed: vec![
                    "@@HOMEBREW_PREFIX@@/opt/gettext/lib/libintl.8.dylib".to_string(),
                    "@@HOMEBREW_CELLAR@@/git/2.51.0/lib/libgit_helper.dylib".to_string(),
                ],
                rpaths: Vec::new(),
            },
        );
        backend.set(
            Path::new("libgit_helper.dylib"),
            ObjectRefs {
                id: Some("@@HOMEBREW_CELLAR@@/git/2.51.0/lib/libgit_helper.dylib".to_string()),
                needed: vec!["/usr/lib/libSystem.B.dylib".to_string()],
                rpaths: Vec::new(),
            },
        );
        backend.set(
            Path::new("libintl.8.dylib"),
            ObjectRefs {
                id: Some("@@HOMEBREW_PREFIX@@/opt/gettext/lib/libintl.8.dylib".to_string()),
                needed: vec!["/usr/lib/libSystem.B.dylib".to_string()],
                rpaths: Vec::new(),
            },
        );

        let req = BottleRequest {
            program: "git".to_string(),
            archive: git_tar,
            version: "2.51.0".to_string(),
            build: 1,
            triple: AARCH64_APPLE_DARWIN,
            strip_components: 2,
            dep_bottles: vec![dep_tar],
            donor_roots: Vec::new(),
            merge_from: None,
            key: None,
            out: tmp.path().join("out"),
            signing: Signing::Unsigned,
            upload: false,
        };
        let _out = pack_bottle_with(&ctx, &req, &backend, &RawFrames::WHOLE)
            .expect("git bottle with bin/git + in-stage lib packs");

        // Verify `libexec/git-core/git` was relocated at depth 2 (`../../lib/atpkg-vendored` and
        // `../../lib`), not depth 1 (`../lib/atpkg-vendored`).
        let map = backend.refs.lock().expect("lock");
        let core_git_refs = map
            .iter()
            .find_map(|(k, v)| k.ends_with("libexec/git-core/git").then_some(v))
            .expect("libexec/git-core/git refs recorded");
        assert_eq!(
            core_git_refs.needed,
            vec![
                "@rpath/libintl.8.dylib".to_string(),
                "@rpath/libgit_helper.dylib".to_string(),
            ]
        );
        assert!(
            core_git_refs
                .rpaths
                .contains(&"@loader_path/../../lib/atpkg-vendored".to_string()),
            "libexec/git-core/git must have depth-2 vendored rpath: {:?}",
            core_git_refs.rpaths
        );
        assert!(
            core_git_refs
                .rpaths
                .contains(&"@loader_path/../../lib".to_string()),
            "libexec/git-core/git must have depth-2 in-stage lib/ rpath: {:?}",
            core_git_refs.rpaths
        );
        assert!(
            !core_git_refs
                .rpaths
                .contains(&"@loader_path/../lib/atpkg-vendored".to_string()),
            "bin/git must be wrapped before relocation so depth-1 rpath is not written to core git: {:?}",
            core_git_refs.rpaths
        );
        drop(map);

        // Negative control: `check_stage_tree` refuses a Mach-O object that still carries an
        // unrewritten `@@HOMEBREW_*@@` load command.
        let bad_stage = tmp.path().join("bad-stage");
        std::fs::create_dir_all(bad_stage.join("bin")).expect("mkdir");
        let bad_gh = bad_stage.join("bin/gh");
        std::fs::write(&bad_gh, &arm64).expect("write");
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&bad_gh, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        let bad_backend = FakeBackend::default();
        bad_backend.set(
            &bad_gh,
            ObjectRefs {
                id: None,
                needed: Vec::new(),
                rpaths: vec!["@@HOMEBREW_PREFIX@@/lib".to_string()],
            },
        );
        assert!(
            check_stage_tree(&bad_stage, AARCH64_APPLE_DARWIN, &["gh"], &bad_backend).is_err(),
            "leftover @@HOMEBREW_*@@ load command on native object must fail closed"
        );
    }
}
