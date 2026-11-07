// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The update pass and the update verb meet ALab's codex ([`crate::alab`]).

use super::*;
use crate::alab::tests::{self as at, Both};
use crate::vendor_direct::lane::world::{self, Fake};

fn now() -> i64 {
    world::NOW
}

fn anchor(_: &crate::store::Layout) -> crate::Anchor {
    crate::sig::testkit::anchor()
}

fn nothing(_: &crate::store::Layout) {}

fn no_warm(_: &crate::store::Layout, _: &std::collections::BTreeSet<String>) {}

fn no_processes() -> Option<Vec<std::path::PathBuf>> {
    Some(Vec::new())
}

fn seams(trust: &crate::vendor_direct::lane::Trust) -> PassSeams<'_> {
    PassSeams {
        scope: UpdateScope::Whole,
        config: None,
        anchor,
        now,
        trust,
        shell_hooks: nothing,
        rustup_seam: &nothing,
        progress: None,
        path: None,
        warm_agents: no_warm,
        translated: || false,
        flip_policy: || crate::quiet::FlipPolicy::Now,
        running: &no_processes,
        vendor_read_grace: VENDOR_READ_GRACE,
        read_ahead_in: &no_read_ahead_note,
    }
}

fn codex() -> &'static crate::vendor_direct::VendorSpec {
    crate::vendor_direct::spec("codex").unwrap()
}

fn vendor_codex(layout: &crate::store::Layout, vendor: &Fake, version: &str) -> u64 {
    vendor.publish_codex_default(version);
    let policy = crate::vendor_direct::policy::Policy::default();
    let trust = world::trust();
    let lane = crate::vendor_direct::lane::Lane {
        layout,
        fetcher: vendor,
        triple: world::triple(),
        policy: &policy,
        trust: &trust,
        legacy_index: None,
        asked: false,
        now: now(),
        download_turn: None,
    };
    let o = crate::vendor_direct::lane::install_one(&lane, codex(), false);
    assert!(
        matches!(
            o.verdict,
            crate::vendor_direct::lane::Verdict::Installed { .. }
        ),
        "{o:?}"
    );
    crate::vendor_direct::Version::parse(version)
        .unwrap()
        .build_id()
}

fn active(layout: &crate::store::Layout) -> Option<u64> {
    crate::active_builds(layout).get("codex").copied()
}

#[test]
fn a_pass_moves_a_vendor_codex_onto_the_alab_pin_and_the_legacy_row_leaves_it() {
    if current_triple() != "aarch64-apple-darwin" {
        return;
    }
    for (label, repo, moves) in [
        ("alab-pin", "codex-alab", true),
        ("legacy-row", "aterm", false),
    ] {
        let layout = world::layout(label);
        let vendor = Fake::default();
        let vendor_build = vendor_codex(&layout, &vendor, "0.165.0");
        vendor.gets.borrow_mut().clear();
        let ch = at::channel(
            repo,
            at::BUILD,
            at::pack_archive(at::BUILD, &at::signed_exe(), true),
        );
        let both = Both {
            index: &ch,
            vendor: &vendor,
        };
        let trust = world::trust();
        let installed = crate::active_builds(&layout);
        let cfg = crate::config::PackagesConfig::default();
        run_update_pass(&layout, &cfg, &both, &installed, false, &seams(&trust));
        let codex_gets = vendor
            .gets
            .borrow()
            .iter()
            .filter(|(url, _)| url.contains("codex"))
            .count();
        if moves {
            assert_eq!(active(&layout), Some(at::BUILD), "{label}");
            assert_eq!(
                crate::alab::active_alab_build(&layout, "codex", current_triple()),
                Some(at::BUILD)
            );
            assert_eq!(
                codex_gets, 0,
                "{label}: the vendor lane left codex to the index"
            );
            let row = crate::status::read(&layout).unwrap().programs["codex"].clone();
            assert_eq!(row.installed_build, Some(at::BUILD));
            assert!(!row.tree_root.is_empty(), "{label}: tree_root recorded");
            assert!(
                crate::state::source_words("codex", &row.state, 0).starts_with("ALab build"),
                "{}",
                row.state
            );
            // If status.toml loses tree_root while codex is already at at::BUILD, the next pass
            // recovers the signed attestation for the ALab store build.
            let mut cleared = crate::status::read(&layout).unwrap();
            cleared.programs.get_mut("codex").unwrap().tree_root.clear();
            crate::status::write(&layout, &cleared).unwrap();
            let installed = crate::active_builds(&layout);
            run_update_pass(&layout, &cfg, &both, &installed, false, &seams(&trust));
            assert_eq!(
                crate::status::read(&layout).unwrap().programs["codex"].tree_root,
                row.tree_root,
                "{label}: recover_missing_roots restores the ALab build's tree_root"
            );
        } else {
            assert_eq!(active(&layout), Some(vendor_build), "{label}");
            assert!(ch.asked.borrow().is_empty(), "{label}: no manifest asked");
            assert!(
                codex_gets > 0,
                "{label}: the vendor lane still checks codex"
            );
        }
    }
}

