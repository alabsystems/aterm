// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The vendor lane meets an ALab build ([`crate::alab`]): kept before any head is read,
//! asked or not; a yank or the signed floor rolls back onto an older ALab build nothing
//! revokes, else disables it, never onto the vendor's; only a person's recorded choice lets
//! the vendor's head in. Each with the control that shows
//! the lane deciding as before once the marker is gone.

use super::world::{self, Fake, NOW, native_exe};
use super::*;

/// An ALab build of codex, laid in the store at `build`, complete; activated when `active`.
/// Its entrypoint is the executable this host's shim runs ([`world::bin`]).
fn alab_codex(l: &Layout, build: u64, active: bool) {
    let dir = l.build_dir("codex", build);
    let entry = world::bin("codex");
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join(&entry), native_exe(&format!("alab {build}"))).unwrap();
    std::fs::write(
        dir.join(crate::alab::PACKAGE_JSON),
        format!(
            r#"{{"layoutVersion":1,"version":"0.164.0-dev.{build}+gabc","entrypoint":"{entry}"}}"#
        ),
    )
    .unwrap();
    std::fs::write(dir.join(crate::alab::ALAB_BUILD_JSON), b"{}").unwrap();
    crate::store::mark_build_ready(&dir).unwrap();
    if active {
        crate::activate::activate_build(l, &dir).unwrap();
        crate::activate::install_shims(l, &dir, &["codex".to_string()], crate::Aliases::Off)
            .unwrap();
    }
}

fn codex() -> &'static VendorSpec {
    super::super::spec("codex").unwrap()
}

fn lane<'a>(
    l: &'a Layout,
    f: &'a Fake,
    policy: &'a Policy,
    trust: &'a Trust,
    asked: bool,
) -> Lane<'a> {
    Lane {
        layout: l,
        fetcher: f,
        triple: world::triple(),
        policy,
        trust,
        legacy_index: None,
        asked,
        now: NOW,
        download_turn: None,
    }
}

/// The target ALab builds codex for ([`crate::alab`]): the source rule's input on every ALab
/// arm here, whatever this host is — the synthetic fixture executable is never run, and those
/// arms read no vendor document. A vendor install keeps [`lane`]'s host triple, the one the
/// fake publishes for.
const ALAB_TARGET: &str = "aarch64-apple-darwin";

/// [`lane`], deciding for [`ALAB_TARGET`].
fn alab_lane<'a>(
    l: &'a Layout,
    f: &'a Fake,
    policy: &'a Policy,
    trust: &'a Trust,
    asked: bool,
) -> Lane<'a> {
    Lane {
        triple: ALAB_TARGET,
        ..lane(l, f, policy, trust, asked)
    }
}

fn active(l: &Layout) -> Option<u64> {
    crate::ops::active_builds(l).get("codex").copied()
}

const B: u64 = 2_026_101_601;

/// No regression to the vendor (P5, P6): with a NEWER vendor head published, an active
/// ALab build is kept unattended and when a person asks, and no vendor host is asked
/// anything. The control: the same build without its `alab-build.json` is a legacy build the
/// person's ask replaces with the vendor's head, as before.
#[test]
fn an_alab_build_is_kept_asked_or_not_and_no_vendor_head_is_read() {
    let l = world::layout("alab-kept");
    alab_codex(&l, B, true);
    let f = Fake::default();
    f.publish_codex_default("0.165.0");
    let (policy, trust) = (Policy::default(), world::trust());
    for asked in [false, true] {
        let o = install_one(&alab_lane(&l, &f, &policy, &trust, asked), codex(), false);
        assert!(
            matches!(o.verdict, Verdict::AlabKept { .. }),
            "asked={asked}: {o:?}"
        );
        assert!(!o.is_failure());
        assert!(!o.line().contains("behind"), "{}", o.line());
        assert!(o.line().contains("ALab build 0.164.0-dev."), "{}", o.line());
        assert!(
            o.into_install(&l)
                .is_ok_and(|r| r.already_current && r.build == B)
        );
    }
    assert!(f.gets.borrow().is_empty(), "no vendor document was read");
    assert!(f.downloads.borrow().is_empty());
    assert_eq!(active(&l), Some(B));

    // Control: the marker gone, the person's ask moves it to the vendor's head.
    std::fs::remove_file(l.build_dir("codex", B).join(crate::alab::ALAB_BUILD_JSON)).unwrap();
    let o = install_one(&lane(&l, &f, &policy, &trust, true), codex(), false);
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    assert!(!f.gets.borrow().is_empty());
    assert_ne!(active(&l), Some(B));
}

