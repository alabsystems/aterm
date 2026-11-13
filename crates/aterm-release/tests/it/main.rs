// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release cutter's integration tests, as one test binary.
//!
//! The cutter is binary-only on purpose (nothing outside the cut tool may link its code; the
//! crate's lib, whose tests run in its own binary, holds only the repair worker a failed cut
//! opens, src/repair.rs), so the tests mount the pipeline's modules by `#[path]` at the crate
//! root, where they reach each other through `crate::`. Each mount compiles the cutter and its
//! inline `#[cfg(test)]` tests, so the mounts live here once and the bin target has `test =
//! false`.
//! Every mount is `allow(dead_code)`: a test exercises part of a module, and the binary's own
//! build holds the dead-code line for src/.
#![allow(
    clippy::disallowed_types,
    reason = "never linked into the shipped binary: the release cutter, run from source"
)]

// The durable flushes of this binary's process tree are plain `fsync(2)`s on Apple
// (aterm-durable's seam): the cutter's journals flush at the points they ship with, without the
// drive-cache drain only a power loss shows.
aterm_durable::plain_durable_flushes_in_tests!();

// Every git this binary runs, the cutter's own included, reads no configuration of the machine
// it runs on: a host's `insteadOf`, commit signing, hooks path, ignore rules or attributes would
// change what its engine fixtures see.
mod hermetic_git;

#[path = "../../src/main_lane.rs"]
#[allow(dead_code)]
mod main_lane;

#[path = "../../src/accepted_measure.rs"]
#[allow(dead_code)]
mod accepted_measure;
#[path = "../../src/apple.rs"]
#[allow(dead_code)]
mod apple;
#[path = "../../src/artifact.rs"]
#[allow(dead_code)]
mod artifact;
#[path = "../../src/build_inputs.rs"]
#[allow(dead_code)]
mod build_inputs;
#[path = "../../src/buildplan.rs"]
#[allow(dead_code)]
mod buildplan;
#[path = "../../src/bump_claim.rs"]
mod bump_claim;
#[path = "../../src/bundle.rs"]
#[allow(dead_code)]
mod bundle;
#[path = "../../src/changelog.rs"]
#[allow(dead_code)]
mod changelog;
#[path = "../../src/channel.rs"]
#[allow(dead_code)]
mod channel;
#[path = "../../src/check_content.rs"]
#[allow(dead_code)]
mod check_content;
#[path = "../../src/check_record.rs"]
#[allow(dead_code)]
mod check_record;
#[path = "../../src/checkout_bin.rs"]
#[allow(dead_code)]
mod checkout_bin;
#[path = "../../src/clean.rs"]
#[allow(dead_code)]
mod clean;
#[path = "../../src/cli.rs"]
#[allow(dead_code)]
mod cli;
#[path = "../../src/credential_wait.rs"]
#[allow(dead_code)]
mod credential_wait;
#[path = "../../src/cut_timing.rs"]
#[allow(dead_code)]
mod cut_timing;
#[path = "../../src/cutter.rs"]
#[allow(dead_code)]
mod cutter;
#[path = "../../src/cutter_inputs.rs"]
#[allow(dead_code)]
mod cutter_inputs;
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
#[path = "../../src/github_remote.rs"]
#[allow(dead_code)]
mod github_remote;
#[path = "../../src/harness_versions.rs"]
#[allow(dead_code)]
mod harness_versions;
#[path = "../../src/hook.rs"]
#[allow(dead_code)]
mod hook;
#[path = "../../src/input_record.rs"]
mod input_record;
#[path = "../../src/interrupt.rs"]
#[allow(dead_code)]
mod interrupt;
#[path = "../../src/ledger.rs"]
#[allow(dead_code)]
mod ledger;
#[path = "../../src/line.rs"]
#[allow(dead_code)]
mod line;
#[path = "../../src/line_claim.rs"]
#[allow(dead_code)]
mod line_claim;
#[path = "../../src/machines.rs"]
#[allow(dead_code)]
mod machines;
#[path = "../../src/manifest_out.rs"]
#[allow(dead_code)]
mod manifest_out;
#[path = "../../src/postlink.rs"]
#[allow(dead_code)]
mod postlink;
#[path = "../../src/profile_pilot.rs"]
#[allow(dead_code)]
mod profile_pilot;
#[path = "../../src/provision.rs"]
#[allow(dead_code)]
mod provision;
#[path = "../../src/publish.rs"]
#[allow(dead_code)]
mod publish;
#[path = "../../src/qualify_pilot.rs"]
#[allow(dead_code)]
mod qualify_pilot;
#[path = "../../src/qualify_replay.rs"]
mod qualify_replay;
#[path = "../../src/release.rs"]
#[allow(dead_code)]
mod release;
#[path = "../../src/release_disk.rs"]
#[allow(dead_code)]
mod release_disk;
#[path = "../../src/release_keychain.rs"]
#[allow(dead_code)]
mod release_keychain;
#[path = "../../src/release_status.rs"]
#[allow(dead_code)]
mod release_status;
#[path = "../../src/release_steps.rs"]
#[allow(dead_code)]
mod release_steps;
#[path = "../../src/release_target.rs"]
#[allow(dead_code)]
mod release_target;
#[path = "../../src/release_timeline.rs"]
#[allow(dead_code)]
mod release_timeline;
#[path = "../../src/release_tools.rs"]
#[allow(dead_code)]
mod release_tools;
#[path = "../../src/service_qualification.rs"]
#[allow(
    dead_code,
    reason = "integration tests mount the binary's private modules"
)]
mod service_qualification;

