// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::unix::fs::{PermissionsExt as _, symlink};

fn header() -> &'static [u8] {
    if cfg!(target_os = "macos") {
        b"\xcf\xfa\xed\xfe\0\0\0\0"
    } else {
        b"\x7fELF\0\0\0\0"
    }
}

fn fixture() -> (aterm_tempfile::TempDir, crate::store::Layout, PathBuf) {
    let dir = aterm_tempfile::tempdir().unwrap();
    let prefix = std::fs::canonicalize(dir.path()).unwrap();
    let layout = crate::store::Layout { prefix };
    let target = layout
        .prefix
        .join("store/claude/1000002000001000319/bin/claude");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, header()).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(layout.prefix.join("agents")).unwrap();
    (dir, layout, target)
}

fn write(layout: &crate::store::Layout, body: &str) {
    std::fs::write(layout.prefix.join("agents/claude"), body).unwrap();
}

fn args() -> Vec<String> {
    vec!["--resume".into(), "conversation".into()]
}

/// The projection of the laid twin for [`args`], with no token judgment of the caller's.
fn project(layout: &crate::store::Layout) -> Result<NativeClaudeRestart, NativeProjectionRefusal> {
    native_claude_restart(layout, &args(), None)
}

/// The same, under the caller's judgment that this launch is off its vault token.
fn judged(layout: &crate::store::Layout) -> Result<NativeClaudeRestart, NativeProjectionRefusal> {
    native_claude_restart(layout, &args(), Some(TokenBlock::JudgedOff))
}

#[test]
fn canonical_factory_and_exact_direct_freeze_target_and_exports() {
    let (_dir, layout, target) = fixture();
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".into()]).unwrap();
    let prelude = super::super::twin_prelude(
        "claude",
        &layout.prefix,
        None,
        crate::selfupdate::verbs_of("claude"),
        &crate::yolo::policy_of("claude"),
    );
    write(
        &layout,
        &super::super::sh_shim_body(&target, &env, None, None, &prelude),
    );
    let selected = project(&layout).unwrap();
    assert_eq!(selected.target, target);
    assert_eq!(selected.env.entries(), env.entries());
    assert!(!selected.token_block);
    assert_eq!(
        selected.blocks,
        [
            TwinBlock::Head,
            TwinBlock::SelfUpdate,
            TwinBlock::NoHarness,
            TwinBlock::Posture,
            TwinBlock::Exports,
            TwinBlock::Exec
        ]
    );
    write(
        &layout,
        &format!(
            "#!/bin/sh\nexec {} \"$@\"\n",
            super::super::sh_shim_quote(&target)
        ),
    );
    let legacy = project(&layout).unwrap();
    assert_eq!(legacy.target, selected.target);
    assert_eq!(legacy.blocks, [TwinBlock::Head, TwinBlock::Exec]);
    for argv in [
        Vec::new(),
        vec!["--".into(), "--resume".into(), "conversation".into()],
        vec!["--yolo".into(), "--resume".into(), "conversation".into()],
    ] {
        assert_eq!(
            native_claude_restart(&layout, &argv, None).unwrap_err(),
            NativeProjectionRefusal::Posture
        );
    }
    let next = layout
        .prefix
        .join("store/claude/1000002000001000320/bin/claude");
    std::fs::create_dir_all(next.parent().unwrap()).unwrap();
    std::fs::write(&next, header()).unwrap();
    std::fs::set_permissions(&next, std::fs::Permissions::from_mode(0o755)).unwrap();
    write(
        &layout,
        &super::super::sh_shim_body(&next, &env, None, None, ""),
    );
    assert_eq!(project(&layout).unwrap().target, next);
    assert_eq!(
        selected.target, target,
        "a new twin cannot retarget captured data"
    );
    std::fs::write(&next, "#!/bin/sh\nexit 0\n").unwrap();
    assert_eq!(
        project(&layout).unwrap_err(),
        NativeProjectionRefusal::TargetNotNative,
        "a script is not the selected physical image"
    );
}