/// The one way back to the vendor is a person's recorded choice: with it, the person's
/// ask installs the vendor's head over the ALab build.
#[test]
fn a_persons_vendor_choice_is_the_one_way_back_to_the_vendor() {
    let l = world::layout("alab-chosen");
    alab_codex(&l, B, true);
    let f = Fake::default();
    f.publish_codex_default("0.165.0");
    crate::alab::choose_vendor(&l, "codex").unwrap();
    let (policy, trust) = (Policy::default(), world::trust());
    let o = install_one(&lane(&l, &f, &policy, &trust, true), codex(), false);
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    assert_eq!(
        active(&l),
        Some(Version::parse("0.165.0").unwrap().build_id())
    );
}

/// A signed yank of the active ALab build rolls back onto the newest older ALab build no
/// yank names (P5) — skipping a legacy build of the vendor's bytes between them. With none
/// admissible the yanked build's commands are disabled, as the index lane disables a revoked
/// build, and the next pass — asked or not, a newer vendor head published — still installs
/// nothing of the vendor's: the tombstoned build is ALab's, read off its activation link.
#[test]
fn a_yanked_alab_build_rolls_back_onto_an_older_alab_build_and_never_to_the_vendor() {
    let l = world::layout("alab-yank");
    alab_codex(&l, B - 2, false);
    // A legacy index build of the vendor's bytes, retained between them.
    let legacy = l.build_dir("codex", B - 1);
    std::fs::create_dir_all(legacy.join("bin")).unwrap();
    std::fs::write(legacy.join(world::bin("codex")), native_exe("legacy")).unwrap();
    crate::store::mark_build_ready(&legacy).unwrap();
    alab_codex(&l, B, true);
    let f = Fake::default();
    f.publish_codex_default("0.165.0");
    let trust = world::trust();
    let yank = format!("codex@{B}");
    let policy = Policy::from_entries([yank.as_str()]);
    let o = install_one(&alab_lane(&l, &f, &policy, &trust, false), codex(), false);
    assert!(
        matches!(o.verdict, Verdict::AlabRolledBack { to_build, .. } if to_build == B - 2),
        "{o:?}"
    );
    assert_eq!(active(&l), Some(B - 2));

    // Both ALab builds yanked: the active one is disabled, and the vendor is never asked.
    let older = format!("codex@{}", B - 2);
    let policy = Policy::from_entries([yank.as_str(), older.as_str()]);
    for asked in [false, true, false] {
        let o = install_one(&alab_lane(&l, &f, &policy, &trust, asked), codex(), false);
        assert!(
            matches!(
                o.verdict,
                Verdict::AlabTombstoned {
                    why: Revoked::Yanked,
                    ..
                }
            ),
            "asked={asked}: {o:?}"
        );
        assert!(o.is_failure());
        assert_eq!(active(&l), None, "a revoked build's commands are disabled");
        assert_eq!(crate::alab::current_build(&l, "codex"), Some(B - 2));
    }
    assert!(f.gets.borrow().is_empty(), "no vendor document was read");
    assert!(f.downloads.borrow().is_empty());

    // The yank lifted: the ALab build's commands are laid back, still no vendor read.
    let policy = Policy::from_entries([yank.as_str()]);
    let o = install_one(&alab_lane(&l, &f, &policy, &trust, false), codex(), false);
    assert!(matches!(o.verdict, Verdict::AlabKept { .. }), "{o:?}");
    assert_eq!(active(&l), Some(B - 2));
    assert!(f.gets.borrow().is_empty());
}

