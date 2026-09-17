// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The shell integration re-sources atpkg's hook LIVE (2026-09-16, owner ask: "all the
//! latest and best MUST WORK IN THE SAME TAB with live update") — so the two crates
//! now share two facts that nothing else ties together, and this file pins both from
//! atpkg's side (`aterm-shell-integration` cannot depend on `atpkg`):
//!
//! 1. THE HOOK PATH SPELLING. Each script hard-codes `~/.aterm/shell.d/00-atpkg.<ext>`
//!    as the file it re-sources when `$ATPKG_AGENTS` is missing or the copy on disk
//!    moved. It must be `hooks::HOOK_BASENAME` under the directory `hooks::refresh`
//!    writes, or a renamed hook would silently freeze every already-open tab again.
//! 2. THE HOOK BODY. The live tests over there lay a hook in "hooks.rs's exact format"
//!    from a golden fixture; this asserts [`atpkg::hooks::hook_files`] still produces
//!    exactly that golden for the fixture paths, so a change to the template fails
//!    HERE, naming the fixture to regenerate, instead of letting the other crate test
//!    a hook atpkg no longer writes.

use std::path::Path;

const FIXTURE_AGENTS: &str = "/opt/aterm-si fixture/pkg/agents";
const FIXTURE_BIN: &str = "/opt/aterm-si fixture/pkg/bin";

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
fn every_shell_integration_script_re_sources_the_hook_atpkg_actually_writes() {
    for (ext, script) in [
        ("zsh", si_file!("scripts/aterm_shell_integration.zsh")),
        ("bash", si_file!("scripts/aterm_shell_integration.bash")),
        ("fish", si_file!("scripts/aterm_shell_integration.fish")),
        ("ps1", si_file!("scripts/aterm_shell_integration.ps1")),
    ] {
        let spelled = format!(".aterm/shell.d/{}.{ext}", atpkg::hooks::HOOK_BASENAME);
        assert!(
            script.contains(&spelled),
            "{SI}scripts/aterm_shell_integration.{ext} must name the hook atpkg writes ({spelled:?}) — \
             the live re-source keys on it"
        );
        // And it is the hook file that pass writes, not merely a string that looks like one.
        let written: Vec<String> =
            atpkg::hooks::hook_files(Path::new(FIXTURE_BIN), Path::new(FIXTURE_AGENTS))
                .into_iter()
                .map(|(name, _)| name)
                .collect();
        assert!(
            written.contains(&format!("{}.{ext}", atpkg::hooks::HOOK_BASENAME)),
            "hook_files writes no .{ext} hook: {written:?}"
        );
    }
}

#[test]
fn the_shell_integration_hook_goldens_are_what_hook_files_writes() {
    let files = atpkg::hooks::hook_files(Path::new(FIXTURE_BIN), Path::new(FIXTURE_AGENTS));
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
        "regenerate {SI}fixtures/atpkg-hook-posix.golden from hook_files({FIXTURE_BIN:?}, {FIXTURE_AGENTS:?})"
    );
    assert_eq!(
        body(&format!("{}.bash", atpkg::hooks::HOOK_BASENAME)),
        posix,
        "the bash hook is the same POSIX body as the zsh one"
    );
    assert_eq!(
        body(&format!("{}.fish", atpkg::hooks::HOOK_BASENAME)),
        fish,
        "regenerate {SI}fixtures/atpkg-hook-fish.golden from hook_files({FIXTURE_BIN:?}, {FIXTURE_AGENTS:?})"
    );
}
