// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pack-time relocation for self-contained bundles.

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
    /// The object's own install-name or soname, if it declares one.
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
    /// The ELF interpreter, absent for shared libraries, static binaries and other formats.
    fn interpreter(&self, _path: &Path) -> Result<Option<String>, String> {
        Ok(None)
    }
    /// Rewrite an ELF interpreter to the target system's dynamic loader.
    fn set_interpreter(&self, _path: &Path, _loader: &str) -> Result<(), String> {
        Err("this relocation backend cannot rewrite an ELF interpreter".into())
    }
    /// Rewrite the object's own install-name/soname to the portable form.
    fn set_id_portable(&self, path: &Path, basename: &str) -> Result<(), String>;
    /// Repoint an absolute machine-local dependency to the portable form.
    fn repoint_dep(&self, path: &Path, from: &str, basename: &str) -> Result<(), String>;
    /// Ensure the object searches `rel_origin` (an `@loader_path`/`$ORIGIN` relative dir) and no
    /// machine-local rpath.
    fn fix_rpaths(
        &self,
        path: &Path,
        rel_origin: &str,
        keep: &[String],
        drop: &[String],
    ) -> Result<(), String>;
    /// Re-establish a valid code signature after a rewrite (macOS: mandatory on arm64; Linux:
    /// no-op).
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
/// I/O, a missing relocation tool, a rewrite failure, or a library that would be vendored
/// from a stock toolchain. Unresolved builder refs, including a library outside every donor root,
/// and a failed rpath rewrite are returned in the [`Report`] so the caller decides hard fail or
/// advisory.
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

    // Explicit admitted toolchains are both the copy allowlist and a source of
    // runtime libraries. A portable input may have only origin-relative rpaths
    // (MC's toolchain install link), so no absolute rpath identifies its donor.
    let mut donors: BTreeSet<PathBuf> = donor_roots
        .iter()
        .map(|root| root.join("lib"))
        .filter(|lib| lib.is_dir())
        .collect();
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
                if !unresolved_in_payload || have.contains(&base) {
                    continue;
                }
                if let Some(src) = find_in_donors(&base, &donors) {
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
                } else if dep.starts_with("@rpath/") {
                    report.unresolved.push(format!(
                        "{}: {dep} is absent from the payload and declared donor libraries",
                        rel_display(stage, &obj)
                    ));
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

fn in_donor_root<'r>(path: &str, donor_roots: &'r [PathBuf]) -> Option<&'r PathBuf> {
    if donor_roots.is_empty() || !path.starts_with('/') {
        return None;
    }
    let p = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    donor_roots.iter().find(|r| p.starts_with(r))
}

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
    let _ = std::fs::remove_file(&dest);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    opts.open(&dest)
        .and_then(|mut f| {
            use std::io::Write as _;
            f.write_all(text.as_bytes())
        })
        .map_err(|e| format!("write {}: {e}", dest.display()))
}

struct VendorTo<'a> {
    dir: &'a Path,
    backend: &'a dyn Backend,
    donor_roots: &'a [PathBuf],
}

fn stock_vendor_refusal(base: &str, src: &Path, place: &str) -> String {
    format!(
        "{base}: a stock toolchain library cannot be vendored — {} resolves into {place}. A \
         bundle vendors only from a Trust toolchain named with --donor-root; build what \
         loads it against one",
        src.display()
    )
}

/// Copy `src` into `to.dir` as `base`, make its id portable and re-sign it. Returns whether
/// a new file was vendored.
///
/// # Errors
/// A stock toolchain's library (checked on the path as named and as resolved), a copy that
/// differs from its donor, or an I/O, rewrite or re-sign failure.
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
    crate::identity::copy_regular_file(&canonical, &dest)
        .map_err(|e| format!("vendor {}: {e}", src.display()))?;
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

