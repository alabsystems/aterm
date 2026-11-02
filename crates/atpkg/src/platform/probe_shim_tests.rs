// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Inert producer/descriptor controls. No file made here is executed.

use super::*;
use crate::platform::{
    sh_shim_content_managed, sh_shim_content_routed, sh_shim_content_twin, sh_yolo_prelude,
};
use std::os::unix::fs::PermissionsExt as _;

fn image(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, [0xcf, 0xfa, 0xed, 0xfe, 0xff]).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn text(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    crate::lay::write_in_process(&crate::lay::Executable::new(path, body)).unwrap();
}

fn bridge(root: &Path) -> ProbeForwarder {
    let executable = root.join("inert-current-image");
    image(&executable);
    ProbeForwarder {
        executable,
        parent_pid: 4242,
    }
}

fn mains(snapshot: &ProbeShim) -> Vec<PathBuf> {
    snapshot
        .images
        .iter()
        .filter(|image| !image.helper)
        .map(|image| image.physical.clone())
        .collect()
}

fn full(program: &str, prefix: &Path, atpkg: Option<&Path>) -> String {
    let mut prelude = crate::platform::twin_prelude(
        program,
        prefix,
        atpkg,
        crate::selfupdate::verbs_of(program),
        &crate::yolo::policy_of(program),
    );
    prelude.push_str(&crate::launch_tap::sh_prelude(program, atpkg));
    prelude
}

#[test]
fn native_default_producers_remain_inspectable_without_an_unpinned_launch_route() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    let prefix = root.join("pkg");
    let target = prefix.join("store/codex/1/bin/codex");
    let wrapper = prefix.join("bin/codex");
    let helper = root.join("app/aterm");
    image(&target);
    image(&helper);
    let prelude = crate::stub::with_current_exe(&helper, || full("codex", &prefix, Some(&helper)));
    let body = sh_shim_content_twin(&target, &crate::shim_env::ShimEnv::NONE, None, &prelude);
    assert!(body.contains(crate::native_defaults::VERB));
    let captured = probe_shim(&wrapper, &body, &bridge(&root)).unwrap();
    assert_eq!(mains(&captured), [target]);
    assert!(!captured.body.contains(crate::native_defaults::VERB));
    assert!(captured.images.iter().all(|image| image.physical != helper));
}

#[test]
fn a_full_factory_snapshot_keeps_policy_exports_tap_and_published_image_identity() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    let prefix = root.join("person's prefix");
    let wrapper = prefix.join("agents/claude");
    let a = prefix.join("store/claude/1/bin/claude");
    let b = prefix.join("store/claude/2/bin/claude");
    image(&a);
    image(&b);
    let atpkg = prefix.join("libexec/atpkg");
    image(&atpkg);
    let tap = atpkg.with_file_name("aterm");
    let tap_a = root.join("tap-a");
    let tap_b = root.join("tap-b");
    image(&tap_a);
    image(&tap_b);
    std::os::unix::fs::symlink(&tap_a, &tap).unwrap();
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    let prelude = full("claude", &prefix, Some(&atpkg));
    let file = crate::platform::twin_executable_to_env(&wrapper, &a, &env, &prelude).unwrap();
    std::fs::create_dir_all(wrapper.parent().unwrap()).unwrap();
    crate::lay::write_in_process(&file).unwrap();
    let snapshot = probe_shim(
        &wrapper,
        std::str::from_utf8(&file.body).unwrap(),
        &bridge(&root),
    )
    .unwrap();
    assert_eq!(mains(&snapshot), [std::fs::canonicalize(&a).unwrap()]);
    assert!(snapshot.body.contains(&sh_yolo_prelude(
        "claude",
        &crate::yolo::policy_of("claude"),
        crate::selfupdate::verbs_of("claude")
    )));
    assert!(snapshot.body.contains("export DISABLE_AUTOUPDATER='1'"));
    assert!(
        snapshot.body.contains("__atpkg_probe_parent=$$; exec "),
        "tap's bridge has the shell as its parent"
    );
    assert!(
        snapshot
            .images
            .iter()
            .any(|image| image.helper && image.physical == tap_a && image.argv0 == tap)
    );
    let next = crate::platform::twin_executable_to_env(&wrapper, &b, &env, &prelude).unwrap();
    crate::lay::write_in_process(&next).unwrap();
    crate::platform::atomic_symlink(&tap_b, &tap).unwrap();
    assert_eq!(mains(&snapshot), [a]);
    assert!(
        !snapshot
            .images
            .iter()
            .any(|image| image.physical == b || image.physical == tap_b)
    );
    assert!(
        snapshot
            .body
            .contains("'harness' 'tap' '--launch' '--' \"$@\"")
    );
}

