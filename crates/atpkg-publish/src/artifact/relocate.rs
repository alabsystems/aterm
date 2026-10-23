// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pack-time relocation (§10.1 "self-contained" bundle path).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Where vendored shared libraries are copied, relative to the stage root.
pub const VENDOR_REL: &str = "lib/atpkg-vendored";

/// The record of what was vendored, inside [`VENDOR_REL`].
pub const VENDORED_RECORD: &str = "VENDORED";

/// A path that hard-codes THIS build machine's layout.
#[must_use]
pub fn is_machine_local(path: &str) -> bool {
    path.starts_with("/Users/")
        || path.starts_with("/home/")
        || atpkg::lineage::stock_location(Path::new(path)).is_some()
}

/// The load-command view of one native object, produced by a [`Backend`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ObjectRefs {
    /// The object's own install-name / soname, if it declares one (dylibs/`.so`).
    pub id: Option<String>,
    /// Dependency references.
    pub needed: Vec<String>,
    /// Runtime search paths (`LC_RPATH` / `DT_RPATH`+`DT_RUNPATH`).
    pub rpaths: Vec<String>,
}

/// The per-OS mechanics of reading and rewriting a native object.
pub trait Backend {
    /// Is `path` a native executable/shared object this backend relocates?
    fn is_native_object(&self, path: &Path) -> bool;
    /// Read the object's id / needed / rpaths.
    fn read_refs(&self, path: &Path) -> Result<ObjectRefs, String>;
    /// Rewrite the object's own install-name/soname to the portable form.
    fn set_id_portable(&self, path: &Path, basename: &str) -> Result<(), String>;
    /// Repoint an absolute machine-local dependency to the portable form.
    fn repoint_dep(&self, path: &Path, from: &str, basename: &str) -> Result<(), String>;
    /// Ensure the object searches `rel_origin` (an `@loader_path`/`$ORIGIN` relative dir)
    /// and no machine-local rpath.
    fn fix_rpaths(
        &self,
        path: &Path,
        rel_origin: &str,
        keep: &[String],
        drop: &[String],
    ) -> Result<(), String>;
    /// Re-establish a valid code signature after a rewrite (macOS: mandatory on
    /// arm64; Linux: no-op).
    fn resign(&self, path: &Path, sign_id: Option<&str>) -> Result<(), String>;
    /// The relative-origin token for this platform (`@loader_path` / `$ORIGIN`).
    fn origin_token(&self) -> &'static str;
}

/// One vendored copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vendored {
    /// The file name under `lib/atpkg-vendored/`.
    pub name: String,
    /// sha256 of the donor file.
    pub sha256: String,
    /// The donor file's path inside its donor root.
    pub donor_rel: String,
}

/// Outcome of relocating one staged payload.
#[derive(Debug, Default)]
pub struct Report {
    /// Basenames vendored into `lib/atpkg-vendored/`.
    pub vendored: Vec<String>,
    /// Each vendored copy's provenance (written to [`VENDORED_RECORD`]).
    pub vendored_from: Vec<Vendored>,
    /// Objects whose load commands were rewritten.
    pub rewritten: usize,
    /// Machine-local references that could not be resolved.
    pub unresolved: Vec<String>,
}

impl Report {
    /// Fail-closed self-containment assertion.
    pub fn require_self_contained(&self) -> Result<(), String> {
        if self.unresolved.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "relocation left {} unresolved machine-local reference(s); a self-contained \
                 bundle must vendor them all:\n  {}",
                self.unresolved.len(),
                self.unresolved.join("\n  ")
            ))
        }
    }
}

/// The backend for the host OS.
///
/// # Errors
/// When the host OS has no relocation backend (e.g. Windows).
pub fn backend_for_host() -> Result<Box<dyn Backend>, String> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(macho::MachoBackend))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(elf::ElfBackend))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err("relocation is only implemented for macOS (Mach-O) and Linux (ELF)".to_string())
    }
}

/// Relocate a staged payload in place.
///
/// # Errors
/// I/O, a missing relocation tool, a rewrite failure, or a library that would be vendored from a stock toolchain.
pub fn relocate_stage(
    stage: &Path,
    sign_id: Option<&str>,
    donor_roots: &[PathBuf],
) -> Result<Report, String> {
    let backend = backend_for_host()?;
    relocate_with(stage, backend.as_ref(), sign_id, donor_roots)
}

