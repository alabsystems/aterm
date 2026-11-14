// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::contract;

fn grok_alab() -> &'static Row {
    contract::row_named("grok-alab").expect("rostered")
}

/// THE NEGATIVE CONTROL'S aterm (`ATERM_DEV_NO_CONTRACT3=1`): it serves no `contract=` and
/// refuses a claim naming one as the v2 grammar's usage error; unset, or any other value, it
/// serves `2,3` and reads every claim as before; a v2 claim is read as before either way.
#[test]
fn the_seam_answers_as_an_aterm_before_contract_three() {
    use std::ffi::OsStr;
    assert_eq!(served_with(None).as_deref(), Some("2,3"));
    assert_eq!(served_with(Some(OsStr::new("0"))).as_deref(), Some("2,3"));
    assert_eq!(served_with(Some(OsStr::new(""))).as_deref(), Some("2,3"));
    assert_eq!(served_with(Some(OsStr::new("1"))), None);
    let v3 = ["contract=3", "scope=relaunch,upgrade"];
    assert!(refused_before_v3(false, &v3));
    assert!(!refused_before_v3(true, &v3), "the serving aterm reads it");
    assert!(!refused_before_v3(false, &["ttl=5000"]), "a v2 lease");
    assert!(!refused_before_v3(false, &[]), "a v2 claim");
}

/// A layout in a scratch prefix holding `store/grok-alab/<build>/bin/grok-alab`, complete
/// (`<build>.ready` as atpkg writes it) when `ready`.
fn store_with_build(
    root: &Path,
    build: u64,
    ready: bool,
) -> (atpkg::store::Layout, std::path::PathBuf) {
    let layout = atpkg::store::Layout {
        prefix: root.join("pkg"),
    };
    let dir = layout.build_dir("grok-alab", build);
    std::fs::create_dir_all(dir.join("bin")).expect("build dir");
    let exe = dir.join("bin").join("grok-alab");
    std::fs::write(&exe, b"#!/bin/sh\n").expect("exe");
    if ready {
        atpkg::store::mark_build_ready(&dir).expect("ready");
    }
    (layout, exe)
}

/// THE KERNEL RULES are codex-alab's, keyed on the roster: the peer that leads the
/// foreground and runs a build of the row's program proves `grok-alab@<pid>`; any other peer,
/// foreground or image does not, and neither does a holder off the roster or codex-alab's.
#[test]
fn only_the_rostered_peer_that_leads_the_foreground_on_its_build_proves() {
    let facts = |peer, foreground, image| crate::self_claim::Facts {
        peer,
        foreground,
        alab_image: image,
    };
    assert_eq!(
        proves("grok-alab@500", facts(Some(500), 500, true)),
        Some(500)
    );
    assert_eq!(proves("grok-alab@500", facts(Some(500), 500, false)), None);
    assert_eq!(proves("grok-alab@500", facts(Some(501), 500, true)), None);
    assert_eq!(proves("grok-alab@500", facts(Some(500), 499, true)), None);
    assert_eq!(proves("grok-alab@500", facts(None, 500, true)), None);
    assert_eq!(proves("grok-alab@500", facts(Some(500), -1, true)), None);
    assert_eq!(proves("codex-alab@500", facts(Some(500), 500, true)), None);
    assert_eq!(
        proves("kimi-alab@500", facts(Some(500), 500, true)),
        Some(500)
    );
    assert_eq!(
        proves("claude-code-alab@500", facts(Some(500), 500, true)),
        Some(500)
    );
    assert_eq!(
        proves("claude-code-alab@500", facts(Some(500), 500, false)),
        None
    );
    assert_eq!(proves("claude-alab@500", facts(Some(500), 500, true)), None);
    assert_eq!(proves("other-alab@500", facts(Some(500), 500, true)), None);
    let me = std::process::id();
    let proven = prove(
        &format!("grok-alab@{me}"),
        facts(Some(me), i32::try_from(me).expect("pid"), true),
    )
    .expect("this process proves its own claim");
    assert_eq!(proven.pid, me);
    assert!(proven.alive());
}

