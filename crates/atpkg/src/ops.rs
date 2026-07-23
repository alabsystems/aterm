// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Local store operations backing the read / maintenance CLI verbs (§11): `which`,
//! `list`, and `uninstall`. All operate on the on-disk [`Layout`] only — no network, no
//! signatures (those gate *installs*, §8). `uninstall` is **fail-closed**: it removes a
//! path only after confirming it resolves *inside* the managed prefix, so a tampered
//! symlink can never make it delete something elsewhere (§10.2).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::Layout;

/// Resolve `tool` to the store path its `bin/<tool>` shim points at, if installed
/// (`atpkg which`). Returns the raw symlink target (e.g.
/// `…/store/<program>/<build>/bin/<tool>`), or `None` when there is no shim.
#[must_use]
pub fn which(layout: &Layout, tool: &str) -> Option<PathBuf> {
    crate::platform::resolve_shim(&layout.shim(tool))
}

/// Parse `(program, build)` out of a shim target like
/// `…/store/<program>/<build>/bin/<tool>` — the component right after `store` and the
/// numeric one after it. `None` if the path isn't a store shim target.
// `pub(crate)` so `gc::discover_kani_pinned` reuses the exact same store-path parser.
pub(crate) fn program_build_of_target(target: &Path) -> Option<(String, u64)> {
    // `Path::iter` / `Iter::next` / `OsStr::to_str` go via `call1`: std's INLINED
    // `unsafe` (the `OsStr` byte-slice casts, the `from_utf8_unchecked` fast path)
    // is otherwise attributed to this function's spans as missing-SAFETY-comment
    // refutations under the strict Trust gate (see `lib.rs`). Same calls, same
    // receivers; behavior identical (`collect` is exactly this push loop). The
    // `saturating_add`s are identical too: `position` returns an in-bounds index,
    // so `i + 2` can never approach `usize::MAX`.
    let mut it = crate::call1(std::path::Path::iter, target);
    let mut comps: Vec<&std::ffi::OsStr> = Vec::new();
    while let Some(c) = crate::call1(<std::path::Iter<'_> as Iterator>::next, &mut it) {
        comps.push(c);
    }
    let i = comps.iter().position(|c| *c == "store")?;
    let program: &std::ffi::OsStr = comps.get(i.saturating_add(1))?;
    let program = crate::call1(std::ffi::OsStr::to_str, program)?.to_string();
    let build: &std::ffi::OsStr = comps.get(i.saturating_add(2))?;
    let build = crate::call1(std::ffi::OsStr::to_str, build)?
        .parse::<u64>()
        .ok()?;
    Some((program, build))
}

/// The **ACTIVE** build of each program: the build its `bin/` shims currently point INTO
/// (`store/<program>/<build>/bin/<tool>`). Unlike [`list_installed`] — which reports every
/// COMPLETE build dir on disk, including one that was staged but never activated — this
/// reflects what is actually live on `PATH`, so the update decision ([`crate::gate::decide`])
/// never mistakes a merely-staged build for the running one (which would report "up to date"
/// while the user keeps running the older active build).
#[must_use]
pub fn active_builds(layout: &Layout) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    let Ok(shims) = std::fs::read_dir(layout.bin_dir()) else {
        return out;
    };
    for shim in shims.flatten() {
        let Some(target) = crate::platform::resolve_shim(&shim.path()) else {
            continue;
        };
        if let Some((program, build)) = program_build_of_target(&target) {
            // A program's tools all point into the same active build; last write wins
            // (they agree). BTreeMap keeps it deterministic.
            out.insert(program, build);
        }
    }
    out
}

