// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn live(program: &str, build: u64) -> LiveBuild {
    LiveBuild {
        program: program.to_string(),
        build,
    }
}

#[test]
fn live_from_claims_reconciles_the_two_views() {
    fn claims(pairs: &[(&str, &[u64])]) -> BTreeMap<String, BTreeSet<u64>> {
        pairs
            .iter()
            .map(|(p, b)| ((*p).to_string(), b.iter().copied().collect()))
            .collect()
    }
    let build_of = |set: &LiveSet| set.live.get("ay").map(|l| l.build);
    let link = |pairs: &[(&str, u64)]| -> BTreeMap<String, u64> {
        pairs.iter().map(|(p, b)| ((*p).to_string(), *b)).collect()
    };

    let set = live_from_claims(&link(&[("ay", 19)]), &claims(&[("ay", &[19])]));
    assert_eq!(build_of(&set), Some(19));
    assert!(set.diverged.is_empty());
    let set = live_from_claims(&link(&[("ay", 19)]), &claims(&[]));
    assert_eq!(build_of(&set), Some(19));
    assert!(set.diverged.is_empty());

    let set = live_from_claims(&link(&[("ay", 19)]), &claims(&[("ay", &[18])]));
    assert!(set.live.is_empty());
    assert_eq!(set.diverged.len(), 1);
    for (a, sh) in [
        (link(&[("ay", 19)]), claims(&[("ay", &[18, 19])])),
        (link(&[]), claims(&[("ay", &[19])])),
    ] {
        let set = live_from_claims(&a, &sh);
        assert!(set.live.is_empty(), "no witness from {a:?} / {sh:?}");
        assert_eq!(set.diverged.len(), 1);
    }
}

#[test]
fn reclaimable_keeps_current_plus_one_rollback() {
    let rows: [(&[u64], &[u64], &str); 4] = [
        (
            &[16, 17, 18, 19],
            &[16, 17],
            "live 19, rollback 18: the older 16, 17 go",
        ),
        (&[19], &[], "a single installed build that is live is kept"),
        (
            &[19, 20, 21],
            &[20, 21],
            "no rollback below the live build: only it stays",
        ),
        (
            &[18, 16, 19, 17, 16],
            &[16, 17],
            "duplicates and unsorted input",
        ),
    ];
    for (installed, reclaimed, why) in rows {
        let got = reclaimable(installed, &live("ay", 19), &|_| true);
        assert_eq!(got, reclaimed, "{why}");
        assert!(
            !got.contains(&19),
            "the live build is never reclaimed: {why}"
        );
    }
}

use crate::activate::{activate_build, install_shims};

fn tool(name: &str) -> crate::store::ToolName {
    crate::store::ToolName::new(name).unwrap()
}

fn layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-gc-{label}-"))
}

fn seed(layout: &Layout, program: &str, build: u64, shim: bool) -> PathBuf {
    let dir = layout.build_dir(program, build);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(
        dir.join("bin").join(tool(program).exe_file()),
        b"#!/bin/true\n",
    )
    .unwrap();
    if shim {
        install_shims(
            layout,
            &dir,
            &[program.to_string()],
            crate::activate::Aliases::Off,
        )
        .unwrap();
        activate_build(layout, &dir).unwrap();
    }
    crate::store::mark_build_ready(&dir).unwrap();
    dir
}

fn seed_partial(layout: &Layout, program: &str, build: u64) -> PathBuf {
    let dir = layout.build_dir(program, build);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(
        dir.join("bin").join(tool(program).exe_file()),
        b"#!/bin/true\n",
    )
    .unwrap();
    assert!(
        !crate::store::build_is_complete(&dir),
        "the fixture is only interesting while it is marker-less"
    );
    dir
}

fn seed_scratch(layout: &Layout, program: &str, name: &str) -> PathBuf {
    let dir = layout.prefix.join("store").join(program).join(name);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join("bin").join("payload"), b"half an extract\n").unwrap();
    dir
}