/// THE IMAGE RULE: a complete store build of the row's program proves; the same file in a
/// build that is not ready, in another program's store, or anywhere else does not; a dev
/// link's checkout proves.
#[test]
fn only_a_complete_store_build_or_the_dev_link_of_the_rows_program_proves() {
    let dir = aterm_tempfile::TempDir::new().expect("dir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical");
    let (layout, exe) = store_with_build(&root, 412, true);
    assert!(image_in(&layout, grok_alab(), &exe), "positive control");

    let (unready, unready_exe) = store_with_build(&root.join("b"), 413, false);
    assert!(!image_in(&unready, grok_alab(), &unready_exe), "not ready");

    let other = layout.build_dir("grok", 7);
    std::fs::create_dir_all(other.join("bin")).expect("dir");
    let vendor = other.join("bin").join("grok");
    std::fs::write(&vendor, b"x").expect("vendor");
    atpkg::store::mark_build_ready(&other).expect("ready");
    assert!(
        !image_in(&layout, grok_alab(), &vendor),
        "the vendor's store is not the harness's"
    );

    let stray = root.join("stray-grok-alab");
    std::fs::write(&stray, b"x").expect("stray");
    assert!(
        !image_in(&layout, grok_alab(), &stray),
        "a file named like the harness"
    );
    assert!(
        !image_in(&layout, grok_alab(), &root.join("missing")),
        "nothing there"
    );

    let checkout = root.join("checkout");
    let built = checkout.join("target").join("release").join("grok-alab");
    std::fs::create_dir_all(built.parent().expect("dir")).expect("dir");
    std::fs::write(&built, b"x").expect("built");
    assert!(!image_in(&layout, grok_alab(), &built), "not linked yet");
    atpkg::link(
        &layout,
        "grok-alab",
        &checkout,
        &[std::path::PathBuf::from("target/release/grok-alab")],
    )
    .expect("dev link");
    assert!(
        image_in(&layout, grok_alab(), &built),
        "the dev link's checkout"
    );
    assert!(
        !image_in(&layout, grok_alab(), &stray),
        "still nothing outside it"
    );
}

/// claude-code-alab's IMAGE (`docs/DESIGN-harness-repos-2026-10-24.md` §11 D19, D53): its tool is
/// `claude-alab`, and the rule reads the package's store, never the tool's name. A complete
/// build of `claude-code-alab` proves its row; the vendor's own store, another harness's build
/// and its build read under another row do not.
#[test]
fn claude_code_alabs_image_is_its_packages_build_whatever_its_tool_is_named() {
    let row = contract::row_named("claude-code-alab").expect("rostered");
    let dir = aterm_tempfile::TempDir::new().expect("dir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical");
    let layout = atpkg::store::Layout {
        prefix: root.join("pkg"),
    };
    let build = |program: &str, id: u64, tool: &str, ready: bool| {
        let dir = layout.build_dir(program, id);
        std::fs::create_dir_all(dir.join("bin")).expect("build dir");
        let exe = dir.join("bin").join(tool);
        std::fs::write(&exe, b"#!/bin/sh\n").expect("exe");
        if ready {
            atpkg::store::mark_build_ready(&dir).expect("ready");
        }
        exe
    };
    let tool = build("claude-code-alab", 14275, "claude-alab", true);
    assert!(image_in(&layout, row, &tool), "its package's build");
    assert!(
        !image_in(&layout, grok_alab(), &tool),
        "one row's build proves no other row"
    );
    let vendor = build("claude", 2_001_310, "claude", true);
    assert!(
        !image_in(&layout, row, &vendor),
        "Claude Code's own store is not the harness's"
    );
    let other = build("grok-alab", 412, "claude-alab", true);
    assert!(
        !image_in(&layout, row, &other),
        "a tool of its name in another harness's store"
    );
    let unready = build("claude-code-alab", 14276, "claude-alab", false);
    assert!(!image_in(&layout, row, &unready), "a build not ready");
}