/// THE TOKEN BLOCK PROJECTS AS ITS AS-TYPED ARM, AND ONLY UNDER THE CALLER'S JUDGMENT: the twin
/// atpkg lays with the helper named (the self-update, `--no-harness`, `--yolo`, tap and renderer
/// blocks, the exports and the token route) projects to the same build and exports as the same
/// body without the token block, saying it carried one. CONTROLS: with no judgment of the
/// caller's the token block is refused as unjudged; one changed byte is an unknown producer; a
/// route line and a managed launch entry are refused as themselves; a missing resume, a defaulted
/// posture and `--no-harness` are refused as the posture; a store link that escapes the store is
/// refused as outside it. Projection starts no helper or vendor: the helper is producer data here.
#[test]
fn a_canonical_factory_with_its_helper_tap_and_token_spelling_is_projected_without_execution() {
    let (dir, layout, target) = fixture();
    let helper = layout.prefix.join("libexec/atpkg");
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".into()]).unwrap();
    let mut prelude = super::super::twin_prelude(
        "claude",
        &layout.prefix,
        Some(&helper),
        crate::selfupdate::verbs_of("claude"),
        &crate::yolo::policy_of("claude"),
    );
    prelude.push_str(&crate::launch_tap::sh_prelude("claude", Some(&helper)));
    let body = super::super::sh_shim_body(&target, &env, None, None, &prelude);
    write(&layout, &body);
    let plain = project(&layout).unwrap();
    assert_eq!(plain.target, target);
    assert_eq!(plain.env.entries(), env.entries());
    assert!(!plain.token_block);
    let token_body = crate::launch_tap::with_token_block("claude", body.clone(), Some(&helper));
    write(&layout, &token_body);
    let wrapper = layout.agent_shim(&crate::store::ToolName::new("claude").unwrap());
    assert!(
        super::super::shim_producer::recognize(&wrapper, &token_body)
            .unwrap()
            .token_route
    );
    let selected = judged(&layout).unwrap();
    assert_eq!(selected.target, plain.target);
    assert_eq!(selected.env.entries(), plain.env.entries());
    assert!(selected.token_block);
    let mut without = selected.blocks.clone();
    without.retain(|block| *block != TwinBlock::TokenRoute);
    assert_eq!(without, plain.blocks);
    assert_eq!(
        selected.blocks,
        [
            TwinBlock::Head,
            TwinBlock::SelfUpdate,
            TwinBlock::NoHarness,
            TwinBlock::Posture,
            TwinBlock::UsageTap,
            TwinBlock::Renderer,
            TwinBlock::Exports,
            TwinBlock::TokenRoute,
            TwinBlock::Exec
        ]
    );
    assert_eq!(
        project(&layout).unwrap_err(),
        NativeProjectionRefusal::TokenUnjudged
    );
    for argv in [
        vec!["conversation".to_string()],
        vec!["--yolo".into(), "--resume".into(), "conversation".into()],
        vec![
            "--no-harness".into(),
            "--permission-mode".into(),
            "default".into(),
            "--resume".into(),
            "conversation".into(),
        ],
    ] {
        assert_eq!(
            native_claude_restart(&layout, &argv, Some(TokenBlock::JudgedOff)).unwrap_err(),
            NativeProjectionRefusal::Posture,
            "{argv:?}"
        );
    }
    write(
        &layout,
        &token_body.replacen("#!/bin/sh\n", "#!/bin/sh\nexit 7\n", 1),
    );
    assert_eq!(
        judged(&layout).unwrap_err(),
        NativeProjectionRefusal::ProducerUnknown
    );
    for (route, launch, refusal) in [
        (
            Some(layout.prefix.join("compat/claude")),
            None,
            NativeProjectionRefusal::Route,
        ),
        (
            None,
            Some(layout.prefix.join("launch/claude")),
            NativeProjectionRefusal::Launch,
        ),
    ] {
        let routed = super::super::sh_shim_body(
            &target,
            &env,
            route.as_deref(),
            launch.as_deref(),
            &prelude,
        );
        write(
            &layout,
            &crate::launch_tap::with_token_block("claude", routed, Some(&helper)),
        );
        assert_eq!(judged(&layout).unwrap_err(), refusal);
    }
    let outside = dir.path().join("outside/claude");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, header()).unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).unwrap();
    write(&layout, &token_body);
    std::fs::remove_file(&target).unwrap();
    symlink(&outside, &target).unwrap();
    assert_eq!(
        judged(&layout).unwrap_err(),
        NativeProjectionRefusal::OutsideStore
    );
    assert!(outside.exists());
}

