// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::UNMOUNTED_TEST;
use crate::guards::tests::{run, spots};

/// NEGATIVE CONTROL: `[lib] test = false` silently drops inline `#[test]` functions in `src/`
/// unless an integration test target mounts the module.
#[test]
fn lib_test_false_flags_orphaned_unit_test_and_passes_path_mounted_module() {
    let found = run(
        &UNMOUNTED_TEST,
        &[
            (
                "crates/a/Cargo.toml",
                "[package]\nname = \"a\"\n\n[lib]\ntest = false\n",
            ),
            (
                "crates/a/src/lib.rs",
                "mod orphan;\nmod shared;\n/// Doc mentioning `#[test]` in a comment.\npub fn f() {}\n",
            ),
            (
                "crates/a/src/orphan.rs",
                "#[cfg(test)]\nmod tests {\n    #[test]\n    fn dead() {}\n}\n",
            ),
            ("crates/a/src/shared.rs", "#[test]\nfn runs_in_it() {}\n"),
            (
                "crates/a/tests/it.rs",
                "#[path = \"../src/shared.rs\"]\nmod shared;\n",
            ),
        ],
    );
    assert_eq!(spots(&found), ["crates/a/src/orphan.rs:3"]);
}

/// NEGATIVE CONTROL: `autotests = false` silently drops any `tests/*.rs` file that is neither
/// a `[[test]]` root nor mounted by `mod` / `#[path]` / `include!`.
#[test]
fn autotests_false_flags_unlisted_test_file_and_passes_suite_modules() {
    let found = run(
        &UNMOUNTED_TEST,
        &[
            (
                "crates/b/Cargo.toml",
                "[package]\nname = \"b\"\nautotests = false\n\n[[test]]\nname = \"suite\"\npath = \"tests/suite.rs\"\n",
            ),
            ("crates/b/src/lib.rs", "pub fn ok() {}\n"),
            ("crates/b/tests/suite.rs", "mod mounted;\n"),
            ("crates/b/tests/mounted.rs", "#[test]\nfn live() {}\n"),
            ("crates/b/tests/forgotten.rs", "#[test]\nfn dead() {}\n"),
        ],
    );
    assert_eq!(spots(&found), ["crates/b/tests/forgotten.rs:0"]);
}

/// NEGATIVE CONTROL: an unloaded `src/extra_tests.rs` or a `harness = false` target holding
/// `#[test]` is flagged, while a nested Cargo package under `tests/toy/` owns its own tree.
#[test]
fn unloaded_module_and_harness_false_are_flagged_while_nested_package_is_isolated() {
    let found = run(
        &UNMOUNTED_TEST,
        &[
            (
                "crates/c/Cargo.toml",
                "[package]\nname = \"c\"\n\n[[test]]\nname = \"drv\"\npath = \"tests/drv.rs\"\nharness = false\n",
            ),
            (
                "crates/c/src/lib.rs",
                "pub fn live() { let _ = \"#[test]\"; }\n",
            ),
            (
                "crates/c/src/dangling_tests.rs",
                "#[test]\nfn never_loaded() {}\n",
            ),
            (
                "crates/c/tests/drv.rs",
                "#[test]\nfn no_harness() {}\nfn main() {}\n",
            ),
            (
                "crates/c/tests/toy/Cargo.toml",
                "[package]\nname = \"toy\"\n",
            ),
            (
                "crates/c/tests/toy/src/lib.rs",
                "#[test]\nfn toy_unit() {}\n",
            ),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/c/src/dangling_tests.rs:1",
            "crates/c/tests/drv.rs:1",
        ]
    );
}

/// NEGATIVE CONTROL: Cargo skips `examples/*.rs` under `targo test --tests` unless `[[example]]`
/// sets `test = true`, whereas cross-crate `#[path]` plus `CARGO_MANIFEST_DIR` `include!` resolves
/// per compiling package.
#[test]
fn example_without_test_true_is_flagged_and_manifest_dir_include_resolves_per_pkg() {
    let found = run(
        &UNMOUNTED_TEST,
        &[
            (
                "crates/d/Cargo.toml",
                "[package]\nname = \"d\"\n\n[[example]]\nname = \"live_ex\"\ntest = true\n",
            ),
            ("crates/d/src/lib.rs", "mod api;\npub fn d() {}\n"),
            (
                "crates/d/src/api.rs",
                "#[cfg(test)]\nmod tests {\n    include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src/api_tests.rs\"));\n}\n",
            ),
            ("crates/d/src/api_tests.rs", "#[test]\nfn d_api() {}\n"),
            (
                "crates/d/examples/live_ex.rs",
                "fn main() {}\n#[test]\nfn ok() {}\n",
            ),
            (
                "crates/d/examples/dead_ex.rs",
                "fn main() {}\n#[test]\nfn skipped() {}\n",
            ),
            ("crates/e/Cargo.toml", "[package]\nname = \"e\"\n"),
            (
                "crates/e/src/lib.rs",
                "#[path = \"../../d/src/api.rs\"]\nmod api;\n",
            ),
            ("crates/e/src/api_tests.rs", "#[test]\nfn e_api() {}\n"),
        ],
    );
    assert_eq!(spots(&found), ["crates/d/examples/dead_ex.rs:2"]);
}
