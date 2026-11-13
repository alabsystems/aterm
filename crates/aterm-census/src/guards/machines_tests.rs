// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::tests::{run, spots};
use super::*;

#[test]
fn only_aterm_machines_names_a_root_tool_or_reaches_its_agent() {
    let outside = "fn a() { spawnfd::command(\"/usr/bin/pmset\").args([\"-a\", \"sleep\", \"0\"]); }\n\
                   // pmset and systemsetup in a comment say nothing\n\
                   const DROP_IN: &str = \"/etc/sudoers.d/aterm\";\n\
                   fn b() { let argv = [\"/usr/libexec/ApplicationFirewall/socketfilterfw\", \"--setglobalstate\", \"on\"]; }\n\
                   fn c() { aterm_machines::agent::main(&args, &sys); }\n\
                   use aterm_machines::{agent, cli};\n\
                   fn d() -> ExitCode { aterm_machines::cli::main_entry(forwarded) }\n\
                   fn e() { let s = \"systemsetup -getremotelogin\"; }\n\
                   fn f() { let s = \"softwareupdate --install-rosetta\"; }\n\
                   #[cfg(test)]\nmod tests {\n    const T: &str = \"/usr/sbin/installer\";\n}\n";
    let found = run(
        &MACHINES_ROOT_DOOR,
        &[
            ("crates/aterm-cli/src/lib.rs", outside),
            (
                "crates/aterm-machines/src/items/power.rs",
                "const PMSET: &str = \"/usr/bin/pmset\";\nfn a() { crate::agent::main(a, s); }\n",
            ),
            (
                "crates/aterm-machines-extra/src/lib.rs",
                "const V: &str = \"/usr/sbin/visudo\";\n",
            ),
            (
                "crates/aterm-gui/tests/updates.rs",
                "const S: &str = \"/usr/sbin/softwareupdate\";\n",
            ),
            (
                "apps/aterm-mac/src/helper.rs",
                "fn a() { run(\"/usr/sbin/installer\", &[\"-pkg\", p]); }\n",
            ),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "apps/aterm-mac/src/helper.rs:1",
            "crates/aterm-cli/src/lib.rs:1",
            "crates/aterm-cli/src/lib.rs:3",
            "crates/aterm-cli/src/lib.rs:4",
            "crates/aterm-cli/src/lib.rs:5",
            "crates/aterm-cli/src/lib.rs:6",
            "crates/aterm-cli/src/lib.rs:8",
            "crates/aterm-machines-extra/src/lib.rs:1",
        ],
        "{found:#?}"
    );
}

/// The admitted-name rule's two whole path lines in aterm-types, and the lab lane's guest rule
/// inside `sudo_asks_script` alone. NEGATIVE CONTROLS: the same path in another aterm-types
/// line or another file, and the lane's tools outside that function, are findings.
#[test]
fn the_rule_paths_and_the_lanes_guest_rule_alone_name_sudoers() {
    let types = "pub const ADMITTED_NAME_RULE: &str = \"/private/etc/sudoers.d/aterm-admitted-name\";\n\
                 pub const ADMITTED_NAME_RULE_REFUSED: &str =\n    \"/private/etc/sudoers.d/.aterm-admitted-name.refused\";\n\
                 pub const OTHER: &str = \"/private/etc/sudoers.d/other\";\n";
    let lane = "fn sudo_asks_script() -> String {\n    \"f=/private/etc/sudoers.d/.x; /usr/sbin/visudo -c -f $f\".to_owned()\n}\n\
                fn other() { let v = \"/usr/sbin/visudo\"; }\n";
    let found = run(
        &MACHINES_ROOT_DOOR,
        &[
            (RULE_PATHS.0, types),
            (LANE_GUEST_RULE.0, lane),
            ("crates/aterm-gui/src/clt_install.rs", types),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-gui/src/clt_install.rs:1",
            "crates/aterm-gui/src/clt_install.rs:3",
            "crates/aterm-gui/src/clt_install.rs:4",
            "crates/aterm-lab/src/mac_nux.rs:4",
            "crates/aterm-types/src/macos_setup.rs:3",
            "crates/aterm-types/src/macos_setup.rs:4",
        ],
        "{found:#?}"
    );
}