/// THE PROJECTION IS BOUND TO WHAT ATPKG ACTUALLY LAYS: a store-installed Claude Code build,
/// activated and reconciled by the real `activate::reconcile_agents` with the helper named
/// beside a live aterm, gets the twin every standard Mac carries (the self-update, `--no-harness`,
/// `--yolo`, usage tap and renderer blocks, its exports, the token route and the plain exec), and
/// that twin projects to its build under the caller's token judgment. Every block it laid has a
/// rule, and the projection accounts for every byte of it. CONTROLS: the same laid twin with no
/// judgment of the caller's is refused as unjudged, and once the build moves outside the store
/// (a checkout with no live link) it is refused as outside it.
#[test]
fn every_block_reconcile_lays_for_a_store_claude_has_a_projection_rule() {
    let (dir, layout, _) = fixture();
    let bundle = layout.prefix.join("bundle/MacOS");
    std::fs::create_dir_all(&bundle).unwrap();
    let live = bundle.join("aterm");
    std::fs::write(&live, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&live, std::fs::Permissions::from_mode(0o755)).unwrap();
    symlink("aterm", bundle.join("atpkg")).unwrap();
    let build = layout.build_dir("claude", 1000002000001000326);
    let target = build.join("bin/claude");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, header()).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    let claude = crate::store::ToolName::new("claude").unwrap();
    let env = crate::shim_env::ShimEnv::admit(&[
        "DISABLE_AUTOUPDATER=1".into(),
        "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1".into(),
    ])
    .unwrap();
    let laid = crate::stub::with_current_exe(&live, || {
        crate::activate::install_tools_env(
            &layout,
            &build,
            std::slice::from_ref(&claude),
            crate::activate::Aliases::Off,
            &env,
        )
        .unwrap();
        crate::activate::reconcile_agents(&layout);
        std::fs::read_to_string(layout.agent_shim(&claude)).unwrap()
    });
    assert!(laid.contains("token-run --auto"), "{laid}");
    assert!(laid.contains("harness tap --launch"), "{laid}");
    let selected = judged(&layout).unwrap();
    assert_eq!(selected.target, target);
    assert_eq!(selected.env.entries(), env.entries());
    assert!(selected.token_block);
    for block in &selected.blocks {
        assert!(
            !matches!(projection_rule(*block), ProjectionRule::Refused(_)),
            "{block:?}: a block the producer lays for a store build is refused"
        );
    }
    for block in [
        TwinBlock::Head,
        TwinBlock::SelfUpdate,
        TwinBlock::Posture,
        TwinBlock::UsageTap,
        TwinBlock::Renderer,
        TwinBlock::Exports,
        TwinBlock::TokenRoute,
        TwinBlock::Exec,
    ] {
        assert!(selected.blocks.contains(&block), "{block:?} in {laid}");
    }
    assert_eq!(
        project(&layout).unwrap_err(),
        NativeProjectionRefusal::TokenUnjudged
    );
    let outside = dir.path().join("checkout/target/debug/claude");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, header()).unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_file(&target).unwrap();
    symlink(&outside, &target).unwrap();
    assert_eq!(
        judged(&layout).unwrap_err(),
        NativeProjectionRefusal::OutsideStore
    );
}

