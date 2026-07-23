// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Rollback-aware, `~/.kani`-aware GC retention (§10.2) — the pure decision of which store
//! builds may be reclaimed.
//!
//! Retention is **current + 1 rollback** per coherence group, and a build that a **live
//! `~/.kani` symlink** still references is **never** reclaimed (deleting a superseded store
//! build that `~/.kani` points at resurrects the cryptic *"Reading release bundle
//! rust-toolchain-version: No such file or directory"* failure `setup-trust-mc.sh` warns
//! about). So [`reclaimable`] keeps the current build, the single most-recent *superseded*
//! build (the rollback target), and any pinned (kani-referenced) build; everything else is
//! safe to delete. Pure — the caller does the actual removal, fail-closed, only inside the
//! managed prefix.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::store::Layout;

/// The store builds (of one program) that may be reclaimed, given all `installed` builds,
/// the `current` active build, and any `pinned` builds a live `~/.kani` symlink references.
///
/// Kept: `current`, the highest installed build **below** `current` (the rollback target),
/// and every `pinned` build. Returned (reclaimable): everything else, ascending. The
/// current build is never reclaimed even if it is also pinned/has no rollback.
#[must_use]
pub fn reclaimable(installed: &[u64], current: u64, pinned: &[u64]) -> Vec<u64> {
    let mut keep: BTreeSet<u64> = pinned.iter().copied().collect();
    keep.insert(current);
    // The rollback target is the most-recent build that `current` superseded.
    if let Some(rollback) = installed.iter().copied().filter(|&b| b < current).max() {
        keep.insert(rollback);
    }
    let mut out: Vec<u64> = installed
        .iter()
        .copied()
        .filter(|b| !keep.contains(b))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// What a GC pass reclaimed: per program, the superseded builds discarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcReport {
    /// `(program, discarded builds ascending)` for every program that had reclaimable builds.
    pub reclaimed: Vec<(String, Vec<u64>)>,
}

/// Discover the live `~/.kani` pinned set: for every `~/.kani/kani-*` symlink whose RAW
/// target resolves UNDER the managed store and names `store/<program>/<build>/…`, record
/// `(program, build)`. Uses the raw link target ([`std::fs::read_link`], never
/// `canonicalize`) and requires `starts_with(store)` — a hostile `~/.kani` can therefore
/// only ADD to the keep set (over-retain, safe), never cause a delete. No `~/.kani` ⇒ no
/// pins.
#[must_use]
pub fn discover_kani_pinned(layout: &Layout, kani_home: &Path) -> BTreeMap<String, Vec<u64>> {
    let mut out: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    let store = layout.prefix.join("store");
    let Ok(entries) = std::fs::read_dir(kani_home) else {
        return out; // no ~/.kani => no pins
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("kani-") {
            continue;
        }
        let Ok(target) = std::fs::read_link(e.path()) else {
            continue;
        };
        if !target.starts_with(&store) {
            continue; // ours only; foreign / outside-prefix ignored
        }
        if let Some((program, build)) = crate::ops::program_build_of_target(&target) {
            out.entry(program).or_default().push(build);
        }
    }
    out
}