#[test]
fn only_the_rosetta_install_argv_in_atpkgs_platform_file_names_softwareupdate() {
    let rel = ROSETTA_INSTALL.0;
    let admitted = "/// The install.\n\
                    const ROSETTA_INSTALL: [&str; 3] = [\n\
                    \x20   \"/usr/sbin/softwareupdate\",\n\
                    \x20   \"--install-rosetta\",\n\
                    \x20   \"--agree-to-license\",\n\
                    ];\n\
                    const ONE_LINE: [&str; 3] = [\"/usr/sbin/softwareupdate\", \"--install-rosetta\", \"--agree-to-license\"];\n\
                    // a comment naming /usr/sbin/softwareupdate says nothing\n";
    let near_misses = "fn a() { let mut c = spawnfd::command(\"/usr/sbin/softwareupdate\"); }\n\
                       const ALL: [&str; 3] = [\"/usr/sbin/softwareupdate\", \"--install\", \"--all\"];\n\
                       const MORE: [&str; 4] = [\"/usr/sbin/softwareupdate\", \"--install-rosetta\", \"--agree-to-license\", \"--restart\"];\n\
                       const LESS: [&str; 2] = [\"/usr/sbin/softwareupdate\", \"--install-rosetta\"];\n\
                       const SUDO: [&str; 4] = [\"/usr/bin/sudo\", \"/usr/sbin/softwareupdate\", \"--install-rosetta\", \"--agree-to-license\"];\n\
                       fn b() { run(\"/usr/sbin/softwareupdate\", \"--install-rosetta\", \"--agree-to-license\"); }\n\
                       const TWO: [&str; 3] = [\"/usr/sbin/softwareupdate\", \"--install-rosetta\", \"--agree-to-license\"]; const I: &str = \"/usr/sbin/installer\";\n\
                       const TWICE: [&str; 3] = [\"/usr/sbin/softwareupdate\", \"--install-rosetta\", \"--agree-to-license\"]; const S: &str = \"/usr/sbin/softwareupdate\";\n\
                       const SPACED: [&str; 3] = [\"/usr/sbin/softwareupdate\", \"--install-rosetta \", \"--agree-to-license\"];\n";
    assert!(
        run(&MACHINES_ROOT_DOOR, &[(rel, admitted)]).is_empty(),
        "the admitted argv, wrapped and on one line"
    );
    let found = run(
        &MACHINES_ROOT_DOOR,
        &[
            (rel, &format!("{admitted}{near_misses}")),
            ("crates/atpkg/src/machine.rs", admitted),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/atpkg/src/machine.rs:3",
            "crates/atpkg/src/machine.rs:7",
            "crates/atpkg/src/platform/unix.rs:9",
            "crates/atpkg/src/platform/unix.rs:10",
            "crates/atpkg/src/platform/unix.rs:11",
            "crates/atpkg/src/platform/unix.rs:12",
            "crates/atpkg/src/platform/unix.rs:13",
            "crates/atpkg/src/platform/unix.rs:14",
            "crates/atpkg/src/platform/unix.rs:15",
            "crates/atpkg/src/platform/unix.rs:16",
            "crates/atpkg/src/platform/unix.rs:17",
        ],
        "{found:#?}"
    );
    assert!(
        found
            .iter()
            .filter(|f| f.detail.contains("softwareupdate"))
            .all(|f| f
                .detail
                .contains("one execution exception is atpkg's Rosetta 2 install")),
        "a refused spelling names the admitted one: {found:#?}"
    );
}

#[test]
fn the_installer_probe_only_names_softwareupdate_as_a_closed_path_identity() {
    let identity = "fn is_softwareupdate(path: &::std::path::Path) -> bool { path == ::std::path::Path::new(\"/usr/sbin/softwareupdate\") }\n";
    let wrapped = "fn is_softwareupdate(path: &::std::path::Path) -> bool {\n\
                   path == ::std::path::Path::new(\n\
                   \"/usr/sbin/softwareupdate\"\n\
                   )\n\
                   }\n";
    for allowed in [identity, wrapped] {
        assert!(
            run(&MACHINES_ROOT_DOOR, &[(INSTALLER_PROBE, allowed)]).is_empty(),
            "a complete boolean identity, regardless of wrapping: {allowed}"
        );
    }
    for refused in [
        "fn is_softwareupdate(path: &::std::path::Path) -> bool { let tool = ::std::path::Path::new(\"/usr/sbin/softwareupdate\"); path == tool }\n",
        "fn is_softwareupdate(path: &::std::path::Path) -> bool { spawnfd::command(\"/usr/sbin/softwareupdate\"); true }\n",
        "fn is_softwareupdate(path: &::std::path::Path) -> bool { extra(); path == ::std::path::Path::new(\"/usr/sbin/softwareupdate\") }\n",
        "fn is_softwareupdate(path: &::std::path::Path) -> bool { path != ::std::path::Path::new(\"/usr/sbin/softwareupdate\") }\n",
        "fn is_softwareupdate(path: &::std::path::Path) -> &::std::path::Path { ::std::path::Path::new(\"/usr/sbin/softwareupdate\") }\n",
        "fn is_softwareupdate(path: &::std::path::Path) -> bool { path == ::std::path::Path::new(\"/usr/bin/pmset\") }\n",
        "fn is_softwareupdate(path: &::std::path::Path) -> bool { path == ::std::path::Path::new(\"/usr/sbin/softwareupdate\") } const TOOL: &str = \"/usr/sbin/softwareupdate\";\n",
    ] {
        let found = run(&MACHINES_ROOT_DOOR, &[(INSTALLER_PROBE, refused)]);
        assert!(
            !found.is_empty(),
            "an executable path must not escape: {refused}"
        );
    }
    assert!(
        !run(
            &MACHINES_ROOT_DOOR,
            &[("crates/aterm-gui/src/other.rs", identity)]
        )
        .is_empty(),
        "the read-only probe does not open other files"
    );
}

