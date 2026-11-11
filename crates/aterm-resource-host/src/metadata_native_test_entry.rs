// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one early entry of this crate's library test image. It runs the
//! production internal exec role, then the native metadata fixture's roles,
//! before libtest parses argv, without a helper executable. Only this image
//! mounts it: an integration image that compiles the same fixture modules
//! keeps its own argv.

extern "C" fn enter() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if matches!(args.as_slice(), [argument] if argument == "--resource-capabilities")
        && let Some(marker) = std::env::var_os("ATERM_TEST_RECIPE_PROBE_MARKER")
    {
        // A native canary, not a maintained AY implementation or source grant.
        std::fs::write(marker, b"actual candidate executed").unwrap();
        println!(
            "{{\"version\":3,\"native_campaign\":true,\"admission\":\"resource-inspect-v3\",\"worker_argument\":\"--resource-worker-v3\",\"subgroup_custody\":\"host-owned-v3\",\"subgroup_argument\":\"--resource-subgroups-v3\",\"commands\":[\"bench run\"]}}"
        );
        std::io::Write::flush(&mut std::io::stdout()).unwrap();
        std::process::exit(0);
    }
    if let Some(result) = crate::metadata_prelude::entry(&args) {
        std::process::exit(if result == std::process::ExitCode::SUCCESS {
            0
        } else {
            75
        });
    }
    // The intermediate image inherits selected non-loader environment. Its
    // private exec verb must run before final-image bootstrap is considered.
    crate::solver_bootstrap::tests::entry();
    crate::actor::metadata_native_role();
}

#[used]
#[unsafe(link_section = "__DATA,__mod_init_func")]
static ENTRY: extern "C" fn() = enter;