#[test]
fn producer_context_survives_a_differently_named_wrapper_and_prefix_alias() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    let prefix = root.join("pkg");
    let target = prefix.join("store/claude/1/bin/claude");
    image(&target);
    let wrapper = prefix.join("agents/claude");
    let body = sh_shim_content_twin(
        &target,
        &crate::shim_env::ShimEnv::NONE,
        None,
        &full("claude", &prefix, None),
    );
    text(&wrapper, &body);
    let alias_prefix = root.join("prefix-alias");
    std::os::unix::fs::symlink(&prefix, &alias_prefix).unwrap();
    let alias = alias_prefix.join("agents/another-name");
    std::os::unix::fs::symlink(&wrapper, &alias).unwrap();
    let snapshot = probe_shim(&alias, &body, &bridge(&root)).unwrap();
    assert_eq!(mains(&snapshot), [target]);
}

#[test]
fn grok_snapshots_live_ready_development_and_symlink_handoffs_with_vendor_fallback() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    let prefix = root.join("pkg");
    let layout = crate::store::Layout {
        prefix: prefix.clone(),
    };
    let vendor = prefix.join("store/grok/1/bin/grok");
    image(&vendor);
    let wrapper = prefix.join("agents/grok");
    let body = sh_shim_content_twin(
        &vendor,
        &crate::shim_env::ShimEnv::NONE,
        None,
        &full("grok", &prefix, None),
    );
    text(&wrapper, &body);
    let forwarder = bridge(&root);
    let absent = probe_shim(&wrapper, &body, &forwarder).unwrap();
    assert_eq!(
        mains(&absent),
        std::slice::from_ref(&vendor),
        "known absent harness keeps ordinary Grok functional"
    );

    let harness = prefix.join("store/grok-alab/7/bin/grok-alab");
    image(&harness);
    let ready = prefix.join("store/grok-alab/7.ready");
    std::fs::write(&ready, b"inert ready record").unwrap();
    let shim = prefix.join("bin/grok-alab");
    text(
        &shim,
        &sh_shim_content_twin(&harness, &crate::shim_env::ShimEnv::NONE, None, ""),
    );
    let captured = probe_shim(&wrapper, &body, &forwarder).unwrap();
    let selected = mains(&captured);
    assert!(selected.contains(&vendor) && selected.contains(&harness));
    assert!(captured.body.contains("${ATERM_NO_HARNESS-}"));
    assert!(captured.body.contains("'run' '--' \"$@\""));
    let replacement = prefix.join("store/grok-alab/8/bin/grok-alab");
    image(&replacement);
    std::fs::write(
        prefix.join("store/grok-alab/8.ready"),
        b"inert ready record",
    )
    .unwrap();
    text(
        &shim,
        &sh_shim_content_twin(&replacement, &crate::shim_env::ShimEnv::NONE, None, ""),
    );
    assert!(
        !captured
            .images
            .iter()
            .any(|image| image.physical == replacement)
    );
    assert!(mains(&probe_shim(&wrapper, &body, &forwarder).unwrap()).contains(&replacement));

    std::fs::remove_file(prefix.join("store/grok-alab/8.ready")).unwrap();
    assert_eq!(
        mains(&probe_shim(&wrapper, &body, &forwarder).unwrap()),
        std::slice::from_ref(&vendor)
    );
    let development = root.join("checkout/bin/grok-alab");
    image(&development);
    text(
        &shim,
        &sh_shim_content_twin(&development, &crate::shim_env::ShimEnv::NONE, None, ""),
    );
    let marker = layout.link_marker("grok-alab");
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, b"inert development marker").unwrap();
    assert!(mains(&probe_shim(&wrapper, &body, &forwarder).unwrap()).contains(&development));
    std::fs::remove_file(&marker).unwrap();
    std::fs::remove_file(&shim).unwrap();
    std::os::unix::fs::symlink(&development, &shim).unwrap();
    let linked = probe_shim(&wrapper, &body, &forwarder).unwrap();
    assert!(
        linked
            .images
            .iter()
            .any(|image| !image.helper && image.physical == development && image.argv0 == shim)
    );
    std::fs::remove_file(&shim).unwrap();
    text(&shim, "#!/bin/sh\nexit 70\n");
    assert_eq!(
        mains(&probe_shim(&wrapper, &body, &forwarder).unwrap()),
        [vendor]
    );
}