#[test]
fn a_pass_keeps_an_installed_alab_codex_without_a_pin_and_reads_no_vendor_head() {
    if current_triple() != "aarch64-apple-darwin" {
        return;
    }
    let layout = world::layout("alab-keep");
    let vendor = Fake::default();
    vendor.publish_codex_default("0.165.0");
    let alab = at::channel(
        "codex-alab",
        at::BUILD,
        at::pack_archive(at::BUILD, &at::signed_exe(), true),
    );
    let trust = world::trust();
    let cfg = crate::config::PackagesConfig::default();
    let both = Both {
        index: &alab,
        vendor: &vendor,
    };
    run_update_pass(
        &layout,
        &cfg,
        &both,
        &crate::active_builds(&layout),
        false,
        &seams(&trust),
    );
    let report = crate::flow::install(
        &both,
        &layout,
        &crate::sig::testkit::anchor(),
        &crate::flow::InstallRequest {
            channel: "stable",
            program: "codex",
            triple: current_triple(),
            installed: None,
        },
        crate::sig::BuildFloor::default(),
        now(),
    )
    .expect("the ALab pack");
    assert_eq!(report.build, at::BUILD);
    for (label, repo) in [("legacy", "aterm"), ("none", "")] {
        vendor.gets.borrow_mut().clear();
        let ch = if repo.is_empty() {
            at::Channel::default()
        } else {
            at::channel(
                repo,
                at::BUILD + 1,
                at::pack_archive(at::BUILD + 1, &at::signed_exe(), true),
            )
        };
        let both = Both {
            index: &ch,
            vendor: &vendor,
        };
        run_update_pass(
            &layout,
            &cfg,
            &both,
            &crate::active_builds(&layout),
            false,
            &seams(&trust),
        );
        assert_eq!(active(&layout), Some(at::BUILD), "{label}");
        assert!(
            !vendor
                .gets
                .borrow()
                .iter()
                .any(|(u, _)| u.contains("codex")),
            "{label}: a vendor head was read"
        );
    }
}

#[test]
fn the_source_flags_refuse_before_anything_runs() {
    let argv = |a: &[&str]| -> Vec<String> { a.iter().map(|s| (*s).to_string()).collect() };
    assert_eq!(
        cmd_update_argv(&argv(&["codex", "--vendor", "--alab"])),
        ExitCode::from(2)
    );
    assert_eq!(cmd_update_argv(&argv(&["--vendor"])), ExitCode::from(2));
    assert_eq!(
        cmd_update_argv(&argv(&["claude", "--vendor"])),
        ExitCode::from(2)
    );
    assert_eq!(cmd_update_argv(&argv(&["ay", "--alab"])), ExitCode::from(2));
    assert!(arity_refusal(&argv(&["codex", "--vendor"]), "update").is_none());
    assert!(arity_refusal(&argv(&["codex", "--alab", "--retry"]), "update").is_none());
}

#[test]
fn a_link_of_a_package_directory_names_its_entrypoint() {
    let d = crate::testdir::dir("atpkg-cli-alab-link-");
    let pkg = d.join("pkg");
    std::fs::create_dir_all(pkg.join("bin")).unwrap();
    std::fs::write(pkg.join("bin/codex"), b"x").unwrap();
    std::fs::write(
        pkg.join(crate::alab::PACKAGE_JSON),
        r#"{"layoutVersion":1,"entrypoint":"bin/codex"}"#,
    )
    .unwrap();
    assert_eq!(
        link_bins_for("codex", &pkg),
        vec![std::path::PathBuf::from("bin/codex")]
    );
    assert_eq!(link_bins_for("codex", &d), default_link_bins("codex"));
    assert_eq!(link_bins_for("claude", &pkg), default_link_bins("claude"));
}

