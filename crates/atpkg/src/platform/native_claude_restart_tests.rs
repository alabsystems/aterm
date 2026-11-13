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
    let selected = native_claude_restart(&layout, &args()).unwrap();
    assert_eq!(selected.target, target);
    assert_eq!(selected.env.entries(), env.entries());
    write(
        &layout,
        &format!(
            "#!/bin/sh\nexec {} \"$@\"\n",
            super::super::sh_shim_quote(&target)
        ),
    );
    assert_eq!(
        native_claude_restart(&layout, &args()).unwrap().target,
        selected.target
    );
    assert!(native_claude_restart(&layout, &[]).is_err());
    assert!(
        native_claude_restart(
            &layout,
            &["--".into(), "--resume".into(), "conversation".into()]
        )
        .is_err()
    );
    assert!(
        native_claude_restart(
            &layout,
            &["--yolo".into(), "--resume".into(), "conversation".into()]
        )
        .is_err()
    );
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
    assert_eq!(
        native_claude_restart(&layout, &args()).unwrap().target,
        next
    );
    assert_eq!(
        selected.target, target,
        "a new twin cannot retarget captured data"
    );
    std::fs::write(&next, "#!/bin/sh\nexit 0\n").unwrap();
    assert!(
        native_claude_restart(&layout, &args()).is_err(),
        "a script is not the selected physical image"
    );
}

#[test]
fn a_canonical_factory_with_its_helper_and_tap_spelling_is_projected_without_execution() {
    let (_dir, layout, target) = fixture();
    // The helper is producer data here. Projection starts no helper or vendor.
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
    let selected = native_claude_restart(&layout, &args()).unwrap();
    assert_eq!(selected.target, target);
    assert_eq!(selected.env.entries(), env.entries());
    let token_body = crate::launch_tap::with_token_block("claude", body.clone(), Some(&helper));
    write(&layout, &token_body);
    let wrapper = layout.agent_shim(&crate::store::ToolName::new("claude").unwrap());
    assert!(
        super::super::shim_producer::recognize(&wrapper, &token_body)
            .unwrap()
            .token_route
    );
    assert!(native_claude_restart(&layout, &args()).is_err());
    write(
        &layout,
        &body.replacen("#!/bin/sh\n", "#!/bin/sh\nexit 7\n", 1),
    );
    assert!(native_claude_restart(&layout, &args()).is_err());
}

#[test]
fn unknown_routes_and_canonical_target_escape_are_refused() {
    let (dir, layout, target) = fixture();
    let direct =
        super::super::sh_shim_body(&target, &crate::shim_env::ShimEnv::NONE, None, None, "");
    write(&layout, &format!("{direct}echo unrecognized\n"));
    assert!(native_claude_restart(&layout, &args()).is_err());
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
    assert!(native_claude_restart(&layout, &args()).is_err());
    for (route, launch) in [
        (Some(layout.prefix.join("compat/claude")), None),
        (None, Some(layout.prefix.join("launch/claude"))),
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
        assert!(native_claude_restart(&layout, &args()).is_err());
    }
    let outside = dir.path().join("outside/claude");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, header()).unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).unwrap();
    write(&layout, &direct);
    std::fs::remove_file(&target).unwrap();
    symlink(&outside, &target).unwrap();
    assert!(native_claude_restart(&layout, &args()).is_err());
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
    assert!(native_claude_restart(&layout, &args()).is_err());
    crate::linkmode::link(&layout, "claude", &checkout, &[relative]).unwrap();
    assert_eq!(
        native_claude_restart(&layout, &args()).unwrap().target,
        std::fs::canonicalize(&target).unwrap()
    );
    std::fs::remove_file(layout.link_marker("claude")).unwrap();
    assert!(native_claude_restart(&layout, &args()).is_err());
}
