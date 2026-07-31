// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pack-time relocation (§10.1 "self-contained" bundle path) — make a staged
//! sysroot/toolchain payload run on a machine that lacks the BUILDER's layout
//! (`~/.rustup`, `/opt/homebrew`, `/home/<builder>`), so a `trust`/`trust-mc`
//! bundle can be installed with NO rustup and NO Developer ID on the user side.
//!
//! This is the producer counterpart to the consumer's install-time
//! [`crate::sysroot`] wiring. The design fixes relocation at PACK time, not install
//! time, for two hard reasons (audit: relocation-architecture):
//!   1. install-time binary rewriting would mutate the tree AFTER the signed
//!      `tree_root` was computed, breaking [`crate::install`]'s apply-time
//!      re-verify;
//!   2. a package manager cannot assume the relocation toolchain
//!      (`install_name_tool`/`patchelf`) exists on every USER's machine — but it
//!      is always present on the (single) build box.
//!
//! Cross-platform by construction: one OS-agnostic orchestration
//! ([`relocate_stage`]) over a per-OS [`Backend`] — Mach-O
//! (`install_name_tool`/`otool`/`codesign`, `@loader_path`/`@rpath`) on macOS,
//! ELF (`patchelf`, `$ORIGIN` `RUNPATH`, no code signature) on Linux. The wire
//! anchor is the Ed25519 signature over the tarball, so nothing here depends on
//! Apple signing.
//!
//! Self-containment is fail-closed: any machine-local reference the pass cannot
//! vendor is a HARD error (a "portable" bundle that still dlopens the builder's
//! `~/.rustup` is worse than none).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where vendored machine-local shared libraries are copied, relative to the
/// stage root. Kept distinct from any real `lib/` content the toolchain ships.
pub const VENDOR_REL: &str = "lib/atpkg-vendored";

/// A path that hard-codes THIS build machine's layout — the thing relocation
/// must eliminate from a self-contained bundle. `/home/` covers a Linux
/// builder's `~/.rustup`; `/Users//` + `/opt/homebrew` cover macOS.
#[must_use]
pub fn is_machine_local(path: &str) -> bool {
    path.starts_with("/opt/homebrew")
        || path.starts_with(concat!("/", "Users", "/"))
        || path.starts_with("/home/")
        || path.contains("/.rustup/")
        || path.contains("/.cargo/")
}

/// The load-command view of one native object, produced by a [`Backend`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ObjectRefs {
    /// The object's own install-name / soname, if it declares one (dylibs/`.so`).
    pub id: Option<String>,
    /// Dependency references: absolute paths or `@rpath/…` (Mach-O), or bare
    /// sonames (ELF).
    pub needed: Vec<String>,
    /// Runtime search paths (`LC_RPATH` / `DT_RPATH`+`DT_RUNPATH`).
    pub rpaths: Vec<String>,
}

/// The per-OS mechanics of reading and rewriting a native object. The
/// orchestration in [`relocate_stage`] is identical across platforms; only these
/// primitives differ.
pub trait Backend {
    /// Is `path` a native executable/shared object this backend relocates?
    fn is_native_object(&self, path: &Path) -> bool;
    /// Read the object's id / needed / rpaths.
    fn read_refs(&self, path: &Path) -> Result<ObjectRefs, String>;
    /// Rewrite the object's own install-name/soname to the portable form.
    fn set_id_portable(&self, path: &Path, basename: &str) -> Result<(), String>;
    /// Repoint an absolute machine-local dependency to the portable form.
    fn repoint_dep(&self, path: &Path, from: &str, basename: &str) -> Result<(), String>;
    /// Ensure the object searches `rel_origin` (an `@loader_path`/`$ORIGIN`
    /// relative dir) and NO machine-local rpath. `drop` lists the absolute rpaths
    /// to remove. Backends that can only set the whole rpath (ELF `patchelf`) use
    /// `keep`+`rel_origin` and ignore `drop`.
    fn fix_rpaths(
        &self,
        path: &Path,
        rel_origin: &str,
        keep: &[String],
        drop: &[String],
    ) -> Result<(), String>;
    /// Re-establish a valid code signature after a rewrite (macOS: mandatory on
    /// arm64; Linux: no-op). `sign_id` = Developer-ID identity or ad-hoc when
    /// `None`.
    fn resign(&self, path: &Path, sign_id: Option<&str>) -> Result<(), String>;
    /// How this object references a vendored dependency by basename in its
    /// `needed` list (Mach-O: `@rpath/<base>`; ELF: the bare `<base>` soname).
    fn portable_dep_ref(&self, basename: &str) -> String;
    /// The relative-origin token for this platform (`@loader_path` / `$ORIGIN`).
    fn origin_token(&self) -> &'static str;
}