#[test]
fn an_alias_with_an_executing_constructor_is_not_a_path_identity() {
    let source = "use crate::ExecutingPath as Path;\n\
                  #[derive(PartialEq)] struct ExecutingPath;\n\
                  impl ExecutingPath { fn new(executable: &str) -> &'static Self { let _ = spawnfd::command(executable).status(); &ExecutingPath } }\n\
                  fn is_softwareupdate(path: &Path) -> bool { path == Path::new(\"/usr/sbin/softwareupdate\") }\n";
    let found = run(&MACHINES_ROOT_DOOR, &[(INSTALLER_PROBE, source)]);
    assert_eq!(spots(&found), [format!("{INSTALLER_PROBE}:4")]);
}

#[test]
fn the_installer_probe_compares_both_apple_installers_without_exposing_either() {
    let identity = concat!(
        "fn is_install_process(path: &::std::path::Path) -> bool {\n",
        "path == ::std::path::Path::new(\"/usr/sbin/softwareupdate\")\n",
        "|| path == ::std::path::Path::new(\"/usr/sbin/installer\")\n",
        "}\n",
    );
    let compact = identity.replace('\n', " ");
    let reversed = concat!(
        "fn is_install_process(path: &::std::path::Path) -> bool {\n",
        "path == ::std::path::Path::new(\"/usr/sbin/installer\")\n",
        "|| path == ::std::path::Path::new(\"/usr/sbin/softwareupdate\")\n",
        "}\n",
    );
    for allowed in [identity, compact.as_str(), reversed] {
        assert!(
            run(&MACHINES_ROOT_DOOR, &[(INSTALLER_PROBE, allowed)]).is_empty(),
            "only closed boolean comparisons: {allowed}"
        );
    }
    let binding = "fn is_install_process(path: &::std::path::Path) -> bool { let tool = ::std::path::Path::new(\"/usr/sbin/installer\"); path == tool }";
    let execution = "fn is_install_process(path: &::std::path::Path) -> bool { spawnfd::command(\"/usr/sbin/installer\"); true }";
    let returned = "fn is_install_process(path: &::std::path::Path) -> &::std::path::Path { ::std::path::Path::new(\"/usr/sbin/installer\") }";
    for refused in [
        binding.to_owned(),
        execution.to_owned(),
        returned.to_owned(),
        identity.replace("path ==", "path !="),
        identity.replace("||", "&&"),
        identity.replace("/usr/sbin/installer", "/usr/bin/pmset"),
        identity.replacen('{', "{ extra();", 1),
        identity.replace("\n}", " || true\n}"),
        format!("{compact} const TOOL: &str = \"/usr/sbin/softwareupdate\";"),
        format!("{compact} const TOOL: &str = \"/usr/sbin/installer\";"),
    ] {
        assert!(
            !run(&MACHINES_ROOT_DOOR, &[(INSTALLER_PROBE, &refused)]).is_empty(),
            "neither path can escape its closed identity: {refused}"
        );
    }
    assert!(
        !run(
            &MACHINES_ROOT_DOOR,
            &[("crates/aterm-gui/src/other.rs", identity)]
        )
        .is_empty()
    );
}

#[test]
fn admitted_tool_spellings_never_hide_agent_access_on_the_same_line() {
    for (rel, admitted) in [
        (
            INSTALLER_PROBE,
            "fn is_softwareupdate(path: &::std::path::Path) -> bool { path == ::std::path::Path::new(\"/usr/sbin/softwareupdate\") }",
        ),
        (
            INSTALLER_PROBE,
            "fn is_install_process(path: &::std::path::Path) -> bool { path == ::std::path::Path::new(\"/usr/sbin/softwareupdate\") || path == ::std::path::Path::new(\"/usr/sbin/installer\") }",
        ),
        (
            ROSETTA_INSTALL.0,
            "const ARGV: [&str; 3] = [\"/usr/sbin/softwareupdate\", \"--install-rosetta\", \"--agree-to-license\"];",
        ),
    ] {
        let source =
            format!("{admitted} fn forbidden() {{ aterm_machines::agent::main(a, s); }}\n");
        let found = run(&MACHINES_ROOT_DOOR, &[(rel, &source)]);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(found[0].detail.contains("reaches `aterm_machines::agent`"));
    }
}