/// [`relocate_stage`] against an explicit backend (the seam unit tests drive).
pub fn relocate_with(
    stage: &Path,
    backend: &dyn Backend,
    sign_id: Option<&str>,
    donor_roots: &[PathBuf],
) -> Result<Report, String> {
    let mut report = Report::default();
    let vendored = stage.join(VENDOR_REL);
    let builder = |p: &str| is_machine_local(p) || in_donor_root(p, donor_roots).is_some();

    let files = walk_files(stage)?;

    let mut have: BTreeSet<String> = BTreeSet::new();
    for rel in &files {
        if is_shared_lib_name(rel)
            && let Some(b) = rel.file_name().and_then(|n| n.to_str())
        {
            have.insert(b.to_string());
        }
    }

    let mut donors: BTreeSet<PathBuf> = BTreeSet::new();
    let mut queue: Vec<PathBuf> = Vec::new();
    let mut prescanned: BTreeMap<PathBuf, ObjectRefs> = BTreeMap::new();
    let mut group_first: BTreeMap<(u64, u64), PathBuf> = BTreeMap::new();
    let mut aliases: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for rel in &files {
        let p = stage.join(rel);
        if backend.is_native_object(&p) {
            if let Some(key) = hard_link_key(&p) {
                match group_first.entry(key) {
                    std::collections::btree_map::Entry::Occupied(first) => {
                        aliases.entry(first.get().clone()).or_default().push(p);
                        continue;
                    }
                    std::collections::btree_map::Entry::Vacant(slot) => {
                        slot.insert(p.clone());
                    }
                }
            }
            let refs = backend.read_refs(&p)?;
            for r in &refs.rpaths {
                if builder(r) {
                    donors.insert(PathBuf::from(r));
                }
            }
            queue.push(p.clone());
            prescanned.insert(p, refs);
        }
    }

    let mut processed: BTreeSet<PathBuf> = BTreeSet::new();
    while let Some(obj) = queue.pop() {
        if !processed.insert(obj.clone()) {
            continue;
        }
        let refs = match prescanned.remove(&obj) {
            Some(refs) => refs,
            None => backend.read_refs(&obj)?,
        };
        let group: &[PathBuf] = aliases.get(&obj).map_or(&[], Vec::as_slice);
        let mut changed = false;

        if let Some(id) = &refs.id
            && builder(id)
        {
            backend.set_id_portable(&obj, &basename(id))?;
            changed = true;
        }

        for dep in &refs.needed {
            if builder(dep) {
                let base = basename(dep);
                if vendor_file(
                    Path::new(dep),
                    &VendorTo {
                        dir: &vendored,
                        backend,
                        donor_roots,
                    },
                    &base,
                    &mut have,
                    &mut report,
                )? {
                    let dest = vendored.join(&base);
                    prescanned.remove(&dest);
                    queue.push(dest);
                }
                backend.repoint_dep(&obj, dep, &base)?;
                changed = true;
            } else {
                let base = basename(dep);
                let unresolved_in_payload =
                    dep.starts_with("@rpath/") || is_shared_lib_basename(&base);
                if unresolved_in_payload
                    && !have.contains(&base)
                    && let Some(src) = find_in_donors(&base, &donors)
                {
                    if vendor_file(
                        &src,
                        &VendorTo {
                            dir: &vendored,
                            backend,
                            donor_roots,
                        },
                        &base,
                        &mut have,
                        &mut report,
                    )? {
                        let dest = vendored.join(&base);
                        prescanned.remove(&dest);
                        queue.push(dest);
                    }
                    changed = true;
                }
            }
        }

        let ml_rpaths: Vec<String> = refs.rpaths.iter().filter(|r| builder(r)).cloned().collect();
        let mut keep: Vec<String> = refs
            .rpaths
            .iter()
            .filter(|r| !builder(r))
            .cloned()
            .collect();
        let needs_vendor = vendored.is_dir()
            && refs.needed.iter().any(|n| {
                n.starts_with("@rpath/") || builder(n) || is_shared_lib_basename(&basename(n))
            });
        if needs_vendor || !ml_rpaths.is_empty() {
            let mut dirs: Vec<&Path> = vec![obj.parent().unwrap_or(stage)];
            for alias in group {
                let d = alias.parent().unwrap_or(stage);
                if !dirs.contains(&d) {
                    dirs.push(d);
                }
            }
            let mut drop = ml_rpaths;
            for (i, dir) in dirs.iter().enumerate() {
                let rel = origin_relative(backend.origin_token(), dir, &vendored);
                if i > 0 && keep.contains(&rel) {
                    continue;
                }
                if let Err(e) = backend.fix_rpaths(&obj, &rel, &keep, &drop) {
                    report.unresolved.push(format!(
                        "{}: rpath rewrite failed: {e}",
                        rel_display(stage, &obj)
                    ));
                    break;
                }
                changed = true;
                if !keep.contains(&rel) {
                    keep.push(rel);
                }
                drop.clear();
            }
        }

        if changed {
            backend.resign(&obj, sign_id)?;
            report.rewritten += 1;
            relink_aliases(stage, &obj, group)?;
        }
    }

    report.vendored.sort();
    report.vendored.dedup();
    write_vendored_record(&vendored, &report)?;
    Ok(report)
}

/// The canonical donor root `path` lies inside, if any. The path is canonicalized when
/// it exists (a linker records `/var/…` where the root is `/private/var/…`).
fn in_donor_root<'r>(path: &str, donor_roots: &'r [PathBuf]) -> Option<&'r PathBuf> {
    if donor_roots.is_empty() || !path.starts_with('/') {
        return None;
    }
    let p = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    donor_roots.iter().find(|r| p.starts_with(r))
}

/// Write [`VENDORED_RECORD`] when anything was vendored.
fn write_vendored_record(vendored: &Path, report: &Report) -> Result<(), String> {
    if report.vendored_from.is_empty() {
        return Ok(());
    }
    let mut rows: Vec<&Vendored> = report.vendored_from.iter().collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    let mut text = String::from(
        "# VENDORED: each library pack-time relocation copied into this directory, as\n\
         # <sha256 of the donor file> <file name> <path inside its declared donor root>.\n\
         # The copy was byte-identical to the donor before its re-sign.\n",
    );
    for v in rows {
        text.push_str(&format!("{} {} {}\n", v.sha256, v.name, v.donor_rel));
    }
    let dest = vendored.join(VENDORED_RECORD);
    std::fs::write(&dest, text).map_err(|e| format!("write {}: {e}", dest.display()))
}

/// Where [`vendor_file`] copies to, and what may be copied.
struct VendorTo<'a> {
    dir: &'a Path,
    backend: &'a dyn Backend,
    donor_roots: &'a [PathBuf],
}

/// The refusal for a library that would be vendored out of a stock toolchain.
fn stock_vendor_refusal(base: &str, src: &Path, place: &str) -> String {
    format!(
        "{base}: a stock toolchain library cannot be vendored — {} resolves into {place}. A \
         bundle vendors only from a Trust toolchain named with --donor-root; build what \
         loads it against one",
        src.display()
    )
}