/// The signed build floor revokes an ALab build as a yank does (the index lane's floor,
/// `min_build_for`): the active build below it is disabled, and a rollback never lands on
/// a retained build below it. The control: at the floor, the build is kept.
#[test]
fn the_signed_floor_revokes_an_alab_build_and_bounds_its_rollback() {
    let l = world::layout("alab-floor");
    alab_codex(&l, B - 2, false);
    alab_codex(&l, B, true);
    let f = Fake::default();
    f.publish_codex_default("0.165.0");
    let trust = world::trust();
    let floored = |floor: u64, yanked: &[String]| {
        let yanked: Vec<String> = yanked.iter().map(|y| format!("\"{y}\"")).collect();
        let doc = format!(
            "schema = 2\nindex_build = 1\nvalid_until = \"2099-01-01T00:00:00Z\"\n\
             [[channels]]\nname = \"stable\"\nyanked = [{}]\n\
             min_build_by_program = {{ codex = {floor} }}\n",
            yanked.join(", ")
        );
        Policy::from_index(&aterm_toml::from_str::<crate::manifest::Index>(&doc).unwrap())
    };
    // At the floor: kept (the control).
    let policy = floored(B, &[]);
    let o = install_one(&alab_lane(&l, &f, &policy, &trust, false), codex(), false);
    assert!(matches!(o.verdict, Verdict::AlabKept { .. }), "{o:?}");
    // Yanked, with the floor above the build behind it: no rollback below the floor.
    let policy = floored(B - 1, &[format!("codex@{B}")]);
    let o = install_one(&alab_lane(&l, &f, &policy, &trust, false), codex(), false);
    assert!(
        matches!(
            o.verdict,
            Verdict::AlabTombstoned {
                why: Revoked::Yanked,
                ..
            }
        ),
        "{o:?}"
    );
    assert_eq!(active(&l), None);
    assert!(
        roll_back_by_hand(&l, &policy, codex()).is_err(),
        "a rollback by hand never lands below the floor"
    );
    // Below the floor, nothing yanked: disabled all the same.
    let l = world::layout("alab-floor-only");
    alab_codex(&l, B, true);
    let policy = floored(B + 1, &[]);
    let o = install_one(&alab_lane(&l, &f, &policy, &trust, true), codex(), false);
    assert!(
        matches!(
            o.verdict,
            Verdict::AlabTombstoned {
                why: Revoked::BelowFloor,
                ..
            }
        ),
        "{o:?}"
    );
    assert_eq!(active(&l), None);
    assert!(f.gets.borrow().is_empty(), "no vendor document was read");
}

/// An ALab build whose `bin/` shim is gone (an unlink that laid nothing back, a hand edit)
/// or tombstoned is still the machine's ALab build: unless the signed policy revokes it, the
/// lane lays its commands back, asked or not, rather than reading the machine as having no
/// codex and installing the vendor's head.
#[test]
fn an_alab_build_hidden_by_a_lost_or_tombstoned_shim_is_laid_back_never_replaced() {
    let l = world::layout("alab-lost-shim");
    alab_codex(&l, B, true);
    let shim = l.shim(&crate::store::ToolName::new("codex").unwrap());
    std::fs::remove_file(&shim).unwrap();
    assert_eq!(active(&l), None);
    let f = Fake::default();
    f.publish_codex_default("0.165.0");
    let (policy, trust) = (Policy::default(), world::trust());
    for asked in [false, true] {
        let o = install_one(&alab_lane(&l, &f, &policy, &trust, asked), codex(), false);
        assert!(matches!(o.verdict, Verdict::AlabKept { .. }), "{o:?}");
        assert_eq!(active(&l), Some(B));
        // A tombstone over it hides it the same way (the index lane disabled it under a
        // yank since lifted): read off `current`, laid back, never the vendor's.
        crate::flow::install_tombstone_shims(&l, "codex", Some(B));
        assert_eq!(active(&l), None);
    }
    let o = install_one(&alab_lane(&l, &f, &policy, &trust, false), codex(), false);
    assert!(matches!(o.verdict, Verdict::AlabKept { .. }), "{o:?}");
    assert_eq!(active(&l), Some(B));
    assert!(f.gets.borrow().is_empty(), "no vendor document was read");
    assert!(f.downloads.borrow().is_empty());
}