/// Reclaim superseded builds per program: the current (active shim target) build + one
/// rollback + every `~/.kani`-pinned build are kept; the rest are discarded. Runs after a
/// successful activate and behind `atpkg gc`. A program with NO active build is SKIPPED
/// (no safe `current` to compute a rollback target from — never blanket-reclaim). Pure
/// deletion of superseded builds inside the vetted prefix; it never touches an active,
/// staged, or pinned tree, so no `tree_root` is perturbed and no shim is removed.
#[must_use]
pub fn run(layout: &Layout, kani_home: &Path) -> GcReport {
    let active = crate::ops::active_builds(layout);
    let pinned = discover_kani_pinned(layout, kani_home);
    let mut by_prog: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for (p, b) in crate::ops::list_installed(layout) {
        by_prog.entry(p).or_default().push(b);
    }
    let mut reclaimed = Vec::new();
    for (program, installed) in by_prog {
        let Some(&current) = active.get(&program) else {
            continue;
        };
        let pins = pinned.get(&program).cloned().unwrap_or_default();
        let dead = reclaimable(&installed, current, &pins);
        if dead.is_empty() {
            continue;
        }
        for b in &dead {
            crate::store::discard_build(&layout.build_dir(&program, *b));
        }
        reclaimed.push((program, dead));
    }
    GcReport { reclaimed }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_current_plus_one_rollback() {
        // current 19, rollback 18 ⇒ reclaim the older 16, 17.
        assert_eq!(reclaimable(&[16, 17, 18, 19], 19, &[]), vec![16, 17]);
    }

    #[test]
    fn never_reclaims_a_kani_referenced_build() {
        // 16 is pinned by a live ~/.kani symlink ⇒ keep it even though it is old.
        assert_eq!(reclaimable(&[16, 17, 18, 19], 19, &[16]), vec![17]);
    }

    #[test]
    fn current_is_never_reclaimed() {
        assert!(reclaimable(&[19], 19, &[]).is_empty());
        // Even a single installed == current, with no rollback, keeps it.
        assert!(!reclaimable(&[19], 19, &[]).contains(&19));
    }

    #[test]
    fn no_rollback_below_current_keeps_only_current() {
        // current is the lowest installed ⇒ no rollback target ⇒ the higher ones (somehow
        // present but not current) are reclaimable; current stays.
        assert_eq!(reclaimable(&[19, 20, 21], 19, &[]), vec![20, 21]);
    }

    #[test]
    fn handles_duplicates_and_unsorted_input() {
        assert_eq!(reclaimable(&[18, 16, 19, 17, 16], 19, &[]), vec![16, 17]);
    }

    // --- the imperative executor + ~/.kani pinned-set discovery -----------------------

    use crate::activate::{activate_channel, install_shims};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-gc-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        Layout { prefix: p }
    }

    /// Lay down a COMPLETE (marker-written) build dir with `bin/<program>`. `shim` also
    /// installs + activates it (making it the ACTIVE build); otherwise it is a complete
    /// but inactive build on disk.
    fn seed(layout: &Layout, program: &str, build: u64, shim: bool) -> PathBuf {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin").join(program), b"#!/bin/true\n").unwrap();
        if shim {
            install_shims(layout, &dir, &[program.to_string()]).unwrap();
            activate_channel(layout, "stable", &dir).unwrap();
        }
        crate::store::mark_build_ready(&dir).unwrap();
        dir
    }

    /// A synthetic `~/.kani` home with a `kani-<v>` symlink INTO the store build.
    #[cfg(unix)] // symlink fixture — Unix-only
    fn kani_home_pinning(layout: &Layout, label: &str, links: &[(&str, &str, u64)]) -> PathBuf {
        let home =
            std::env::temp_dir().join(format!("atpkg-gc-kani-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        for (link, program, build) in links {
            let target = layout.build_dir(program, *build).join("sysroot");
            std::os::unix::fs::symlink(&target, home.join(link)).unwrap();
        }
        home
    }

    #[cfg(unix)] // symlink fixture — Unix-only
    #[test]
    fn discover_kani_pinned_records_only_managed_targets() {
        let l = layout("discover");
        seed(&l, "trust", 671, false);
        let home =
            std::env::temp_dir().join(format!("atpkg-gc-kani-discover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        // Managed: kani-0.67.0 -> prefix/store/trust/671/sysroot.
        std::os::unix::fs::symlink(
            l.build_dir("trust", 671).join("sysroot"),
            home.join("kani-0.67.0"),
        )
        .unwrap();
        // Foreign: kani-evil -> /tmp/evil (outside the store) is ignored.
        std::os::unix::fs::symlink("/tmp/evil", home.join("kani-evil")).unwrap();
        // Not a kani-* name: ignored.
        std::os::unix::fs::symlink(l.build_dir("trust", 671), home.join("other")).unwrap();

        let got = discover_kani_pinned(&l, &home);
        assert_eq!(got, BTreeMap::from([("trust".to_string(), vec![671u64])]));
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[cfg(unix)] // symlink fixture — Unix-only
    #[test]
    fn run_keeps_current_rollback_and_pinned_reclaims_the_rest() {
        let l = layout("run-keeps");
        for b in [16u64, 17, 18] {
            seed(&l, "ay", b, false);
        }
        seed(&l, "ay", 19, true); // ay@19 is active
        let home = kani_home_pinning(&l, "run-keeps", &[("kani-a", "ay", 16)]);

        let report = run(&l, &home);
        // reclaimable(&[16,17,18,19], 19, &[16]) == [17,18]? No: keep 19 (current), 18
        // (rollback = highest below current), 16 (pinned) => reclaim only 17.
        assert_eq!(report.reclaimed, vec![("ay".to_string(), vec![17u64])]);
        assert!(!l.build_dir("ay", 17).exists(), "17 reclaimed");
        assert!(!crate::store::build_is_complete(&l.build_dir("ay", 17)));
        for keep in [16u64, 18, 19] {
            assert!(l.build_dir("ay", keep).exists(), "{keep} survives");
        }
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn run_skips_a_program_with_no_active_build() {
        let l = layout("run-noactive");
        // Complete builds on disk but NO shim => no active build => never reclaim.
        seed(&l, "ay", 17, false);
        seed(&l, "ay", 18, false);
        let home = std::env::temp_dir().join(format!("atpkg-gc-none-{}", std::process::id()));
        let report = run(&l, &home);
        assert!(
            report.reclaimed.is_empty(),
            "no active build => nothing reclaimed"
        );
        assert!(l.build_dir("ay", 17).exists());
        assert!(l.build_dir("ay", 18).exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[cfg(unix)] // symlink fixture — Unix-only
    #[test]
    fn run_never_touches_a_kani_pinned_old_build() {
        let l = layout("run-pinned");
        seed(&l, "ay", 16, false);
        seed(&l, "ay", 17, false);
        seed(&l, "ay", 18, false);
        seed(&l, "ay", 19, true); // active
        // Pin 16 AND 17 via ~/.kani; only 18 (rollback) + 19 (current) + 16,17 (pinned)
        // are kept => nothing reclaimable.
        let home = kani_home_pinning(
            &l,
            "run-pinned",
            &[("kani-16", "ay", 16), ("kani-17", "ay", 17)],
        );
        let report = run(&l, &home);
        assert!(
            report.reclaimed.is_empty(),
            "every old build is pinned or rollback/current"
        );
        for b in [16u64, 17, 18, 19] {
            assert!(l.build_dir("ay", b).exists(), "pinned build {b} survives");
        }
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}