/// Copy `src` → `<to.dir>/<base>` once (from inside a donor root only, byte-identical
/// before the re-sign, recorded), make its own id portable, and re-sign ad hoc.
///
/// # Errors
/// A stock toolchain's library (checked on the path as named and as resolved, before
/// anything is read or written), or a copy that differs from its donor.
fn vendor_file(
    src: &Path,
    to: &VendorTo<'_>,
    base: &str,
    have: &mut BTreeSet<String>,
    report: &mut Report,
) -> Result<bool, String> {
    if have.contains(base) {
        return Ok(false);
    }
    if let Some(place) = atpkg::lineage::stock_location(src) {
        return Err(stock_vendor_refusal(base, src, place));
    }
    if !src.is_file() {
        report
            .unresolved
            .push(format!("{}: source not found to vendor", src.display()));
        return Ok(false);
    }
    let canonical =
        std::fs::canonicalize(src).map_err(|e| format!("resolve {}: {e}", src.display()))?;
    if let Some(place) = atpkg::lineage::stock_location(&canonical) {
        return Err(stock_vendor_refusal(base, &canonical, place));
    }
    let Some(root) = to.donor_roots.iter().find(|r| canonical.starts_with(r)) else {
        report.unresolved.push(format!(
            "{base}: {} is not inside a declared donor root — relocate vendors only from a \
             Trust toolchain named with --donor-root",
            canonical.display()
        ));
        return Ok(false);
    };
    std::fs::create_dir_all(to.dir).map_err(|e| format!("create vendored dir: {e}"))?;
    let dest = to.dir.join(base);
    std::fs::copy(&canonical, &dest).map_err(|e| format!("vendor {}: {e}", src.display()))?;
    let donor_sha = atpkg::tree::sha256_file(&canonical)
        .map_err(|e| format!("hash {}: {e}", canonical.display()))?;
    let copy_sha =
        atpkg::tree::sha256_file(&dest).map_err(|e| format!("hash {}: {e}", dest.display()))?;
    if donor_sha != copy_sha {
        let _ = std::fs::remove_file(&dest);
        return Err(format!(
            "{base}: the vendored copy ({copy_sha}) is not byte-identical to its donor {} \
             ({donor_sha}) — the donor changed while it was copied",
            canonical.display()
        ));
    }
    report.vendored_from.push(Vendored {
        name: base.to_string(),
        sha256: donor_sha,
        donor_rel: canonical
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    });
    to.backend.set_id_portable(&dest, base)?;
    to.backend.resign(&dest, None)?;
    have.insert(base.to_string());
    report.vendored.push(base.to_string());
    Ok(true)
}

/// `(device, inode)` of a regular file that has more than one name, else `None`. Only
/// such files can belong to a hard-link group; everything else is its own object.
#[cfg(unix)]
fn hard_link_key(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).ok()?;
    (meta.is_file() && meta.nlink() > 1).then(|| (meta.dev(), meta.ino()))
}

/// No relocation backend exists off Unix, so there is no group to keep.
#[cfg(not(unix))]
fn hard_link_key(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// Point every other name of `obj`'s hard-link group at `obj`'s new inode. Each alias is
/// linked to a hidden sibling and renamed over, so it is never absent: a failure leaves it
/// on its old bytes, and the error names it.
fn relink_aliases(stage: &Path, obj: &Path, aliases: &[PathBuf]) -> Result<(), String> {
    for alias in aliases {
        if hard_link_key(obj).is_some() && hard_link_key(obj) == hard_link_key(alias) {
            // the rewrite went through the inode: still one file
            continue;
        }
        let name = alias
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tmp = alias.with_file_name(format!(".{name}.atpkg-relink"));
        let _ = std::fs::remove_file(&tmp);
        std::fs::hard_link(obj, &tmp)
            .and_then(|()| std::fs::rename(&tmp, alias))
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!(
                    "re-link {} onto the rewritten {}: {e}",
                    rel_display(stage, alias),
                    rel_display(stage, obj)
                )
            })?;
    }
    Ok(())
}

fn find_in_donors(base: &str, donors: &BTreeSet<PathBuf>) -> Option<PathBuf> {
    donors.iter().map(|d| d.join(base)).find(|p| p.is_file())
}

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// A relative path from a Mach-O/ELF file's directory to the vendored dir, prefixed with the platform origin token.
pub fn origin_relative(origin_token: &str, macho_dir: &Path, vendored: &Path) -> String {
    let rel = relative(macho_dir, vendored);
    let mut out = String::from(origin_token);
    for c in rel.components() {
        out.push('/');
        out.push_str(&c.as_os_str().to_string_lossy());
    }
    out
}

/// Relative path from `from` to `to` (both absolute, no symlink resolution).
fn relative(from: &Path, to: &Path) -> PathBuf {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut rel = PathBuf::new();
    for _ in common..from.len() {
        rel.push("..");
    }
    for c in &to[common..] {
        rel.push(c.as_os_str());
    }
    rel
}

fn rel_display(stage: &Path, p: &Path) -> String {
    p.strip_prefix(stage).unwrap_or(p).display().to_string()
}

/// A `.dylib` (macOS) or `.so`/`.so.N` (Linux) file name.
fn is_shared_lib_name(rel: &Path) -> bool {
    rel.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_shared_lib_basename)
}

fn is_shared_lib_basename(name: &str) -> bool {
    name.ends_with(".dylib") || name.contains(".so.") || name.ends_with(".so")
}

