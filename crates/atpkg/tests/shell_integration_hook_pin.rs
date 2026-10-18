// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The shell integration's live tests lay a hook in "hooks.rs's exact format" from a
//! golden fixture (`aterm-shell-integration` cannot depend on `atpkg`); this drift gate
//! asserts [`atpkg::hooks::hook_files`] still produces exactly that golden for the
//! fixture paths, so a change to the template fails HERE, naming the fixture to
//! regenerate, instead of letting the other crate test a hook atpkg no longer writes.

use std::path::Path;

const FIXTURE_AGENTS: &str = "/opt/aterm-si fixture/pkg/agents";
const FIXTURE_BIN: &str = "/opt/aterm-si fixture/pkg/bin";
const FIXTURE_REROUTE: &str = "/opt/aterm-si fixture/pkg/reroute";

const SI: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aterm-shell-integration/src/"
);

macro_rules! si_file {
    ($rel:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../aterm-shell-integration/src/",
            $rel
        ))
    };
}

#[test]
fn the_shell_integration_hook_goldens_are_what_hook_files_writes() {
    let files = atpkg::hooks::hook_files(
        Path::new(FIXTURE_BIN),
        Path::new(FIXTURE_AGENTS),
        Path::new(FIXTURE_REROUTE),
    );
    let body = |name: &str| -> String {
        files
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.clone())
            .unwrap_or_else(|| panic!("hook_files writes no {name}"))
    };
    let posix = si_file!("fixtures/atpkg-hook-posix.golden");
    let fish = si_file!("fixtures/atpkg-hook-fish.golden");
    assert_eq!(
        body(&format!("{}.zsh", atpkg::hooks::HOOK_BASENAME)),
        posix,
        "regenerate {SI}fixtures/atpkg-hook-posix.golden from hook_files({FIXTURE_BIN:?}, {FIXTURE_AGENTS:?}, {FIXTURE_REROUTE:?})"
    );
    assert_eq!(
        body(&format!("{}.bash", atpkg::hooks::HOOK_BASENAME)),
        posix,
        "the bash hook is the same POSIX body as the zsh one"
    );
    assert_eq!(
        body(&format!("{}.fish", atpkg::hooks::HOOK_BASENAME)),
        fish,
        "regenerate {SI}fixtures/atpkg-hook-fish.golden from hook_files({FIXTURE_BIN:?}, {FIXTURE_AGENTS:?}, {FIXTURE_REROUTE:?})"
    );
}
