// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for the vendor lane when an ALab build ([`crate::alab`]) is present: retention,
//! yank/floor rollback and tombstoning, and explicit vendor choice.

use super::world::{self, Fake, NOW, native_exe};
use super::*;

/// Lay a complete ALab build of `codex` in the store at `build`, activating it when `active`.
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

/// Target triple for which ALab builds `codex` ([`crate::alab`]).
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

/// An active ALab build is kept without querying the vendor host until its marker is removed.
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

    std::fs::remove_file(l.build_dir("codex", B).join(crate::alab::ALAB_BUILD_JSON)).unwrap();
    let o = install_one(&lane(&l, &f, &policy, &trust, true), codex(), false);
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    assert!(!f.gets.borrow().is_empty());
    assert_ne!(active(&l), Some(B));
}

/// Recording a vendor choice allows an explicit install to replace the ALab build with the vendor head.
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

/// A yanked ALab build rolls back only to an unrevoked ALab build, tombstoning if none remains.
#[test]
fn a_yanked_alab_build_rolls_back_onto_an_older_alab_build_and_never_to_the_vendor() {
    let l = world::layout("alab-yank");
    alab_codex(&l, B - 2, false);
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

    let policy = Policy::from_entries([yank.as_str()]);
    let o = install_one(&alab_lane(&l, &f, &policy, &trust, false), codex(), false);
    assert!(matches!(o.verdict, Verdict::AlabKept { .. }), "{o:?}");
    assert_eq!(active(&l), Some(B - 2));
    assert!(f.gets.borrow().is_empty());
}

/// The signed `min_build_by_program` floor revokes an ALab build and bounds its rollback.
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
    let policy = floored(B, &[]);
    let o = install_one(&alab_lane(&l, &f, &policy, &trust, false), codex(), false);
    assert!(matches!(o.verdict, Verdict::AlabKept { .. }), "{o:?}");
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

/// An unrevoked ALab build whose shim was removed or tombstoned has its shim restored without querying the vendor.
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
        crate::flow::install_tombstone_shims(&l, "codex", Some(B));
        assert_eq!(active(&l), None);
    }
    let o = install_one(&alab_lane(&l, &f, &policy, &trust, false), codex(), false);
    assert!(matches!(o.verdict, Verdict::AlabKept { .. }), "{o:?}");
    assert_eq!(active(&l), Some(B));
    assert!(f.gets.borrow().is_empty(), "no vendor document was read");
    assert!(f.downloads.borrow().is_empty());
}

/// Manual rollback from an ALab build skips legacy vendor builds and lands only on an older ALab build.
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
    let only = world::layout("alab-byhand-only");
    let legacy = only.build_dir("codex", B - 1);
    std::fs::create_dir_all(legacy.join("bin")).unwrap();
    std::fs::write(legacy.join(world::bin("codex")), native_exe("legacy")).unwrap();
    crate::store::mark_build_ready(&legacy).unwrap();
    alab_codex(&only, B, true);
    roll_back_by_hand(&only, &Policy::default(), codex()).expect_err("no older ALab build");
    assert_eq!(active(&only), Some(B));
}

/// A filesystem error while restoring a missing ALab shim reports a failed outcome.
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

/// `codex-old` always installs upstream vendor bytes while `codex` remains on its ALab build.
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

/// An active ALab codex lends codex-old nothing: its build is no vendor build of the release
/// codex-old lands and carries no record of OpenAI's bytes, so codex-old downloads them.
#[test]
fn an_alab_codex_lends_codex_old_nothing() {
    let l = world::layout("alab-lends-nothing");
    alab_codex(&l, B, true);
    let f = Fake::default();
    f.publish_codex_default("0.163.0");
    let (policy, trust) = (Policy::default(), world::trust());
    let old = super::super::spec("codex-old").unwrap();
    let o = install_one(&lane(&l, &f, &policy, &trust, false), old, false);
    assert!(matches!(o.verdict, Verdict::Installed { .. }), "{o:?}");
    assert_eq!(
        f.downloads.borrow().as_slice(),
        [Fake::codex_payload_url("0.163.0")]
    );
    let dir = l.build_dir(
        "codex-old",
        super::super::Version::parse("0.163.0").unwrap().build_id(),
    );
    assert_eq!(complete_record(&dir).unwrap().tree_from, None);
    let alias = dir.join(world::bin("codex-old"));
    assert_eq!(std::fs::read(&alias).unwrap(), native_exe("codex 0.163.0"));
    assert!(!crate::alab::exe_is_alab(&alias));
    assert_eq!(active(&l), Some(B), "codex stays on its ALab build");
}