#[test]
fn run_keeps_current_and_rollback_reclaims_the_rest() {
    let l = layout("run-keeps");
    for b in [16u64, 17, 18] {
        seed(&l, "ay", b, false);
    }
    seed(&l, "ay", 19, true);
    let report = run(&l, None);
    assert_eq!(report.reclaimed, vec![("ay".to_string(), vec![16u64, 17])]);
    assert!(report.diverged.is_empty(), "the two views agree");
    for gone in [16u64, 17] {
        assert!(!l.build_dir("ay", gone).exists(), "{gone} reclaimed");
    }
    for keep in [18u64, 19] {
        assert!(l.build_dir("ay", keep).exists(), "{keep} survives");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let stuck = seed(&l, "ay", 15, false);
        let mode = |m| {
            for d in [stuck.join("bin"), stuck.clone()] {
                std::fs::set_permissions(&d, std::fs::Permissions::from_mode(m)).unwrap();
            }
        };
        mode(0o500);
        let report = run(&l, None);
        mode(0o755);
        assert!(
            stuck.join("bin").join(tool("ay").exe_file()).exists(),
            "the fixture's payload could not be removed"
        );
        assert!(report.reclaimed.is_empty(), "{:?}", report.reclaimed);
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A build a tree's trust.lock resolved within the keep window survives gc though it is
/// neither current nor the rollback; the same build unmarked, or marked past the window, goes.
#[test]
fn run_keeps_a_build_a_lock_resolved_lately() {
    let l = layout("run-locked");
    for b in [16u64, 17, 18] {
        seed(&l, "ay", b, false);
    }
    seed(&l, "ay", 19, true);
    let mark = l
        .build_dir("ay", 16)
        .with_file_name(format!("16{}", crate::trust_lock::LOCKED_SUFFIX));
    std::fs::write(&mark, b"").unwrap();
    let report = run(&l, None);
    assert_eq!(report.reclaimed, vec![("ay".to_string(), vec![17u64])]);
    assert!(l.build_dir("ay", 16).exists(), "the locked build survives");
    let stale = std::time::SystemTime::now()
        - crate::trust_lock::LOCKED_KEEP
        - std::time::Duration::from_secs(60);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&mark)
        .unwrap()
        .set_modified(stale)
        .unwrap();
    let report = run(&l, None);
    assert_eq!(report.reclaimed, vec![("ay".to_string(), vec![16u64])]);
    assert!(!mark.exists(), "the mark goes with its build");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn run_skips_a_program_with_no_active_build() {
    let l = layout("run-noactive");
    seed(&l, "ay", 17, false);
    seed(&l, "ay", 18, false);
    let report = run(&l, None);
    assert!(
        report.reclaimed.is_empty(),
        "no witness => nothing reclaimed"
    );
    assert!(report.diverged.is_empty(), "unclaimed is not diverged");
    assert!(l.build_dir("ay", 17).exists());
    assert!(l.build_dir("ay", 18).exists());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_stale_shim_never_deletes_the_linked_live_build() {
    let l = layout("stale-shim-brick");
    seed(&l, "ay", 18, false);
    let b19 = seed(&l, "ay", 19, false);
    activate_build(&l, &b19).unwrap();
    crate::platform::ensure_private_dir(&l.bin_dir()).unwrap();
    let ay = tool("ay");
    crate::platform::install_shim(&l.build_dir("ay", 18).join("bin"), &ay, &l.shim(&ay)).unwrap();

    let report = run(&l, None);
    assert!(
        report.reclaimed.is_empty(),
        "a contested program is never reclaimed"
    );
    assert!(
        l.build_dir("ay", 19).exists(),
        "the link's LIVE build must survive a stale shim"
    );
    assert!(l.build_dir("ay", 18).exists(), "and so must the shims'");
    assert_eq!(
        report.diverged,
        vec![Divergence {
            program: "ay".to_string(),
            reason: Diverged::LinkShimMismatch {
                link_says: 19,
                shims_say: 18,
            },
        }]
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn tools_of_one_program_split_across_builds_yield_no_witness() {
    let l = layout("split-shims");
    let b18 = seed(&l, "ay", 18, false);
    let b19 = seed(&l, "ay", 19, false);
    activate_build(&l, &b19).unwrap();
    install_shims(&l, &b19, &["ay".to_string()], crate::activate::Aliases::Off).unwrap();
    let aylint = tool("aylint");
    crate::platform::install_shim(&b18.join("bin"), &aylint, &l.shim(&aylint)).unwrap();

    let report = run(&l, None);
    assert!(report.reclaimed.is_empty());
    assert!(l.build_dir("ay", 18).exists() && l.build_dir("ay", 19).exists());
    assert_eq!(
        report.diverged,
        vec![Divergence {
            program: "ay".to_string(),
            reason: Diverged::ShimsDisagree {
                builds: vec![18, 19],
            },
        }]
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// An authority that cannot be resolved (dangling, out-of-store, absent) gives no witness,
/// and there is no fallback to the shim view.
#[cfg(unix)]
#[test]
fn a_broken_authority_skips_the_program_instead_of_reclaiming() {
    let l = layout("bad-channel");
    seed(&l, "ay", 17, false);
    seed(&l, "ay", 18, false);
    seed(&l, "ay", 19, true);
    let links = [l.program_current("ay")];

    for link in &links {
        crate::activate::atomic_symlink(&l.build_dir("ay", 77), link).unwrap();
    }
    let report = run(&l, None);
    assert!(
        report.reclaimed.is_empty(),
        "a dangling authority reclaims nothing"
    );
    assert_eq!(
        report.diverged,
        vec![Divergence {
            program: "ay".to_string(),
            reason: Diverged::NoLiveWitness { shims_say: 19 },
        }]
    );

    let outside = l.prefix.join("elsewhere").join("ay").join("19");
    std::fs::create_dir_all(&outside).unwrap();
    for link in &links {
        crate::activate::atomic_symlink(&outside, link).unwrap();
    }
    assert!(
        run(&l, None).reclaimed.is_empty(),
        "an out-of-store authority proves nothing"
    );

    std::fs::remove_file(l.program_current("ay")).unwrap();
    assert!(
        run(&l, None).reclaimed.is_empty(),
        "no links => no witnesses"
    );

    for b in [17u64, 18, 19] {
        assert!(l.build_dir("ay", b).exists(), "{b} must survive all three");
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_program_that_was_never_activated_is_reported_diverged() {
    let l = layout("no-channel");
    seed(&l, "ay", 17, false);
    let b18 = seed(&l, "ay", 18, false);
    install_shims(&l, &b18, &["ay".to_string()], crate::activate::Aliases::Off).unwrap();

    let report = run(&l, None);
    assert!(
        report.reclaimed.is_empty(),
        "no witness => nothing reclaimed"
    );
    assert!(l.build_dir("ay", 17).exists());
    assert_eq!(
        report.diverged,
        vec![Divergence {
            program: "ay".to_string(),
            reason: Diverged::NoLiveWitness { shims_say: 18 },
        }]
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn two_programs_on_one_channel_are_both_reclaimed() {
    let l = layout("two-progs");
    for b in [16u64, 17, 18] {
        seed(&l, "ay", b, false);
    }
    seed(&l, "ay", 19, true);
    seed(&l, "ny", 6, false);
    seed(&l, "ny", 7, true);

    let report = run(&l, None);
    assert_eq!(
        report.reclaimed,
        vec![("ay".to_string(), vec![16u64, 17])],
        "ay must still be witnessed after ny was activated"
    );
    assert!(
        report.diverged.is_empty(),
        "neither program is contested: {:?}",
        report.diverged
    );
    for keep in [("ay", 18u64), ("ay", 19), ("ny", 6), ("ny", 7)] {
        assert!(
            l.build_dir(keep.0, keep.1).exists(),
            "{}@{} survives",
            keep.0,
            keep.1
        );
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
}

fn unlink_current(link: &Path) {
    let _ = std::fs::remove_file(link);
    let _ = std::fs::remove_dir(link);
    assert!(
        std::fs::symlink_metadata(link).is_err(),
        "{} must be gone for the pre-migration fixture",
        link.display()
    );
}

#[test]
fn a_retired_channel_link_is_swept_and_is_never_a_witness() {
    let l = layout("legacy-channel");
    seed(&l, "ay", 17, false);
    seed(&l, "ay", 18, false);
    let b19 = seed(&l, "ay", 19, true);
    unlink_current(&l.program_current("ay"));
    let legacy = l.prefix.join("channels").join("stable");
    std::fs::create_dir_all(&legacy).unwrap();
    crate::activate::atomic_symlink(&b19, &legacy.join("current")).unwrap();

    let report = run(&l, None);
    assert!(
        report.reclaimed.is_empty(),
        "the retired link is not an authority: {:?}",
        report.reclaimed
    );
    assert_eq!(
        report.diverged,
        vec![Divergence {
            program: "ay".to_string(),
            reason: Diverged::NoLiveWitness { shims_say: 19 },
        }]
    );
    assert!(
        std::fs::symlink_metadata(l.prefix.join("channels")).is_err(),
        "the retired directory is swept"
    );
    for b in [17u64, 18, 19] {
        assert!(
            l.build_dir("ay", b).is_dir(),
            "{b} is untouched by the sweep"
        );
    }
    activate_build(&l, &b19).unwrap();
    assert_eq!(
        run(&l, None).reclaimed,
        vec![("ay".to_string(), vec![17u64])]
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn the_channel_sweep_leaves_what_it_did_not_write() {
    let l = layout("legacy-channel-foreign");
    let b19 = seed(&l, "ay", 19, true);
    let beta = l.prefix.join("channels").join("beta");
    std::fs::create_dir_all(&beta).unwrap();
    crate::activate::atomic_symlink(&b19, &beta.join("current")).unwrap();
    std::fs::write(beta.join("notes.txt"), b"mine\n").unwrap();
    sweep_retired_channel_links(&l);
    assert!(std::fs::symlink_metadata(beta.join("current")).is_err());
    assert!(beta.join("notes.txt").is_file(), "a foreign file survives");
    assert!(b19.is_dir());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The shim view's containment test is anchored to the prefix, so a dev-link into
/// `~/src/store/ay/18/bin/aydev` claims nothing about ay@18 and does not contest its witness.
#[cfg(unix)]
#[test]
fn a_shim_into_a_lookalike_store_outside_the_prefix_claims_nothing() {
    let l = layout("foreign-store-claim");
    seed(&l, "ay", 17, false);
    seed(&l, "ay", 18, false);
    seed(&l, "ay", 19, true);

    let devco = l
        .prefix
        .parent()
        .unwrap()
        .join(format!("atpkg-gc-devco-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&devco);
    let checkout = devco.join("store/ay/18/bin");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(checkout.join("aydev"), b"#!/bin/true\n").unwrap();
    let aydev = tool("aydev");
    std::os::unix::fs::symlink(checkout.join("aydev"), l.shim(&aydev)).unwrap();

    let report = run(&l, None);
    assert_eq!(
        report.reclaimed,
        vec![("ay".to_string(), vec![17u64])],
        "a target outside <prefix>/store is not a claim about ay"
    );
    assert!(report.diverged.is_empty(), "{:?}", report.diverged);
    assert!(l.build_dir("ay", 18).exists() && l.build_dir("ay", 19).exists());
    let _ = std::fs::remove_dir_all(&devco);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn an_unclaimed_partial_tree_is_swept_and_never_counted_as_a_reclaim() {
    let l = layout("sweep-partial");
    for b in [16u64, 17, 18] {
        seed(&l, "ay", b, false);
    }
    seed(&l, "ay", 19, true);
    let partial = seed_partial(&l, "ay", 20);

    let report = run(&l, None);
    assert!(!partial.exists(), "the interrupted tree must be reclaimed");
    assert_eq!(report.swept_partial, vec![("ay".to_string(), vec![20u64])]);
    assert_eq!(
        report.reclaimed,
        vec![("ay".to_string(), vec![16u64, 17])],
        "an ordinary reclaim must not absorb the sweep"
    );
    assert!(report.diverged.is_empty(), "{:?}", report.diverged);
    for keep in [18u64, 19] {
        assert!(l.build_dir("ay", keep).exists(), "{keep} survives");
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn an_interrupted_fresh_install_is_swept_although_nothing_is_live() {
    let l = layout("sweep-fresh");
    let partial = seed_partial(&l, "ny", 7);

    let report = run(&l, None);
    assert!(
        !partial.exists(),
        "a never-activated program leaks disk too"
    );
    assert_eq!(report.swept_partial, vec![("ny".to_string(), vec![7u64])]);
    assert!(report.reclaimed.is_empty());
    assert!(
        report.diverged.is_empty(),
        "a program that never finished installing is not a disagreement"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_live_build_that_lost_its_marker_is_claimed_and_never_swept() {
    let l = layout("marker-less-live");
    seed(&l, "ay", 18, false);
    let b19 = seed(&l, "ay", 19, true);
    crate::store::clear_build_ready(&b19).unwrap();
    assert!(!crate::store::build_is_complete(&b19));

    let report = run(&l, None);
    assert!(
        report.swept_partial.is_empty(),
        "a claimed tree is not debris: {:?}",
        report.swept_partial
    );
    assert!(b19.exists(), "the live tree must survive losing its marker");
    assert!(l.build_dir("ay", 18).exists(), "and so must its rollback");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_marker_less_build_the_shims_still_run_is_never_swept() {
    let l = layout("marker-less-contested");
    let b18 = seed(&l, "ay", 18, false);
    let b19 = seed(&l, "ay", 19, false);
    activate_build(&l, &b19).unwrap();
    crate::platform::ensure_private_dir(&l.bin_dir()).unwrap();
    let ay = tool("ay");
    crate::platform::install_shim(&b18.join("bin"), &ay, &l.shim(&ay)).unwrap();
    crate::store::clear_build_ready(&b18).unwrap();

    let report = run(&l, None);
    assert!(
        report.swept_partial.is_empty(),
        "the shims' claim is a claim: {:?}",
        report.swept_partial
    );
    assert!(b18.exists(), "the tree on PATH must survive");
    assert!(b19.exists(), "and so must the one the link selects");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn stage_scratch_of_a_killed_install_is_swept_by_name_not_by_build() {
    let l = layout("sweep-scratch");
    seed(&l, "ay", 18, false);
    seed(&l, "ay", 19, true);
    let incoming = seed_scratch(&l, "ay", "20.incoming-4242");
    let superseded = seed_scratch(&l, "ay", "19.superseded-4242");

    let report = run(&l, None);
    assert!(!incoming.exists() && !superseded.exists(), "scratch swept");
    assert_eq!(
        report.swept_scratch,
        vec![(
            "ay".to_string(),
            vec![
                "19.superseded-4242".to_string(),
                "20.incoming-4242".to_string(),
            ]
        )]
    );
    assert!(report.swept_partial.is_empty(), "scratch is not a build");
    assert!(
        l.build_dir("ay", 19).exists(),
        "the live build survives the sweep of its own scratch"
    );
    assert!(l.build_dir("ay", 18).exists());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn gc_scratch_removal_observes_its_own_first_exec_mark() {
    let l = layout("scratch-first-exec");
    let current = seed(&l, "ay", 19, true);
    let current_hold = crate::store::hold_first_exec(&current.join("bin/ay")).unwrap();
    let scratch = seed_scratch(&l, "ay", "20.incoming-4242");
    let scratch_hold = crate::store::hold_first_exec(&scratch.join("bin/payload")).unwrap();
    let held = run_with(&l, &|_| None, &|| Some(Vec::new()));
    assert!(
        scratch.join("bin/payload").is_file(),
        "the actual scratch tree is assessed"
    );
    assert!(held.swept_scratch.is_empty());
    scratch_hold.ran();
    let unheld = run_with(&l, &|_| None, &|| Some(Vec::new()));
    assert!(!scratch.exists());
    assert_eq!(
        unheld.swept_scratch,
        vec![("ay".to_string(), vec!["20.incoming-4242".to_string()])]
    );
    assert!(
        crate::store::held_unproven(&current),
        "a different held build stays held"
    );
    current_hold.ran();
}

#[test]
fn only_the_producers_own_scratch_shape_is_sweepable() {
    for ours in [
        "18.incoming-1",
        "18.superseded-4242",
        "0.incoming-999999",
        "18446744073709551615.incoming-7",
    ] {
        assert!(is_stage_scratch(ours), "{ours} is ours");
    }
    for theirs in [
        "18",
        "18.ready",
        "notes.incoming-drafts",
        "18.incoming-",
        "18.incoming-4242x",
        "ay.incoming-4242",
        ".incoming-4242",
        "18.pending-4242",
        "18incoming-4242",
        "18.incoming-42.42",
        "+18.incoming-1",
        "018.incoming-1",
        "+0.incoming-1",
        "0018.superseded-4242",
    ] {
        assert!(!is_stage_scratch(theirs), "{theirs} is NOT ours to delete");
    }
}

#[test]
fn a_lookalike_directory_survives_a_gc_pass() {
    let l = layout("sweep-lookalike");
    seed(&l, "ay", 18, false);
    seed(&l, "ay", 19, true);
    let theirs = seed_scratch(&l, "ay", "notes.incoming-drafts");
    let plus_scratch = seed_scratch(&l, "ay", "+20.incoming-4242");
    let plus_build = seed_scratch(&l, "ay", "+21");
    let ours = seed_scratch(&l, "ay", "20.incoming-4242");

    let report = run(&l, None);
    assert!(!ours.exists(), "our own scratch is still swept");
    assert!(theirs.exists(), "a user directory is not ours to delete");
    assert!(
        plus_scratch.exists(),
        "`+20.incoming-4242` is not a name this manager ever wrote"
    );
    assert!(
        plus_build.exists(),
        "`+21` is not a build directory this manager ever wrote"
    );
    assert_eq!(
        report.swept_scratch,
        vec![("ay".to_string(), vec!["20.incoming-4242".to_string()])]
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_marker_bearing_lookalike_cannot_displace_the_rollback_target() {
    let l = layout("lookalike-rollback-slot");
    let real_rollback = seed(&l, "ay", 17, false);
    seed(&l, "ay", 25, true);
    let lookalike = l.prefix.join("store").join("ay").join("+19");
    std::fs::create_dir_all(lookalike.join("bin")).unwrap();
    std::fs::write(
        lookalike.join("bin").join(tool("ay").exe_file()),
        b"#!/bin/true\n",
    )
    .unwrap();
    crate::store::mark_build_ready(&lookalike).unwrap();
    assert!(
        crate::store::build_is_complete(&lookalike),
        "the fixture must look every bit as complete as a real build, or it proves nothing"
    );

    let report = run(&l, None);

    assert!(
        real_rollback.exists(),
        "the real rollback target was reclaimed to make room for a build number that \
         never existed: {:?}",
        report.reclaimed
    );
    assert!(
        report.reclaimed.is_empty(),
        "nothing here is reclaimable: {:?}",
        report.reclaimed
    );
    assert!(
        lookalike.exists(),
        "and a directory this manager never wrote is not its to delete"
    );
    assert!(l.build_dir("ay", 25).exists(), "the live build survives");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

fn seed_killed_mid_swap(l: &Layout, program: &str, build: u64) -> (PathBuf, PathBuf) {
    let dir = l.build_dir(program, build);
    let parked = dir.with_file_name(format!("{build}.superseded-4242"));
    crate::store::clear_build_ready(&dir).unwrap();
    std::fs::rename(&dir, &parked).unwrap();
    assert!(
        !dir.exists() && parked.is_dir(),
        "the fixture is only interesting in the two-rename window"
    );
    (dir, parked)
}

#[test]
fn a_build_the_other_slice_installed_is_not_an_interrupted_install() {
    let l = layout("other-slice");
    seed(&l, "ay", 18, false);
    seed(&l, "ay", 19, true);
    let marker = l.prefix.join("store").join("ay").join("18.ready");
    std::fs::write(&marker, b"ok\nplatform=sparc64-solaris\n").unwrap();
    assert!(
        !crate::store::build_is_complete(&l.build_dir("ay", 18)),
        "PRECONDITION: this slice reads it as not-complete and re-stages it"
    );

    let report = run(&l, None);

    assert!(
        l.build_dir("ay", 18).join("bin").exists(),
        "a finished install of the other slice is not an interrupted one"
    );
    assert!(
        report.swept_partial.is_empty(),
        "and it must not be reported as reclaimed debris: {:?}",
        report.swept_partial
    );

    let partial = seed_partial(&l, "ay", 17);
    let report = run(&l, None);
    assert!(
        !partial.exists(),
        "a real interrupted install is still reclaimed"
    );
    assert_eq!(report.swept_partial, vec![("ay".to_string(), vec![17u64])]);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_large_other_slice_ready_marker_is_never_partial_debris() {
    let l = layout("large-other-slice");
    let other = seed(&l, "ay", 18, false);
    seed(&l, "ay", 19, true);
    let marker = l.prefix.join("store/ay/18.ready");
    std::fs::write(&marker, b"ok\nplatform=sparc64-solaris\n").unwrap();
    assert!(!crate::store::build_is_complete(&other));
    let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
    assert!(
        other.is_dir() && report.swept_partial.is_empty(),
        "small other-slice control"
    );
    let partial = seed_partial(&l, "ay", 17);
    let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
    assert!(!partial.exists());
    assert_eq!(report.swept_partial, [("ay".to_string(), vec![17])]);

    for n in 0..330 {
        let name = format!("tool-{n:03}-{}", "x".repeat(191));
        std::fs::write(other.join("bin").join(name), b"inert inventory entry").unwrap();
    }
    crate::store::mark_build_ready(&other).unwrap();
    assert!(
        crate::store::build_is_complete(&other),
        "actual producer completes this slice"
    );
    let produced = std::fs::read_to_string(&marker).unwrap();
    assert!(produced.len() > 64 * 1024, "large aggregate inventory");
    let platform = produced
        .lines()
        .find(|line| line.starts_with("platform="))
        .unwrap();
    let foreign = produced.replacen(platform, "platform=sparc64-solaris", 1);
    std::fs::write(&marker, &foreign).unwrap();
    assert!(
        !crate::store::build_is_complete(&other),
        "other slice is not installed for this slice"
    );
    let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
    assert!(
        other.is_dir(),
        "a complete other-slice build is not interrupted debris"
    );
    assert!(
        report.swept_partial.is_empty(),
        "{:?}",
        report.swept_partial
    );
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), foreign);
}

#[test]
fn unreadable_ready_evidence_never_authorizes_partial_deletion() {
    for case in ["utf8", "overlong", "directory"] {
        let l = layout("refused-ready");
        let kept = seed(&l, "ay", 18, false);
        seed(&l, "ay", 19, true);
        let marker = kept.with_file_name("18.ready");
        std::fs::remove_file(&marker).unwrap();
        let bytes = match case {
            "utf8" => vec![0xff],
            "overlong" => vec![b'x'; 4097],
            _ => Vec::new(),
        };
        if case == "directory" {
            std::fs::create_dir(&marker).unwrap();
        } else {
            std::fs::write(&marker, &bytes).unwrap();
        }
        let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
        assert!(kept.is_dir() && report.swept_partial.is_empty(), "{case}");
        if case == "directory" {
            assert!(marker.is_dir());
        } else {
            assert_eq!(std::fs::read(&marker).unwrap(), bytes, "{case}");
        }
    }
}

#[test]
fn a_tree_parked_by_a_killed_swap_is_moved_back_not_swept() {
    let l = layout("swap-window-recover");
    seed(&l, "ay", 18, false);
    seed(&l, "ay", 19, true);
    let (build19, parked) = seed_killed_mid_swap(&l, "ay", 19);

    let report = run(&l, None);

    assert!(
        build19.is_dir(),
        "the only copy of the live tree was swept as scratch"
    );
    assert!(
        build19.join("bin").join(tool("ay").exe_file()).exists(),
        "and it is THAT tree, not an empty shell"
    );
    assert!(!parked.exists(), "the scratch name is released");
    assert!(
        report.swept_scratch.is_empty(),
        "a recovered tree must not be reported as reclaimed disk: {:?}",
        report.swept_scratch
    );
    assert!(
        report.swept_partial.is_empty(),
        "and the claim guard must keep the recovered tree: {:?}",
        report.swept_partial
    );
    assert!(
        !crate::store::build_is_complete(&build19),
        "recovery must never claim a completeness it cannot prove"
    );
    assert!(l.build_dir("ay", 18).exists(), "and 18 is untouched");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_recovered_tree_nothing_claims_is_still_swept_as_a_partial() {
    let l = layout("swap-window-orphan");
    seed(&l, "ay", 19, true);
    seed(&l, "ny", 7, false);
    let (build7, parked) = seed_killed_mid_swap(&l, "ny", 7);

    let report = run(&l, None);

    assert!(!parked.exists(), "the scratch name is released either way");
    assert!(!build7.exists(), "an unclaimed orphan is still reclaimed");
    assert_eq!(
        report.swept_partial,
        vec![("ny".to_string(), vec![7u64])],
        "and it is reported as a partial, not as scratch"
    );
    assert!(report.swept_scratch.is_empty());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn an_ambiguous_pair_with_the_build_absent_is_reported_and_left_standing() {
    let l = layout("swap-window-ambiguous");
    seed(&l, "ay", 19, true);
    let (build19, first) = seed_killed_mid_swap(&l, "ay", 19);
    let second = seed_scratch(&l, "ay", "19.superseded-9999");
    let incoming = seed_scratch(&l, "ay", "19.incoming-4242");

    let report = run(&l, None);

    assert!(
        !build19.exists(),
        "an ambiguous window must not be guessed at"
    );
    assert!(
        first.is_dir() && second.is_dir(),
        "the only copies of build 19 went to the sweep the refusal called off"
    );
    assert!(
        first.join("bin").join(tool("ay").exe_file()).exists(),
        "and the parked tree is intact, not an emptied shell"
    );
    assert!(!incoming.exists(), "a half-extract is still swept");
    assert_eq!(
        report.swept_scratch,
        vec![("ay".to_string(), vec!["19.incoming-4242".to_string()])],
        "a tree still standing must not be reported as reclaimed disk"
    );
    assert!(
        report.diverged.iter().any(|d| d.program == "ay"),
        "and the state IS reported, not silently kept: {:?}",
        report.diverged
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn discard_superseded_refuses_the_live_build() {
    let l = layout("refuse-live");
    seed(&l, "ay", 18, false);
    seed(&l, "ay", 19, true);
    let set = live_builds(&l);
    let witness = set.get("ay").expect("link + shims agree on 19").clone();
    assert_eq!((witness.program(), witness.build()), ("ay", 19));

    assert_eq!(
        discard_superseded(&l, &witness, 19),
        Err(Retained::IsLive {
            program: "ay".to_string(),
            build: 19,
        })
    );
    assert!(l.build_dir("ay", 19).exists(), "the live tree is untouched");
    assert_eq!(discard_superseded(&l, &witness, 18), Ok(()));
    assert!(!l.build_dir("ay", 18).exists());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn the_scoped_sweep_touches_only_its_programs() {
    let l = layout("scoped");
    for b in [16, 17] {
        seed(&l, "ay", b, false);
    }
    seed(&l, "ay", 18, true);
    for b in [4, 5] {
        seed(&l, "claude", b, false);
    }
    seed(&l, "claude", 6, true);
    let other_partial = seed_partial(&l, "ay", 19);
    let ay_staging = l.staging_dir("ay");
    let claude_staging = l.staging_dir("claude");
    std::fs::create_dir_all(&ay_staging).unwrap();
    std::fs::create_dir_all(&claude_staging).unwrap();
    let resume = ay_staging.join("ay-20.tar.zst.part");
    let carried = ay_staging.join("ay-20.tar.zst");
    let stale = claude_staging.join("claude-5.part");
    let spared = claude_staging.join("claude-7.part");
    for p in [&resume, &carried, &stale, &spared] {
        std::fs::write(p, b"x").unwrap();
    }
    let scope = BTreeSet::from([String::from("claude")]);
    let report = run_for_programs(&l, &scope, &|program| {
        (program == "claude").then(|| String::from("claude-7"))
    });
    assert_eq!(report.reclaimed, vec![(String::from("claude"), vec![4])]);
    assert!(
        resume.exists() && carried.exists(),
        "ay's staging is not ours"
    );
    assert!(l.build_dir("ay", 16).exists(), "ay's builds are not ours");
    assert!(other_partial.exists(), "nor its debris");
    assert!(!stale.exists(), "claude's superseded partial is swept");
    assert!(spared.exists(), "claude's resolved partial is spared");
    assert_eq!(
        report.swept_staging,
        vec![(String::from("claude"), vec![String::from("claude-5.part")])]
    );
    let report = run(&l, None);
    assert!(!resume.exists() && !l.build_dir("ay", 16).exists());
    assert!(!other_partial.exists());
    assert!(report.reclaimed.contains(&(String::from("ay"), vec![16])));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn the_pass_closing_sweep_spares_the_live_pinned_archive_and_partial_only() {
    let l = layout("staging-keep");
    let staging = l.staging_dir("ay");
    std::fs::create_dir_all(&staging).unwrap();
    let pinned = staging.join("ay-18.tar.zst.part");
    let carried = staging.join("ay-18.tar.zst");
    let superseded = staging.join("ay-17.tar.zst.part");
    let debris = staging.join("ay-17.tar.zst");
    let other = l.staging_dir("ny");
    std::fs::create_dir_all(&other).unwrap();
    let unpinned = other.join("ny-7.tar.zst.part");
    for p in [&pinned, &carried, &superseded, &debris, &unpinned] {
        std::fs::write(p, b"x").unwrap();
    }

    let report = run_keeping_pinned_partials(&l, None, &|program| {
        (program == "ay").then(|| "ay-18.tar.zst".to_string())
    });

    assert!(
        pinned.exists(),
        "the pinned partial is the next pass's resume state"
    );
    assert!(
        carried.exists(),
        "the pinned COMPLETE archive is an aborted tuple's carry — the next pass \
         re-stages that member from it rather than re-downloading a multi-GB asset"
    );
    assert!(!superseded.exists(), "a superseded partial is still swept");
    assert!(
        !debris.exists(),
        "a SUPERSEDED complete asset is still swept — only the pinned one is spared"
    );
    assert!(
        !unpinned.exists(),
        "a program the caller pins nothing for is fully swept"
    );
    assert_eq!(
        report.swept_staging,
        vec![
            (
                "ay".to_string(),
                vec![
                    "ay-17.tar.zst".to_string(),
                    "ay-17.tar.zst.part".to_string()
                ]
            ),
            ("ny".to_string(), vec!["ny-7.tar.zst.part".to_string()]),
        ],
        "the report lists what went, never what was spared"
    );

    let _ = run(&l, None);
    assert!(
        !pinned.exists(),
        "a plain `gc` still reclaims the spared partial"
    );
    assert!(
        !carried.exists(),
        "…and the spared archive: an aborted tuple never permanently strands bytes"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn an_unread_vendor_head_keeps_the_resume_state_of_the_head_it_pursues() {
    use crate::vendor_direct::{ProgramStamp, Version, spec};
    let l = layout("pursued-head");
    let v = |text: &str| Version::parse(text).unwrap();
    let asset = |version| {
        let triple = crate::cli::current_triple();
        crate::vendor_direct::resolve::staging_asset(spec("muse").unwrap(), version, triple)
    };
    let head = v("1.4.9-R6299");
    let staging = l.staging_dir("muse");
    std::fs::create_dir_all(&staging).unwrap();
    let part = staging.join(format!("{}.part", asset(head)));
    let carried = staging.join(asset(head));
    let stale = format!("{}.part", asset(v("1.4.9-R6298")));
    for p in [&part, &carried, &staging.join(&stale)] {
        std::fs::write(p, b"x").unwrap();
    }
    ProgramStamp::record_check(&l, "muse", 1_790_000_000, Some(head), None).unwrap();
    let muse = BTreeSet::from(["muse".to_string()]);

    let report = run_keeping_pinned_partials(&l, None, &|_| None);
    assert!(part.exists(), "the pursued head's resume state survives");
    assert!(carried.exists(), "…and its carried archive");
    assert_eq!(
        report.swept_staging,
        vec![("muse".to_string(), vec![stale])]
    );
    let _ = run_for_programs(&l, &muse, &|_| None);
    assert!(
        part.exists() && carried.exists(),
        "the scoped form spares them too"
    );

    seed(&l, "muse", head.build_id(), true);
    let _ = run_keeping_pinned_partials(&l, None, &|_| None);
    assert!(!part.exists(), "an installed head pursues nothing");
    assert!(!carried.exists());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_held_flips_staged_archive_survives_every_gc_while_the_wait_stands() {
    let l = layout("held-archive");
    seed(&l, "trust", 9192, true);
    let staging = l.staging_dir("trust");
    std::fs::create_dir_all(&staging).unwrap();
    let held = staging.join("trust-9200.tar.zst");
    let stray = staging.join("trust-9100.tar.zst");
    for p in [&held, &stray] {
        std::fs::write(p, b"x").unwrap();
    }
    assert!(crate::quiet::note_deferred(
        &l,
        "rustc",
        1_790_000_000,
        1_790_000_000,
        &crate::quiet::Busy::Quiet,
        &crate::quiet::Moves {
            from: BTreeMap::from([("trust".to_string(), 9192u64)]),
            to: BTreeMap::from([("trust".to_string(), 9200u64)]),
            assets: BTreeMap::from([("trust".to_string(), "trust-9200.tar.zst".to_string())]),
        },
    ));
    let report = run_keeping_pinned_partials(&l, None, &|program| {
        (program == "ay").then(|| "ay-18.tar.zst".to_string())
    });
    assert!(held.exists(), "an install's sweep keeps the held archive");
    assert!(!stray.exists(), "…and takes the rest");
    assert_eq!(
        report.held_staging,
        vec![("trust".to_string(), "trust-9200.tar.zst".to_string())],
        "and says what it kept"
    );
    let report = run(&l, None);
    assert!(held.exists(), "a plain gc keeps the held archive");
    assert_eq!(report.held_staging.len(), 1);
    crate::ops::uninstall(&l, "trust").unwrap();
    assert!(!l.build_dir("trust", 9192).exists());
    let report = run(&l, None);
    assert!(!held.exists(), "a stale record keeps nothing");
    assert!(report.held_staging.is_empty());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

fn codex_with_a_reclaimable_build(label: &str) -> crate::testdir::ScratchLayout {
    let l = layout(label);
    seed(&l, "codex", 18, false);
    seed(&l, "codex", 19, false);
    seed(&l, "codex", 20, true);
    l
}

#[test]
fn an_agent_build_a_live_process_runs_from_is_not_reclaimed() {
    let l = codex_with_a_reclaimable_build("agent-live");
    let exe = l.build_dir("codex", 18).join("bin/codex");
    let running = || Some(vec![PathBuf::from("/usr/bin/login"), exe.clone()]);
    let report = run_with(&l, &|_| None, &running);
    assert!(report.reclaimed.is_empty(), "{:?}", report.reclaimed);
    assert!(crate::store::build_is_complete(&l.build_dir("codex", 18)));

    let report = run_with(&l, &|_| None, &|| {
        Some(vec![PathBuf::from("/usr/bin/login")])
    });
    assert_eq!(report.reclaimed, vec![("codex".to_string(), vec![18u64])]);
    assert!(!l.build_dir("codex", 18).exists());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn an_unreadable_process_table_keeps_every_agent_and_toolchain_build() {
    let l = codex_with_a_reclaimable_build("agent-unknown");
    for b in [16u64, 17] {
        seed(&l, "ay", b, false);
    }
    seed(&l, "ay", 18, true);
    for b in [8580u64, 8590] {
        seed(&l, "trust", b, false);
    }
    seed(&l, "trust", 9192, true);
    let report = run_with(&l, &|_| None, &|| None);
    assert_eq!(report.reclaimed, vec![("ay".to_string(), vec![16u64])]);
    assert!(l.build_dir("codex", 18).exists());
    assert!(
        l.build_dir("trust", 8580).exists(),
        "the toolchain is kept too"
    );
    let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
    assert!(
        report
            .reclaimed
            .contains(&("trust".to_string(), vec![8580u64])),
        "{:?}",
        report.reclaimed
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A build that ships a Rust sysroot, as a real directory at `store/<program>/<build>`.
#[cfg(target_os = "macos")]
fn seed_toolchain(l: &Layout, program: &str, build: u64, shim: bool) -> PathBuf {
    let lib = l
        .build_dir(program, build)
        .join("lib/rustlib/aarch64-apple-darwin/lib");
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(lib.join("libstd-1.rlib"), b"an rlib").unwrap();
    seed(l, program, build, shim)
}

#[cfg(target_os = "macos")]
fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

#[cfg(target_os = "macos")]
fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

#[cfg(target_os = "macos")]
fn exposed(l: &Layout) -> Vec<(String, u64, bool)> {
    crate::store::spotlight_exposed_builds(&l.prefix)
        .into_iter()
        .map(|b| (b.program, b.build, b.active))
        .collect()
}

#[cfg(target_os = "macos")]
#[test]
fn gc_lays_an_unused_toolchain_build_aside_and_never_the_active_one() {
    let l = layout("aside-move");
    let old = seed_toolchain(&l, "trust", 8590, false);
    let live = seed_toolchain(&l, "trust", 9192, true);
    let plain = seed(&l, "ay", 17, false);
    seed(&l, "ay", 18, true);
    assert_eq!(
        exposed(&l),
        [
            ("trust".to_owned(), 8590, false),
            ("trust".to_owned(), 9192, true)
        ]
    );

    let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
    assert_eq!(
        report.laid_aside,
        vec![("trust".to_string(), vec![8590u64])]
    );
    assert!(report.reclaimed.is_empty(), "{:?}", report.reclaimed);
    assert!(is_link(&old) && is_real_dir(&old.with_file_name("8590.noindex")));
    assert!(crate::store::build_is_complete(&old));
    assert!(is_real_dir(&live), "the build in use stays where it is");
    assert!(
        is_real_dir(&plain),
        "a build with no sysroot is not Spotlight's problem"
    );
    assert_eq!(exposed(&l), [("trust".to_owned(), 9192, true)]);
    assert!(
        run_with(&l, &|_| None, &|| Some(Vec::new()))
            .laid_aside
            .is_empty()
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[cfg(target_os = "macos")]
#[test]
fn a_toolchain_build_in_use_leased_or_unaccounted_for_stays_where_it_is() {
    #[cfg(unix)]
    if !crate::testdir::isolated_lifecycle(
        "gc::tests::a_toolchain_build_in_use_leased_or_unaccounted_for_stays_where_it_is",
    ) {
        return;
    }
    let l = layout("aside-keep");
    let old = seed_toolchain(&l, "trust", 8590, false);
    seed_toolchain(&l, "trust", 9192, true);
    let mc = seed_toolchain(&l, "trust-mc", 6, false);
    seed_toolchain(&l, "trust-mc", 7, true);

    let exe = old.join("bin/trust");
    let report = run_with(&l, &|_| None, &|| Some(vec![exe.clone()]));
    assert_eq!(
        report.laid_aside,
        vec![("trust-mc".to_string(), vec![6u64])]
    );
    assert!(is_real_dir(&old), "a live process runs from it");
    assert!(is_link(&mc));

    let report = run_with(&l, &|_| None, &|| None);
    assert!(report.laid_aside.is_empty(), "no process table, no move");
    assert!(is_real_dir(&old));

    let subject = crate::lease::Subject::build("trust", 8590).unwrap();
    let lease = crate::lease::take(
        &l.prefix,
        &subject,
        "a run in /w",
        crate::lease::DEFAULT_WAIT,
    )
    .unwrap();
    let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
    assert!(
        report.laid_aside.is_empty() && is_real_dir(&old),
        "a run holds it"
    );
    assert!(report.leased.is_empty(), "a move reclaims nothing to keep");
    lease.release();

    let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
    assert_eq!(
        report.laid_aside,
        vec![("trust".to_string(), vec![8590u64])]
    );
    assert!(is_link(&old));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The build `current` names stays put though nothing else does (a relative link, which
/// gc's claims do not read), and so does a build a shim still runs after `current` moved on.
#[cfg(target_os = "macos")]
#[test]
fn a_build_current_or_a_shim_names_stays_where_it_is() {
    let l = layout("aside-current");
    let old = seed_toolchain(&l, "trust", 8590, false);
    let live = seed_toolchain(&l, "trust", 9192, false);
    std::os::unix::fs::symlink("9192", l.program_current("trust")).unwrap();
    let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
    assert_eq!(
        report.laid_aside,
        vec![("trust".to_string(), vec![8590u64])]
    );
    assert!(is_link(&old) && is_real_dir(&live), "{report:?}");
    let _ = std::fs::remove_dir_all(&l.prefix);

    let l = layout("aside-shim");
    let pinned = seed_toolchain(&l, "trust", 8590, false);
    seed_toolchain(&l, "trust", 9192, true);
    std::os::unix::fs::symlink(pinned.join("bin/trust"), l.bin_dir().join("trust-8590")).unwrap();
    let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
    assert!(
        report.laid_aside.is_empty() && is_real_dir(&pinned),
        "{report:?}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A shim written through a build's folder (`<build>.noindex`, the path a link resolves
/// to) claims the build, so gc never reclaims what it runs.
#[cfg(target_os = "macos")]
#[test]
fn a_shim_through_a_builds_folder_claims_the_build() {
    let l = layout("aside-claim");
    let oldest = seed_toolchain(&l, "trust", 8580, false);
    seed_toolchain(&l, "trust", 8590, false);
    seed_toolchain(&l, "trust", 9192, true);
    crate::store::lay_aside(&oldest).unwrap();
    let folder = oldest.with_file_name("8580.noindex");
    std::os::unix::fs::symlink(folder.join("bin/trust"), l.bin_dir().join("trust-8580")).unwrap();
    let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
    assert!(
        report.reclaimed.is_empty() && is_link(&oldest) && is_real_dir(&folder),
        "{report:?}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[cfg(target_os = "macos")]
#[test]
fn every_resolver_reads_a_build_through_its_link() {
    let l = layout("aside-resolve");
    let oldest = seed_toolchain(&l, "trust", 8580, false);
    let old = seed_toolchain(&l, "trust", 8590, false);
    seed_toolchain(&l, "trust", 9192, true);
    for dir in [&oldest, &old] {
        crate::store::lay_aside(dir).unwrap();
    }
    let aside = old.with_file_name("8590.noindex");

    assert!(crate::ops::list_installed(&l).contains(&("trust".to_string(), 8590)));
    let real = std::fs::canonicalize(old.join("bin")).unwrap();
    assert_eq!(real, std::fs::canonicalize(aside.join("bin")).unwrap());
    assert_eq!(
        crate::lease::Subject::of_dir(&l.prefix, &real),
        crate::lease::Subject::build("trust", 8590),
        "the sysroot a compiler reports names the build"
    );
    let exe = std::fs::canonicalize(oldest.join("bin/trust")).unwrap();
    assert!(exe.starts_with(std::fs::canonicalize(oldest.with_file_name("8580.noindex")).unwrap()));
    let report = run_with(&l, &|_| None, &|| Some(vec![exe.clone()]));
    assert!(
        report.reclaimed.is_empty() && is_link(&oldest),
        "a process runs from its folder, as the kernel names it: {report:?}"
    );

    install_shims(
        &l,
        &old,
        &["trust".to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    activate_build(&l, &old).unwrap();
    assert_eq!(crate::ops::active_builds(&l).get("trust"), Some(&8590));
    assert_eq!(
        crate::ops::which(&l, "trust"),
        Some(old.join("bin/trust")),
        "the shim names the build, not its folder"
    );
    assert!(old.join("bin/trust").is_file());
    assert_eq!(
        std::fs::read_link(l.program_current("trust")).unwrap(),
        old,
        "current names the build"
    );
    assert_eq!(
        exposed(&l),
        [("trust".to_owned(), 9192, false)],
        "rolled back onto the moved build, the other is now the one to move"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[cfg(target_os = "macos")]
#[test]
fn gc_reclaims_a_build_laid_aside_link_and_folder_both() {
    let l = layout("aside-reclaim");
    let oldest = seed_toolchain(&l, "trust", 8580, false);
    seed_toolchain(&l, "trust", 8590, false);
    seed_toolchain(&l, "trust", 9192, true);
    crate::store::lay_aside(&oldest).unwrap();
    let aside = oldest.with_file_name("8580.noindex");
    assert!(is_link(&oldest) && is_real_dir(&aside));

    let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
    assert_eq!(report.reclaimed, vec![("trust".to_string(), vec![8580u64])]);
    assert!(
        std::fs::symlink_metadata(&oldest).is_err(),
        "the link is gone"
    );
    assert!(std::fs::symlink_metadata(&aside).is_err(), "and its folder");
    assert!(!oldest.with_file_name("8580.ready").exists());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// What a crash leaves of a build laid aside: an aside with no link gets it back, a link to
/// nothing is swept, and a half-staged tree behind a link goes whole.
#[cfg(target_os = "macos")]
#[test]
fn gc_puts_a_crashed_aside_in_order() {
    let l = layout("aside-crash");
    let relinked = seed_toolchain(&l, "trust", 8590, false);
    let dangling = seed_toolchain(&l, "trust", 8595, false);
    let partial = seed_toolchain(&l, "trust", 8597, false);
    seed_toolchain(&l, "trust", 9192, true);
    for dir in [&relinked, &dangling, &partial] {
        crate::store::lay_aside(dir).unwrap();
    }
    std::fs::remove_file(&relinked).unwrap();
    std::fs::remove_dir_all(dangling.with_file_name("8595.noindex")).unwrap();
    crate::store::clear_build_ready(&partial).unwrap();

    let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
    assert!(is_link(&relinked) && crate::store::build_is_complete(&relinked));
    assert!(
        std::fs::symlink_metadata(&dangling).is_err(),
        "a link to nothing goes"
    );
    assert!(std::fs::symlink_metadata(&partial).is_err());
    assert!(std::fs::symlink_metadata(partial.with_file_name("8597.noindex")).is_err());
    assert_eq!(
        report.swept_partial,
        vec![("trust".to_string(), vec![8595u64, 8597])]
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_leased_build_is_kept_until_its_run_ends() {
    #[cfg(unix)]
    if !crate::testdir::isolated_lifecycle("gc::tests::a_leased_build_is_kept_until_its_run_ends") {
        return;
    }
    let l = layout("leased");
    for b in [8580u64, 8590, 8595] {
        seed(&l, "trust", b, false);
    }
    seed(&l, "trust", 9192, true);
    let subject = crate::lease::Subject::build("trust", 8590).unwrap();
    let lease = crate::lease::take(
        &l.prefix,
        &subject,
        "aterm-release (pid 7) — a release cut in /w",
        crate::lease::DEFAULT_WAIT,
    )
    .unwrap();
    let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
    assert_eq!(report.reclaimed, vec![("trust".to_string(), vec![8580u64])]);
    assert!(crate::store::build_is_complete(&l.build_dir("trust", 8590)));
    assert_eq!(
        report.leased,
        vec![(
            "trust".to_string(),
            8590,
            crate::lease::Holders::Held(vec![
                "aterm-release (pid 7) — a release cut in /w".to_string()
            ])
        )]
    );
    lease.release();
    let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
    assert_eq!(report.reclaimed, vec![("trust".to_string(), vec![8590u64])]);
    assert!(report.leased.is_empty());
    assert!(!l.build_dir("trust", 8590).exists());
    let left: Vec<String> = std::fs::read_dir(crate::lease::dir(&l.prefix))
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("store-trust-8590"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A lease whose state cannot be read keeps its build — never read as free.
#[cfg(unix)]
#[test]
fn an_unreadable_lease_directory_keeps_every_candidate() {
    use std::os::unix::fs::PermissionsExt as _;
    let l = layout("lease-unreadable");
    for b in [16u64, 17] {
        seed(&l, "ay", b, false);
    }
    seed(&l, "ay", 18, true);
    let leases = crate::lease::dir(&l.prefix);
    std::fs::create_dir_all(&leases).unwrap();
    std::fs::set_permissions(&leases, std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = std::fs::read_dir(&leases).is_ok();
    let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
    std::fs::set_permissions(&leases, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        !readable,
        "NOT RUN as written: this process reads a 0000 directory (root?)"
    );
    assert!(report.reclaimed.is_empty(), "{:?}", report.reclaimed);
    assert_eq!(report.leased.len(), 1, "{:?}", report.leased);
    assert!(matches!(
        report.leased[0].2,
        crate::lease::Holders::Unknown(_)
    ));
    assert!(l.build_dir("ay", 16).exists());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_live_process_keeps_any_programs_build_and_the_table_is_read_once() {
    let l = layout("any-once");
    let calls = std::cell::Cell::new(0u32);
    let inside = l.build_dir("ay", 16).join("bin/ay");
    let running = || {
        calls.set(calls.get() + 1);
        Some(vec![inside.clone()])
    };
    seed(&l, "ay", 19, true);
    let _ = run_with(&l, &|_| None, &running);
    assert_eq!(calls.get(), 0, "no candidate, no enumeration");

    for b in [16u64, 17, 18] {
        seed(&l, "ay", b, false);
    }
    seed(&l, "codex", 17, false);
    seed(&l, "codex", 18, false);
    seed(&l, "codex", 19, false);
    seed(&l, "codex", 20, true);
    let report = run_with(&l, &|_| None, &running);
    assert_eq!(
        report.reclaimed,
        vec![
            ("ay".to_string(), vec![17u64]),
            ("codex".to_string(), vec![17u64, 18])
        ],
        "16 runs `ay`: kept"
    );
    assert!(l.build_dir("ay", 16).exists());
    assert_eq!(calls.get(), 1, "one enumeration serves every candidate");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_trust_build_running_from_its_exec_root_is_not_reclaimed() {
    let l = layout("trust-root");
    for b in [8580u64, 8590, 8595] {
        seed(&l, "trust", b, false);
    }
    seed(&l, "trust", 9192, true);
    let root = crate::compat::root_dir(&l, 8590);
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let exe = root.join("bin/trustc");
    let report = run_with(&l, &|_| None, &|| Some(vec![exe.clone()]));
    assert_eq!(report.reclaimed, vec![("trust".to_string(), vec![8580u64])]);
    assert!(l.build_dir("trust", 8590).exists());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_partial_agent_tree_a_process_runs_from_is_not_swept() {
    let l = layout("agent-partial");
    let partial = seed_partial(&l, "claude", 7);
    let exe = partial.join("bin/claude");
    let report = run_with(&l, &|_| None, &|| Some(vec![exe.clone()]));
    assert!(report.swept_partial.is_empty());
    assert!(partial.exists());
    let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
    assert_eq!(
        report.swept_partial,
        vec![("claude".to_string(), vec![7u64])]
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// `runs_from` sees a process by the kernel's path: a launch through a symlink is caught,
/// and a sibling directory whose name merely starts the same is not.
#[cfg(target_os = "macos")]
#[test]
fn runs_from_sees_a_symlinked_launch_and_not_a_prefix_sibling() {
    let scratch = std::mem::ManuallyDrop::new(layout("runs-from"));
    let root = scratch.prefix.clone();
    let app = root.join("A.app");
    let macos = app.join("Contents/MacOS");
    std::fs::create_dir_all(&macos).expect("layout");
    let exe = macos.join("A");
    aterm_uds::deadline::ad_hoc_copy(Path::new("/bin/cat"), &exe);
    let link = root.join("via-link");
    std::os::unix::fs::symlink(&exe, &link).expect("symlink");
    std::fs::create_dir_all(root.join("A.app.rollback")).expect("sibling");

    assert_eq!(runs_from(&app), Some(false), "nothing runs from it yet");
    let mut child = aterm_uds::spawnfd::command(&link)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("spawn through the symlink");
    let stdin = aterm_uds::deadline::cat_echoes(&mut child);
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while runs_from(&app) != Some(true) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let seen = runs_from(&app);
    let sibling = runs_from(&root.join("A.app.rollback"));
    let alive = child.try_wait().expect("poll the cat").is_none();
    let _ = child.kill();
    let _ = child.wait();
    drop(stdin);
    drop(std::mem::ManuallyDrop::into_inner(scratch));
    assert!(
        alive,
        "the process seen is running, not a dead one awaiting its reap"
    );
    assert_eq!(seen, Some(true), "a symlinked launch runs from the bundle");
    assert_eq!(sibling, Some(false), "`A.app.rollback` is not `A.app`");
}

/// The real enumeration lists this test process, in the resolved spelling [`InUse`] compares
/// against.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
#[test]
fn the_process_table_includes_this_process() {
    let me = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
    let running = running_executables().expect("the process table reads");
    assert!(
        running.contains(&me),
        "{} not among {} processes",
        me.display(),
        running.len()
    );
}

#[test]
fn orphan_sidecars_are_swept_and_a_discard_takes_the_vendor_and_exposes_records() {
    let l = layout("orphan-sidecars");
    seed(&l, "claude", 30, true);
    let beside_live =
        |suffix| crate::store::sidecar_path(&l.build_dir("claude", 30), suffix).unwrap();
    let orphan = |b, suffix| crate::store::sidecar_path(&l.build_dir("claude", b), suffix).unwrap();
    for suffix in crate::store::STAGE_SIDECAR_SUFFIXES {
        std::fs::write(beside_live(suffix), b"kept").unwrap();
        std::fs::write(orphan(31, suffix), b"orphan").unwrap();
    }
    let not_ours = l.prefix.join("store/claude/notes.vendor");
    std::fs::write(&not_ours, b"a file the user put here").unwrap();
    let refusal = l.build_dir("claude", 32).with_file_name("32.refused");
    std::fs::write(&refusal, b"stage-refusal v1\n").unwrap();

    let _ = run_with(&l, &|_| None, &|| Some(Vec::new()));
    for suffix in crate::store::STAGE_SIDECAR_SUFFIXES {
        assert!(
            beside_live(suffix).is_file(),
            "{suffix} beside a live build stays"
        );
        assert!(
            !orphan(31, suffix).exists(),
            "{suffix} of a gone build is swept"
        );
    }
    assert!(not_ours.is_file(), "only `<build><suffix>` names are ours");
    assert!(
        refusal.is_file(),
        "a refusal memo outlives its build by design"
    );

    let dir = seed(&l, "claude", 29, false);
    let records = [
        crate::store::VENDOR_SIDECAR_SUFFIX,
        crate::store::EXPOSES_SUFFIX,
        crate::store::VERSION_SUFFIX,
    ];
    for suffix in records {
        std::fs::write(orphan(29, suffix), b"record").unwrap();
    }
    crate::store::discard_build(&dir);
    for suffix in records {
        assert!(!orphan(29, suffix).exists(), "{suffix} goes with its build");
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A NAMED BUILD'S REFUSAL MEMO OUTLIVES ITS TREE AND GOES WITH THE KEEP WINDOW (A14): gc's sweep
/// of a tree that never completed (a refused build a first-exec hold kept) keeps the memo
/// beside it, so the next install does not download that pack again, and a named memo of a
/// gone build older than a lock's keep window is forgotten. NEGATIVE CONTROL: a fresh named
/// memo of a gone build, an update-lane memo of a gone build as old, and an old named memo
/// beside a build the store holds all stay.
#[test]
fn a_named_refusal_outlives_its_partial_tree_and_goes_with_the_keep_window() {
    let l = layout("named-refusal-gc");
    seed(&l, "trust", 40, true);
    let partial = seed_partial(&l, "trust", 41);
    let memo = |b: u64| {
        l.build_dir("trust", b)
            .with_file_name(format!("{b}.refused"))
    };
    let now = crate::flow::now_unix();
    let digest = (crate::manifest::DigestAlg::Sha256, "aa");
    let named = |b: u64, at: i64| {
        crate::store::record_refusal_in(
            crate::store::MemoLane::Named,
            crate::store::RefusalKind::Held,
            &l.build_dir("trust", b),
            digest,
            "bb",
            "resolve",
            at,
        )
        .unwrap();
    };
    named(40, 0);
    named(41, now);
    named(42, 0);
    named(43, now);
    crate::store::record_stage_refusal(&l.build_dir("trust", 44), digest, "bb", "digest", 0)
        .unwrap();

    let _ = run_with(&l, &|_| None, &|| Some(Vec::new()));
    assert!(!partial.exists(), "the partial tree is swept");
    assert!(memo(41).is_file(), "its memo stays");
    assert!(!memo(42).exists(), "a named memo past the keep window goes");
    assert!(memo(43).is_file(), "a fresh one stays");
    assert!(
        memo(40).is_file(),
        "an old named memo beside its build stays"
    );
    assert!(memo(44).is_file(), "the lane's memo is the lane's to bound");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn sidecars_of_a_partial_or_a_swap_parked_build_are_kept() {
    let l = layout("orphan-sidecars-kept");
    seed(&l, "claude", 40, true);
    let partial = seed_partial(&l, "claude", 34);
    seed(&l, "claude", 33, false);
    let (build33, first) = seed_killed_mid_swap(&l, "claude", 33);
    let second = seed_scratch(&l, "claude", "33.superseded-9999");
    let sidecar =
        |b, suffix| crate::store::sidecar_path(&l.build_dir("claude", b), suffix).unwrap();
    for suffix in crate::store::STAGE_SIDECAR_SUFFIXES {
        std::fs::write(sidecar(34, suffix), b"partial").unwrap();
        std::fs::write(sidecar(33, suffix), b"parked").unwrap();
    }
    let exe = partial.join("bin/claude");

    let _ = run_with(&l, &|_| None, &|| Some(vec![exe.clone()]));
    assert!(
        partial.exists(),
        "the fixture keeps the partial through the pass"
    );
    assert!(
        !build33.exists() && first.is_dir() && second.is_dir(),
        "and the ambiguous swap stays parked"
    );
    for suffix in crate::store::STAGE_SIDECAR_SUFFIXES {
        assert!(sidecar(34, suffix).is_file(), "{suffix} beside a partial");
        assert!(sidecar(33, suffix).is_file(), "{suffix} of a parked build");
    }

    let report = run_with(&l, &|_| None, &|| Some(Vec::new()));
    assert_eq!(
        report.swept_partial,
        vec![("claude".to_string(), vec![34u64])]
    );
    let _ = run_with(&l, &|_| None, &|| Some(Vec::new()));
    for suffix in crate::store::STAGE_SIDECAR_SUFFIXES {
        assert!(
            !sidecar(34, suffix).exists(),
            "{suffix} follows its partial"
        );
        assert!(sidecar(33, suffix).is_file(), "{suffix} of a parked build");
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn an_active_alab_build_keeps_its_alab_rollback_target_and_respects_channel_floors() {
    let l = layout("alab-rollback-gc");
    let b = 2_026_101_602_u64;
    for (build, is_alab, is_live) in [(b - 2, true, false), (b - 1, false, false), (b, true, true)]
    {
        let dir = seed(&l, "codex", build, is_live);
        if is_alab {
            let entry = format!("bin/{}", tool("codex").exe_file());
            std::fs::write(
                dir.join(crate::alab::PACKAGE_JSON),
                format!(
                    r#"{{"layoutVersion":1,"version":"0.164.0-dev.{build}+gabc","entrypoint":"{entry}"}}"#
                ),
            )
            .unwrap();
            std::fs::write(dir.join(crate::alab::ALAB_BUILD_JSON), b"{}").unwrap();
        }
    }
    let report = run_in(&l, None, None, &|_| Vec::new(), &|| Some(Vec::new()));
    assert_eq!(
        report.reclaimed,
        vec![("codex".to_string(), vec![b - 1])],
        "a non-ALab build between two ALab builds is reclaimed while the ALab rollback target is kept"
    );
    assert!(l.build_dir("codex", b - 2).is_dir());

    let ch = Channel {
        name: "stable".into(),
        channel_build: 1,
        min_build: 0,
        min_build_by_program: BTreeMap::from([("codex".to_string(), b)]),
        yanked: Vec::new(),
        pin: BTreeMap::new(),
        pin_by_target: BTreeMap::new(),
    };
    let report = run_in(&l, None, Some(&ch), &|_| Vec::new(), &|| Some(Vec::new()));
    assert_eq!(
        report.reclaimed,
        vec![("codex".to_string(), vec![b - 2])],
        "an ALab build below the channel's min_build_by_program floor is reclaimed"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}
