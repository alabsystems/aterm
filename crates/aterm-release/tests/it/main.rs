// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release cutter's integration tests, as one test binary.
//!
//! aterm-release is binary-only on purpose (no `lib.rs`: nothing outside the cut tool may link
//! this code), so the tests mount the pipeline's modules by `#[path]` at the crate root, where
//! they reach each other through `crate::`. Each mount compiles the cutter and its inline
//! `#[cfg(test)]` tests, so the mounts live here once and the bin target has `test = false`.
//! Every mount is `allow(dead_code)`: a test exercises part of a module, and the binary's own
//! build holds the dead-code line for src/.
#![allow(
    clippy::disallowed_types,
    reason = "never linked into the shipped binary: the release cutter, run from source"
)]

#[path = "../../src/apple.rs"]
#[allow(dead_code)]
mod apple;
#[path = "../../src/artifact.rs"]
#[allow(dead_code)]
mod artifact;
#[path = "../../src/buildplan.rs"]
#[allow(dead_code)]
mod buildplan;
#[path = "../../src/bundle.rs"]
#[allow(dead_code)]
mod bundle;
#[path = "../../src/changelog.rs"]
#[allow(dead_code)]
mod changelog;
#[path = "../../src/channel.rs"]
#[allow(dead_code)]
mod channel;
#[path = "../../src/clean.rs"]
#[allow(dead_code)]
mod clean;
#[path = "../../src/cli.rs"]
#[allow(dead_code)]
mod cli;
#[path = "../../src/credential_wait.rs"]
#[allow(dead_code)]
mod credential_wait;
#[path = "../../src/cutter.rs"]
#[allow(dead_code)]
mod cutter;
#[path = "../../src/dev_app.rs"]
#[allow(dead_code)]
mod dev_app;
#[path = "../../src/dev_identity.rs"]
#[allow(dead_code)]
mod dev_identity;
#[path = "../../src/dmg.rs"]
#[allow(dead_code)]
mod dmg;
#[path = "../../src/durable.rs"]
#[allow(dead_code)]
mod durable;
#[path = "../../src/fixtures.rs"]
#[allow(dead_code)]
mod fixtures;
#[path = "../../src/gates.rs"]
#[allow(dead_code)]
mod gates;
#[path = "../../src/github.rs"]
#[allow(dead_code)]
mod github;
#[path = "../../src/hook.rs"]
#[allow(dead_code)]
mod hook;
#[path = "../../src/interrupt.rs"]
#[allow(dead_code)]
mod interrupt;
#[path = "../../src/ledger.rs"]
#[allow(dead_code)]
mod ledger;
#[path = "../../src/machines.rs"]
#[allow(dead_code)]
mod machines;
#[path = "../../src/manifest_out.rs"]
#[allow(dead_code)]
mod manifest_out;
#[path = "../../src/provision.rs"]
#[allow(dead_code)]
mod provision;
#[path = "../../src/publish.rs"]
#[allow(dead_code)]
mod publish;
#[path = "../../src/release.rs"]
#[allow(dead_code)]
mod release;
#[path = "../../src/release_steps.rs"]
#[allow(dead_code)]
mod release_steps;
#[path = "../../src/sign.rs"]
#[allow(dead_code)]
mod sign;
#[path = "../../src/site/mod.rs"]
#[allow(dead_code)]
mod site;
#[path = "../../src/snapshot.rs"]
#[allow(dead_code)]
mod snapshot;
#[path = "../../src/tap.rs"]
#[allow(dead_code)]
mod tap;
#[path = "../../src/transforms.rs"]
#[allow(dead_code)]
mod transforms;
#[path = "../../src/trust_facts.rs"]
#[allow(dead_code)]
mod trust_facts;
#[path = "../../src/trust_lane.rs"]
#[allow(dead_code)]
mod trust_lane;
#[path = "../../src/trust_oblig.rs"]
#[allow(dead_code)]
mod trust_oblig;
#[path = "../../src/verify.rs"]
#[allow(dead_code)]
mod verify;

mod apple_tier;
mod build_once;
mod changelog_golden;
mod channel_floor_model;
mod channel_latest;
mod claim_landing_model;
mod durable_post_intent_model;
mod export_policy;
mod handoff_policy_bundle;
mod hooks;
mod journal_prefix_model;
mod ledger_race;
mod machine_roster;
mod plist_stamp;
mod publisher_fence_liveness;
mod publisher_fence_model;
mod release_fixtures;
mod release_flow;
mod release_lease;
mod resume;
mod signconf;
mod site_sync;
mod snapshot_take;
mod template;
mod trust_facts_model;

/// This process's `PATH` with git's exec-path first, as git itself puts it for its own
/// children, resolved once per process: handed to code under test that builds its own `git`
/// from `PATH` (an env pair it takes), it finds the same git without a shim in front of it
/// (macOS's `/usr/bin/git` is xcrun's, a second exec that doubles what each git process
/// costs), and nothing but git's own commands moves ahead.
fn git_first_path() -> &'static str {
    static PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let o = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"))
            .arg("--exec-path")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output()
            .unwrap();
        assert!(o.status.success(), "git --exec-path: {o:?}");
        let exec = String::from_utf8_lossy(&o.stdout).trim().to_string();
        match std::env::var("PATH") {
            Ok(path) => format!("{exec}:{path}"),
            Err(_) => exec,
        }
    })
}