/// `aterm pkg rollback codex` from an ALab build lands on an older ALab build only: a
/// legacy build of the vendor's bytes below it is never the target.
#[test]
fn a_rollback_by_hand_from_an_alab_build_lands_on_alab_builds_only() {
    let l = world::layout("alab-byhand");
    alab_codex(&l, B - 2, false);
    let legacy = l.build_dir("codex", B - 1);
    std::fs::create_dir_all(legacy.join("bin")).unwrap();
    std::fs::write(legacy.join(world::bin("codex")), native_exe("legacy")).unwrap();
    crate::store::mark_build_ready(&legacy).unwrap();
    alab_codex(&l, B, true);
    let moved = roll_back_by_hand(&l, &Policy::default(), codex()).expect("an older ALab build");
    assert_eq!(moved.to_build, B - 2);
    assert!(moved.to.starts_with("ALab build"), "{moved:?}");
    // From the oldest ALab build, the legacy build below is no target.
    let only = world::layout("alab-byhand-only");
    let legacy = only.build_dir("codex", B - 1);
    std::fs::create_dir_all(legacy.join("bin")).unwrap();
    std::fs::write(legacy.join(world::bin("codex")), native_exe("legacy")).unwrap();
    crate::store::mark_build_ready(&legacy).unwrap();
    alab_codex(&only, B, true);
    roll_back_by_hand(&only, &Policy::default(), codex()).expect_err("no older ALab build");
    assert_eq!(active(&only), Some(B));
}

/// Restoring a kept build is still an operation that can fail. A nonempty directory at the
/// exact shim path is this fixture's deterministic obstruction; no permissions or host
/// state are changed, and no vendor payload is needed to reach the restoration.
#[test]
fn an_alab_command_restore_error_is_a_failed_outcome() {
    let l = world::layout("alab-restore-error");
    alab_codex(&l, B, true);
    let shim = l.shim(&crate::store::ToolName::new("codex").unwrap());
    let f = Fake::default();
    let (policy, trust) = (Policy::default(), world::trust());
    let mut config = alab_lane(&l, &f, &policy, &trust, false);

    std::fs::remove_file(&shim).unwrap();
    let restored = install_one(&config, codex(), false);
    assert!(!restored.is_failure());
    assert_eq!(active(&l), Some(B), "ordinary missing-shim restoration");
    std::fs::remove_file(&shim).unwrap();
    std::fs::create_dir(&shim).unwrap();
    let obstruction = shim.join("keep");
    std::fs::write(&obstruction, b"fixture obstruction").unwrap();
    let mut failed = Vec::new();
    for asked in [false, true] {
        config.asked = asked;
        let outcome = install_one(&config, codex(), false);
        assert_eq!(active(&l), None, "the obstructed shim was not restored");
        assert_eq!(std::fs::read(&obstruction).unwrap(), b"fixture obstruction");
        failed.push(outcome.is_failure());
    }
    assert!(f.gets.borrow().is_empty(), "no vendor head was needed");
    assert!(f.downloads.borrow().is_empty());
    // Removing only our obstruction permits a later pass to restore the retained build.
    std::fs::remove_file(&obstruction).unwrap();
    std::fs::remove_dir(&shim).unwrap();
    let recovered = install_one(&config, codex(), false);
    assert!(!recovered.is_failure());
    assert_eq!(active(&l), Some(B));
    assert_eq!(
        failed,
        [true, true],
        "each actual restoration error must fail its pass"
    );
}