/// EVERY BLOCK HAS ONE RULE AND EVERY REFUSAL ITS OWN WAIT WORD: the rule table is total over
/// [`TwinBlock::ALL`], refuses exactly the blocks that exec something other than the build, and
/// each [`NativeProjectionRefusal`] names a distinct `native-twin-<kind>` word with a reason.
#[test]
fn every_block_has_a_rule_and_every_refusal_its_own_word() {
    let refused: Vec<TwinBlock> = TwinBlock::ALL
        .into_iter()
        .filter(|block| matches!(projection_rule(*block), ProjectionRule::Refused(_)))
        .collect();
    assert_eq!(
        refused,
        [TwinBlock::BuildCache, TwinBlock::Route, TwinBlock::Launch]
    );
    let words: std::collections::BTreeSet<&str> = NativeProjectionRefusal::ALL
        .into_iter()
        .map(NativeProjectionRefusal::word)
        .collect();
    assert_eq!(words.len(), NativeProjectionRefusal::ALL.len());
    for refusal in NativeProjectionRefusal::ALL {
        assert!(refusal.word().starts_with("native-twin-"), "{refusal:?}");
        assert!(!refusal.why().is_empty());
    }
}

#[test]
fn unknown_routes_and_canonical_target_escape_are_refused() {
    let (dir, layout, target) = fixture();
    let direct =
        super::super::sh_shim_body(&target, &crate::shim_env::ShimEnv::NONE, None, None, "");
    write(&layout, &format!("{direct}echo unrecognized\n"));
    assert_eq!(
        project(&layout).unwrap_err(),
        NativeProjectionRefusal::ProducerUnknown
    );
    let foreign_prefix = layout.prefix.join("foreign-prefix");
    let foreign_prelude = super::super::twin_prelude(
        "claude",
        &foreign_prefix,
        None,
        crate::selfupdate::verbs_of("claude"),
        &crate::yolo::policy_of("claude"),
    );
    write(
        &layout,
        &super::super::sh_shim_body(
            &target,
            &crate::shim_env::ShimEnv::NONE,
            None,
            None,
            &foreign_prelude,
        ),
    );
    assert_eq!(
        project(&layout).unwrap_err(),
        NativeProjectionRefusal::ProducerUnknown
    );
    for (route, launch, refusal) in [
        (
            Some(layout.prefix.join("compat/claude")),
            None,
            NativeProjectionRefusal::Route,
        ),
        (
            None,
            Some(layout.prefix.join("launch/claude")),
            NativeProjectionRefusal::Launch,
        ),
    ] {
        write(
            &layout,
            &super::super::sh_shim_body(
                &target,
                &crate::shim_env::ShimEnv::NONE,
                route.as_deref(),
                launch.as_deref(),
                "",
            ),
        );
        assert_eq!(project(&layout).unwrap_err(), refusal);
    }
    let outside = dir.path().join("outside/claude");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, header()).unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).unwrap();
    write(&layout, &direct);
    std::fs::remove_file(&target).unwrap();
    symlink(&outside, &target).unwrap();
    assert_eq!(
        project(&layout).unwrap_err(),
        NativeProjectionRefusal::OutsideStore
    );
    assert!(outside.exists());
}

#[test]
fn a_live_claude_checkout_is_required_for_an_out_of_store_target() {
    let (dir, layout, _) = fixture();
    let checkout = dir.path().join("checkout");
    let relative = PathBuf::from("target/debug/claude");
    let target = checkout.join(&relative);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, header()).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    write(
        &layout,
        &super::super::sh_shim_body(&target, &crate::shim_env::ShimEnv::NONE, None, None, ""),
    );
    assert_eq!(
        project(&layout).unwrap_err(),
        NativeProjectionRefusal::OutsideStore
    );
    crate::linkmode::link(&layout, "claude", &checkout, &[relative]).unwrap();
    assert_eq!(
        project(&layout).unwrap().target,
        std::fs::canonicalize(&target).unwrap()
    );
    std::fs::remove_file(layout.link_marker("claude")).unwrap();
    assert_eq!(
        project(&layout).unwrap_err(),
        NativeProjectionRefusal::OutsideStore
    );
}

