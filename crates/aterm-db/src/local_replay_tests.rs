// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const SECRET: &[u8] = b"1111111111111111111111111111111111111111";

/// A build before the `no_authority` mark counted a control record against no quota, and the
/// journal it left has to open still, or its rows are out of reach, its rekey included.
/// NEGATIVE CONTROL: with replay going through `History::insert`, the open fails:
/// `` writer's retained history quota is exhausted ``.
#[test]
fn control_records_an_earlier_build_kept_past_a_quota_do_not_close_the_folder() {
    let root = crate::tests::tmp();
    let victim = root.path().join("victim");
    ensure_fleet(&victim, SECRET).unwrap();
    test_forget_no_authority(&victim).unwrap();
    let keys = FleetKeys::derive(SECRET).unwrap();
    let cap = Capability::control(&keys.manifest, &keys.root_seed).unwrap();
    let plain = format!(
        "{{\"v\":1,\"pad\":\"{}\"}}",
        "x".repeat(crypto::MAX_BODY - 32)
    );
    let mut unmarked = Database::open(&victim).unwrap();
    let mut previous: Option<Digest> = None;
    for n in 0..(crate::database::MAX_WRITER_BYTES / crypto::MAX_BODY + 2) as u64 {
        let parents: Vec<Digest> = previous.into_iter().collect();
        let wire = crypto::seal(
            &keys.manifest,
            &keys.key,
            &keys.root_seed,
            &cap,
            crypto::Causal {
                sequence: n + 1,
                previous,
                parents: &parents,
            },
            plain.as_bytes(),
        )
        .unwrap();
        previous = Some(aterm_digest::Sha256::digest(&wire));
        unmarked.ingest(&wire).unwrap();
    }
    drop(unmarked);

    ensure_fleet(&victim, SECRET).unwrap();
    assert!(Database::open(&victim).is_ok());
}

#[test]
fn bootstrap_never_replaces_missing_custody_of_a_retained_journal() {
    let root = crate::tests::tmp();
    let retained = root.path().join("retained");
    init(&retained, "private-requests").unwrap();
    let mut db = Database::open(&retained).unwrap();
    let value = json!({"why": "retained request", "state": "accepted"});
    db.commit(Command::Write {
        request: [1; 16],
        changes: vec![Change {
            collection: "requests".into(),
            key: "one".into(),
            value: Some(value.clone()),
        }],
    })
    .unwrap();
    let actor = db.credentials.capability.actor;
    drop(db);
    let original = private_file::read_private(&retained, CONFIG, 4096).unwrap();
    let identity = private_file::read_private(&retained, IDENTITY, 48).unwrap();
    let other = root.path().join("other");
    init(&other, "other").unwrap();
    let authority = Credentials::load(&other).unwrap();
    let invitation = authority.invitation(&actor).unwrap();
    std::fs::remove_file(retained.join(CONFIG)).unwrap();
    assert!(init(&retained, "replacement").is_err());
    assert!(join(&retained, &invitation, authority.manifest.root).is_err());
    assert!(ensure_fleet(&retained, SECRET).is_err());
    assert!(!retained.join(CONFIG).try_exists().unwrap());
    assert_eq!(
        private_file::read_private(&retained, IDENTITY, 48).unwrap(),
        identity
    );
    private_file::write_private(&retained, CONFIG, &original).unwrap();
    let reopened = Database::open(&retained).unwrap();
    let view = reopened.view();
    let cell = &view.cells[&("requests".into(), "one".into())];
    assert_eq!(cell.candidates.len(), 1);
    assert_eq!(cell.candidates[0].value, value);
}