/// CODEX-OLD IS ALWAYS THE VENDOR'S BYTES (`codex` is codex-alab
/// where ALab builds it, upstream Codex is `codex-old`, both bundled). With `codex` on an
/// active ALab build that its lane keeps without reading a head, the codex-old lane reads
/// OpenAI's head and lands OpenAI's package in its own store: its shim and twin run that
/// build's clone of OpenAI's entry, which is no ALab executable, and `codex` stays on its
/// ALab build. Even an ALab package laid in codex-old's own store is never kept as ALab's:
/// no ALab row names the program, so the lane decides it against the vendor's head.
///
/// NEGATIVE CONTROL: the same ALab package in codex's store is kept, as above.
#[test]
fn codex_old_is_the_vendors_bytes_where_codex_is_alab() {
    let l = world::layout("alab-codex-old");
    alab_codex(&l, B, true);
    let f = Fake::default();
    f.publish_codex_default("0.163.0");
    let (policy, trust) = (Policy::default(), world::trust());
    let old = super::super::spec("codex-old").unwrap();
    let kept = install_one(&alab_lane(&l, &f, &policy, &trust, false), codex(), false);
    assert!(matches!(kept.verdict, Verdict::AlabKept { .. }), "{kept:?}");
    assert!(
        f.gets.borrow().is_empty(),
        "PRECONDITION: codex read no head"
    );

    let o = install_one(&lane(&l, &f, &policy, &trust, false), old, false);
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    let build = super::super::Version::parse("0.163.0").unwrap().build_id();
    let alias = l
        .build_dir("codex-old", build)
        .join(world::bin("codex-old"));
    assert_eq!(std::fs::read(&alias).unwrap(), native_exe("codex 0.163.0"));
    assert!(!crate::alab::exe_is_alab(&alias));
    let tool = |name: &str| crate::store::ToolName::new(name).unwrap();
    assert_eq!(
        crate::platform::resolve_shim(&l.shim(&tool("codex-old"))),
        Some(alias.clone())
    );
    assert_eq!(crate::activate::twin_target(&l, "codex-old"), Some(alias));
    assert_eq!(active(&l), Some(B), "codex stays on its ALab build");

    // An ALab package in codex-old's own store is no ALab build of it: the vendor's head,
    // asked for, replaces it, where codex's own is kept.
    let alab_old = l.build_dir("codex-old", B);
    std::fs::create_dir_all(alab_old.join("bin")).unwrap();
    std::fs::write(alab_old.join(world::bin("codex-old")), native_exe("alab")).unwrap();
    std::fs::write(
        alab_old.join(crate::alab::PACKAGE_JSON),
        format!(
            r#"{{"layoutVersion":1,"version":"0.164.0-dev.1+gabc","entrypoint":"{}"}}"#,
            world::bin("codex-old")
        ),
    )
    .unwrap();
    std::fs::write(alab_old.join(crate::alab::ALAB_BUILD_JSON), b"{}").unwrap();
    crate::store::mark_build_ready(&alab_old).unwrap();
    crate::activate::activate_build(&l, &alab_old).unwrap();
    crate::activate::install_shims(
        &l,
        &alab_old,
        &["codex-old".to_string()],
        crate::Aliases::Off,
    )
    .unwrap();
    assert!(crate::alab::active_alab_build(&l, "codex-old", ALAB_TARGET).is_none());
    let o = install_one(&lane(&l, &f, &policy, &trust, true), old, false);
    assert!(!matches!(o.verdict, Verdict::AlabKept { .. }), "{o:?}");
    assert_eq!(
        crate::ops::active_builds(&l).get("codex-old").copied(),
        Some(build),
        "{o:?}"
    );
    let kept = install_one(&alab_lane(&l, &f, &policy, &trust, true), codex(), false);
    assert!(matches!(kept.verdict, Verdict::AlabKept { .. }), "{kept:?}");
}
