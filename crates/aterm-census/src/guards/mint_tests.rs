// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::tests::{run, spots};
use super::*;

#[test]
fn only_a_launchers_shipped_code_names_the_mint() {
    let mints = "fn f() { let a = unsafe { aterm_cap::Authority::root_authority() }; }\n\
                 // Authority::root_authority() in prose\n\
                 const S: &str = \"root_authority\";\n\
                 #[cfg(test)]\nmod tests {\n    fn t() { unsafe { Authority::root_authority() }; }\n}\n";
    let found = run(
        &LAUNCHER_MINT,
        &[
            ("crates/aterm-pty/src/lib.rs", mints),
            ("crates/aterm-pty/build.rs", mints),
            ("crates/aterm-pty/examples/demo.rs", mints),
            ("crates/aterm-pty/benches/perf.rs", mints),
            ("crates/aterm-pty/src/unix_tests.rs", mints),
            ("crates/aterm-pty/tests/smoke.rs", mints),
            ("crates/aterm-gui/src/lib.rs", mints),
            ("crates/aterm-cli/src/driver.rs", mints),
            (
                "crates/aterm-cap/src/lib.rs",
                "pub unsafe fn root_authority() -> Authority { Authority }\n",
            ),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-pty/benches/perf.rs:1",
            "crates/aterm-pty/build.rs:1",
            "crates/aterm-pty/examples/demo.rs:1",
            "crates/aterm-pty/src/lib.rs:1",
        ]
    );
}

#[test]
fn a_shipped_table_enabling_the_feature_is_found_and_a_dev_edge_is_not() {
    let text = "[package]\nname = \"x\"\n\n[features]\nopen = [\"aterm-cap/launcher-mint\"]\n\
                weak = [\"aterm-cap?/launcher-mint\"]\nalias = [\"cap/launcher-mint\"]\n\
                fine = []\n\n[dependencies]\n\
                aterm-cap = { workspace = true, features = [\"launcher-mint\"] }\n\
                cap = { package = \"aterm-cap\", path = \"../aterm-cap\", features = [\"launcher-mint\"] }\n\n\
                [dependencies.cap_dotted]\n\
                package = \"aterm-cap\"\n\
                path = \"../aterm-cap\"\n\
                features = [\"launcher-mint\"]\n\n\
                [target.'cfg(unix)'.dependencies]\n\
                aterm-cap = { path = \"../aterm-cap\", features = [\"launcher-mint\"] }\n\n\
                [dev-dependencies]\n\
                aterm-cap = { workspace = true, features = [\"launcher-mint\"] }\n";
    let found =
        crate::scan_set::shipped_feature_enablers(text, "crates/x/Cargo.toml", CAP, FEATURE)
            .expect("parses");
    assert_eq!(found.len(), 7, "{found:?}");
    assert!(found.iter().all(|f| !f.contains("dev-dependencies")));
}

#[test]
fn the_manifests_read_are_the_root_and_each_non_launcher_crate() {
    let paths: Vec<String> = [
        "Cargo.toml",
        "crates/aterm-pty/Cargo.toml",
        "crates/aterm-gui/Cargo.toml",
        "crates/aterm-cli/Cargo.toml",
        "crates/aterm-census/tests/fixtures/x/Cargo.toml",
        "vendor/winit/Cargo.toml",
    ]
    .map(String::from)
    .to_vec();
    let read: Vec<&str> = manifests(&paths).collect();
    assert_eq!(read, ["Cargo.toml", "crates/aterm-pty/Cargo.toml"]);
}