/// The **tool names** whose `bin/` shims currently point into `store/<program>/<build>/`
/// — the exact tool set a rollback must re-point (or drop). Reuses
/// [`program_build_of_target`], so it matches only shims that actually resolve into this
/// program's given build. Sorted for determinism. Empty if `bin/` is unreadable or nothing
/// points into the build. Names are LOGICAL (the shim file name minus the platform
/// [`crate::platform::SHIM_SUFFIX`], so `ay.cmd` reports as `ay` on Windows): callers feed
/// them back through `Layout::shim`/`install_shims`/`install_tombstone_shim`, which append
/// the suffix again — returning the raw file name would double it (`bin/ay.cmd.cmd`),
/// writing tombstones/rollback shims BESIDE the live shim instead of replacing it.
#[must_use]
pub fn active_tools(layout: &Layout, program: &str, build: u64) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(layout.bin_dir()) else {
        return out;
    };
    for e in entries.flatten() {
        // resolve_shim, NOT std::fs::read_link: on Windows a shim is a `.cmd` regular file
        // (read_link Errs on it), so a raw read_link returns [] and a rollback/tombstone
        // pass would re-point nothing. resolve_shim reads the symlink on Unix and parses the
        // `.cmd` on Windows — matching active_builds() above.
        let Some(target) = crate::platform::resolve_shim(&e.path()) else {
            continue;
        };
        if let Some((p, b)) = program_build_of_target(&target)
            && p == program
            && b == build
            && let Some(name) = e.file_name().to_str()
        {
            // Strip the concrete shim suffix back off (`.cmd` on Windows; empty on Unix,
            // where `strip_suffix("")` is the identity) — see the doc comment above.
            let tool = name
                .strip_suffix(crate::platform::SHIM_SUFFIX)
                .unwrap_or(name);
            out.push(tool.to_string());
        }
    }
    out.sort();
    out
}

/// List installed `(program, build)` pairs by walking `store/<program>/<build>/`
/// (`atpkg list`). Sorted by program then build. Non-numeric build dirs are ignored.
#[must_use]
pub fn list_installed(layout: &Layout) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let store = layout.prefix.join("store");
    let Ok(programs) = std::fs::read_dir(&store) else {
        return out;
    };
    for prog in programs.flatten() {
        if !prog.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        // `OsString::as_os_str` / `OsStr::to_str` go via `call1` (both here and
        // in the build loop below): std's INLINED `unsafe` (the `OsString` →
        // `OsStr` byte-slice cast, the `from_utf8_unchecked` fast path) is
        // otherwise attributed to this function's spans as
        // missing-SAFETY-comment refutations under the strict Trust gate (see
        // `lib.rs`). Same calls, same receivers; behavior identical.
        let pname_os = prog.file_name();
        let pname_str = crate::call1(std::ffi::OsString::as_os_str, &pname_os);
        let Some(pname) = crate::call1(std::ffi::OsStr::to_str, pname_str).map(str::to_string)
        else {
            continue;
        };
        if let Ok(builds) = std::fs::read_dir(prog.path()) {
            for b in builds.flatten() {
                let bname_os = b.file_name();
                let bname_str = crate::call1(std::ffi::OsString::as_os_str, &bname_os);
                if let Some(n) = crate::call1(std::ffi::OsStr::to_str, bname_str)
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    // Only COMPLETE builds count. A build dir left partial by a crash
                    // mid-extract has no completeness marker; counting it as installed
                    // would make the manager report up-to-date and never repair it.
                    if crate::store::build_is_complete(&b.path()) {
                        out.push((pname.clone(), n));
                    }
                }
            }
        }
    }
    out.sort();
    out
}