/// Outcome of relocating one staged payload.
#[derive(Debug, Default)]
pub struct Report {
    /// Basenames vendored into `lib/atpkg-vendored/`.
    pub vendored: Vec<String>,
    /// Objects whose load commands were rewritten.
    pub rewritten: usize,
    /// Machine-local references that could NOT be resolved — for a self-contained
    /// bundle the caller MUST treat a non-empty list as a hard failure.
    pub unresolved: Vec<String>,
}

impl Report {
    /// Fail-closed self-containment assertion: no machine-local reference may
    /// remain in a bundle that claims to be self-contained.
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

/// The backend for the host OS. Producers run on the target OS (or a matching
/// cross environment), so host == artifact triple's OS.
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

/// Relocate a staged payload in place. Vendors every machine-local dependency
/// (transitively) into `lib/atpkg-vendored/`, rewrites objects to reference them
/// through a relative-origin rpath, deletes machine-local rpaths, and re-signs.
///
/// # Errors
/// I/O, a missing relocation tool, or a rewrite failure. Unresolved machine-local
/// refs are returned in the [`Report`] (not an error here) so the caller decides
/// self-contained (hard fail) vs advisory.
pub fn relocate_stage(stage: &Path, sign_id: Option<&str>) -> Result<Report, String> {
    let backend = backend_for_host()?;
    relocate_with(stage, backend.as_ref(), sign_id)
}

/// [`relocate_stage`] against an explicit backend (the seam unit tests drive).
pub fn relocate_with(
    stage: &Path,
    backend: &dyn Backend,
    sign_id: Option<&str>,
) -> Result<Report, String> {
    let mut report = Report::default();
    let vendored = stage.join(VENDOR_REL);

    // ONE recursive traversal of the stage feeds BOTH passes below. The stage is a
    // multi-GB toolchain payload, so walking it twice was a second full `read_dir` +
    // sort over tens of thousands of entries for a list we already had.
    let files = walk_files(stage)?;

    // Shared libs already in the payload, by basename — never re-vendor them.
    let mut have: BTreeSet<String> = BTreeSet::new();
    for rel in &files {
        if is_shared_lib_name(rel)
            && let Some(b) = rel.file_name().and_then(|n| n.to_str())
        {
            have.insert(b.to_string());
        }
    }

    // Machine-local rpath dirs anywhere in the payload are the donor search path
    // for `@rpath/<name>` / soname deps that don't resolve inside the payload.
    let mut donors: BTreeSet<PathBuf> = BTreeSet::new();
    let mut queue: Vec<PathBuf> = Vec::new();
    // The pre-scan's load commands, KEPT rather than thrown away. `read_refs` is not a
    // parse — it SPAWNS a subprocess per object (`otool -l` on macOS; three `patchelf`
    // calls on Linux) — and the queue loop below immediately re-read every object the
    // pre-scan had just read. A staged sysroot holds hundreds to thousands of native
    // objects, so that was hundreds-to-thousands of redundant fork+exec cycles.
    let mut prescanned: BTreeMap<PathBuf, ObjectRefs> = BTreeMap::new();
    for rel in &files {
        let p = stage.join(rel);
        if backend.is_native_object(&p) {
            let refs = backend.read_refs(&p)?;
            for r in &refs.rpaths {
                if is_machine_local(r) {
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
        // Serve the pre-scan when it has this object, else read it. The miss case is
        // exactly the files VENDORED mid-loop (pushed below): those are freshly copied
        // and re-id'd, so they must never be served from a pre-mutation entry — hence
        // both push sites also drop any pre-scan entry for the destination. Everything
        // else is read strictly before the loop's first mutation and `processed`
        // guarantees each object is handled at most once, so a served entry is exactly
        // what a fresh read would return.
        let refs = match prescanned.remove(&obj) {
            Some(refs) => refs,
            None => backend.read_refs(&obj)?,
        };
        let mut changed = false;

        // 1. A machine-local install-id → the portable form.
        if let Some(id) = &refs.id
            && is_machine_local(id)
        {
            backend.set_id_portable(&obj, &basename(id))?;
            changed = true;
        }

        // 2. Dependencies.
        for dep in &refs.needed {
            if is_machine_local(dep) {
                // Absolute machine-local load (Mach-O) → vendor + repoint.
                let base = basename(dep);
                if vendor_file(Path::new(dep), &vendored, &base, &mut have, &mut report)? {
                    let dest = vendored.join(&base);
                    prescanned.remove(&dest); // freshly copied + re-id'd: never stale-serve it
                    queue.push(dest);
                }
                backend.repoint_dep(&obj, dep, &base)?;
                changed = true;
            } else {
                // `@rpath/<name>` (Mach-O) or a bare soname (ELF) that does not
                // resolve inside the payload → find it in a donor rpath + vendor.
                let base = basename(dep);
                let unresolved_in_payload =
                    dep.starts_with("@rpath/") || is_shared_lib_basename(&base);
                // A soname resolvable via the system loader (libc, …) is NOT in a
                // donor dir → `find_in_donors` returns None and it is left alone
                // (correct: never vendor system libs).
                if unresolved_in_payload
                    && !have.contains(&base)
                    && let Some(src) = find_in_donors(&base, &donors)
                {
                    if vendor_file(&src, &vendored, &base, &mut have, &mut report)? {
                        let dest = vendored.join(&base);
                        prescanned.remove(&dest); // freshly copied + re-id'd (see above)
                        queue.push(dest);
                    }
                    changed = true;
                }
            }
        }

        // 3./4. Point the object at the vendored dir via a relative rpath and drop
        // machine-local rpaths.
        let ml_rpaths: Vec<String> = refs
            .rpaths
            .iter()
            .filter(|r| is_machine_local(r))
            .cloned()
            .collect();
        let keep: Vec<String> = refs
            .rpaths
            .iter()
            .filter(|r| !is_machine_local(r))
            .cloned()
            .collect();
        let needs_vendor = vendored.is_dir()
            && refs.needed.iter().any(|n| {
                n.starts_with("@rpath/")
                    || is_machine_local(n)
                    || is_shared_lib_basename(&basename(n))
            });
        if needs_vendor || !ml_rpaths.is_empty() {
            let dir = obj.parent().unwrap_or(stage);
            let rel = origin_relative(backend.origin_token(), dir, &vendored);
            if let Err(e) = backend.fix_rpaths(&obj, &rel, &keep, &ml_rpaths) {
                report.unresolved.push(format!(
                    "{}: rpath rewrite failed: {e}",
                    rel_display(stage, &obj)
                ));
            } else {
                changed = true;
            }
        }

        if changed {
            backend.resign(&obj, sign_id)?;
            report.rewritten += 1;
        }
    }

    report.vendored.sort();
    report.vendored.dedup();
    Ok(report)
}

/// Copy `src` → `vendored/<base>` once, make its own id portable, and re-sign
/// ad-hoc. Returns whether a NEW file was vendored (so the caller chases ITS
/// deps). Records an unresolved entry when the source is missing.
fn vendor_file(
    src: &Path,
    vendored: &Path,
    base: &str,
    have: &mut BTreeSet<String>,
    report: &mut Report,
) -> Result<bool, String> {
    if have.contains(base) {
        return Ok(false);
    }
    if !src.is_file() {
        report
            .unresolved
            .push(format!("{}: source not found to vendor", src.display()));
        return Ok(false);
    }
    std::fs::create_dir_all(vendored).map_err(|e| format!("create vendored dir: {e}"))?;
    let dest = vendored.join(base);
    std::fs::copy(src, &dest).map_err(|e| format!("vendor {}: {e}", src.display()))?;
    let backend = backend_for_host()?;
    backend.set_id_portable(&dest, base)?;
    backend.resign(&dest, None)?;
    have.insert(base.to_string());
    report.vendored.push(base.to_string());
    Ok(true)
}

fn find_in_donors(base: &str, donors: &BTreeSet<PathBuf>) -> Option<PathBuf> {
    donors.iter().map(|d| d.join(base)).find(|p| p.is_file())
}

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// A relative path from a Mach-O/ELF file's directory to the vendored dir,
/// prefixed with the platform origin token. Joined with `/` explicitly — the token is
/// consumed by the TARGET binary's loader (dyld/ld.so), whose path syntax is always
/// `/`-separated, so a Windows host (whose `Path` renders `\`) must never leak
/// backslashes into an rpath. (On Unix this is byte-identical to `Path` display.)
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

/// Recursively collect regular files under `dir` (symlinks NOT followed), paths
/// relative to `dir`, sorted.
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

/// Whether `path` is a native executable/shared object (Mach-O OR ELF) — the
/// only files a dynamic-loader resolve check is meaningful for. Cross-platform
/// (no backend needed): a script/data file has no dylib deps to resolve.
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

fn run(tool: &str, args: &[&str], file: &Path) -> Result<std::process::Output, String> {
    Command::new(tool)
        .args(args)
        .arg(file)
        .output()
        .map_err(|e| format!("spawn {tool}: {e}"))
}

// ---------------------------------------------------------------------------
// macOS / Mach-O backend (proven end-to-end: trust-mc's librustc_driver vendored,
// `trust-mc-compiler --version` runs with HOME hidden).
// ---------------------------------------------------------------------------
pub mod macho {
    use super::*;

    pub struct MachoBackend;

    /// Mach-O magic (thin arm64/x86_64 little-endian + universal).
    pub fn is_macho_magic(m: &[u8; 4]) -> bool {
        matches!(
            m,
            [0xcf, 0xfa, 0xed, 0xfe] // 64-bit LE (arm64 / x86_64)
                | [0xce, 0xfa, 0xed, 0xfe] // 32-bit LE
                | [0xca, 0xfe, 0xba, 0xbe] // universal (fat)
                | [0xbe, 0xba, 0xfe, 0xca] // universal, byte-swapped
        )
    }

    /// Pure parser over `otool -l <file>` output.
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

    fn strip_offset(s: &str) -> String {
        s.split(" (offset").next().unwrap_or(s).trim().to_string()
    }

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
            // Mach-O rpaths are additive: add the vendored search path, then delete
            // each machine-local one (keep the rest untouched).
            //
            // "Is `rel_origin` already there?" is answered from `keep` instead of a THIRD
            // `otool -l` spawn on this object. `keep` and `drop` ARE the caller's exact
            // `is_machine_local` partition of this object's rpaths, and neither `-id` nor
            // `-change` (the only rewrites between that read and here) touches LC_RPATH —
            // so `keep ∪ drop` is still the live rpath set. `rel_origin` is an
            // `@loader_path`-relative path, never machine-local, so it can only ever land
            // in `keep` (asserted by `origin_relative_rpath_is_never_machine_local`).
            if !keep.iter().any(|r| r == rel_origin) {
                install_name_tool(&["-add_rpath", rel_origin], path)?;
            }
            for d in drop {
                install_name_tool(&["-delete_rpath", d], path)?;
            }
            Ok(())
        }
        fn resign(&self, path: &Path, sign_id: Option<&str>) -> Result<(), String> {
            let mut c = Command::new("/usr/bin/codesign");
            c.arg("--force");
            match sign_id {
                Some(id) => c.args(["--timestamp", "--sign", id]),
                None => c.args(["--sign", "-"]), // ad-hoc: arm64 needs a valid sig to exec
            };
            let out = c
                .arg(path)
                .output()
                .map_err(|e| format!("spawn codesign: {e}"))?;
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
        fn portable_dep_ref(&self, basename: &str) -> String {
            format!("@rpath/{basename}")
        }
        fn origin_token(&self) -> &'static str {
            "@loader_path"
        }
    }
}

// ---------------------------------------------------------------------------
// Linux / ELF backend (implemented; the pure parsing/command logic is unit-
// tested, but it has NOT been executed against real ELF on this macOS box —
// needs a Linux validation run with `patchelf` present).
// ---------------------------------------------------------------------------
pub mod elf {
    use super::*;

    pub struct ElfBackend;

    /// ELF magic (`\x7fELF`).
    pub fn is_elf_magic(m: &[u8; 4]) -> bool {
        *m == [0x7f, b'E', b'L', b'F']
    }

    /// Split a `patchelf --print-rpath` value (`:`-separated) into entries.
    pub fn parse_rpath(value: &str) -> Vec<String> {
        value
            .split(':')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// Parse `patchelf --print-needed` (one soname per line).
    pub fn parse_needed(text: &str) -> Vec<String> {
        text.lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// The `patchelf --set-rpath` value making an object search the vendored dir
    /// (relative, `$ORIGIN`-based) plus any portable rpaths it already had —
    /// machine-local entries dropped. Deterministic order, deduped.
    pub fn combined_rpath(rel_origin: &str, keep: &[String]) -> String {
        let mut out: Vec<String> = vec![rel_origin.to_string()];
        for k in keep {
            if !is_machine_local(k) && !out.contains(k) {
                out.push(k.clone());
            }
        }
        out.join(":")
    }

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

    impl Backend for ElfBackend {
        fn is_native_object(&self, path: &Path) -> bool {
            magic4(path).is_some_and(|m| is_elf_magic(&m))
        }
        fn read_refs(&self, path: &Path) -> Result<ObjectRefs, String> {
            // soname is optional (executables have none); tolerate its absence.
            let id = patchelf_out(&["--print-soname"], path).ok().and_then(|s| {
                let s = s.trim().to_string();
                (!s.is_empty() && s != "no soname").then_some(s)
            });
            let needed = parse_needed(&patchelf_out(&["--print-needed"], path)?);
            let rpaths = parse_rpath(&patchelf_out(&["--print-rpath"], path)?);
            Ok(ObjectRefs { id, needed, rpaths })
        }
        fn set_id_portable(&self, path: &Path, basename: &str) -> Result<(), String> {
            // An ELF soname is a bare name (never machine-local); normalize to the
            // basename so consumers resolve it by name via the vendored rpath.
            patchelf(&["--set-soname", basename], path)
        }
        fn repoint_dep(&self, path: &Path, from: &str, basename: &str) -> Result<(), String> {
            // ELF DT_NEEDED is a soname, not an absolute path, so this only fires
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
            // ELF: set the whole RUNPATH at once (machine-local entries are simply
            // not carried into the combined value).
            let value = elf::combined_rpath(rel_origin, keep);
            patchelf(&["--set-rpath", &value, "--force-rpath"], path)
        }
        fn resign(&self, _path: &Path, _sign_id: Option<&str>) -> Result<(), String> {
            Ok(()) // ELF has no code signature; the Ed25519 tarball anchor covers integrity
        }
        fn portable_dep_ref(&self, basename: &str) -> String {
            basename.to_string()
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
        assert!(!is_machine_local("@rpath/libx.dylib"));
        assert!(!is_machine_local("$ORIGIN/../lib"));
        assert!(!is_machine_local("/usr/lib/libSystem.B.dylib"));
        assert!(!is_machine_local("libc.so.6"));
    }

    #[test]
    fn origin_relative_is_platform_tokened() {
        let stage = Path::new("/s");
        let vendored = stage.join(VENDOR_REL); // /s/lib/atpkg-vendored
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

    // The Mach-O `fix_rpaths` decides "is the vendored rpath already on this object?"
    // from the caller's `keep` list rather than re-spawning `otool -l`. That is sound
    // because `keep`/`drop` are the exact `is_machine_local` partition of the object's
    // rpaths AND the rpath being added is origin-relative, so it can only ever be
    // classified into `keep` — never into `drop`, and never missed. It holds even when
    // the stage itself sits under a machine-local prefix, because `origin_relative`
    // strips the common prefix before emitting the token-relative tail.
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
        let text = "          cmd LC_ID_DYLIB\n         name /Users//b/deps/libkani.dylib (offset 24)\n          cmd LC_LOAD_DYLIB\n         name @rpath/librustc_driver.dylib (offset 24)\n          cmd LC_RPATH\n         path /Users//b/.rustup/x/lib (offset 12)\n          cmd LC_RPATH\n         path @loader_path/../toolchain/lib (offset 12)\n";
        let r = macho::parse_otool(text);
        assert_eq!(r.id.as_deref(), Some("/Users//b/deps/libkani.dylib"));
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
        // combined_rpath keeps $ORIGIN + portable entries, drops machine-local ones.
        assert_eq!(
            elf::combined_rpath(
                "$ORIGIN/../lib/atpkg-vendored",
                &["/home/b/.rustup/x/lib".into(), "$ORIGIN/../lib".into()]
            ),
            "$ORIGIN/../lib/atpkg-vendored:$ORIGIN/../lib"
        );
    }

    #[test]
    fn require_self_contained_fails_on_leftover() {
        let mut r = Report::default();
        assert!(r.require_self_contained().is_ok());
        r.unresolved
            .push("bin/x: @rpath/libfoo.so not found".into());
        assert!(r.require_self_contained().is_err());
    }
}
