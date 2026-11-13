// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const CRASH_ROOT: &str = "ATPKG_TEST_PACKAGE_CAMPAIGN_CRASH_ROOT";

fn intent() -> Intent {
    Intent {
        schema: 2,
        program: "codex".into(),
        build: 2_026_111_202,
        source: "a".repeat(40),
        version: "0.7.0".into(),
        target: "aarch64-apple-darwin".into(),
        package_tree: "b".repeat(64),
        packed_tree: "b".repeat(64),
        repository: "codex-alab".into(),
        staging_owner: "fixture-dev".into(),
        public_owner: "fixture-public".into(),
    }
}

fn prepared() -> Observation {
    Observation::IndexPrepared {
        build: 134,
        quad: std::array::from_fn(|_| "c".repeat(64)),
        predecessor_build: 133,
        predecessor: std::array::from_fn(|_| "d".repeat(64)),
    }
}

fn qualified() -> Observation {
    Observation::Qualified {
        files: PackageFiles {
            manifest: "1".repeat(64),
            signature: "2".repeat(64),
            archive: "3".repeat(64),
            tree_root: "4".repeat(64),
        },
    }
}

#[test]
fn crash_writer() {
    let Some(root) = std::env::var_os(CRASH_ROOT) else {
        return;
    };
    let mut campaign = Campaign::open(Path::new(&root), &intent()).unwrap();
    campaign.record(prepared()).unwrap();
    // The caller receives failure, not a successful publication acknowledgement.
    // No destructor runs: the kernel must release the process's exact lease.
    std::process::exit(71);
}

#[test]
fn process_death_keeps_the_exact_unconfirmed_index_without_claiming_publication() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().join("campaign");
    let executable = std::env::current_exe().unwrap();
    let mut child = aterm_uds::spawnfd::command(executable);
    child
        .args([
            "--exact",
            "package_campaign::tests::crash_writer",
            "--test-threads=1",
        ])
        .env(CRASH_ROOT, &root);
    let result = aterm_uds::deadline::output_until(
        &mut child,
        std::time::Instant::now() + Duration::from_secs(20),
    )
    .unwrap();
    assert_eq!(result.status.code(), Some(71));
    let before = std::fs::read(root.join(receipt_name(0))).unwrap();
    let mut resumed = Campaign::open(&root, &intent()).unwrap();
    assert_eq!(
        resumed.observations().collect::<Vec<_>>(),
        vec![&prepared()]
    );
    resumed.record(prepared()).unwrap();
    assert_eq!(std::fs::read(root.join(receipt_name(0))).unwrap(), before);
    assert!(!root.join(receipt_name(1)).exists());
    assert!(
        !resumed
            .observations()
            .any(|event| matches!(event, Observation::IndexPublished { .. }))
    );
}

#[test]
fn changed_package_or_destination_never_replaces_the_original_campaign() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().join("campaign");
    let mut original = Campaign::open(&root, &intent()).unwrap();
    original.record(prepared()).unwrap();
    drop(original);
    let frozen = std::fs::read(root.join("intent.json")).unwrap();
    let mut other = intent();
    other.package_tree = "d".repeat(64);
    assert!(Campaign::open(&root, &other).is_err());
    other = intent();
    other.packed_tree = "e".repeat(64);
    assert!(Campaign::open(&root, &other).is_err());
    other = intent();
    other.public_owner = "different-public".into();
    assert!(Campaign::open(&root, &other).is_err());
    assert_eq!(std::fs::read(root.join("intent.json")).unwrap(), frozen);
    let resumed = Campaign::open(&root, &intent()).unwrap();
    assert_eq!(resumed.observations().count(), 1);
}

#[test]
fn truncated_or_missing_receipts_are_unknown_and_preserved() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().join("campaign");
    let mut campaign = Campaign::open(&root, &intent()).unwrap();
    campaign.record(qualified()).unwrap();
    campaign.record(prepared()).unwrap();
    drop(campaign);
    let path = root.join(receipt_name(1));
    std::fs::write(&path, b"{\"schema\":1,").unwrap();
    assert!(Campaign::open(&root, &intent()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"{\"schema\":1,");
    std::fs::remove_file(root.join(receipt_name(0))).unwrap();
    assert!(Campaign::open(&root, &intent()).is_err());
    assert!(path.exists());
}

#[test]
fn replacing_a_live_lease_does_not_leave_two_accepted_writers() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().join("campaign");
    let mut first = Campaign::open(&root, &intent()).unwrap();
    assert!(Campaign::open(&root, &intent()).is_err());
    std::fs::remove_file(root.join("campaign.lock")).unwrap();
    let mut replacement = Campaign::open(&root, &intent()).unwrap();
    assert!(first.record(qualified()).is_err());
    replacement.record(prepared()).unwrap();
    drop(first);
    assert!(Campaign::open(&root, &intent()).is_err());
    assert_eq!(replacement.observations().count(), 1);
}

#[test]
fn replacing_the_private_directory_cannot_redirect_the_live_campaign() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().join("campaign");
    let moved = scratch.path().join("retained");
    let mut original = Campaign::open(&root, &intent()).unwrap();
    original.record(prepared()).unwrap();
    std::fs::rename(&root, &moved).unwrap();
    private_file::create_private_dir(&root).unwrap();
    assert!(original.record(qualified()).is_err());
    assert!(moved.join(receipt_name(0)).exists());
    assert!(!root.join(receipt_name(0)).exists());
}

#[test]
fn a_campaign_without_frozen_payload_identity_is_preserved_and_refused() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().join("campaign");
    drop(Campaign::open(&root, &intent()).unwrap());
    let mut old = aterm_json::to_value(intent()).unwrap();
    old.as_object_mut()
        .unwrap()
        .insert("schema".into(), 1.into());
    old.as_object_mut().unwrap().remove("packed_tree");
    let bytes = aterm_json::to_vec(&old).unwrap();
    let path = root.join("intent.json");
    std::fs::write(&path, &bytes).unwrap();
    assert!(Campaign::open(&root, &intent()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}