#[test]
fn managed_entry_selection_is_bound_with_its_own_argv0_and_fallback_metadata() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    let prefix = root.join("pkg");
    let wrapper = prefix.join("bin/codex");
    let a = prefix.join("store/codex/1/bin/codex");
    let b = prefix.join("store/codex/2/bin/codex");
    image(&a);
    image(&b);
    let entry = crate::activate::exec_dir(
        &crate::store::Layout {
            prefix: prefix.clone(),
        },
        "codex",
    )
    .join("codex");
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&b, &entry).unwrap();
    let body = sh_shim_content_managed(&a, &crate::shim_env::ShimEnv::NONE, &entry, "");
    let forwarder = bridge(&root);
    let captured = probe_shim(&wrapper, &body, &forwarder).unwrap();
    assert_eq!(mains(&captured), std::slice::from_ref(&b));
    assert!(
        captured
            .images
            .iter()
            .any(|image| image.physical == b && image.argv0 == entry)
    );
    crate::platform::atomic_symlink(&a, &entry).unwrap();
    assert_eq!(mains(&captured), [b]);
    std::fs::remove_file(&entry).unwrap();
    let fallback = probe_shim(&wrapper, &body, &forwarder).unwrap();
    assert_eq!(mains(&fallback), [a]);
    assert!(
        fallback
            .body
            .contains("export ATPKG_MANAGED_LAUNCH_PID=\"$$\"")
    );
    assert!(
        fallback
            .body
            .contains("export ATPKG_MANAGED_LAUNCH_TARGET=")
    );
}

#[test]
fn a_compatibility_forward_and_its_plain_fallback_keep_the_factory_guard_decision() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    let prefix = root.join("pkg");
    let wrapper = prefix.join("bin/tippy");
    let target = prefix.join("store/trust/42/bin/tippy");
    let route = prefix.join("compat/trust/42/bin/tippy");
    image(&target);
    image(&route);
    let marker = crate::compat::root_marker(&prefix.join("compat/trust/42"));
    std::fs::write(&marker, b"inert compatibility marker").unwrap();
    let body = sh_shim_content_routed(&target, &crate::shim_env::ShimEnv::NONE, Some(&route));
    let forwarder = bridge(&root);
    let captured = probe_shim(&wrapper, &body, &forwarder).unwrap();
    assert_eq!(mains(&captured), [target.clone(), route]);
    std::fs::remove_file(marker).unwrap();
    assert_eq!(
        mains(&probe_shim(&wrapper, &body, &forwarder).unwrap()),
        [target]
    );
}

#[test]
fn unknown_preludes_incomplete_metadata_and_recursive_wrappers_refuse() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    let wrapper = root.join("agents/claude");
    let target = root.join("store/claude/1/bin/claude");
    image(&target);
    let forwarder = bridge(&root);
    let direct = sh_shim_content_twin(&target, &crate::shim_env::ShimEnv::NONE, None, "");
    assert!(probe_shim(&wrapper, &direct, &forwarder).is_some());
    let unknown = sh_shim_content_twin(
        &target,
        &crate::shim_env::ShimEnv::NONE,
        None,
        "exec dynamically-selected\n",
    );
    assert!(probe_shim(&wrapper, &unknown, &forwarder).is_none());
    let missing = sh_shim_content_twin(
        &target.with_file_name("missing"),
        &crate::shim_env::ShimEnv::NONE,
        None,
        "",
    );
    assert!(probe_shim(&wrapper, &missing, &forwarder).is_none());
    let recursive = sh_shim_content_twin(&wrapper, &crate::shim_env::ShimEnv::NONE, None, "");
    text(&wrapper, &recursive);
    assert!(probe_shim(&wrapper, &recursive, &forwarder).is_none());
}