/// Recursively collect regular files under `dir` (symlinks not followed), paths relative to `dir`, sorted.
pub fn walk_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let rd = std::fs::read_dir(&d).map_err(|e| format!("read {}: {e}", d.display()))?;
        for e in rd {
            let e = e.map_err(|e| format!("read {}: {e}", d.display()))?;
            let ft = e.file_type().map_err(|e| format!("stat: {e}"))?;
            let p = e.path();
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file()
                && let Ok(rel) = p.strip_prefix(dir)
            {
                out.push(rel.to_path_buf());
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Whether `path` is a native executable or shared object (Mach-O or ELF), the only files a dynamic-loader resolve check means anything for.
#[must_use]
pub fn is_native_object(path: &Path) -> bool {
    magic4(path).is_some_and(|m| macho::is_macho_magic(&m) || elf::is_elf_magic(&m))
}

/// Read the first 4 magic bytes of a file (native-object sniff).
pub fn magic4(path: &Path) -> Option<[u8; 4]> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut m = [0u8; 4];
    f.read_exact(&mut m).ok()?;
    Some(m)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn run(tool: &str, args: &[&str], file: &Path) -> Result<std::process::Output, String> {
    aterm_uds::spawnfd::command(tool)
        .args(args)
        .arg(file)
        .output()
        .map_err(|e| format!("spawn {tool}: {e}"))
}

pub mod macho {
    #[cfg(any(target_os = "macos", test))]
    use super::*;

    #[cfg(target_os = "macos")]
    pub struct MachoBackend;

    /// How long one `codesign --force --sign` may run before the relocate is
    /// refused ([`MachoBackend`]'s `resign`): a hang detector, never a budget.
    #[cfg(target_os = "macos")]
    const RESIGN_HANG: std::time::Duration = std::time::Duration::from_secs(60);

    /// Mach-O magic (thin arm64/x86_64 little-endian + universal).
    pub const fn is_macho_magic(m: &[u8; 4]) -> bool {
        matches!(
            m,
            [0xcf, 0xfa, 0xed, 0xfe]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
        )
    }

    /// Pure parser over `otool -l <file>` output.
    #[cfg(any(target_os = "macos", test))]
    pub fn parse_otool(otool_l: &str) -> ObjectRefs {
        let mut r = ObjectRefs::default();
        let mut section: Option<&str> = None;
        for line in otool_l.lines() {
            let t = line.trim_start();
            if let Some(cmd) = t.strip_prefix("cmd ") {
                section = match cmd.trim() {
                    "LC_ID_DYLIB" => Some("id"),
                    "LC_LOAD_DYLIB" | "LC_LOAD_WEAK_DYLIB" | "LC_REEXPORT_DYLIB" => Some("dylib"),
                    "LC_RPATH" => Some("rpath"),
                    _ => None,
                };
                continue;
            }
            match section {
                Some("id") => {
                    if let Some(v) = t.strip_prefix("name ") {
                        r.id = Some(strip_offset(v));
                        section = None;
                    }
                }
                Some("dylib") => {
                    if let Some(v) = t.strip_prefix("name ") {
                        r.needed.push(strip_offset(v));
                        section = None;
                    }
                }
                Some("rpath") => {
                    if let Some(v) = t.strip_prefix("path ") {
                        r.rpaths.push(strip_offset(v));
                        section = None;
                    }
                }
                _ => {}
            }
        }
        r
    }

    #[cfg(any(target_os = "macos", test))]
    fn strip_offset(s: &str) -> String {
        s.split(" (offset").next().unwrap_or(s).trim().to_string()
    }

    #[cfg(target_os = "macos")]
    fn install_name_tool(args: &[&str], file: &Path) -> Result<(), String> {
        let out = run("/usr/bin/install_name_tool", args, file)?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "install_name_tool {} {}: {}",
                args.join(" "),
                file.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    #[cfg(target_os = "macos")]
    impl Backend for MachoBackend {
        fn is_native_object(&self, path: &Path) -> bool {
            magic4(path).is_some_and(|m| is_macho_magic(&m))
        }
        fn read_refs(&self, path: &Path) -> Result<ObjectRefs, String> {
            let out = run("/usr/bin/otool", &["-l"], path)?;
            if !out.status.success() {
                return Err(format!(
                    "otool -l {}: {}",
                    path.display(),
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            Ok(parse_otool(&String::from_utf8_lossy(&out.stdout)))
        }
        fn set_id_portable(&self, path: &Path, basename: &str) -> Result<(), String> {
            install_name_tool(&["-id", &format!("@rpath/{basename}")], path)
        }
        fn repoint_dep(&self, path: &Path, from: &str, basename: &str) -> Result<(), String> {
            install_name_tool(&["-change", from, &format!("@rpath/{basename}")], path)
        }
        fn fix_rpaths(
            &self,
            path: &Path,
            rel_origin: &str,
            keep: &[String],
            drop: &[String],
        ) -> Result<(), String> {
            for d in drop {
                install_name_tool(&["-delete_rpath", d], path)?;
            }
            if !keep.iter().any(|r| r == rel_origin) {
                install_name_tool(&["-add_rpath", rel_origin], path)?;
            }
            Ok(())
        }
        fn resign(&self, path: &Path, sign_id: Option<&str>) -> Result<(), String> {
            let mut c = aterm_uds::spawnfd::command("/usr/bin/codesign");
            c.arg("--force");
            match sign_id {
                Some(id) => c.args(["--timestamp", "--sign", id]),
                None => c.args(["--sign", "-"]),
            };
            let out = aterm_uds::deadline::output_until(
                c.arg(path),
                std::time::Instant::now() + RESIGN_HANG,
            )
            .map_err(|e| match e {
                aterm_uds::deadline::RunError::TimedOut => format!(
                    "codesign did not finish in {} s (macOS's code-signing service (trustd) \
                     may not be answering); {} was not re-signed, relocate refused",
                    RESIGN_HANG.as_secs(),
                    path.display()
                ),
                other => format!("codesign {}: {other}", path.display()),
            })?;
            if out.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "codesign {}: {}",
                    path.display(),
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        }
        fn origin_token(&self) -> &'static str {
            "@loader_path"
        }
    }
}

pub mod elf {
    #[cfg(any(target_os = "linux", test))]
    use super::*;

    #[cfg(target_os = "linux")]
    pub struct ElfBackend;

    /// ELF magic (`\x7fELF`).
    pub fn is_elf_magic(m: &[u8; 4]) -> bool {
        *m == [0x7f, b'E', b'L', b'F']
    }

    /// Split a `patchelf --print-rpath` value (`:`-separated) into entries.
    #[cfg(any(target_os = "linux", test))]
    pub fn parse_rpath(value: &str) -> Vec<String> {
        value
            .split(':')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// Parse `patchelf --print-needed` (one soname per line).
    #[cfg(any(target_os = "linux", test))]
    pub fn parse_needed(text: &str) -> Vec<String> {
        text.lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// The `patchelf --set-rpath` value making an object search the vendored dir (relative, `$ORIGIN`-based) plus any portable rpaths it already had.
    #[cfg(any(target_os = "linux", test))]
    pub fn combined_rpath(rel_origin: &str, keep: &[String]) -> String {
        let mut out: Vec<String> = vec![rel_origin.to_string()];
        for k in keep {
            if !is_machine_local(k) && !out.contains(k) {
                out.push(k.clone());
            }
        }
        out.join(":")
    }

    #[cfg(target_os = "linux")]
    fn patchelf(args: &[&str], file: &Path) -> Result<(), String> {
        let out = run("patchelf", args, file)
            .map_err(|e| format!("{e} (install patchelf on the Linux build box)"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "patchelf {} {}: {}",
                args.join(" "),
                file.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    #[cfg(target_os = "linux")]
    fn patchelf_out(args: &[&str], file: &Path) -> Result<String, String> {
        let out = run("patchelf", args, file)
            .map_err(|e| format!("{e} (install patchelf on the Linux build box)"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        } else {
            Err(format!(
                "patchelf {} {}: {}",
                args.join(" "),
                file.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    #[cfg(target_os = "linux")]
    impl Backend for ElfBackend {
        fn is_native_object(&self, path: &Path) -> bool {
            magic4(path).is_some_and(|m| is_elf_magic(&m))
        }
        fn read_refs(&self, path: &Path) -> Result<ObjectRefs, String> {
            let id = patchelf_out(&["--print-soname"], path).ok().and_then(|s| {
                let s = s.trim().to_string();
                (!s.is_empty() && s != "no soname").then_some(s)
            });
            let needed = parse_needed(&patchelf_out(&["--print-needed"], path)?);
            let rpaths = parse_rpath(&patchelf_out(&["--print-rpath"], path)?);
            Ok(ObjectRefs { id, needed, rpaths })
        }
        fn set_id_portable(&self, path: &Path, basename: &str) -> Result<(), String> {
            // basename so consumers resolve it by name via the vendored rpath.
            patchelf(&["--set-soname", basename], path)
        }
        fn repoint_dep(&self, path: &Path, from: &str, basename: &str) -> Result<(), String> {
            // if a builder emitted an absolute NEEDED (rare) — normalize it.
            patchelf(&["--replace-needed", from, basename], path)
        }
        fn fix_rpaths(
            &self,
            path: &Path,
            rel_origin: &str,
            keep: &[String],
            _drop: &[String],
        ) -> Result<(), String> {
            let value = elf::combined_rpath(rel_origin, keep);
            patchelf(&["--set-rpath", &value, "--force-rpath"], path)
        }
        fn resign(&self, _path: &Path, _sign_id: Option<&str>) -> Result<(), String> {
            // ELF has no code signature; the Ed25519 tarball anchor covers integrity
            Ok(())
        }
        fn origin_token(&self) -> &'static str {
            "$ORIGIN"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_local_classification_covers_both_oses() {
        assert!(is_machine_local("/opt/homebrew/lib/libz.dylib"));
        assert!(is_machine_local("/Users//example/.rustup/x/lib"));
        assert!(is_machine_local("/home/builder/.rustup/toolchains/n/lib"));
        assert!(is_machine_local("/root/.cargo/registry/x"));
        assert!(is_machine_local(
            "/usr/local/rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu/lib/libstd.so"
        ));
        assert!(is_machine_local(
            "/usr/local/Cellar/zstd/1.5.7/lib/libzstd.1.dylib"
        ));
        assert!(!is_machine_local("@rpath/libx.dylib"));
        assert!(!is_machine_local("$ORIGIN/../lib"));
        assert!(!is_machine_local("/usr/lib/libSystem.B.dylib"));
        assert!(!is_machine_local("libc.so.6"));
    }

    #[test]
    fn origin_relative_is_platform_tokened() {
        let stage = Path::new("/s");
        let vendored = stage.join(VENDOR_REL);
        assert_eq!(
            origin_relative("@loader_path", &stage.join("bin"), &vendored),
            "@loader_path/../lib/atpkg-vendored"
        );
        assert_eq!(
            origin_relative("$ORIGIN", &stage.join("bin"), &vendored),
            "$ORIGIN/../lib/atpkg-vendored"
        );
        assert_eq!(
            origin_relative("@loader_path", &vendored, &vendored),
            "@loader_path"
        );
        assert_eq!(
            origin_relative("$ORIGIN", &stage.join("lib/rustlib/x/bin"), &vendored),
            "$ORIGIN/../../../atpkg-vendored"
        );
    }

    #[test]
    fn origin_relative_rpath_is_never_machine_local() {
        for stage in [
            Path::new("/Users//builder/stage"),
            Path::new("/opt/homebrew/var/stage"),
            Path::new("/home/builder/.cargo/checkout/stage"),
        ] {
            let vendored = stage.join(VENDOR_REL);
            for dir in [
                stage.join("bin"),
                stage.join("lib/rustlib/x/bin"),
                vendored.clone(),
            ] {
                for token in ["@loader_path", "$ORIGIN"] {
                    let rel = origin_relative(token, &dir, &vendored);
                    assert!(rel.starts_with(token));
                    assert!(!is_machine_local(&rel), "{rel} must partition into `keep`");
                }
            }
        }
    }

    #[test]
    fn shared_lib_name_detection() {
        assert!(is_shared_lib_basename("librustc_driver-abc.dylib"));
        assert!(is_shared_lib_basename("libstd.so"));
        assert!(is_shared_lib_basename("libLLVM.so.19.1"));
        assert!(!is_shared_lib_basename("trustc"));
        assert!(!is_shared_lib_basename("libfoo.rlib"));
    }

    #[test]
    fn macho_magic_and_otool_parse() {
        assert!(macho::is_macho_magic(&[0xcf, 0xfa, 0xed, 0xfe]));
        assert!(macho::is_macho_magic(&[0xca, 0xfe, 0xba, 0xbe]));
        assert!(!macho::is_macho_magic(&[0x7f, b'E', b'L', b'F']));
        let text = "          cmd LC_ID_DYLIB\n         name /Users//b/deps/libchecker.dylib (offset 24)\n          cmd LC_LOAD_DYLIB\n         name @rpath/librustc_driver.dylib (offset 24)\n          cmd LC_RPATH\n         path /Users//b/.rustup/x/lib (offset 12)\n          cmd LC_RPATH\n         path @loader_path/../toolchain/lib (offset 12)\n";
        let r = macho::parse_otool(text);
        assert_eq!(r.id.as_deref(), Some("/Users//b/deps/libchecker.dylib"));
        assert_eq!(r.needed, vec!["@rpath/librustc_driver.dylib"]);
        assert_eq!(
            r.rpaths,
            vec!["/Users//b/.rustup/x/lib", "@loader_path/../toolchain/lib"]
        );
    }

    #[test]
    fn elf_magic_and_patchelf_parse() {
        assert!(elf::is_elf_magic(&[0x7f, b'E', b'L', b'F']));
        assert!(!elf::is_elf_magic(&[0xcf, 0xfa, 0xed, 0xfe]));
        assert_eq!(
            elf::parse_needed("libc.so.6\nlibrustc_driver.so\n"),
            vec!["libc.so.6", "librustc_driver.so"]
        );
        assert_eq!(
            elf::parse_rpath("/home/b/.rustup/x/lib:$ORIGIN/../lib"),
            vec!["/home/b/.rustup/x/lib", "$ORIGIN/../lib"]
        );
        assert_eq!(
            elf::combined_rpath(
                "$ORIGIN/../lib/atpkg-vendored",
                &["/home/b/.rustup/x/lib".into(), "$ORIGIN/../lib".into()]
            ),
            "$ORIGIN/../lib/atpkg-vendored:$ORIGIN/../lib"
        );
    }

    /// A backend that behaves like `install_name_tool` and `codesign` where it matters here:
    /// every rewrite replaces the file (a sibling renamed over, a new inode), and the
    /// "signature" names the file it was written under, as an ad-hoc identifier does.
    #[cfg(unix)]
    struct ReplacingBackend {
        log: std::cell::RefCell<Vec<String>>,
    }

    #[cfg(unix)]
    impl ReplacingBackend {
        fn rewrite(&self, path: &Path, what: &str) -> Result<(), String> {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
            self.log
                .borrow_mut()
                .push(format!("{what} {}", name.unwrap_or_default()));
            let mut bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            bytes.extend_from_slice(format!("|{what}").as_bytes());
            let tmp = path.with_extension("fake-rewrite");
            std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, path).map_err(|e| e.to_string())
        }
    }

    #[cfg(unix)]
    impl Backend for ReplacingBackend {
        fn is_native_object(&self, path: &Path) -> bool {
            std::fs::read(path).is_ok_and(|b| b.starts_with(b"OBJ"))
        }
        fn read_refs(&self, _path: &Path) -> Result<ObjectRefs, String> {
            Ok(ObjectRefs {
                id: None,
                needed: Vec::new(),
                rpaths: vec!["/Users//builder/.rustup/lib".to_string()],
            })
        }
        fn set_id_portable(&self, _path: &Path, _basename: &str) -> Result<(), String> {
            Ok(())
        }
        fn repoint_dep(&self, _path: &Path, _from: &str, _basename: &str) -> Result<(), String> {
            Ok(())
        }
        fn fix_rpaths(
            &self,
            path: &Path,
            rel_origin: &str,
            _keep: &[String],
            _drop: &[String],
        ) -> Result<(), String> {
            self.rewrite(path, &format!("rpath={rel_origin}"))
        }
        fn resign(&self, path: &Path, _sign_id: Option<&str>) -> Result<(), String> {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
            self.rewrite(path, &format!("sig={}", name.unwrap_or_default()))
        }
        fn origin_token(&self) -> &'static str {
            "@loader_path"
        }
    }

    /// A backend over fake objects (files starting `OBJ`) whose load commands are
    /// scripted per path; a re-sign appends `|sig`, so a test can tell the donor's bytes
    /// from what the signature pass added.
    #[cfg(unix)]
    struct ScriptedBackend {
        refs: BTreeMap<PathBuf, ObjectRefs>,
    }

    #[cfg(unix)]
    impl Backend for ScriptedBackend {
        fn is_native_object(&self, path: &Path) -> bool {
            std::fs::read(path).is_ok_and(|b| b.starts_with(b"OBJ"))
        }
        fn read_refs(&self, path: &Path) -> Result<ObjectRefs, String> {
            Ok(self
                .refs
                .get(path)
                .map_or_else(ObjectRefs::default, |r| ObjectRefs {
                    id: r.id.clone(),
                    needed: r.needed.clone(),
                    rpaths: r.rpaths.clone(),
                }))
        }
        fn set_id_portable(&self, _path: &Path, _basename: &str) -> Result<(), String> {
            Ok(())
        }
        fn repoint_dep(&self, _path: &Path, _from: &str, _basename: &str) -> Result<(), String> {
            Ok(())
        }
        fn fix_rpaths(&self, _: &Path, _: &str, _: &[String], _: &[String]) -> Result<(), String> {
            Ok(())
        }
        fn resign(&self, path: &Path, _sign_id: Option<&str>) -> Result<(), String> {
            let mut bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            bytes.extend_from_slice(b"|sig");
            std::fs::write(path, bytes).map_err(|e| e.to_string())
        }
        fn origin_token(&self) -> &'static str {
            "@loader_path"
        }
    }

    /// A scratch dir holding `stage/bin` (removed on drop), and its canonical path.
    #[cfg(unix)]
    fn scratch(label: &str) -> (aterm_tempfile::TempDir, PathBuf) {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-reloc-{label}-"))
            .tempdir()
            .unwrap();
        std::fs::create_dir_all(dir.path().join("stage/bin")).unwrap();
        let path = std::fs::canonicalize(dir.path()).unwrap();
        (dir, path)
    }

    /// A tool loading rustup's nightly `librustc_driver`, by absolute path or through an
    /// rpath into rustup's directory, is refused before a byte is copied, and declaring that
    /// directory a donor root does not launder it.
    #[cfg(unix)]
    #[test]
    fn a_stock_toolchain_library_is_never_vendored() {
        let (_dir, d) = scratch("stock");
        let stage = d.join("stage");
        let tool = stage.join("bin/tool");
        std::fs::write(&tool, b"OBJ tool").unwrap();
        let absolute = "/Users//x/.rustup/toolchains/nightly-2025-12-03-aarch64-apple-darwin/lib/librustc_driver-3136bc78c25198f1.dylib";
        let backend = ScriptedBackend {
            refs: BTreeMap::from([(
                tool.clone(),
                ObjectRefs {
                    needed: vec![absolute.to_string()],
                    ..ObjectRefs::default()
                },
            )]),
        };
        let err = relocate_with(&stage, &backend, None, &[]).expect_err("stock by absolute path");
        assert!(
            err.contains("a stock toolchain library cannot be vendored"),
            "{err}"
        );
        assert!(
            !stage.join(VENDOR_REL).exists(),
            "nothing may be written to {VENDOR_REL}"
        );

        let system = d.join("usr/local/rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu/lib");
        std::fs::create_dir_all(&system).unwrap();
        std::fs::write(system.join("libstd-0123456789abcdef.so"), b"OBJ stock std").unwrap();
        let backend = ScriptedBackend {
            refs: BTreeMap::from([(
                tool.clone(),
                ObjectRefs {
                    needed: vec![
                        system
                            .join("libstd-0123456789abcdef.so")
                            .display()
                            .to_string(),
                    ],
                    ..ObjectRefs::default()
                },
            )]),
        };
        let err = relocate_with(&stage, &backend, None, std::slice::from_ref(&system))
            .expect_err("a system rustup's library, even as a declared donor");
        assert!(
            err.contains("a stock toolchain library cannot be vendored"),
            "{err}"
        );
        assert!(
            !stage.join(VENDOR_REL).exists(),
            "nothing may be written to {VENDOR_REL}"
        );

        let nightly = d.join("fake/.rustup/toolchains/nightly-2025-12-03-aarch64-apple-darwin/lib");
        std::fs::create_dir_all(&nightly).unwrap();
        std::fs::write(
            nightly.join("librustc_driver-3136bc78c25198f1.dylib"),
            b"OBJ stock driver",
        )
        .unwrap();
        let backend = ScriptedBackend {
            refs: BTreeMap::from([(
                tool.clone(),
                ObjectRefs {
                    needed: vec!["@rpath/librustc_driver-3136bc78c25198f1.dylib".to_string()],
                    rpaths: vec![nightly.display().to_string()],
                    ..ObjectRefs::default()
                },
            )]),
        };
        let err = relocate_with(&stage, &backend, None, std::slice::from_ref(&nightly))
            .expect_err("stock through an rpath, even as a declared donor");
        assert!(
            err.contains("a stock toolchain library cannot be vendored"),
            "{err}"
        );
        assert!(
            !stage.join(VENDOR_REL).exists(),
            "nothing may be written to {VENDOR_REL}"
        );
    }

    /// The Trust shape: a library from inside a declared donor root is copied
    /// byte-identical (the only later change is the re-sign), and the copy is recorded.
    #[cfg(unix)]
    #[test]
    fn a_donor_root_library_is_vendored_byte_identical_and_recorded() {
        let (_dir, d) = scratch("donor");
        let stage = d.join("stage");
        let seal = d.join("seal");
        std::fs::create_dir_all(seal.join("lib")).unwrap();
        let donor = seal.join("lib/librustc_driver-0123456789abcdef.dylib");
        std::fs::write(&donor, b"OBJ trust driver").unwrap();
        let tool = stage.join("bin/tool");
        std::fs::write(&tool, b"OBJ tool").unwrap();
        let backend = ScriptedBackend {
            refs: BTreeMap::from([(
                tool.clone(),
                ObjectRefs {
                    needed: vec![donor.display().to_string()],
                    ..ObjectRefs::default()
                },
            )]),
        };
        let report =
            relocate_with(&stage, &backend, None, std::slice::from_ref(&seal)).expect("relocate");
        assert!(report.unresolved.is_empty(), "{:?}", report.unresolved);
        assert_eq!(
            report.vendored,
            vec!["librustc_driver-0123456789abcdef.dylib"]
        );
        let want = atpkg::tree::sha256_file(&donor).unwrap();
        assert_eq!(
            report.vendored_from,
            vec![Vendored {
                name: "librustc_driver-0123456789abcdef.dylib".to_string(),
                sha256: want.clone(),
                donor_rel: "lib/librustc_driver-0123456789abcdef.dylib".to_string(),
            }]
        );
        let copy = std::fs::read(
            stage
                .join(VENDOR_REL)
                .join("librustc_driver-0123456789abcdef.dylib"),
        )
        .unwrap();
        assert_eq!(
            copy, b"OBJ trust driver|sig",
            "byte-identical to the donor before the re-sign"
        );
        let record = std::fs::read_to_string(stage.join(VENDOR_REL).join(VENDORED_RECORD)).unwrap();
        assert!(
            record.contains(&format!("{want} librustc_driver-0123456789abcdef.dylib lib/librustc_driver-0123456789abcdef.dylib")),
            "{record}"
        );
    }

    /// A library that exists outside every declared donor root is left unresolved and not
    /// one byte is copied.
    #[cfg(unix)]
    #[test]
    fn a_library_outside_every_donor_root_is_never_vendored() {
        let (_dir, d) = scratch("undeclared");
        let stage = d.join("stage");
        let seal = d.join("seal");
        std::fs::create_dir_all(seal.join("lib")).unwrap();
        let outside = d.join("elsewhere/libfoo.dylib");
        std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
        std::fs::write(&outside, b"OBJ builder-local foo").unwrap();
        let vendored = stage.join(VENDOR_REL);
        let undeclared = |report: &Report| {
            report.unresolved.len() == 1
                && report.unresolved[0].starts_with("libfoo.dylib: ")
                && report.unresolved[0].contains("is not inside a declared donor root")
        };

        let backend = ScriptedBackend {
            refs: BTreeMap::new(),
        };
        let mut have = BTreeSet::new();
        let mut report = Report::default();
        let copied = vendor_file(
            &outside,
            &VendorTo {
                dir: &vendored,
                backend: &backend,
                donor_roots: std::slice::from_ref(&seal),
            },
            "libfoo.dylib",
            &mut have,
            &mut report,
        )
        .expect("an undeclared library is unresolved, never an error");
        assert!(!copied, "a library outside every donor root was vendored");
        assert!(undeclared(&report), "{:?}", report.unresolved);
        assert!(report.vendored.is_empty() && report.vendored_from.is_empty());
        assert!(have.is_empty());
        assert!(!vendored.exists(), "nothing may be written to {VENDOR_REL}");

        std::os::unix::fs::symlink(&outside, seal.join("lib/libfoo.dylib")).unwrap();
        let tool = stage.join("bin/tool");
        std::fs::write(&tool, b"OBJ tool").unwrap();
        let backend = ScriptedBackend {
            refs: BTreeMap::from([(
                tool,
                ObjectRefs {
                    needed: vec!["@rpath/libfoo.dylib".to_string()],
                    rpaths: vec![seal.join("lib").display().to_string()],
                    ..ObjectRefs::default()
                },
            )]),
        };
        let report =
            relocate_with(&stage, &backend, None, std::slice::from_ref(&seal)).expect("relocate");
        assert!(report.vendored.is_empty(), "{:?}", report.vendored);
        assert!(undeclared(&report), "{:?}", report.unresolved);
        assert!(report.require_self_contained().is_err());
        assert!(
            !vendored.join("libfoo.dylib").exists(),
            "nothing may be written to {VENDOR_REL}"
        );
    }

    /// A hard-link group leaves relocation as one file: rewritten and signed once under its
    /// first name, the other names re-linked onto the result.
    #[cfg(unix)]
    #[test]
    fn a_hard_link_group_is_relocated_once_and_stays_one_file() {
        use std::os::unix::fs::MetadataExt;
        let scratch = aterm_tempfile::Builder::new()
            .prefix("atpkg-reloc-hardlink-")
            .tempdir()
            .unwrap();
        let stage = scratch.path().to_path_buf();
        std::fs::create_dir_all(stage.join("bin")).unwrap();
        std::fs::create_dir_all(stage.join("lib/rustlib/t/lib")).unwrap();
        std::fs::write(stage.join("bin/rustc"), b"OBJ compiler").unwrap();
        std::fs::hard_link(stage.join("bin/rustc"), stage.join("bin/trustc")).unwrap();
        std::fs::write(stage.join("lib/libdriver.dylib"), b"OBJ driver").unwrap();
        std::fs::hard_link(
            stage.join("lib/libdriver.dylib"),
            stage.join("lib/rustlib/t/lib/libdriver.dylib"),
        )
        .unwrap();
        std::fs::write(stage.join("bin/solo"), b"OBJ solo").unwrap();

        let backend = ReplacingBackend {
            log: std::cell::RefCell::default(),
        };
        let report = relocate_with(&stage, &backend, None, &[]).unwrap();
        assert!(report.unresolved.is_empty(), "{:?}", report.unresolved);
        assert_eq!(report.rewritten, 3, "one rewrite per object, not per name");

        let ino = |rel: &str| std::fs::metadata(stage.join(rel)).unwrap().ino();
        let text = |rel: &str| std::fs::read_to_string(stage.join(rel)).unwrap();
        assert_eq!(
            ino("bin/rustc"),
            ino("bin/trustc"),
            "bin/rustc must still be bin/trustc"
        );
        assert_eq!(
            text("bin/trustc"),
            "OBJ compiler|rpath=@loader_path/../lib/atpkg-vendored|sig=rustc"
        );
        assert_eq!(
            ino("lib/libdriver.dylib"),
            ino("lib/rustlib/t/lib/libdriver.dylib"),
            "the dylib must not ship twice"
        );
        assert_eq!(
            text("lib/rustlib/t/lib/libdriver.dylib"),
            "OBJ driver|rpath=@loader_path/atpkg-vendored\
             |rpath=@loader_path/../../../atpkg-vendored|sig=libdriver.dylib"
        );
        assert_eq!(
            text("bin/solo"),
            "OBJ solo|rpath=@loader_path/../lib/atpkg-vendored|sig=solo"
        );
        let log = backend.log.borrow();
        assert_eq!(
            log.iter().filter(|l| l.starts_with("sig=")).count(),
            3,
            "{log:?}"
        );
        assert!(
            !log.iter().any(|l| l.starts_with("sig=trustc")),
            "trustc must not be signed under its own name: {log:?}"
        );
        let leftovers: Vec<PathBuf> = walk_files(&stage)
            .unwrap()
            .into_iter()
            .filter(|p| p.to_string_lossy().contains("atpkg-relink"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "re-link scratch left behind: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&stage);
    }
}