#[test]
fn a_dev_linked_codex_names_the_build_unlink_returns_to() {
    let alab = world::layout("alab-unlink-line");
    let dir = alab.build_dir("codex", at::BUILD);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join("bin/codex"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(
        dir.join(crate::alab::PACKAGE_JSON),
        r#"{"layoutVersion":1,"version":"0.164.0-dev.1+gabc","entrypoint":"bin/codex"}"#,
    )
    .unwrap();
    std::fs::write(dir.join(crate::alab::ALAB_BUILD_JSON), b"{}").unwrap();
    crate::store::mark_build_ready(&dir).unwrap();
    crate::activate::activate_build(&alab, &dir).unwrap();
    assert!(unlink_returns_to_alab(&alab, "codex"));
    assert_eq!(
        dev_linked_line("codex", true),
        "atpkg: codex is dev-linked; run `aterm pkg unlink codex`, then `aterm pkg install \
         codex`, to return to the codex-alab build"
    );
    assert!(
        !unlink_returns_to_alab(&alab, "claude"),
        "ALab never builds claude"
    );
    crate::alab::choose_vendor(&alab, "codex").unwrap();
    assert!(
        !unlink_returns_to_alab(&alab, "codex"),
        "a person chose the vendor's build"
    );

    let vendor = world::layout("alab-unlink-line-vendor");
    vendor_codex(&vendor, &Fake::default(), "0.165.0");
    assert!(!unlink_returns_to_alab(&vendor, "codex"));
    assert_eq!(
        dev_linked_line("codex", false),
        "atpkg: codex is dev-linked; run `aterm pkg unlink codex`, then `aterm pkg install \
         codex`, to install OpenAI's latest"
    );
}

#[test]
fn a_rollback_onto_an_alab_build_names_alab_s_channel() {
    let l = world::layout("alab-rollback-line");
    let dir = l.build_dir("codex", at::BUILD);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join("bin/codex"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(
        dir.join(crate::alab::PACKAGE_JSON),
        r#"{"layoutVersion":1,"version":"0.164.0-dev.1+gabc","entrypoint":"bin/codex"}"#,
    )
    .unwrap();
    std::fs::write(dir.join(crate::alab::ALAB_BUILD_JSON), b"{}").unwrap();
    crate::store::mark_build_ready(&dir).unwrap();
    assert_eq!(alab_channel_of(&l, "codex", at::BUILD), Some("codex-alab"));
    assert_eq!(
        vendor_rolled_back_line(codex(), "b", "a", false, Some("codex-alab")),
        "atpkg: rolled back codex from b to a; `aterm pkg pin codex` holds it there (otherwise \
         the next update follows codex-alab's channel)"
    );
    assert_eq!(alab_channel_of(&l, "claude", at::BUILD), None);
    let v = crate::vendor_direct::Version::parse("0.163.0")
        .unwrap()
        .build_id();
    assert_eq!(alab_channel_of(&l, "codex", v), None, "a vendor build id");
    crate::alab::choose_vendor(&l, "codex").unwrap();
    assert_eq!(alab_channel_of(&l, "codex", at::BUILD), None);
    assert_eq!(
        vendor_rolled_back_line(codex(), "b", "a", false, None),
        "atpkg: rolled back codex from b to a; `aterm pkg pin codex` holds it there (otherwise \
         the next update returns it to OpenAI's latest)"
    );
}

fn alab_store_codex(layout: &crate::store::Layout, record: &[u8]) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = layout.build_dir("codex", at::BUILD);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join("bin/codex"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        dir.join("bin/codex"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(
        dir.join(crate::alab::PACKAGE_JSON),
        r#"{"layoutVersion":1,"version":"0.164.0-dev.1+gabc","entrypoint":"bin/codex"}"#,
    )
    .unwrap();
    std::fs::write(dir.join(crate::alab::ALAB_BUILD_JSON), record).unwrap();
    crate::store::mark_build_ready(&dir).unwrap();
    crate::activate::activate_build(layout, &dir).unwrap();
    crate::activate::install_shims(
        layout,
        &dir,
        &["codex".to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    dir
}

#[test]
fn which_codex_says_the_upstream_an_alab_build_records() {
    let layout = world::layout("alab-which-upstream");
    let dir = alab_store_codex(&layout, br#"{"source":"0"}"#);
    let path = std::env::join_paths([layout.bin_dir()]).unwrap();
    let which = || {
        which_line_in(
            &layout,
            "codex",
            Some(&path),
            crate::reroute::StubEnv::default(),
        )
        .unwrap()
    };
    let older = which();
    assert!(
        older.starts_with(&format!(
            "codex → {} → {} — ",
            layout
                .shim(&crate::store::ToolName::new("codex").unwrap())
                .display(),
            dir.join("bin/codex").display()
        )),
        "{older}"
    );
    assert!(older.contains("ALab build 0.164.0-dev.1+gabc"), "{older}");
    assert!(!older.contains("based on upstream"), "{older}");
    assert_eq!(older.lines().count(), 1, "{older}");
    let record = dir.join(crate::alab::ALAB_BUILD_JSON);
    std::fs::write(&record, at::upstream_record(&at::full_upstream())).unwrap();
    assert_eq!(
        which(),
        format!("{older} — based on upstream rust-v0.163.0+g0123456789ab")
    );
    for bad in [
        at::upstream_record(r#""rust-v0.163.0""#),
        at::upstream_record(r#"{"release":"rust-v0.163.0","commit":"0123456789ab"}"#),
        "{\"upstream\":{\"release\":\"rust-v0.163.0\",".to_string(),
        "\u{0}not json".to_string(),
    ] {
        std::fs::write(&record, &bad).unwrap();
        assert_eq!(which(), older, "{bad}");
    }
}

#[test]
fn which_codex_says_the_upstream_a_dev_linked_package_records() {
    use std::os::unix::fs::PermissionsExt as _;
    let layout = world::layout("alab-which-upstream-link");
    let d = crate::testdir::dir("atpkg-cli-alab-upstream-link-");
    let pkg = std::path::absolute(d.join("pkg")).unwrap();
    std::fs::create_dir_all(pkg.join("bin")).unwrap();
    std::fs::write(pkg.join("bin/codex"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        pkg.join("bin/codex"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(
        pkg.join(crate::alab::PACKAGE_JSON),
        r#"{"layoutVersion":1,"version":"0.164.0-dev.2+gdef","entrypoint":"bin/codex"}"#,
    )
    .unwrap();
    std::fs::write(
        pkg.join(crate::alab::ALAB_BUILD_JSON),
        at::upstream_record(&at::full_upstream()),
    )
    .unwrap();
    crate::link(
        &layout,
        "codex",
        &pkg,
        &[std::path::PathBuf::from("bin/codex")],
    )
    .unwrap();
    let path = std::env::join_paths([layout.bin_dir()]).unwrap();
    let which = || {
        which_line_in(
            &layout,
            "codex",
            Some(&path),
            crate::reroute::StubEnv::default(),
        )
        .unwrap()
    };
    let linked = which();
    assert!(
        linked.ends_with(
            " — dev-linked (managed from that checkout, not the index) — based on upstream \
             rust-v0.163.0+g0123456789ab"
        ),
        "{linked}"
    );
    std::fs::write(pkg.join(crate::alab::ALAB_BUILD_JSON), b"{}").unwrap();
    assert_eq!(
        which(),
        linked
            .strip_suffix(" — based on upstream rust-v0.163.0+g0123456789ab")
            .unwrap()
    );
}

#[test]
fn single_and_default_set_installs_warm_an_alab_codex_landing() {
    if current_triple() != "aarch64-apple-darwin" {
        return;
    }
    thread_local! {
        static WARMED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    fn record_warm(_: &crate::store::Layout, landed: &std::collections::BTreeSet<String>) {
        WARMED.with(|w| w.borrow_mut().extend(landed.iter().cloned()));
    }
    let ch = at::channel(
        "codex-alab",
        at::BUILD,
        at::pack_archive(at::BUILD, &at::signed_exe(), true),
    );
    let vendor = Fake::default();
    let both = Both {
        index: &ch,
        vendor: &vendor,
    };
    let trust = world::trust();
    let mut s = seams(&trust);
    s.warm_agents = record_warm;

    let single = world::layout("alab-warm-single");
    WARMED.with(|w| w.borrow_mut().clear());
    let live_before = crate::flow::live_verifications();
    let first = do_install_with(&single, &both, crate::config::CHANNEL, "codex", &s).unwrap();
    assert!(!first.already_current);
    assert_eq!(
        crate::flow::live_verifications() - live_before,
        1,
        "do_install_with reuses the verified index across index_owning and install_inner"
    );
    assert_eq!(
        WARMED.with(|w| w.borrow().clone()),
        vec!["codex".to_string()],
        "do_install_with warms an ALab codex landing"
    );
    WARMED.with(|w| w.borrow_mut().clear());
    let second = do_install_with(&single, &both, crate::config::CHANNEL, "codex", &s).unwrap();
    assert!(second.already_current);
    assert!(
        WARMED.with(|w| w.borrow().is_empty()),
        "already-current do_install_with does not re-warm"
    );

    let toolset = world::layout("alab-warm-toolset");
    let cfg = crate::config::PackagesConfig {
        exclude: Some(
            crate::vendor_direct::VENDORS
                .iter()
                .map(|v| v.program)
                .filter(|p| *p != "codex")
                .chain(["ay"])
                .map(str::to_string)
                .collect(),
        ),
        ..Default::default()
    };
    WARMED.with(|w| w.borrow_mut().clear());
    assert_eq!(install_toolset(&toolset, &cfg, &both, &s), 0);
    assert_eq!(
        WARMED.with(|w| w.borrow().clone()),
        vec!["codex".to_string()],
        "install_toolset warms an ALab codex landing"
    );
    assert_eq!(
        crate::status::read(&toolset).unwrap().last_index_build,
        first.index_build,
        "install_toolset records the resolved index build in status.toml"
    );
}

#[test]
fn rolling_back_from_a_newer_alab_codex_build_preserves_the_older_builds_signed_tree_root() {
    if current_triple() != "aarch64-apple-darwin" {
        return;
    }
    let layout = world::layout("alab-rollback-root");
    let vendor = Fake::default();
    let trust = world::trust();
    let s = seams(&trust);
    let exe = at::signed_exe();

    let ch1 = at::channel(
        "codex-alab",
        at::BUILD,
        at::pack_archive(at::BUILD, &exe, true),
    );
    let both1 = Both {
        index: &ch1,
        vendor: &vendor,
    };
    let r1 = do_install_with(&layout, &both1, crate::config::CHANNEL, "codex", &s).unwrap();
    assert_eq!(r1.build, at::BUILD);
    let root1 = crate::status::read(&layout).unwrap().programs["codex"]
        .tree_root
        .clone();
    assert!(!root1.is_empty());

    let next_build = at::BUILD + 1;
    let ch2 = at::channel(
        "codex-alab",
        next_build,
        at::pack_archive(next_build, &exe, true),
    );
    let both2 = Both {
        index: &ch2,
        vendor: &vendor,
    };
    let r2 = do_install_with(&layout, &both2, crate::config::CHANNEL, "codex", &s).unwrap();
    assert_eq!(r2.build, next_build);

    assert_eq!(cmd_rollback_vendor(&layout, codex()), ExitCode::SUCCESS);
    assert_eq!(active(&layout), Some(at::BUILD));
    assert_eq!(
        crate::status::read(&layout).unwrap().programs["codex"].tree_root,
        root1,
        "rolling back onto the older ALab build restores its signed tree_root"
    );
    assert_eq!(
        crate::verify::verify_all(&layout),
        vec![(
            "codex".to_string(),
            crate::verify::VerifyOutcome::Match { build: at::BUILD }
        )]
    );

    // Re-install next_build, then trigger an automatic AlabRolledBack via record_vendor_status
    // and verify that record_vendor_status reads root1 before saving next_build's root.
    let r3 = do_install_with(&layout, &both2, crate::config::CHANNEL, "codex", &s).unwrap();
    assert_eq!(r3.build, next_build);
    let policy = crate::vendor_direct::policy::Policy::from_entries([format!(
        "codex@{next_build}"
    )
    .as_str()]);
    let lane = crate::vendor_direct::lane::Lane {
        layout: &layout,
        fetcher: &vendor,
        triple: current_triple(),
        policy: &policy,
        trust: &trust,
        legacy_index: None,
        asked: false,
        now: now(),
        download_turn: None,
    };
    let outcome = crate::vendor_direct::lane::install_one(&lane, codex(), false);
    assert!(
        matches!(
            outcome.verdict,
            crate::vendor_direct::lane::Verdict::AlabRolledBack {
                to_build: at::BUILD,
                ..
            }
        ),
        "{outcome:?}"
    );
    record_vendor_status(&layout, &outcome, now());
    assert_eq!(active(&layout), Some(at::BUILD));
    assert_eq!(
        crate::status::read(&layout).unwrap().programs["codex"].tree_root,
        root1,
        "automatic AlabRolledBack preserves the older ALab build's signed tree_root"
    );
    assert_eq!(
        crate::verify::verify_all(&layout),
        vec![(
            "codex".to_string(),
            crate::verify::VerifyOutcome::Match { build: at::BUILD }
        )]
    );
}

#[test]
fn plan_update_and_do_install_reuse_the_index_from_index_owning() {
    if current_triple() != "aarch64-apple-darwin" {
        return;
    }
    let layout = world::layout("alab-update-reuse-index");
    let vendor = Fake::default();
    let trust = world::trust();
    let s = seams(&trust);
    let exe = at::signed_exe();

    let ch1 = at::channel(
        "codex-alab",
        at::BUILD,
        at::pack_archive(at::BUILD, &exe, true),
    );
    let both1 = Both {
        index: &ch1,
        vendor: &vendor,
    };
    do_install_with(&layout, &both1, crate::config::CHANNEL, "codex", &s).unwrap();

    let next_build = at::BUILD + 1;
    let ch2 = at::channel(
        "codex-alab",
        next_build,
        at::pack_archive(next_build, &exe, true),
    );
    let both2 = Both {
        index: &ch2,
        vendor: &vendor,
    };
    let anchor = (s.anchor)(&layout);
    let floor = build_floor(&layout);
    let live_before = crate::flow::live_verifications();
    let owned = index_owning(&layout, &both2, &anchor, now(), "codex").expect("index owns codex");
    let (plan, resolved) = crate::flow::plan_update_with_index(
        &both2,
        &layout,
        &anchor,
        crate::config::CHANNEL,
        current_triple(),
        "codex",
        Some(at::BUILD),
        floor,
        now(),
        Some(owned),
    )
    .unwrap();
    assert_eq!(plan.decision, crate::ApplyDecision::Install);
    let report = do_install_with_index(
        &layout,
        &both2,
        crate::config::CHANNEL,
        "codex",
        Some(resolved),
        &s,
    )
    .unwrap();
    assert_eq!(report.build, next_build);
    assert_eq!(
        crate::flow::live_verifications() - live_before,
        1,
        "index_owning + plan_update_with_index + do_install_with_index verify the index once"
    );
    let next_words = crate::alab::build_words(&layout, "codex", next_build);
    let prev_words = crate::alab::build_words(&layout, "codex", at::BUILD);
    let what = crate::vendor_direct::display_have_build(&layout, "codex", next_build);
    assert_eq!(what, format!("codex {next_words}"));
    assert_eq!(
        pin_line_for(&what, "codex", true, true, PinPeers::Alone),
        format!(
            "atpkg: codex {next_words} pinned \u{2014} updates leave it \
             there until `aterm pkg unpin codex` (a yanked build is still replaced)",
        )
    );
    assert_eq!(
        verify_outcome_line(
            &layout,
            "codex",
            &crate::verify::VerifyOutcome::Match { build: next_build }
        ),
        (
            false,
            format!("atpkg: codex {next_words} OK (matches signed tree_root)")
        )
    );
    assert_eq!(
        verify_outcome_line(
            &layout,
            "codex",
            &crate::verify::VerifyOutcome::BuildMismatch {
                active: next_build,
                recorded: Some(at::BUILD)
            }
        ),
        (
            true,
            format!(
                "atpkg: codex active {next_words} differs from recorded \
                 {prev_words} \u{2014} cannot attest; update/reinstall",
            )
        )
    );
}