#[path = "../../src/managed_prerequisites.rs"]
#[allow(dead_code)]
mod managed_prerequisites;
#[path = "../../src/sign.rs"]
#[allow(dead_code)]
mod sign;
#[path = "../../src/site/mod.rs"]
#[allow(dead_code)]
mod site;
#[path = "../../src/slice_units.rs"]
#[allow(dead_code)]
mod slice_units;
#[path = "../../src/snapshot.rs"]
#[allow(dead_code)]
mod snapshot;
#[path = "../../src/source_signing.rs"]
#[allow(dead_code)]
mod source_signing;
#[path = "../../src/stage_local.rs"]
#[allow(
    dead_code,
    reason = "integration tests mount the binary's private modules"
)]
mod stage_local;
#[path = "../../src/tap.rs"]
#[allow(dead_code)]
mod tap;
#[path = "../../src/transforms.rs"]
#[allow(dead_code)]
mod transforms;
#[path = "../../src/trust_credit.rs"]
#[allow(dead_code)]
mod trust_credit;
#[path = "../../src/trust_facts.rs"]
#[allow(dead_code)]
mod trust_facts;
#[path = "../../src/trust_lane.rs"]
#[allow(dead_code)]
mod trust_lane;
#[path = "../../src/trust_lock.rs"]
#[allow(dead_code)]
mod trust_lock;
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
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
mod frozen_cutter;
mod handoff_policy_bundle;
mod hooks;
mod host_git;
mod journal_prefix_model;
mod ledger_race;
mod machine_roster;
mod plist_stamp;
mod publisher_fence_liveness;
mod publisher_fence_model;
mod publisher_origin;
mod publisher_script;
mod push_hook;
mod release_fixtures;
mod release_flow;
mod release_lease;
mod release_line;
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

/// Create a short `.noindex` temporary directory for integration and unit tests in this crate.
pub(crate) fn test_scratch_dir(tag: &str) -> aterm_tempfile::TempDir {
    let short = &tag[..tag.floor_char_boundary(8)];
    let builder = aterm_tempfile::Builder::new()
        .prefix(&format!("arl-{short}-"))
        .suffix(".noindex");
    #[cfg(unix)]
    let made = builder.tempdir_in("/tmp");
    #[cfg(not(unix))]
    let made = builder.tempdir();
    made.expect("test scratch dir")
}