fn relink_aliases(stage: &Path, obj: &Path, aliases: &[PathBuf]) -> Result<(), String> {
    let obj_key = hard_link_key(obj);
    for alias in aliases {
        if obj_key.is_some() && obj_key == hard_link_key(alias) {
            continue;
        }
        super::relink::replace(obj, alias).map_err(|e| {
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

/// A relative path from a Mach-O/ELF file's directory to the vendored dir, prefixed with the
/// platform origin token.
pub fn origin_relative(origin_token: &str, macho_dir: &Path, vendored: &Path) -> String {
    let rel = relative(macho_dir, vendored);
    let mut out = String::from(origin_token);
    for c in rel.components() {
        out.push('/');
        out.push_str(&c.as_os_str().to_string_lossy());
    }
    out
}

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

fn is_shared_lib_name(rel: &Path) -> bool {
    rel.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_shared_lib_basename)
}

fn is_shared_lib_basename(name: &str) -> bool {
    name.ends_with(".dylib") || name.contains(".so.") || name.ends_with(".so")
}

/// Recursively collect regular files under `dir` (symlinks not followed), paths relative to `dir`,
/// sorted.
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

/// Whether `path` is a native executable or shared object (Mach-O or ELF), the only files a
/// dynamic-loader resolve check means anything for.
#[must_use]
pub fn is_native_object(path: &Path) -> bool {
    magic4(path).is_some_and(|m| macho::is_macho_magic(&m) || elf::is_elf_magic(&m))
}

/// Read the first 4 magic bytes of a file (native-object sniff).
pub fn magic4(path: &Path) -> Option<[u8; 4]> {
    use std::io::Read;
    let (mut f, _) = crate::identity::open_regular_nonblock(path).ok()?;
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

    /// Read a supported ELF64 little-endian image's PT_INTERP without invoking its loader.
    ///
    /// # Errors
    /// Malformed headers, an unsupported encoding, or an invalid interpreter string.
    pub fn interpreter(bytes: &[u8]) -> Result<Option<String>, String> {
        metadata(&mut std::io::Cursor::new(bytes), bytes.len() as u64)
            .map(|metadata| metadata.interpreter)
    }

    struct Metadata {
        interpreter: Option<String>,
        #[cfg(any(target_os = "linux", test))]
        dynamic: bool,
    }

    /// Classify only admitted runtime images: absence of dynamic tags in malformed bytes
    /// must not turn a failed tool invocation into a successful static relocation.
    fn metadata(
        input: &mut (impl std::io::Read + std::io::Seek),
        file_size: u64,
    ) -> Result<Metadata, String> {
        let invalid = || "invalid ELF program headers or interpreter".to_string();
        // Read only the fixed header, at most 8192 program headers, and one bounded
        // interpreter. Large ELF payloads never need to be loaded just to classify them.
        let mut bytes_at = |offset: u64, size: usize| -> Result<Vec<u8>, String> {
            if offset
                .checked_add(size as u64)
                .is_none_or(|end| end > file_size)
            {
                return Err(invalid());
            }
            let mut bytes = vec![0; size];
            input
                .seek(std::io::SeekFrom::Start(offset))
                .and_then(|_| input.read_exact(&mut bytes))
                .map_err(|error| format!("read ELF metadata: {error}"))?;
            Ok(bytes)
        };
        let header = bytes_at(0, 64)?;
        if header.get(..7) != Some(b"\x7fELF\x02\x01\x01") {
            return Err("the GNU package requires a little-endian ELF64 object".into());
        }
        let word = |bytes: &[u8], at: usize| {
            bytes
                .get(at..at.checked_add(8)?)
                .and_then(|part| <[u8; 8]>::try_from(part).ok())
                .map(u64::from_le_bytes)
        };
        let half = |at: usize| {
            header
                .get(at..at + 2)
                .and_then(|part| <[u8; 2]>::try_from(part).ok())
                .map(u16::from_le_bytes)
                .map(usize::from)
        };
        let offset = word(&header, 32).ok_or_else(invalid)?;
        let stride = half(54).ok_or_else(invalid)?;
        let count = half(56).ok_or_else(invalid)?;
        if !matches!(half(16), Some(2 | 3))
            || header.get(20..24) != Some(&1_u32.to_le_bytes())
            || half(52) != Some(64)
            || count == 0
            || count > 8192
            || stride != 56
        {
            return Err(invalid());
        }
        let headers = bytes_at(offset, stride * count)?;
        let mut interpreter = None;
        let mut dynamic = false;
        let mut load = false;
        for entry in headers.chunks_exact(stride) {
            let tag = &entry[..4];
            if tag == 0_u32.to_le_bytes() {
                continue;
            }
            let start = word(entry, 8).ok_or_else(invalid)?;
            let size = word(entry, 32).ok_or_else(invalid)?;
            if start.checked_add(size).is_none_or(|end| end > file_size) {
                return Err(invalid());
            }
            if tag == 1_u32.to_le_bytes() {
                load = true;
                let memory = word(entry, 40).ok_or_else(invalid)?;
                if memory < size
                    || word(entry, 16)
                        .and_then(|v| v.checked_add(memory))
                        .is_none()
                {
                    return Err(invalid());
                }
            } else if tag == 2_u32.to_le_bytes() {
                if dynamic {
                    return Err(invalid());
                }
                dynamic = true;
            } else if tag != 3_u32.to_le_bytes() {
                continue;
            } else {
                if interpreter.is_some() {
                    return Err(invalid());
                }
                let size = usize::try_from(size)
                    .ok()
                    .filter(|size| *size <= 4096)
                    .ok_or_else(invalid)?;
                let raw = bytes_at(start, size)?;
                let raw = raw.strip_suffix(&[0]).ok_or_else(invalid)?;
                let text = std::str::from_utf8(raw).map_err(|_| invalid())?;
                if text.is_empty() || text.bytes().any(|b| b.is_ascii_control()) {
                    return Err(invalid());
                }
                interpreter = Some(text.to_string());
            }
        }
        if !load {
            return Err(invalid());
        }
        Ok(Metadata {
            interpreter,
            #[cfg(any(target_os = "linux", test))]
            dynamic,
        })
    }

    #[cfg(any(target_os = "linux", test))]
    fn file_metadata(path: &Path) -> Result<Metadata, String> {
        let (mut file, info) = crate::identity::open_regular_nonblock(path)
            .map_err(|error| format!("open {}: {error}", path.display()))?;
        metadata(&mut file, info.len())
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

    /// The `patchelf --set-rpath` value making an object search the vendored dir (relative,
    /// `$ORIGIN`-based) plus any portable rpaths it already had.
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

    #[cfg(any(target_os = "linux", test))]
    type RefReader<'a> = dyn Fn(&[&str], &Path) -> Result<String, String> + 'a;

    #[cfg(any(target_os = "linux", test))]
    fn read_refs_with(path: &Path, read: &RefReader<'_>) -> Result<ObjectRefs, String> {
        let metadata = file_metadata(path)?;
        if metadata.interpreter.is_none() && !metadata.dynamic {
            return Ok(ObjectRefs::default());
        }
        let id = read(&["--print-soname"], path).ok().and_then(|s| {
            let s = s.trim().to_string();
            (!s.is_empty() && s != "no soname").then_some(s)
        });
        let needed = parse_needed(&read(&["--print-needed"], path)?);
        let rpaths = parse_rpath(&read(&["--print-rpath"], path)?);
        Ok(ObjectRefs { id, needed, rpaths })
    }

    #[cfg(target_os = "linux")]
    impl Backend for ElfBackend {
        fn is_native_object(&self, path: &Path) -> bool {
            magic4(path).is_some_and(|m| is_elf_magic(&m))
        }
        fn read_refs(&self, path: &Path) -> Result<ObjectRefs, String> {
            read_refs_with(path, &patchelf_out)
        }
        fn interpreter(&self, path: &Path) -> Result<Option<String>, String> {
            file_metadata(path).map(|metadata| metadata.interpreter)
        }
        fn set_interpreter(&self, path: &Path, loader: &str) -> Result<(), String> {
            patchelf(&["--set-interpreter", loader], path)
        }
        fn set_id_portable(&self, path: &Path, basename: &str) -> Result<(), String> {
            patchelf(&["--set-soname", basename], path)
        }
        fn repoint_dep(&self, path: &Path, from: &str, basename: &str) -> Result<(), String> {
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
            Ok(())
        }
        fn origin_token(&self) -> &'static str {
            "$ORIGIN"
        }
    }

    #[cfg(test)]
    mod static_tests;
}

#[cfg(test)]
#[path = "relocate_tests.rs"]
mod tests;
