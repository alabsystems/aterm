// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::tests::{run, spots};
use super::*;

fn waiver(machine: &str, action: &str) -> String {
    format!(
        "#[cfg_attr(\n    any(test, debug_assertions),\n    \
         aterm_spec_anchor::spec_unmodeled(machine = \"{machine}\", action = \"{action}\", \
         reason = \"r (x)\")\n)]\npub fn m_environment() {{}}\n"
    )
}

#[test]
fn an_action_anchored_only_where_the_closure_cannot_link_it_is_named() {
    let lib = format!(
        "{}\n#[cfg(test)]\nmod tests {{\n    #[aterm_spec_anchor::spec_unmodeled(\
         machine = \"m\", action = \"B\", reason = \"r\")]\n    fn carrier() {{}}\n    \
         #[aterm_spec_anchor::spec_unmodeled(machine = \"M\", action = \"Shipped\", \
         reason = \"r\")]\n    fn duplicate() {{}}\n}}\n#[cfg(test)]\nmod rig;\nmod seam;\n\
         // aterm_spec_anchor::refines(machine = \"M\", action = \"Z\") in a comment\n",
        waiver("M", "Shipped")
    );
    let gated = "#[cfg_attr(test, aterm_spec_anchor::refines(machine = \"M\", action = \
                 \"G1\"))]\nfn a() {}\n#[cfg_attr(\n    all(test, unix),\n    \
                 aterm_spec_anchor::refines(machine = \"M\", action = \"G2\")\n)]\nfn b() {}\n\
                 #[cfg_attr(test, allow(dead_code))]\n#[cfg_attr(any(test, debug_assertions), \
                 aterm_spec_anchor::refines(machine = \"M\", action = \"C\"))]\nfn c() {}\n\
                 #[cfg_attr(test, allow(dead_code))]\n\
                 #[aterm_spec_anchor::refines(machine = \"M\", action = \"D\")]\nfn d() {}\n\
                 #[aterm_spec_anchor::spec_unmodeled(reason = \"a bare bypass\")]\nfn e() {}\n\
                 #[cfg_attr(all(target_os = \"macos\", test), aterm_spec_anchor::refines(\
                 machine = \"M\", action = \"G3\"))]\nfn f() {}\n\
                 #[cfg_attr(any(test, all(test, unix)), aterm_spec_anchor::refines(\
                 machine = \"M\", action = \"G4\"))]\nfn g() {}\n\
                 #[cfg_attr(any(test, unix), aterm_spec_anchor::refines(\
                 machine = \"M\", action = \"E\"))]\nfn h() {}\n\
                 #[cfg_attr(test, cfg_attr(unix, aterm_spec_anchor::refines[\
                 machine = \"M\", action = \"G5\"]))]\nfn i() {}\n";
    let found = run(
        &CLOSURE_ANCHOR,
        &[
            ("crates/x/src/lib.rs", &lib),
            ("crates/x/src/rig.rs", &waiver("M", "Rig")),
            ("crates/x/src/seam.rs", &waiver("M", "Seam")),
            ("crates/x/src/gated.rs", gated),
            ("crates/x/tests/conformance.rs", &waiver("M", "Integration")),
            ("crates/x/tests/again.rs", &waiver("M", "Seam")),
            ("crates/x/benches/b.rs", &waiver("M", "Bench")),
            ("crates/x/src/main.rs", &waiver("M", "Main")),
            ("crates/x/src/bin/tool.rs", &waiver("M", "Bin")),
            ("crates/x/build.rs", &waiver("M", "Build")),
            (
                "crates/aterm-spec-anchor/tests/fixture.rs",
                &waiver("F", "A"),
            ),
            (
                "crates/aterm-spec-macros/tests/anchors/suite.rs",
                &waiver("F", "A"),
            ),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/x/benches/b.rs:3",
            "crates/x/build.rs:3",
            "crates/x/src/bin/tool.rs:3",
            "crates/x/src/gated.rs:1",
            "crates/x/src/gated.rs:5",
            "crates/x/src/gated.rs:16",
            "crates/x/src/gated.rs:18",
            "crates/x/src/gated.rs:22",
            "crates/x/src/lib.rs:9",
            "crates/x/src/main.rs:3",
            "crates/x/src/rig.rs:3",
            "crates/x/tests/conformance.rs:3",
        ]
    );
    // A finding names its machine as well as its action: one action name recurs across
    // machines (`AnotherCase` in three Codex models).
    assert!(found[0].detail.contains("`M`") && found[0].detail.contains("`Bench`"));
}

/// A module its parent declares test-only stays out of the library though a test target, a
/// bench or an example mounts it too: what a dev target loads ships nothing
/// (`Corpus::shipped_loads`). A shipped line's mount links it.
#[test]
fn a_test_module_a_dev_target_also_mounts_stays_unlinked() {
    let rig = waiver("M", "Rig");
    let mount = "#[path = \"../src/rig.rs\"]\nmod rig;\n";
    let found = |lib: &str, mounter: Option<&str>| {
        let mut files = vec![("crates/x/src/lib.rs", lib), ("crates/x/src/rig.rs", &rig)];
        files.extend(mounter.map(|rel| (rel, mount)));
        spots(&run(&CLOSURE_ANCHOR, &files))
    };
    let declared = "#[cfg(test)]\nmod rig;\n";
    for mounter in [
        None,
        Some("crates/x/tests/mount.rs"),
        Some("crates/x/benches/mount.rs"),
        Some("crates/x/examples/mount.rs"),
    ] {
        assert_eq!(
            found(declared, mounter),
            ["crates/x/src/rig.rs:3"],
            "{mounter:?}"
        );
    }
    let shipped = "#[cfg(test)]\nmod rig;\n#[path = \"rig.rs\"]\nmod again;\n";
    assert!(found(shipped, None).is_empty());
}

/// The library links what a shipped line loads, and test code is what every guard calls it
/// (`TestCode::holds`): a file named for tests that nothing ships, a file only a test-only
/// inline module splices in and a test-only `mod` inside a shipped inline module link
/// nothing, so an anchor there covers no action; a `_tests.rs` a shipped `#[path]` mounts
/// links.
#[test]
fn an_anchor_in_test_code_by_any_road_covers_nothing() {
    let lib = "#[cfg(test)]\nmod rig;\n#[cfg(test)]\nmod checks {\n    \
               include!(\"spliced.rs\");\n}\nmod imp {\n    #[cfg(test)]\n    mod deep;\n}\n\
               #[path = \"shipped_tests.rs\"]\nmod shipped;\n";
    let found = run(
        &CLOSURE_ANCHOR,
        &[
            ("crates/x/src/lib.rs", lib),
            ("crates/x/src/rig.rs", &waiver("M", "Rig")),
            ("crates/x/src/old/tests.rs", &waiver("M", "Rig")),
            ("crates/x/src/spliced.rs", &waiver("M", "Spliced")),
            ("crates/x/src/imp/deep.rs", &waiver("M", "Deep")),
            ("crates/x/src/shipped_tests.rs", &waiver("M", "Shipped")),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/x/src/imp/deep.rs:3",
            "crates/x/src/old/tests.rs:3",
            "crates/x/src/rig.rs:3",
            "crates/x/src/spliced.rs:3",
        ]
    );
}
