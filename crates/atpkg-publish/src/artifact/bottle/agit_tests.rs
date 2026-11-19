// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const UPSTREAM: &str = "03b41523c576f26ad34085378717a0c1b287470f";
const FORK: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const VERSION: &str = "2.56.1.agit.1";

#[test]
fn agit_checks_the_selected_upstream_and_fork_release() {
    let target = crate::triple::AARCH64_APPLE_DARWIN;
    let (product, definition) = super::super::bottle_row("agit", target).unwrap();
    assert_eq!(product.repo, "agit");
    assert_eq!(definition.exposes, ["agit"]);
    assert!(matches!(
        definition.origin,
        crate::programs::BottleOrigin::Agit
    ));
    let mut origin = BuildOrigin {
        product: "agit".to_string(),
        upstream_version: "2.56.1".to_string(),
        upstream_commit: UPSTREAM.to_string(),
        fork_commit: FORK.to_string(),
        runtime_prefix: true,
    };
    assert!(origin.check(VERSION).is_ok());
    for version in ["2.56.1", "2.56.1.agit.0", "2.56.2.agit.1"] {
        assert!(origin.check(version).is_err(), "{version}");
    }
    origin.fork_commit = UPSTREAM.into();
    assert!(origin.check(VERSION).is_err());
    origin.fork_commit = "incomplete".into();
    assert!(origin.check(VERSION).is_err());
    origin.fork_commit = FORK.into();
    origin.runtime_prefix = false;
    assert!(origin.check(VERSION).is_err());
}

#[cfg(all(
    any(target_os = "macos", target_os = "linux"),
    any(target_arch = "aarch64", target_arch = "x86_64")
))]
#[test]
fn agit_refuses_a_script_before_running_any_packaged_command() {
    let target = Triple::host().unwrap();
    let temp = aterm_tempfile::tempdir().unwrap();
    let stage = temp.path();
    std::fs::create_dir_all(stage.join("bin")).unwrap();
    std::fs::write(stage.join("bin/agit"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(stage.join("COPYING"), "GNU GENERAL PUBLIC LICENSE").unwrap();
    let claim = aterm_json::json!({
        "product": "agit", "upstream_version": "2.56.1", "runtime_prefix": true,
        "upstream_commit": UPSTREAM, "fork_commit": FORK
    });
    std::fs::write(stage.join("BUILD-ORIGIN.json"), claim.to_string()).unwrap();
    let err = qualify(stage, VERSION, target).unwrap_err().to_string();
    assert!(err.contains("agit"), "{err}");
    assert!(err.contains("cannot read the architecture"), "{err}");
}