/// THE HARNESS HAND-OFF REFUSES ONLY WHAT ITS BLOCK WOULD HAND TO THE HARNESS: with a harness
/// declared to lead claude (here `claude-code-alab`, tool `claude-alab`), the projection reads
/// `bin/<tool>` exactly as the block's runtime arm does ([`super::super::probe_shim::harness_handoff`]).
/// No shim, a shim whose build is unfinished (no `<build>.ready`) or removed, a tombstone with no
/// exec line, a shim that is not executable, a target holding a quote and a target outside the
/// store with no link marker all run the build as typed, and project; a ready store build, a
/// dev-linked checkout's build and a link to an executable file are a live harness, refused as
/// `native-twin-harness`. With no harness leading claude the block holds nothing.
///
/// NEGATIVE CONTROL: with the rule refusing wherever `bin/<tool>` exists, the unfinished build's
/// shim refuses.
#[test]
fn the_harness_handoff_refuses_only_a_live_harness_build() {
    let (_dir, layout, _target) = fixture();
    let leader = || Some(("claude-code-alab".to_string(), "claude-alab".to_string()));
    let shim = layout.prefix.join("bin/claude-alab");
    let executable = |path: &std::path::Path, body: &[u8]| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let exec_line = |target: &std::path::Path| {
        format!(
            "#!/bin/sh\n# atpkg shim\nexec '{}' \"$@\"\n",
            target.display()
        )
    };
    assert_eq!(handoff_runs_vendor(&layout, None), Ok(()), "no leader");
    assert_eq!(handoff_runs_vendor(&layout, leader()), Ok(()), "no shim");

    let build = layout
        .prefix
        .join("store/claude-code-alab/1000/bin/claude-alab");
    executable(&build, header());
    executable(&shim, exec_line(&build).as_bytes());
    assert_eq!(
        handoff_runs_vendor(&layout, leader()),
        Ok(()),
        "an unfinished build: no 1000.ready"
    );
    let ready = layout.prefix.join("store/claude-code-alab/1000.ready");
    std::fs::write(&ready, b"ready\n").unwrap();
    assert_eq!(
        handoff_runs_vendor(&layout, leader()),
        Err(NativeProjectionRefusal::Harness),
        "a ready store build"
    );
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        handoff_runs_vendor(&layout, leader()),
        Ok(()),
        "a shim that is not executable"
    );
    executable(
        &shim,
        b"#!/bin/sh\necho 'claude-alab was removed' >&2\nexit 70\n",
    );
    assert_eq!(
        handoff_runs_vendor(&layout, leader()),
        Ok(()),
        "a tombstone"
    );
    executable(
        &shim,
        format!("#!/bin/sh\nexec '{}'\\''x' \"$@\"\n", build.display()).as_bytes(),
    );
    assert_eq!(handoff_runs_vendor(&layout, leader()), Ok(()), "a quote");
    executable(&shim, exec_line(&build).as_bytes());
    std::fs::remove_file(&build).unwrap();
    assert_eq!(
        handoff_runs_vendor(&layout, leader()),
        Ok(()),
        "a removed build"
    );

    let checkout = layout.prefix.join("checkout/target/debug/claude-alab");
    executable(&checkout, header());
    executable(&shim, exec_line(&checkout).as_bytes());
    assert_eq!(
        handoff_runs_vendor(&layout, leader()),
        Ok(()),
        "outside the store, not dev-linked"
    );
    let marker = layout.link_marker("claude-code-alab");
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, b"checkout\n").unwrap();
    assert_eq!(
        handoff_runs_vendor(&layout, leader()),
        Err(NativeProjectionRefusal::Harness),
        "a dev-linked checkout's build"
    );
    std::fs::remove_file(&marker).unwrap();
    std::fs::remove_file(&shim).unwrap();
    symlink(&checkout, &shim).unwrap();
    assert_eq!(
        handoff_runs_vendor(&layout, leader()),
        Err(NativeProjectionRefusal::Harness),
        "a link to an executable file"
    );
    std::fs::remove_file(&checkout).unwrap();
    assert_eq!(
        handoff_runs_vendor(&layout, leader()),
        Ok(()),
        "a dangling link"
    );
}
