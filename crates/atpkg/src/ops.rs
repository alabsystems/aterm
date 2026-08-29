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
use crate::store::ToolName;

/// Resolve `tool` to the store path its `bin/<tool>` shim points at, if installed
/// (`atpkg which`). Returns the raw symlink target (e.g.
/// `…/store/<program>/<build>/bin/<tool>`), or `None` when there is no shim.
///
/// Takes a raw `&str` because this is a CLI query over user input; a name
/// [`crate::store::shim_allowed`] refuses could never have been given a shim, so it correctly
/// answers `None` without touching the filesystem.
#[must_use]
pub fn which(layout: &Layout, tool: &str) -> Option<PathBuf> {
    crate::platform::resolve_shim(&layout.shim(&ToolName::new(tool)?))
}

/// Parse `(program, build)` out of a shim target like
/// `…/store/<program>/<build>/bin/<tool>` — the component right after `store` and the
/// numeric one after it. `None` if the path isn't a store shim target.
// `pub(crate)` so the GC scan reuses the exact same store-path parser.
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

/// Parse `(program, build)` out of a path that must lie INSIDE `prefix`'s store:
/// `<prefix>/store/<program>/<build>[/…]`. `None` for anything else.
///
/// The ANCHORED counterpart of [`program_build_of_target`], and the one every predicate that
/// DELETES must use. `program_build_of_target` searches for the first component named
/// `store`, which is right for a target already known to be ours and wrong as a containment
/// test: `/Users//x/src/store/ay/18/bin/ay` — a dev checkout, outside the prefix entirely —
/// parses as `("ay", 18)` there. Stripping `<prefix>/store` instead means only paths that
/// really are inside this managed store can answer, and `..` is rejected for free (a
/// `ParentDir` component is not `Normal`, so a lexical escape cannot masquerade as a program
/// name). A trailing path (`/bin/<tool>`) is allowed, because a shim target has one.
pub(crate) fn store_build_of(prefix: &Path, target: &Path) -> Option<(String, u64)> {
    let rel = target.strip_prefix(prefix.join("store")).ok()?;
    let mut comps = rel.components();
    let (Some(std::path::Component::Normal(program)), Some(std::path::Component::Normal(build))) =
        (comps.next(), comps.next())
    else {
        return None;
    };
    // `OsStr::to_str` goes via `call1`: std's INLINED `unsafe` (the `from_utf8_unchecked`
    // fast path) is otherwise attributed to this function's spans as missing-SAFETY-comment
    // refutations under the strict Trust gate (see `lib.rs`). Same call, same receiver.
    let program = crate::call1(std::ffi::OsStr::to_str, program)?.to_string();
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
///
/// **This is a DERIVED view and it is NOT a GC authority.** The fold below is last-write-wins
/// over `read_dir` order, so when a program's shims disagree — the state a dropped tool used
/// to leave behind, and that a shim-install loop failing partway still can — *which* build
/// this reports depends on directory iteration order. That is harmless for the questions this
/// answers ("is something newer staged?", "is this program installed?") and catastrophic for
/// "what may I delete?": feeding an older build in as `current` classified the live tree as
/// superseded. [`crate::gc::live_builds`] resolves the authoritative `store/<program>/current`
/// link for that decision and requires this view to agree with it.
#[must_use]
/// The command names `program`'s INSTALLED build currently puts on PATH.
///
/// Derived from the shims themselves rather than from a stored manifest, for the same reason
/// [`active_builds`] is: the shims are what a user actually invokes, so they are the
/// authoritative answer to "what does this program expose here", and they are exactly what
/// `atpkg unlink` restores. A manifest copy could disagree with the directory after a partial
/// install; the directory cannot disagree with itself.
///
/// Order is `read_dir`'s, so callers that care must sort — this is a SET, not a sequence.
/// Returns `None` when the program has no shims at all (nothing installed), which is
/// distinguishable from `Some(vec![])` and lets a caller refuse rather than silently link
/// nothing.
///
/// The `alab-<tool>` ALIASES are not exposes — they are derived from the primaries
/// ([`crate::activate::Aliases`]) — so they are left out: a sysroot dev link that had to
/// cover `alab-trustc` would abort on every checkout, and a link is what this feeds.
pub fn installed_exposes(layout: &Layout, program: &str) -> Option<Vec<String>> {
    let shims = std::fs::read_dir(layout.bin_dir()).ok()?;
    let mut out = Vec::new();
    for shim in shims.flatten() {
        let Some(target) = crate::platform::resolve_shim(&shim.path()) else {
            continue;
        };
        let Some((owner, _build)) = program_build_of_target(&target) else {
            continue;
        };
        if owner != program {
            continue;
        }
        let name = shim.file_name().to_string_lossy().into_owned();
        if ToolName::from_shim_file(&name).is_some_and(|t| t.is_alias()) {
            continue;
        }
        out.push(name);
    }
    if out.is_empty() { None } else { Some(out) }
}

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
            // Last write wins. The tools of one program USUALLY agree, and where they do not
            // the winner is `read_dir` order — see the doc comment: never make a destructive
            // decision from this number.
            out.insert(program, build);
        }
    }
    out
}