/// Uninstall `program` (`atpkg uninstall <program>`): remove every `bin/` shim that points
/// into the program's store tree, drop any channel `current` symlink that points into it,
/// then reclaim `store/<program>/`.
///
/// **Fail-closed:** each removal target is first confirmed to resolve *inside* the managed
/// prefix (`store/<program>` for the tree, the prefix for shims/links); anything pointing
/// outside is left untouched and reported, so a tampered symlink can never redirect a
/// delete (§10.2). (`~/.kani` reversal for `trust` lands with Phase 5.)
pub fn uninstall(layout: &Layout, program: &str) -> io::Result<()> {
    // Validate `program` as a single safe path component BEFORE building any filesystem
    // path: the lexical `starts_with` containment check below retains `..` tokens and so
    // cannot, on its own, stop a traversal escape (e.g. `program = "../../tmp/victim"`).
    // Reject empty, `.`, `..`, and any name carrying a separator (`/` or `\`) or NUL —
    // the same shape rule `store::shim_allowed` enforces, minus its shim-specific
    // sensitive-command deny-list. The store layout is always `store/<program>/<build>/`,
    // so a legitimate program is always exactly one directory name; this changes no valid
    // behavior.
    if program.is_empty()
        || program == "."
        || program == ".."
        || program.contains('/')
        || program.contains('\\')
        || program.contains('\0')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "program name must be a single safe path component (no separators, `.`, `..`, or NUL)",
        ));
    }

    let prog_store = layout.prefix.join("store").join(program);

    // 1. Remove shims whose target points into this program's store tree. resolve_shim (not
    //    std::fs::read_link) so this works on Windows, where a shim is a `.cmd` regular file
    //    that read_link Errs on — leaving orphaned shims that still forward to the (about-to-
    //    be-deleted) store tree and that `atpkg which` still reports as installed.
    if let Ok(entries) = std::fs::read_dir(layout.bin_dir()) {
        for e in entries.flatten() {
            let path = e.path();
            if let Some(target) = crate::platform::resolve_shim(&path)
                && target.starts_with(&prog_store)
            {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    // 2. Drop channel `current` links that point into this program's store tree (so no
    //    dangling active-set link remains after the builds are gone). Removal goes through
    //    platform::remove_link, not std::fs::remove_file: on Windows `current` is a directory
    //    JUNCTION and remove_file fails on it (ERROR_ACCESS_DENIED), which would leave the
    //    junction dangling into a deleted tree. remove_link unlinks the junction itself
    //    (remove_dir), never touching the target's contents.
    if let Ok(channels) = std::fs::read_dir(layout.prefix.join("channels")) {
        for ch in channels.flatten() {
            let cur = ch.path().join("current");
            if let Ok(target) = std::fs::read_link(&cur)
                && target.starts_with(&prog_store)
            {
                crate::platform::remove_link(&cur);
            }
        }
    }

    // 3. Reclaim the store tree — but only after confirming it is inside the prefix.
    if prog_store.starts_with(&layout.prefix) && prog_store.exists() {
        std::fs::remove_dir_all(&prog_store)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activate::{activate_channel, install_shims};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-ops-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        Layout { prefix: p }
    }

    /// Install a program build with one binary + shim + channel activation.
    fn install(layout: &Layout, program: &str, build: u64) -> PathBuf {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin").join(program), b"#!/bin/true\n").unwrap();
        install_shims(layout, &dir, &[program.to_string()]).unwrap();
        activate_channel(layout, "stable", &dir).unwrap();
        // A real install marks the build complete as its last step (verify_and_stage).
        crate::store::mark_build_ready(&dir).unwrap();
        dir
    }

    /// A build dir left WITHOUT the completeness marker — a crash mid-extract — must
    /// not be reported as installed, so the manager re-installs it instead of
    /// treating the partial tree as up-to-date. (regression for the partial-install bug)
    #[test]
    fn partial_build_without_marker_is_not_listed() {
        let l = layout("partial");
        // Simulate a crash mid-extract: files present, but no completeness marker.
        let dir = l.build_dir("ay", 18);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/ay"), b"partial").unwrap();
        assert!(!crate::store::build_is_complete(&dir));
        assert!(
            list_installed(&l).is_empty(),
            "partial (marker-less) build is not installed"
        );
        // Once marked complete, it is listed.
        crate::store::mark_build_ready(&dir).unwrap();
        assert_eq!(list_installed(&l), vec![("ay".into(), 18)]);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn which_resolves_installed_shim() {
        let l = layout("which");
        let dir = install(&l, "ay", 18);
        // The shim forwards to the concrete executable (`bin/ay` Unix, `bin\ay.exe` Windows).
        assert_eq!(
            which(&l, "ay"),
            Some(
                dir.join("bin")
                    .join(format!("ay{}", crate::platform::EXE_SUFFIX))
            )
        );
        assert_eq!(which(&l, "nope"), None);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// `active_builds` reports the LIVE build (what the shim points at), even when a newer
    /// build is staged + marked complete on disk but never activated — so the update decision
    /// re-flips it instead of mistaking the staged build for the running one (#19).
    #[test]
    fn active_builds_reflects_the_shim_not_a_staged_inactive_build() {
        let l = layout("active");
        install(&l, "ay", 17); // ay@17 is live (shim -> ay/17)
        // ay@18 staged + marked complete, but NOT activated (no shim repoint).
        let staged = l.build_dir("ay", 18);
        std::fs::create_dir_all(staged.join("bin")).unwrap();
        std::fs::write(staged.join("bin/ay"), b"#!/bin/true\n").unwrap();
        crate::store::mark_build_ready(&staged).unwrap();

        // list_installed sees the max COMPLETE build (18) — the STALE view.
        assert!(list_installed(&l).contains(&("ay".to_string(), 18)));
        // active_builds sees what is LIVE on PATH — ay@17.
        assert_eq!(active_builds(&l).get("ay").copied(), Some(17));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn active_tools_lists_only_this_builds_shims() {
        let l = layout("active-tools");
        install(&l, "ay", 18);
        install(&l, "ny", 9);
        assert_eq!(active_tools(&l, "ay", 18), vec!["ay".to_string()]);
        assert_eq!(active_tools(&l, "ny", 9), vec!["ny".to_string()]);
        // A build no shim points into yields nothing.
        assert!(active_tools(&l, "ay", 17).is_empty());
        assert!(active_tools(&l, "nope", 1).is_empty());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn list_installed_reports_program_builds() {
        let l = layout("list");
        install(&l, "ay", 18);
        install(&l, "ny", 9);
        install(&l, "ny", 10);
        assert_eq!(
            list_installed(&l),
            vec![("ay".into(), 18), ("ny".into(), 9), ("ny".into(), 10)]
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn uninstall_removes_shim_store_and_channel_link() {
        let l = layout("uninstall");
        install(&l, "ay", 18);
        assert!(which(&l, "ay").is_some());

        uninstall(&l, "ay").unwrap();

        assert!(which(&l, "ay").is_none(), "shim removed");
        assert!(!l.build_dir("ay", 18).exists(), "store tree reclaimed");
        assert!(
            !l.prefix.join("store/ay").exists(),
            "program store dir removed"
        );
        // The channel `current` (which pointed into ay's store) is gone, not dangling.
        assert!(std::fs::read_link(l.channel_current("stable")).is_err());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn uninstall_leaves_other_programs_intact() {
        let l = layout("uninstall-other");
        install(&l, "ay", 18);
        install(&l, "ny", 9);
        uninstall(&l, "ay").unwrap();
        // ny is untouched.
        assert!(which(&l, "ny").is_some());
        assert!(l.build_dir("ny", 9).exists());
        assert!(!l.build_dir("ay", 18).exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn uninstall_rejects_path_traversal_names() {
        let l = layout("uninstall-traversal");

        // A sibling directory *outside* the managed prefix that a `..` escape would reach.
        // `store/../../<victim>` resolves to `prefix.parent().parent()/<victim>`.
        let victim = l
            .prefix
            .parent()
            .unwrap()
            .join("atpkg-victim-do-not-delete");
        let _ = std::fs::remove_dir_all(&victim);
        std::fs::create_dir_all(victim.join("keep")).unwrap();

        // Lexically, `prefix/store/../../atpkg-victim-do-not-delete` starts_with(prefix),
        // so the old guard would have let `remove_dir_all` escape the prefix.
        let traversal = "../../atpkg-victim-do-not-delete";
        let err = uninstall(&l, traversal).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(
            victim.join("keep").exists(),
            "out-of-prefix directory must NOT be deleted by a traversal program name"
        );

        // A bare `..` would otherwise resolve `store/..` to the prefix itself and wipe it.
        let err = uninstall(&l, "..").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(
            l.prefix.exists(),
            "managed prefix must survive `uninstall ..`"
        );

        // Other unsafe shapes are refused too, with no filesystem effect.
        for bad in ["", ".", "a/b", "a\\b", "x\0y"] {
            assert_eq!(
                uninstall(&l, bad).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }

        let _ = std::fs::remove_dir_all(&victim);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}