/// The **tool names** whose `bin/` shims currently point into `store/<program>/<build>/`
/// — the exact tool set a rollback must re-point (or drop). Reuses
/// [`program_build_of_target`], so it matches only shims that actually resolve into this
/// program's given build. Sorted for determinism. Empty if `bin/` is unreadable or nothing
/// points into the build.
///
/// The result is [`ToolName`]s, not file names: a caller feeds them straight back through
/// `Layout::shim` / `install_tools` / `install_tombstone_shim`, which re-append the platform
/// suffix, and the reason to keep the type all the way through is that handing those the raw
/// `bin/` entry would double it (`bin/ay.cmd.cmd`) — writing tombstones and rollback shims
/// BESIDE the live shim instead of replacing it. [`ToolName::from_shim_file`] owns that strip.
///
/// PRIMARIES only: the `alab-<tool>` aliases that resolve into the same build are listed
/// by [`active_aliases`] instead, so a caller that re-lays or probes "the tools this
/// program exposes" never treats an alias as a tool of its own.
#[must_use]
pub fn active_tools(layout: &Layout, program: &str, build: u64) -> Vec<ToolName> {
    active_names(layout, program, build, false)
}

/// The `alab-<tool>` ALIAS names whose `bin/` shims currently point into
/// `store/<program>/<build>/` — the counterpart of [`active_tools`], for the passes that
/// must treat an alias exactly like its primary (tombstoning a revoked build, the
/// rollback verb's policy probe). Sorted; empty when the program has no aliases laid.
#[must_use]
pub fn active_aliases(layout: &Layout, program: &str, build: u64) -> Vec<ToolName> {
    active_names(layout, program, build, true)
}

/// The shared scan behind [`active_tools`] (`aliases == false`) and [`active_aliases`]
/// (`true`).
fn active_names(layout: &Layout, program: &str, build: u64, aliases: bool) -> Vec<ToolName> {
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
            && let Some(tool) = ToolName::from_shim_file(name)
            && tool.is_alias() == aliases
        {
            out.push(tool);
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
/// delete (§10.2).
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

    fn tool(name: &str) -> ToolName {
        ToolName::new(name).unwrap()
    }

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
        // The concrete executable the shim forwards to (`<program>.exe` on Windows).
        std::fs::write(
            dir.join("bin").join(tool(program).exe_file()),
            b"#!/bin/true\n",
        )
        .unwrap();
        install_shims(
            layout,
            &dir,
            &[program.to_string()],
            crate::activate::Aliases::Off,
        )
        .unwrap();
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

    /// The anchored parser refuses a target that merely LOOKS like a store path. This is the
    /// difference that matters to `prune_stale_shims`, which deletes files on the user's
    /// PATH: a dev-link into a checkout that happens to contain `store/ay/18/` must not be
    /// mistaken for one of ours.
    #[test]
    fn store_build_of_is_anchored_to_the_prefix() {
        let prefix = Path::new("/p");
        assert_eq!(
            store_build_of(prefix, Path::new("/p/store/ay/18/bin/ay")),
            Some(("ay".to_string(), 18))
        );
        // The build dir itself (no trailing path) parses too.
        assert_eq!(
            store_build_of(prefix, Path::new("/p/store/ay/18")),
            Some(("ay".to_string(), 18))
        );
        // Outside the prefix — the unanchored `program_build_of_target` says yes to this one.
        assert_eq!(
            store_build_of(prefix, Path::new("/other/store/ay/18")),
            None
        );
        assert_eq!(
            program_build_of_target(Path::new("/other/store/ay/18")),
            Some(("ay".to_string(), 18)),
            "the unanchored parser is why the anchored one has to exist"
        );
        // Shapes the store never writes: no build, a non-numeric build, a `..` escape.
        assert_eq!(store_build_of(prefix, Path::new("/p/store/ay")), None);
        assert_eq!(store_build_of(prefix, Path::new("/p/store/ay/head")), None);
        assert_eq!(store_build_of(prefix, Path::new("/p/store/../ay/18")), None);
    }

    #[test]
    fn which_resolves_installed_shim() {
        let l = layout("which");
        let dir = install(&l, "ay", 18);
        // The shim forwards to the concrete executable (`bin/ay` Unix, `bin\ay.exe` Windows).
        assert_eq!(
            which(&l, "ay"),
            Some(dir.join("bin").join(tool("ay").exe_file()))
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
        assert_eq!(active_tools(&l, "ay", 18), vec![tool("ay")]);
        assert_eq!(active_tools(&l, "ny", 9), vec![tool("ny")]);
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
