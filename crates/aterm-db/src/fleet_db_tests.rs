// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const SECRET: &[u8] = b"1111111111111111111111111111111111111111";
const ROTATED: &[u8] = b"2222222222222222222222222222222222222222";

/// A replica of `SECRET`'s dataset at `dir`.
fn replica(dir: &Path) -> Database {
    local::ensure_fleet(dir, SECRET).expect("ensure_fleet");
    Database::open(dir).expect("open")
}

/// Carry every retained operation both ways, as a sync does.
fn exchange(a: &mut Database, b: &mut Database) {
    let carry = |from: &Database, to: &mut Database| {
        let wires: Vec<Vec<u8>> = from
            .history
            .index()
            .inventory()
            .map(|id| from.history.wire(id).expect("wire").to_vec())
            .collect();
        for wire in wires {
            to.ingest(&wire).expect("ingest");
        }
    };
    carry(a, b);
    carry(b, a);
}

fn ok_of(db: &Database) -> Value {
    tables_summary(db).expect("summary")["ok"].clone()
}

#[test]
fn the_key_is_read_from_the_folder_the_fleet_writes_it_to() {
    let state = aterm_types::dirs::state_dir().expect("state dir");
    aterm_uds::private_file::create_private_dir(&state).unwrap();
    aterm_uds::private_file::write_private(&state, "fleet.key", SECRET).unwrap();
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    assert_eq!(
        resolve_fleet_secret(&tmp.path().join("db")).unwrap(),
        SECRET
    );
}

/// An empty credential field holds no secret, so an ordinary read shows it as itself and that
/// redacted read loads back. NEGATIVE CONTROL: with empty fields masked too, the redacted
/// dump's load was refused as "a redacted read".
#[test]
fn an_empty_credential_field_reads_as_itself_and_loads_back() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut a = replica(&root.path().join("a"));
    put_row(
        &mut a,
        "ai_accounts",
        "a1",
        json!({ "id": "a1", "api_key": "", "oauth": { "refresh_token": "" } }),
    )
    .unwrap();
    let shown = dump_all(&a, false).unwrap();
    let row = &shown["ai_accounts"][0];
    assert_eq!(row["api_key"], json!(""), "{row}");
    assert_eq!(row["oauth"]["refresh_token"], json!(""), "{row}");
    load_all(&mut a, &shown).expect("a redacted read holding no secret loads back");
}

#[test]
fn ordinary_reads_redact_credentials_in_every_table_and_nested_candidate() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut a = replica(&root.path().join("a"));
    let mut b = replica(&root.path().join("b"));
    for (table, key) in [
        ("computers", "m1"),
        ("emails", "root@example.com"),
        ("phones", "216-555-0100"),
        ("ai_accounts", "account-1"),
    ] {
        let raw = json!({
            "password": "fixture-password",
            "has_password": "fixture-mislabelled-secret",
            "app_password": "fixture-app-password",
            "webmailPassword": "fixture-webmail-password",
            "oauth_token": "fixture-oauth-token",
            "private-key": "fixture-private-key",
            "googleAPIKeys": ["fixture-prefixed-api-key"],
            "ssh_private_keys": ["fixture-prefixed-private-key"],
            "imapPasswd": "fixture-prefixed-passwd",
            "requestAuthorization": "fixture-prefixed-authorization",
            "clientAuthHeaders": ["fixture-prefixed-auth-header"],
            "browserCookies": ["fixture-prefixed-cookie"],
            "totp": "fixture-totp",
            "totp_seed": "fixture-totp-seed",
            "authenticatorSeed": "fixture-authenticator-seed",
            "otp_seed": "fixture-otp-seed",
            "googleRecoveryCodes": ["fixture-recovery-code"],
            "backup_codes": ["fixture-backup-code"],
            "oauth": { "access_token": "fixture-access", "refreshToken": "fixture-refresh", "token_count": 7 },
            "clients": [{ "client_secret": "fixture-client-secret", "api-key": "fixture-api-key" }],
            "secret_ref": { "vault": "vault-id", "entry": "entry-id" },
            "notes": "visible noncredential metadata"
        });
        put_row(&mut a, table, key, raw).unwrap();
    }
    exchange(&mut a, &mut b);
    // Two causally concurrent versions must both be redacted, including the losing candidate.
    for (db, password) in [(&mut a, "fixture-a-only"), (&mut b, "fixture-b-only")] {
        put_row(
            db,
            "ai_accounts",
            "account-1",
            json!({"password":password,"has_password":password,"oauth":{"access_token":password}}),
        )
        .unwrap();
    }
    exchange(&mut a, &mut b);
    let shown = dump_all(&a, false).unwrap();
    let encoded = aterm_json::to_string(&shown).unwrap();
    assert!(
        !encoded.contains("fixture-"),
        "ordinary output exposed credential contents"
    );
    for table in TABLES {
        let row = &shown[*table][0];
        assert_eq!(row["password"], json!(REDACTED));
        if *table != "ai_accounts" {
            assert_eq!(row["app_password"], json!(REDACTED));
            assert_eq!(row["webmailPassword"], json!(REDACTED));
            assert_eq!(row["oauth_token"], json!(REDACTED));
            assert_eq!(row["private-key"], json!(REDACTED));
            assert_eq!(row["oauth"]["access_token"], json!(REDACTED));
            assert_eq!(row["oauth"]["refreshToken"], json!(REDACTED));
            assert_eq!(row["clients"][0]["client_secret"], json!(REDACTED));
            assert_eq!(row["clients"][0]["api-key"], json!(REDACTED));
            assert_eq!(row["secret_ref"]["entry"], json!("entry-id"));
            assert_eq!(row["oauth"]["token_count"], json!(7));
        }
    }
    assert_eq!(shown["emails"][0]["has_password"], json!(true));
    assert!(
        shown["ai_accounts"][0]["_conflicts"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );
    let retained = aterm_json::to_string(&dump_all(&a, true).unwrap()).unwrap();
    assert!(retained.contains("fixture-a-only"));
    assert!(retained.contains("fixture-b-only"));
    let before: BTreeSet<_> = a.history.index().inventory().copied().collect();
    assert!(
        load_all(&mut a, &shown).is_err(),
        "a redacted export cannot overwrite retained credentials"
    );
    assert_eq!(before, a.history.index().inventory().copied().collect());
    let after = aterm_json::to_string(&dump_all(&a, true).unwrap()).unwrap();
    assert_eq!(retained, after);
}

#[test]
fn protected_email_validation_rejects_nested_credential_fields_at_ingestion() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut db = replica(&root.path().join("a"));
    let email = "protected-owner@example.com";
    let key = storage_key("emails", email).unwrap();
    let metadata = json!({
        "email": email, "protected": true,
        "has_password": false,
        "secret_ref": {"vault": "vault-id", "entry": "entry-id"},
        "metadata": {"token_count": 7, "password_changed_at": 2000}
    });
    put_row(&mut db, "emails", email, metadata.clone()).unwrap();
    for field in [
        "password",
        "app-password",
        "mailPasswd",
        "googleAPIKeys",
        "ssh_private_keys",
        "requestAuthorization",
        "authHeaders",
        "browserCookies",
        "backupTokens",
        "totp",
        "totp_seed",
        "authenticatorSeed",
        "otp_seed",
        "googleRecoveryCodes",
        "backup_codes",
        "has_password",
    ] {
        for nested in [false, true] {
            let mut row = metadata.clone();
            if nested {
                row.as_object_mut()
                    .unwrap()
                    .insert("extensions".into(), json!([{field: "fixture-sensitive"}]));
            } else {
                row.as_object_mut()
                    .unwrap()
                    .insert(field.into(), json!("fixture-sensitive"));
            }
            let before = db.history.index().inventory().count();
            let written = put_row(&mut db, "emails", email, row.clone());
            if field == "has_password" && !nested {
                assert!(
                    written.is_ok(),
                    "local metadata is discarded before storage"
                );
            } else {
                assert!(written.is_err(), "local {field} nested={nested}");
                assert_eq!(
                    before,
                    db.history.index().inventory().count(),
                    "refused credentials must not enter history"
                );
            }
            let bytes = Command::Write {
                request: [1; 16],
                changes: vec![Change {
                    collection: "emails".into(),
                    key: key.clone(),
                    value: Some(row),
                }],
            }
            .encode()
            .unwrap();
            assert!(
                Command::decode(&bytes).is_err(),
                "wire {field} nested={nested}"
            );
        }
    }
    let retained = get_row(&db.view(), "emails", email).unwrap().unwrap();
    assert_eq!(retained["secret_ref"], metadata["secret_ref"]);
    assert!(
        !aterm_json::to_string(&retained)
            .unwrap()
            .contains("fixture-sensitive")
    );
    assert_eq!(
        redact_row("emails", retained, false)["has_password"],
        json!(false)
    );
}

#[test]
fn a_rotated_fleet_key_moves_the_rows_and_keeps_the_old_folder() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("db");
    let mut db = replica(&dir);
    put_row(
        &mut db,
        "computers",
        "m1",
        json!({"label": "m1", "notes": "n"}),
    )
    .unwrap();
    put_row(
        &mut db,
        "emails",
        "root@example.com",
        json!({"email": "root@example.com", "password": "pw"}),
    )
    .unwrap();
    drop(db);

    let refused = local::ensure_fleet(&dir, ROTATED).unwrap_err();
    assert!(refused.to_string().contains("aterm db rekey"), "{refused}");

    let report = rekey_with(&dir, ROTATED).unwrap();
    let report = &report["moved"][0];
    assert_eq!(report["imported"]["computers"], json!(1));
    local::ensure_fleet(&dir, ROTATED).expect("the folder now holds the rotated dataset");
    let moved = Database::open(&dir).unwrap();
    assert_eq!(
        get_row(&moved.view(), "emails", "root@example.com")
            .unwrap()
            .unwrap()["password"],
        json!("pw")
    );
    drop(moved);

    let kept = PathBuf::from(report["kept"].as_str().unwrap());
    let old = Database::open(&kept).expect("the old folder still opens");
    assert!(get_row(&old.view(), "computers", "m1").unwrap().is_some());
    drop(old);

    assert!(rekey_with(&dir, ROTATED).is_err(), "nothing left to move");
}

#[test]
fn a_rekey_cut_short_after_the_rename_finishes_when_run_again() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    for (name, partly_loaded) in [("gone", false), ("partly", true)] {
        let dir = tmp.path().join(name);
        let mut db = replica(&dir);
        put_row(&mut db, "computers", "m1", json!({"label": "m1"})).unwrap();
        put_row(&mut db, "computers", "m2", json!({"label": "m2"})).unwrap();
        let id = hex::encode_lower(&db.credentials.manifest.id[..8]);
        drop(db);

        // What a process killed after the rename, or part way through the load, leaves.
        aterm_uds::private_file::write_private(&dir, REKEY_UNFINISHED, b"").unwrap();
        let kept = dir.with_file_name(format!("{name}.before-rekey-{id}"));
        std::fs::rename(&dir, &kept).unwrap();
        if partly_loaded {
            local::ensure_fleet(&dir, ROTATED).unwrap();
            let mut db = Database::open(&dir).unwrap();
            put_row(
                &mut db,
                "computers",
                "m1",
                json!({"label": "m1", "notes": "since"}),
            )
            .unwrap();
        }

        let report = rekey_with(&dir, ROTATED).unwrap();
        assert_eq!(
            report["moved"][0]["kept"],
            json!(kept.display().to_string())
        );
        let db = Database::open(&dir).unwrap();
        let rows = scan_table(&db.view(), "computers").unwrap();
        assert_eq!(rows.keys().collect::<Vec<_>>(), ["m1", "m2"]);
        if partly_loaded {
            assert_eq!(rows["m1"]["notes"], json!("since"), "an edit since stands");
        }
        drop(db);
        assert!(rekey_with(&dir, ROTATED).is_err(), "nothing left to move");
    }
}

#[test]
fn no_verb_serves_a_dataset_a_cut_short_rekey_is_still_moving() {
    let tmp = fleet_fixture();
    let dir = tmp.path().join("db");
    let secret = resolve_fleet_secret(&dir).unwrap();
    let mut db = open_at(&dir).unwrap();
    put_row(&mut db, "computers", "m1", json!({"label": "m1"})).unwrap();
    let id = hex::encode_lower(&db.credentials.manifest.id[..8]);
    drop(db);
    aterm_uds::private_file::write_private(&dir, REKEY_UNFINISHED, b"").unwrap();
    let kept = dir.with_file_name(format!("db.before-rekey-{id}"));
    std::fs::rename(&dir, &kept).unwrap();

    let refusal = open_at(&dir)
        .err()
        .expect("an empty dataset is not the answer");
    assert!(refusal.0.contains("aterm db rekey"), "{}", refusal.0);
    assert!(!dir.exists(), "and nothing was created in its place");

    rekey_with(&dir, &secret).unwrap();
    let db = open_at(&dir).unwrap();
    assert_eq!(scan_table(&db.view(), "computers").unwrap().len(), 1);
}

#[test]
fn a_stored_placeholder_does_not_stop_a_rekey_and_moves_as_a_word() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("db");
    let mut db = replica(&dir);
    let key = "root@example.com";
    db.commit(Command::Write {
        request: crypto::random::<16>().unwrap(),
        changes: vec![Change {
            collection: "emails".into(),
            key: storage_key("emails", key).unwrap(),
            value: Some(json!({"email": key, "protected": false, "password": REDACTED})),
        }],
    })
    .unwrap();
    drop(db);

    rekey_with(&dir, ROTATED).unwrap();
    let moved = Database::open(&dir).unwrap();
    let row = get_row(&moved.view(), "emails", key).unwrap().unwrap();
    assert_eq!(row["password"], json!(HAS_PASSWORD));
}

#[test]
fn a_control_record_on_a_replica_a_rotation_left_unmarked_does_not_empty_the_rekey() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let victim = tmp.path().join("victim");
    let mut db = replica(&victim);
    put_row(&mut db, "computers", "m1", json!({"label": "m1"})).unwrap();
    drop(db);
    local::test_forget_no_authority(&victim).unwrap();
    let mut unmarked = Database::open(&victim).unwrap();
    for name in ["forger-a", "forger-b"] {
        let forger = tmp.path().join(name);
        local::test_forge_authority(&forger, SECRET).unwrap();
        let mut forger = Database::open(&forger).unwrap();
        let actor = forger.credentials.capability.actor;
        let id = forger
            .commit(Command::Budget {
                request: crypto::random::<16>().unwrap(),
                name: name.to_owned(),
                total: 1,
                shares: BTreeMap::from([(actor, 1)]),
            })
            .unwrap();
        unmarked.ingest(forger.history.wire(&id).unwrap()).unwrap();
    }
    assert!(unmarked.view().authority_fault);
    drop(unmarked);

    let report = rekey_with(&victim, ROTATED).unwrap();
    assert_eq!(report["moved"][0]["imported"]["computers"], json!(1));
}

#[test]
fn a_row_put_accepted_moves_with_a_rekey() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("db");
    let mut db = replica(&dir);
    put_row(&mut db, "computers", "m1", json!({"label": "m1"})).unwrap();
    let key = "big@example.com";
    let notes = "x".repeat(50_000);
    put_row(
        &mut db,
        "emails",
        key,
        json!({"email": key, "notes": notes}),
    )
    .unwrap();
    put_row(&mut db, "phones", "555-234-5678", json!({})).unwrap();
    drop(db);

    rekey_with(&dir, ROTATED).unwrap();
    let db = Database::open(&dir).unwrap();
    let summary = tables_summary(&db).unwrap();
    assert_eq!(summary["tables"]["computers"], json!(1));
    assert_eq!(summary["tables"]["emails"]["total"], json!(1));
    assert_eq!(summary["tables"]["phones"], json!(1));
}

/// The import's record moves with the rows, so the source keys it names stay this replica's.
/// NEGATIVE CONTROL: with `finish_rekey` not carrying it: `` left: None ``.
#[test]
fn the_imports_record_moves_with_a_rekey() {
    use crate::alab::import::covered;
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("db");
    let mut db = replica(&dir);
    put_row(&mut db, "computers", "m1", json!({"label": "m1"})).unwrap();
    drop(db);
    let record = covered::Record {
        covered: false,
        uncovered: 1,
        digest: None,
        epoch: None,
        at_ms: 1,
        sources: ["sr-00ab".to_owned()].into_iter().collect(),
    };
    covered::write(&dir, &record, &[]).unwrap();

    rekey_with(&dir, ROTATED).unwrap();
    assert_eq!(
        covered::read(&dir).unwrap().map(|record| record.sources),
        Some(record.sources)
    );
}

/// Two replicas whose edits to m1 conflict.
fn conflicting(tmp: &Path) -> (PathBuf, Database, PathBuf, Database) {
    let (adir, bdir) = (tmp.join("a"), tmp.join("b"));
    let (mut a, mut b) = (replica(&adir), replica(&bdir));
    put_row(
        &mut a,
        "computers",
        "m1",
        json!({"label": "m1", "notes": "old"}),
    )
    .unwrap();
    exchange(&mut a, &mut b);
    put_row(
        &mut a,
        "computers",
        "m1",
        json!({"label": "m1", "notes": "a"}),
    )
    .unwrap();
    put_row(
        &mut b,
        "computers",
        "m1",
        json!({"label": "m1", "notes": "b"}),
    )
    .unwrap();
    exchange(&mut a, &mut b);
    (adir, a, bdir, b)
}

#[test]
fn loading_a_dump_back_leaves_a_conflict_it_shows() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let (_, mut a, _, _b) = conflicting(tmp.path());
    let before = a.history.index().inventory().count();
    let dump = dump_all(&a, true).unwrap();

    let report = load_all(&mut a, &dump).unwrap();
    assert_eq!(report["operations_committed"], json!(0));
    assert_eq!(a.history.index().inventory().count(), before);
    assert_eq!(tables_summary(&a).unwrap()["conflicts"], json!(1));

    // A document that does differ still wins, and settles the conflict.
    let doc = json!({"computers": [{"label": "m1", "notes": "chosen"}]});
    load_all(&mut a, &doc).unwrap();
    assert_eq!(tables_summary(&a).unwrap()["conflicts"], json!(0));
}

#[test]
fn a_rekey_names_the_conflicts_it_settles() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let (adir, a, _, _b) = conflicting(tmp.path());
    drop(a);
    let report = rekey_with(&adir, ROTATED).unwrap();
    assert_eq!(
        report["moved"][0]["left_behind"]["conflicts_settled_by_digest"],
        json!(["computers/m1"])
    );
}

#[test]
fn a_rekey_carries_a_deletion_another_machine_has_not_seen() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let (adir, bdir) = (tmp.path().join("a"), tmp.path().join("b"));
    let (mut a, mut b) = (replica(&adir), replica(&bdir));
    put_row(&mut a, "computers", "gone", json!({"label": "gone"})).unwrap();
    exchange(&mut a, &mut b);
    delete_row(&mut a, "computers", "gone").unwrap();
    drop(a);
    drop(b);

    rekey_with(&adir, ROTATED).unwrap();
    rekey_with(&bdir, ROTATED).unwrap();
    let (mut a, mut b) = (
        Database::open(&adir).unwrap(),
        Database::open(&bdir).unwrap(),
    );
    exchange(&mut a, &mut b);

    // The deletion is not lost: the row shows it, and `tables` counts it, until settled.
    let row = get_row(&a.view(), "computers", "gone").unwrap().unwrap();
    assert_eq!(row["_conflicts"], json!([{"_deleted": true}]));
    assert_eq!(tables_summary(&a).unwrap()["conflicts"], json!(1));
}

/// Copy a replica's folder, as a backup or a restore does.
fn copy_folder(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    std::fs::set_permissions(to, std::fs::metadata(from).unwrap().permissions()).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let copy = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_folder(&path, &copy);
        } else {
            std::fs::copy(&path, &copy).unwrap();
        }
    }
}

#[test]
fn a_folder_restored_from_a_backup_costs_only_what_it_wrote_since() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let (adir, backup) = (tmp.path().join("a"), tmp.path().join("backup"));
    let mut a = replica(&adir);
    let mut b = replica(&tmp.path().join("b"));
    put_row(&mut a, "computers", "before", json!({"label": "before"})).unwrap();
    drop(a);
    copy_folder(&adir, &backup);

    let mut a = Database::open(&adir).unwrap();
    put_row(&mut a, "computers", "since", json!({"label": "since"})).unwrap();
    exchange(&mut a, &mut b);
    put_row(&mut b, "computers", "theirs", json!({"label": "theirs"})).unwrap();
    drop(a);

    // The restore writes the sequence the original already wrote.
    std::fs::remove_dir_all(&adir).unwrap();
    std::fs::rename(&backup, &adir).unwrap();
    let mut restored = Database::open(&adir).unwrap();
    put_row(
        &mut restored,
        "computers",
        "other",
        json!({"label": "other"}),
    )
    .unwrap();
    exchange(&mut restored, &mut b);

    let summary = tables_summary(&b).unwrap();
    assert_eq!(summary["quarantined_writers"], json!(1));
    assert_eq!(summary["ok"], json!(false));
    let rows = scan_table(&b.view(), "computers").unwrap();
    assert_eq!(rows.keys().collect::<Vec<_>>(), ["before", "theirs"]);
    put_row(&mut b, "computers", "later", json!({"label": "later"})).expect("b can still write");
    assert_eq!(scan_table(&b.view(), "computers").unwrap().len(), 3);
}

#[test]
fn a_write_after_a_kept_control_record_stays_visible_and_writable() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let forger = tmp.path().join("forger");
    local::test_forge_authority(&forger, SECRET).unwrap();
    let mut forger = Database::open(&forger).unwrap();
    let id = forger
        .revoke_writes(crypto::random::<16>().unwrap(), [7; 48], None)
        .unwrap();
    let wire = forger.history.wire(&id).unwrap().to_vec();

    // A replica an earlier build left: it kept the record and wrote with it as a parent.
    let victim = tmp.path().join("victim");
    local::ensure_fleet(&victim, SECRET).unwrap();
    local::test_forget_no_authority(&victim).unwrap();
    let mut db = Database::open(&victim).unwrap();
    db.ingest(&wire).unwrap();
    put_row(&mut db, "computers", "m2", json!({"label": "m2"})).unwrap();
    drop(db);

    let mut healed = replica(&victim);
    assert!(
        get_row(&healed.view(), "computers", "m2")
            .unwrap()
            .is_some()
    );
    put_row(&mut healed, "computers", "m3", json!({"label": "m3"})).expect("it can still write");

    let mut peer = replica(&tmp.path().join("peer"));
    exchange(&mut healed, &mut peer);
    assert_eq!(scan_table(&peer.view(), "computers").unwrap().len(), 2);
}

#[test]
fn no_member_can_stop_a_row_for_good() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let mut a = replica(&tmp.path().join("a"));
    let bdir = tmp.path().join("b");
    let mut b = replica(&bdir);
    let key = "root@example.com";
    put_row(&mut a, "emails", key, json!({"email": key})).unwrap();
    exchange(&mut a, &mut b);
    let first = a.history.index().inventory().next().copied().unwrap();
    let stop = Command::Write {
        request: crypto::random::<16>().unwrap(),
        changes: vec![Change {
            collection: "emails".into(),
            key: storage_key("emails", key).unwrap(),
            value: None,
        }],
    };

    assert!(a.commit(stop.clone()).is_err(), "the verb refuses it");
    // The same record signed by hand, as a member's own tooling can.
    let forged = crypto::seal(
        &a.credentials.manifest,
        &a.credentials.key,
        &a.credentials.seed,
        &a.credentials.capability,
        crypto::Causal {
            sequence: 2,
            previous: Some(first),
            parents: &[first],
        },
        &stop.encode().unwrap(),
    )
    .unwrap();
    b.ingest(&forged).unwrap();
    assert!(get_row(&b.view(), "emails", key).unwrap().is_some());
    assert_eq!(tables_summary(&b).unwrap()["invalid_operations"], json!(1));
    put_row(&mut b, "emails", key, json!({"email": key, "notes": "n"})).expect("still writable");
    drop(b);
    let b = replica(&bdir);
    assert!(
        get_row(&b.view(), "emails", key).unwrap().is_some(),
        "a replay keeps it inert"
    );
}

/// A stored has-password word stays visible, so the import can judge it, and counts as no
/// password. NEGATIVE CONTROL: with `drop_read_fields` nulling it, the import reads nothing
/// and records that cell covered (`alab::command`'s
/// `a_flag_shaped_password_is_covered_only_when_its_word_agrees`):
/// `` left: Number(Number(Unsigned(1))) right: Number(Number(Unsigned(2))) ``.
#[test]
fn a_stored_flag_reads_as_stored_and_a_read_field_is_not_taken_for_the_row() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let mut db = replica(&tmp.path().join("db"));
    let key = "root@example.com";
    db.commit(Command::Write {
        request: crypto::random::<16>().unwrap(),
        changes: vec![Change {
            collection: "emails".into(),
            key: storage_key("emails", key).unwrap(),
            value: Some(json!({
                "email": key,
                "protected": false,
                "password": "1",
                "_conflicts": [{"notes": "stale"}],
            })),
        }],
    })
    .unwrap();

    let row = get_row(&db.view(), "emails", key).unwrap().unwrap();
    assert_eq!(row["password"], json!("1"));
    assert!(row.get("_conflicts").is_none());
    let summary = tables_summary(&db).unwrap();
    assert_eq!(summary["tables"]["emails"]["root_with_password"], json!(0));
    let dump = dump_all(&db, true).unwrap();
    assert_eq!(dump["emails"][0]["password"], json!("1"));
    put_row(&mut db, "emails", key, json!({"email": key, "notes": "n"})).unwrap();
    let row = get_row(&db.view(), "emails", key).unwrap().unwrap();
    assert_eq!(
        row["password"],
        json!("1"),
        "a partial edit changes nothing"
    );

    // A real password replaces the word and is written clean; no word replaces it.
    put_row(
        &mut db,
        "emails",
        key,
        json!({"email": key, "password": "pw"}),
    )
    .unwrap();
    let before = db.history.index().inventory().count();
    for word in ["1", "no", "none", "set", "-"] {
        let refused = put_row(
            &mut db,
            "emails",
            key,
            json!({"email": key, "password": word}),
        );
        assert!(refused.is_err(), "{word}");
        let doc = json!({"emails": [{"email": key, "password": word}]});
        assert!(load_all(&mut db, &doc).is_err(), "{word}");
    }
    assert_eq!(db.history.index().inventory().count(), before);
    let row = get_row(&db.view(), "emails", key).unwrap().unwrap();
    assert_eq!(row["password"], json!("pw"));
    put_row(
        &mut db,
        "emails",
        key,
        json!({"email": key, "password": null}),
    )
    .expect("a null still clears it");
    let row = get_row(&db.view(), "emails", key).unwrap().unwrap();
    assert_eq!(row["password"], Value::Null);
}

/// A rekey carries a stored word on, so the replica's import still has it to judge.
/// NEGATIVE CONTROL: with `drop_read_fields` nulling the word, the new dataset reads no password:
/// `` left: Null right: String("1") ``.
#[test]
fn a_rekey_carries_a_stored_flag_on() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("db");
    let key = "root@example.com";
    let mut db = replica(&dir);
    put_row(
        &mut db,
        "emails",
        key,
        json!({"email": key, "password": "1"}),
    )
    .unwrap();
    drop(db);

    rekey_with(&dir, ROTATED).unwrap();
    let db = Database::open(&dir).unwrap();
    let row = get_row(&db.view(), "emails", key).unwrap().unwrap();
    assert_eq!(row["password"], json!("1"));
}

/// `a` stores a real password over the sheet's `1` while `b`, not yet synced, writes `other`
/// in its place: the cell `a` then holds.
fn a_password_against(tmp: &Path, other: Value) -> Database {
    let key = "root@example.com";
    let row = |password: Value, notes: &str| json!({"email": key, "protected": false, "password": password, "notes": notes});
    let write = |db: &mut Database, value: Value| {
        db.commit(Command::Write {
            request: crypto::random::<16>().unwrap(),
            changes: vec![Change {
                collection: "emails".into(),
                key: storage_key("emails", key).unwrap(),
                value: Some(value),
            }],
        })
        .unwrap();
    };
    let (mut a, mut b) = (replica(&tmp.join("a")), replica(&tmp.join("b")));
    write(&mut a, row(json!("1"), "base"));
    exchange(&mut a, &mut b);
    write(&mut a, row(json!("real-password"), "base"));
    write(&mut b, row(other, "edit"));
    exchange(&mut a, &mut b);
    a
}

/// NEGATIVE CONTROL: with `project_cell`'s password rule dropped, whichever run the digest
/// order puts the word last reads the word (and a put of the notes alone would then erase the
/// password): `` left: String("1") right: String("real-password") ``.
#[test]
fn a_stored_password_outlives_a_concurrent_empty_one_in_either_digest_order() {
    let key = "root@example.com";
    for other in [json!("1"), Value::Null] {
        let mut orders = BTreeSet::new();
        for _ in 0..64 {
            if orders.len() == 2 {
                break;
            }
            let tmp = aterm_tempfile::tempdir().expect("tempdir");
            let mut a = a_password_against(tmp.path(), other.clone());
            let view = a.view();
            let cell = &view.cells[&("emails".to_owned(), storage_key("emails", key).unwrap())];
            orders.insert(cell.candidates.last().unwrap().value["notes"] == json!("edit"));
            let row = get_row(&view, "emails", key).unwrap().unwrap();
            assert_eq!(row["password"], json!("real-password"));
            assert!(row["_conflicts"].as_array().is_some_and(|c| !c.is_empty()));
            let summary = tables_summary(&a).unwrap();
            assert_eq!(summary["tables"]["emails"]["root_with_password"], json!(1));
            assert_eq!(summary["conflicts"], json!(1));

            put_row(&mut a, "emails", key, json!({"email": key, "notes": "n"})).unwrap();
            let row = get_row(&a.view(), "emails", key).unwrap().unwrap();
            assert_eq!(row["password"], json!("real-password"), "a partial edit");
            assert!(row.get("_conflicts").is_none());
        }
        assert_eq!(orders.len(), 2, "both digest orders ran");
    }
}

/// NEGATIVE CONTROL: with the conflict judged on the stored values, the row shows a conflict
/// with itself and `tables` counts it: `` left: Some(Array([..])) right: None ``.
/// NEGATIVE CONTROL: with `project_cell` comparing the stored spelling, an empty cell beside a
/// null reads as a conflict: `` left: Some(Array([Object({"email": String("root@example.com"),
/// "notes": String("n"), "protected": Bool(false)})])) right: None ``.
#[test]
fn password_cells_that_say_the_same_thing_are_no_conflict() {
    let key = "root@example.com";
    let write = |db: &mut Database, password: Option<Value>| {
        let mut row = json!({"email": key, "protected": false, "notes": "n"});
        if let Some(password) = password {
            row.as_object_mut()
                .unwrap()
                .insert("password".into(), password);
        }
        db.commit(Command::Write {
            request: crypto::random::<16>().unwrap(),
            changes: vec![Change {
                collection: "emails".into(),
                key: storage_key("emails", key).unwrap(),
                value: Some(row),
            }],
        })
        .unwrap();
    };
    let concurrent = |tmp: &Path, one: Option<Value>, other: Value| {
        let (mut a, mut b) = (replica(&tmp.join("a")), replica(&tmp.join("b")));
        write(&mut a, Some(Value::Null));
        exchange(&mut a, &mut b);
        write(&mut a, one);
        write(&mut b, Some(other));
        exchange(&mut a, &mut b);
        a
    };
    for (one, other) in [
        (None, json!(null)),
        (Some(json!("")), json!(null)),
        (Some(json!("  ")), json!(null)),
        (Some(json!("0")), json!(null)),
        (Some(json!("none")), json!(null)),
        (Some(json!("0")), json!("no")),
        (Some(json!("yes")), json!("1")),
    ] {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let a = concurrent(tmp.path(), one.clone(), other.clone());
        let row = get_row(&a.view(), "emails", key).unwrap().unwrap();
        assert_eq!(row.get("_conflicts"), None, "{one:?} {other:?}");
        assert_eq!(
            tables_summary(&a).unwrap()["conflicts"],
            json!(0),
            "{one:?} {other:?}"
        );
    }
    for (one, other) in [
        ("1", json!(null)),
        ("1", json!("0")),
        ("real-password", json!(null)),
        ("real-password", json!("1")),
        ("real-password", json!("another-password")),
    ] {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let a = concurrent(tmp.path(), Some(json!(one)), other.clone());
        assert_eq!(
            tables_summary(&a).unwrap()["conflicts"],
            json!(1),
            "{one} {other:?}"
        );
    }
}

/// A none word says what an empty cell says, and stays masked, so a stdout dump of it cannot
/// clear a password elsewhere.
/// NEGATIVE CONTROL: with `redact_row` taking any non-empty cell as set:
/// `` left: Bool(true) right: Bool(false) ``.
#[test]
fn a_none_word_reads_as_no_password() {
    let row = |stored: &str| json!({"email": "root@example.com", "password": stored});
    for stored in ["0", "none", " No "] {
        let shown = redact_row("emails", row(stored), false);
        assert_eq!(shown["has_password"], json!(false), "{stored}");
        assert_eq!(shown["password"], json!(REDACTED), "{stored}");
    }
    for stored in ["1", "yes", "real-password"] {
        let shown = redact_row("emails", row(stored), false);
        assert_eq!(shown["has_password"], json!(true), "{stored}");
        assert_eq!(shown["password"], json!(REDACTED), "{stored}");
    }
}

/// A workbook that is there and cannot be read is named, and when it was the only source of
/// passwords `bootstrap` is not ok: rows it writes stand, so a repaired file comes too late.
/// NEGATIVE CONTROL: with `read_xlsx_email_passwords` skipping it silently:
/// `` left: Null right: Array([String("alab-emails.xlsx")]) ``.
#[test]
fn a_workbook_that_cannot_be_read_is_named() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let sheets = tmp.path().join("sheets");
    std::fs::create_dir(&sheets).unwrap();
    for (name, text) in [
        ("alab-phones.csv", "phone\n"),
        ("alab-computers.csv", "label\n"),
        (
            "alab-emails.csv",
            "email\nflag@example.com,,1,active,proton,,,\n",
        ),
        ("ai-accounts.csv", "m\n"),
        ("alab-emails.xlsx", "not a workbook"),
    ] {
        std::fs::write(sheets.join(name), text).unwrap();
    }
    let mut db = replica(&tmp.path().join("db"));
    let report = bootstrap_from_secrets(&mut db, &sheets).unwrap();
    assert_eq!(report["unread_workbooks"], json!(["alab-emails.xlsx"]));
    assert_eq!(report["ok"], json!(false));
    std::fs::remove_file(sheets.join("alab-emails.xlsx")).unwrap();
    let report = bootstrap_from_secrets(&mut db, &sheets).unwrap();
    assert_eq!(report.get("unread_workbooks"), None);
    assert_eq!(report["ok"], json!(true));
}

#[test]
fn values_that_read_alike_are_no_conflict() {
    let key = "root@example.com";
    for stored in [
        ("_conflicts", json!([{"notes": "stale"}])),
        ("has_password", json!(true)),
    ] {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let (mut a, mut b) = (
            replica(&tmp.path().join("a")),
            replica(&tmp.path().join("b")),
        );
        put_row(&mut a, "emails", key, json!({"email": key, "notes": "old"})).unwrap();
        exchange(&mut a, &mut b);
        let mut junk = json!({"email": key, "protected": false, "password": null, "notes": "n"});
        junk.as_object_mut()
            .unwrap()
            .insert(stored.0.to_owned(), stored.1);
        a.commit(Command::Write {
            request: crypto::random::<16>().unwrap(),
            changes: vec![Change {
                collection: "emails".into(),
                key: storage_key("emails", key).unwrap(),
                value: Some(junk),
            }],
        })
        .unwrap();
        put_row(&mut b, "emails", key, json!({"email": key, "notes": "n"})).unwrap();
        exchange(&mut a, &mut b);

        let row = get_row(&a.view(), "emails", key).unwrap().unwrap();
        assert_eq!(row.get("_conflicts"), None);
        let summary = tables_summary(&a).unwrap();
        assert_eq!(summary["conflicts"], json!(0));
        assert_eq!(summary["ok"], json!(true));
    }
}

/// NEGATIVE CONTROL: with `drop_read_fields` nulling the mark, the other Mac's dump loads and
/// clears the password: `` assertion failed: load_all(&mut holder, &dump).is_err() ``.
#[test]
fn a_dump_holding_a_stored_mark_cannot_clear_a_password() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let key = "root@example.com";
    let (mut holder, mut other) = (
        replica(&tmp.path().join("holder")),
        replica(&tmp.path().join("other")),
    );
    put_row(
        &mut holder,
        "emails",
        key,
        json!({"email": key, "password": "real-password"}),
    )
    .unwrap();
    other
        .commit(Command::Write {
            request: crypto::random::<16>().unwrap(),
            changes: vec![Change {
                collection: "emails".into(),
                key: storage_key("emails", key).unwrap(),
                value: Some(json!({"email": key, "protected": false, "password": REDACTED})),
            }],
        })
        .unwrap();
    let dump = dump_all(&other, true).unwrap();
    assert!(load_all(&mut holder, &dump).is_err());
    let row = get_row(&holder.view(), "emails", key).unwrap().unwrap();
    assert_eq!(row["password"], json!("real-password"));
}

/// NEGATIVE CONTROL: with `refuse_unfinished` out of the round, the peer's older row lands in
/// the partial dataset and stands through the rerun:
/// `` called `Result::unwrap_err()` on an `Ok` value ``.
#[test]
fn no_round_feeds_a_dataset_a_cut_short_rekey_is_still_moving() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    let mut mine = replica(&a);
    let row = |notes: &str| json!({"label": "m1", "notes": notes});
    put_row(&mut mine, "computers", "m1", row("only here")).unwrap();
    drop(mine);
    local::ensure_fleet(&b, ROTATED).unwrap();
    let mut peer = Database::open(&b).unwrap();
    put_row(&mut peer, "computers", "m1", row("older")).unwrap();
    drop(peer);
    let listener =
        aterm_uds::kept_out(|| std::net::TcpListener::bind("127.0.0.1:0")).expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let stop = std::sync::Arc::new(aterm_uds::stop::Stop::new());
    let server = {
        let (b, stop) = (b.clone(), stop.clone());
        std::thread::spawn(move || {
            network::serve_fleet_until(
                &b,
                listener,
                stop,
                std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            )
        })
    };
    cut_short(&a, ROTATED, |_| {});

    let mut repair = network::Repair::default();
    let refused = network::sync_fleet_round_with(&a, addr, &mut repair).unwrap_err();
    assert!(refused.to_string().contains("aterm db rekey"), "{refused}");
    let waiting = network::sync_round_with(&a, addr, &mut repair).unwrap_err();
    assert!(waiting.to_string().contains("aterm db rekey"), "{waiting}");
    let db = Database::open(&a).unwrap();
    assert!(scan_table(&db.view(), "computers").unwrap().is_empty());
    drop(db);

    rekey_with(&a, ROTATED).unwrap();
    let db = Database::open(&a).unwrap();
    let rows = scan_table(&db.view(), "computers").unwrap();
    assert_eq!(rows["m1"]["notes"], json!("only here"));
    drop(db);
    // Once it has finished, a round carries rows as before and both values show.
    network::sync_fleet_round_with(&a, addr, &mut repair).unwrap();
    stop.set();
    let _ = server.join();
    let db = Database::open(&a).unwrap();
    let rows = scan_table(&db.view(), "computers").unwrap();
    assert!(rows["m1"].get("_conflicts").is_some(), "{}", rows["m1"]);
}

/// NEGATIVE CONTROL: with `sequence_follows` back to an addition, the view panics:
/// `` attempt to add with overflow ``.
#[test]
fn a_link_from_the_last_sequence_is_an_impossible_link_not_a_panic() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let mut a = replica(&tmp.path().join("a"));
    let mut b = replica(&tmp.path().join("b"));
    put_row(&mut a, "computers", "m1", json!({"label": "m1"})).unwrap();
    exchange(&mut a, &mut b);
    let first = *a.history.index().inventory().next().unwrap();
    let merge = Command::Merge {
        request: crypto::random::<16>().unwrap(),
    }
    .encode()
    .unwrap();
    let seal = |sequence, previous: [u8; 32]| {
        crypto::seal(
            &a.credentials.manifest,
            &a.credentials.key,
            &a.credentials.seed,
            &a.credentials.capability,
            crypto::Causal {
                sequence,
                previous: Some(previous),
                parents: &[previous],
            },
            &merge,
        )
        .unwrap()
    };
    let last = seal(u64::MAX, first);
    let after = seal(7, Sha256::digest(&last));
    b.ingest(&last).unwrap();
    b.ingest(&after).unwrap();

    let view = b.view();
    assert_eq!(scan_table(&view, "computers").unwrap().len(), 1);
    assert_eq!(view.causal.pending.len(), 2);
}

/// What a process killed part way through the load leaves, under `key`, with `loaded` written.
fn cut_short(dir: &Path, key: &[u8], loaded: impl FnOnce(&mut Database)) {
    let db = Database::open(dir).unwrap();
    let id = hex::encode_lower(&db.credentials.manifest.id[..8]);
    drop(db);
    aterm_uds::private_file::write_private(dir, REKEY_UNFINISHED, b"0").unwrap();
    let name = dir.file_name().unwrap().to_str().unwrap();
    std::fs::rename(dir, dir.with_file_name(format!("{name}.before-rekey-{id}"))).unwrap();
    local::ensure_fleet(dir, key).unwrap();
    loaded(&mut Database::open(dir).unwrap());
}

/// NEGATIVE CONTROL: with `rekey_with` going straight to the kept folder, the partial dataset
/// refuses the second key: `` this database belongs to another fleet key ``.
#[test]
fn a_key_rotated_again_after_a_load_was_cut_short_still_finishes() {
    const THIRD: &[u8] = b"3333333333333333333333333333333333333333";
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("db");
    let mut db = replica(&dir);
    put_row(
        &mut db,
        "computers",
        "m1",
        json!({"label": "m1", "notes": "old"}),
    )
    .unwrap();
    put_row(&mut db, "computers", "m2", json!({"label": "m2"})).unwrap();
    drop(db);
    cut_short(&dir, ROTATED, |db| {
        put_row(
            db,
            "computers",
            "m1",
            json!({"label": "m1", "notes": "since"}),
        )
        .unwrap();
    });

    let report = rekey_with(&dir, THIRD).expect("the second key takes both folders");
    assert_eq!(report["moved"].as_array().unwrap().len(), 2, "{report}");
    assert!(unfinished_rekey(&dir).unwrap().is_none());
    let db = Database::open(&dir).unwrap();
    let rows = scan_table(&db.view(), "computers").unwrap();
    assert_eq!(rows.keys().collect::<Vec<_>>(), ["m1", "m2"]);
    assert_eq!(rows["m1"]["notes"], json!("since"), "an edit since stands");
    drop(db);
    assert!(rekey_with(&dir, THIRD).is_err(), "nothing left to move");
}

/// The order the folders were set aside in decides, not a clock or a directory listing.
/// NEGATIVE CONTROL: sorted oldest first, the older row displaces the edit:
/// `` left: String("old") right: String("since") ``.
#[test]
fn a_rerun_moves_the_folder_set_aside_last_first() {
    const THIRD: &[u8] = b"3333333333333333333333333333333333333333";
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("db");
    let row = |notes: &str| json!({"label": "m1", "notes": notes});
    put_row(&mut replica(&dir), "computers", "m1", row("old")).unwrap();
    cut_short(&dir, ROTATED, |db| {
        put_row(db, "computers", "m1", row("since")).unwrap();
    });
    // Both folders set aside with the second killed before it loaded anything.
    let id = {
        let db = Database::open(&dir).unwrap();
        hex::encode_lower(&db.credentials.manifest.id[..8])
    };
    aterm_uds::private_file::write_private(&dir, REKEY_UNFINISHED, b"1").unwrap();
    std::fs::rename(&dir, dir.with_file_name(format!("db.before-rekey-{id}"))).unwrap();

    rekey_with(&dir, THIRD).unwrap();
    let db = Database::open(&dir).unwrap();
    let rows = scan_table(&db.view(), "computers").unwrap();
    assert_eq!(rows["m1"]["notes"], json!("since"));
}

/// NEGATIVE CONTROL: with the kept folder named by the dataset alone, the third move fails:
/// `` Directory not empty ``.
#[test]
fn a_key_used_again_keeps_each_folder_it_set_aside() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("db");
    put_row(
        &mut replica(&dir),
        "computers",
        "m1",
        json!({"label": "m1"}),
    )
    .unwrap();
    let mut kept = BTreeSet::new();
    for key in [ROTATED, SECRET, ROTATED] {
        let report = rekey_with(&dir, key).unwrap();
        kept.insert(report["moved"][0]["kept"].as_str().unwrap().to_owned());
        local::ensure_fleet(&dir, key).unwrap();
        let db = Database::open(&dir).unwrap();
        assert!(get_row(&db.view(), "computers", "m1").unwrap().is_some());
    }
    assert_eq!(kept.len(), 3, "{kept:?}");
}

/// NEGATIVE CONTROL: with `inherit_email_state` run on a key that stands, the document's
/// `protected: false` refuses the whole document:
/// `` protected real email row cannot have protected downgraded to false ``.
#[test]
fn a_mark_made_since_a_load_was_cut_short_does_not_stop_the_move() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("db");
    let key = "x@example.com";
    let mut db = replica(&dir);
    put_row(&mut db, "computers", "m1", json!({"label": "m1"})).unwrap();
    put_row(
        &mut db,
        "emails",
        key,
        json!({"email": key, "protected": false}),
    )
    .unwrap();
    drop(db);
    cut_short(&dir, ROTATED, |db| {
        put_row(db, "emails", key, json!({"email": key, "protected": true})).unwrap();
    });

    rekey_with(&dir, ROTATED).expect("the mark stands and the rest moves");
    let db = Database::open(&dir).unwrap();
    let view = db.view();
    assert_eq!(
        get_row(&view, "emails", key).unwrap().unwrap()["protected"],
        json!(true)
    );
    assert!(get_row(&view, "computers", "m1").unwrap().is_some());
    drop(db);
    assert!(rekey_with(&dir, ROTATED).is_err(), "nothing left to move");
}

/// NEGATIVE CONTROL: with `unfinished_rekeys` passing on the listing's error, every verb
/// refuses: `` Permission denied ``.
#[test]
fn a_parent_this_user_cannot_list_refuses_no_verb() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let parent = tmp.path().join("parent");
    let dir = parent.join("db");
    put_row(
        &mut replica(&dir),
        "computers",
        "m1",
        json!({"label": "m1"}),
    )
    .unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o300)).unwrap();
    let listed = unfinished_rekeys(&dir);
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(listed.unwrap().is_empty());
}

/// NEGATIVE CONTROL: with `one_writer_holds` always `Ok`, the rename comes first and the load
/// then stops on the writer's quota with nothing left to rerun it past.
#[test]
fn rows_one_writer_cannot_hold_are_refused_before_anything_moves() {
    let row = |n: usize| Change {
        collection: "computers".into(),
        key: format!("m{n}"),
        value: Some(json!({"label": format!("m{n}"), "notes": "x".repeat(50_000)})),
    };
    assert!(one_writer_holds((0..100).map(row).collect()).is_ok());
    assert!(one_writer_holds((0..180).map(row).collect()).is_err());
}

/// NEGATIVE CONTROL: with the server's opens back to `Database::open`, the server drops the
/// connection beside a neighbour: `report["ok"]` is false, naming a TLS `close_notify`.
#[test]
fn a_sync_beside_a_neighbour_of_the_server_gets_every_row() {
    let tmp = fleet_fixture();
    let secret = resolve_fleet_secret(&tmp.path().join("a")).unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    local::ensure_fleet(&a, &secret).unwrap();
    local::ensure_fleet(&b, &secret).unwrap();
    let mut peer = Database::open(&a).unwrap();
    for n in 0..5 {
        let key = format!("m{n}");
        put_row(&mut peer, "computers", &key, json!({"label": key})).unwrap();
    }
    drop(peer);

    let listener =
        aterm_uds::kept_out(|| std::net::TcpListener::bind("127.0.0.1:0")).expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let stop = std::sync::Arc::new(aterm_uds::stop::Stop::new());
    let server = {
        let (a, stop) = (a.clone(), stop.clone());
        std::thread::spawn(move || {
            network::serve_fleet_until(
                &a,
                listener,
                stop,
                std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            )
        })
    };
    let neighbour = {
        let (a, stop) = (a.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.is_set() {
                let held = Database::open(&a);
                std::thread::sleep(std::time::Duration::from_millis(2));
                drop(held);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    };
    let report = sync_peers(&b, &[addr]).unwrap();
    stop.set();
    neighbour.join().unwrap();
    let _ = server.join();

    assert_eq!(report["ok"], json!(true), "{report}");
    let db = Database::open(&b).unwrap();
    assert_eq!(scan_table(&db.view(), "computers").unwrap().len(), 5);
}

#[test]
fn a_control_record_forged_with_the_fleet_key_cannot_hide_the_rows() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let victim = tmp.path().join("victim");
    let mut db = replica(&victim);
    put_row(&mut db, "computers", "m1", json!({"label": "m1"})).unwrap();
    // Two control records from one stream fork it, which withdraws every row.
    let forged: Vec<Vec<u8>> = ["forger-a", "forger-b"]
        .into_iter()
        .map(|name| {
            let forger = tmp.path().join(name);
            local::test_forge_authority(&forger, SECRET).unwrap();
            let mut forger = Database::open(&forger).unwrap();
            let actor = forger.credentials.capability.actor;
            let id = forger
                .commit(Command::Budget {
                    request: crypto::random::<16>().unwrap(),
                    name: name.to_owned(),
                    total: 1,
                    shares: BTreeMap::from([(actor, 1)]),
                })
                .unwrap();
            forger.history.wire(&id).unwrap().to_vec()
        })
        .collect();

    for wire in &forged {
        db.ingest(wire)
            .expect("a replica keeps it as inert evidence");
    }
    let summary = tables_summary(&db).unwrap();
    assert_eq!(summary["authority_fault"], json!(false));
    assert_eq!(summary["tables"]["computers"], json!(1));
    drop(db);

    // Without the mark they wipe the view, which is what the mark prevents ...
    local::test_forget_no_authority(&victim).unwrap();
    let mut unmarked = Database::open(&victim).unwrap();
    for wire in &forged {
        unmarked.ingest(wire).unwrap();
    }
    assert!(unmarked.view().authority_fault);
    assert!(
        scan_table(&unmarked.view(), "computers")
            .unwrap()
            .is_empty()
    );
    drop(unmarked);

    // ... and a replica that kept such records recovers its rows on the next open.
    let healed = replica(&victim);
    let summary = tables_summary(&healed).unwrap();
    assert_eq!(summary["authority_fault"], json!(false));
    assert_eq!(summary["tables"]["computers"], json!(1));
}

#[test]
fn a_concurrent_delete_never_hides_an_edit_and_is_reported_until_settled() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let mut a = replica(&tmp.path().join("a"));
    let mut b = replica(&tmp.path().join("b"));
    put_row(
        &mut a,
        "computers",
        "m1",
        json!({"label": "m1", "notes": "old"}),
    )
    .unwrap();
    exchange(&mut a, &mut b);

    put_row(
        &mut a,
        "computers",
        "m1",
        json!({"label": "m1", "notes": "edit"}),
    )
    .unwrap();
    delete_row(&mut b, "computers", "m1").unwrap();
    exchange(&mut a, &mut b);

    for db in [&a, &b] {
        let row = get_row(&db.view(), "computers", "m1")
            .unwrap()
            .expect("the edit shows");
        assert_eq!(row["notes"], json!("edit"));
        assert_eq!(row["_conflicts"], json!([{"_deleted": true}]));
        let summary = tables_summary(db).unwrap();
        assert_eq!(summary["conflicts"], json!(1));
        assert_eq!(summary["ok"], json!(false));
    }

    delete_row(&mut a, "computers", "m1").unwrap();
    exchange(&mut a, &mut b);
    for db in [&a, &b] {
        assert!(get_row(&db.view(), "computers", "m1").unwrap().is_none());
        assert_eq!(ok_of(db), json!(true));
    }
}

#[test]
fn two_machines_writing_the_same_value_do_not_conflict() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let mut a = replica(&tmp.path().join("a"));
    let mut b = replica(&tmp.path().join("b"));
    let row = json!({"label": "m1", "notes": "same"});
    put_row(&mut a, "computers", "m1", row.clone()).unwrap();
    put_row(&mut b, "computers", "m1", row).unwrap();
    exchange(&mut a, &mut b);

    let seen = get_row(&a.view(), "computers", "m1").unwrap().unwrap();
    assert!(seen.get("_conflicts").is_none());
    assert_eq!(ok_of(&a), json!(true));
}

#[test]
fn an_alias_reads_but_never_changes_a_row() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let mut db = replica(&tmp.path().join("db"));
    put_row(&mut db, "computers", "m2", json!({"label": "m2"})).unwrap();
    put_row(
        &mut db,
        "computers",
        "m1",
        json!({"label": "m1", "nickname": "desk", "aliases": ["m2"]}),
    )
    .unwrap();
    assert!(get_row(&db.view(), "computers", "desk").unwrap().is_some());

    assert!(delete_row(&mut db, "computers", "desk").is_err());
    delete_row(&mut db, "computers", "m2").unwrap();
    // `m2` now only means m1's alias; deleting it again must not take m1 with it.
    assert!(delete_row(&mut db, "computers", "m2").is_err());
    assert!(put_row(&mut db, "computers", "desk", json!({})).is_err());

    let rows = scan_table(&db.view(), "computers").unwrap();
    assert_eq!(rows.keys().collect::<Vec<_>>(), ["m1"]);
}

#[test]
fn a_put_without_a_password_keeps_it_and_null_clears_it() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let mut db = replica(&tmp.path().join("db"));
    let key = "root@example.com";
    put_row(
        &mut db,
        "emails",
        key,
        json!({"email": key, "password": "pw"}),
    )
    .unwrap();

    put_row(&mut db, "emails", key, json!({"email": key, "notes": "n"})).unwrap();
    let row = get_row(&db.view(), "emails", key).unwrap().unwrap();
    assert_eq!(row["password"], json!("pw"));
    assert_eq!(row["notes"], json!("n"));

    // Protecting a row drops its password rather than failing on the kept one.
    put_row(
        &mut db,
        "emails",
        key,
        json!({"email": key, "protected": true}),
    )
    .unwrap();
    let row = get_row(&db.view(), "emails", key).unwrap().unwrap();
    assert_eq!(row["password"], Value::Null);

    let other = "other@example.com";
    put_row(
        &mut db,
        "emails",
        other,
        json!({"email": other, "password": "pw"}),
    )
    .unwrap();
    put_row(
        &mut db,
        "emails",
        other,
        json!({"email": other, "password": null}),
    )
    .unwrap();
    let row = get_row(&db.view(), "emails", other).unwrap().unwrap();
    assert_eq!(row["password"], Value::Null);
}

#[test]
fn a_redacted_dump_cannot_overwrite_a_password() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let mut db = replica(&tmp.path().join("db"));
    let key = "root@example.com";
    put_row(
        &mut db,
        "emails",
        key,
        json!({"email": key, "password": "pw"}),
    )
    .unwrap();

    let redacted = dump_all(&db, false).unwrap();
    assert_eq!(redacted["emails"][0]["password"], json!(REDACTED));
    assert!(load_all(&mut db, &redacted).is_err());
    let refused = put_row(&mut db, "emails", key, redacted["emails"][0].clone());
    assert!(refused.is_err());

    let row = get_row(&db.view(), "emails", key).unwrap().unwrap();
    assert_eq!(row["password"], json!("pw"));
    assert!(row.get("has_password").is_none());
}

#[test]
fn loading_a_dump_again_writes_nothing_and_only_differences_are_written() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let mut db = replica(&tmp.path().join("db"));
    put_row(
        &mut db,
        "computers",
        "m1",
        json!({"label": "m1", "notes": "a"}),
    )
    .unwrap();
    put_row(&mut db, "computers", "m2", json!({"label": "m2"})).unwrap();
    let dump = dump_all(&db, true).unwrap();

    let before = db.history.index().inventory().count();
    let report = load_all(&mut db, &dump).unwrap();
    assert_eq!(report["operations_committed"], json!(0));
    assert_eq!(report["unchanged"], json!(2));
    assert_eq!(db.history.index().inventory().count(), before);

    put_row(
        &mut db,
        "computers",
        "m1",
        json!({"label": "m1", "notes": "b"}),
    )
    .unwrap();
    let report = load_all(&mut db, &dump).unwrap();
    assert_eq!(report["imported"]["computers"], json!(1));
    let row = get_row(&db.view(), "computers", "m1").unwrap().unwrap();
    assert_eq!(row["notes"], json!("a"));
}

#[test]
fn a_stored_redaction_mark_reads_as_a_word_and_blocks_nothing() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("db");
    let mut db = replica(&dir);
    let key = "root@example.com";
    let store_mark = |db: &mut Database| {
        db.commit(Command::Write {
            request: crypto::random::<16>().unwrap(),
            changes: vec![Change {
                collection: "emails".into(),
                key: storage_key("emails", key).unwrap(),
                value: Some(json!({"email": key, "protected": false, "password": REDACTED})),
            }],
        })
        .unwrap();
    };

    store_mark(&mut db);
    let row = get_row(&db.view(), "emails", key).unwrap().unwrap();
    assert_eq!(row["password"], json!(HAS_PASSWORD));
    put_row(&mut db, "emails", key, json!({"email": key, "notes": "n"}))
        .expect("a put that omits the password");

    store_mark(&mut db);
    let dump = dump_all(&db, true).unwrap();
    load_all(&mut db, &dump).expect("its own dump loads");

    store_mark(&mut db);
    drop(db);
    rekey_with(&dir, ROTATED).expect("the key can be rotated away from it");
}

#[test]
fn a_first_import_leaves_edits_and_deletions_alone() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let mut db = replica(&tmp.path().join("db"));
    let doc = json!({"computers": [
        {"label": "m1", "notes": "csv"},
        {"label": "m2", "notes": "csv"},
        {"label": "m3", "notes": "csv"},
    ]});
    import_rows(&mut db, &doc, Existing::Keep).unwrap();
    put_row(
        &mut db,
        "computers",
        "m1",
        json!({"label": "m1", "notes": "edit"}),
    )
    .unwrap();
    delete_row(&mut db, "computers", "m2").unwrap();

    let report = import_rows(&mut db, &doc, Existing::Keep).unwrap();
    assert_eq!(report["operations_committed"], json!(0));
    let rows = scan_table(&db.view(), "computers").unwrap();
    assert_eq!(rows.keys().collect::<Vec<_>>(), ["m1", "m3"]);
    assert_eq!(rows["m1"]["notes"], json!("edit"));
}

#[test]
fn bootstrap_never_stores_a_password_flag_and_a_second_run_writes_nothing() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let sheets = tmp.path().join("sheets");
    std::fs::create_dir(&sheets).unwrap();
    for (name, text) in [
        (
            "alab-phones.csv",
            "phone\n555-234-5678,carrier,mobile,dev,note\n",
        ),
        (
            "alab-computers.csv",
            "label\nm1,Apple,arm64,macOS,M4,128GB,1TB,,desk,,home,note\n",
        ),
        (
            "alab-emails.csv",
            "email\nreal@example.com,,pw-synth,active,proton,,,\nflag@example.com,,1,active,proton,,,\n",
        ),
        ("ai-accounts.csv", "m\n,real@example.com,m1,,1,,,,,,note\n"),
    ] {
        std::fs::write(sheets.join(name), text).unwrap();
    }
    let mut db = replica(&tmp.path().join("db"));

    let first = bootstrap_from_secrets(&mut db, &sheets).unwrap();
    assert_eq!(first["imported"]["emails"], json!(2));
    let view = db.view();
    let password = |key: &str| get_row(&view, "emails", key).unwrap().unwrap()["password"].clone();
    assert_eq!(password("real@example.com"), json!("pw-synth"));
    assert_eq!(password("flag@example.com"), Value::Null);

    let again = bootstrap_from_secrets(&mut db, &sheets).unwrap();
    assert_eq!(again["operations_committed"], json!(0));

    // A key that stands is left alone, so a mark made since does not refuse the sheet.
    put_row(
        &mut db,
        "emails",
        "flag@example.com",
        json!({"email": "flag@example.com", "protected": true}),
    )
    .unwrap();
    let again = bootstrap_from_secrets(&mut db, &sheets).expect("the mark stands");
    assert_eq!(again["operations_committed"], json!(0));
}

#[test]
fn bootstrap_reads_real_passwords_from_xlsx_when_csv_holds_flags() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let sheets = tmp.path().join("sheets");
    std::fs::create_dir(&sheets).unwrap();
    for (name, text) in [
        (
            "alab-phones.csv",
            "phone\n555-234-5678,carrier,mobile,dev,note\n",
        ),
        (
            "alab-computers.csv",
            "label\nm1,Apple,arm64,macOS,M4,128GB,1TB,,desk,,home,note\n",
        ),
        (
            "alab-emails.csv",
            "email\nflag@example.com,,1,active,proton,,,\nprot@example.com,,1,active,proton,,1,\nalias@example.com,flag@example.com,1,active,proton,,,\n",
        ),
        ("ai-accounts.csv", "m\n,flag@example.com,m1,,1,,,,,,note\n"),
    ] {
        std::fs::write(sheets.join(name), text).unwrap();
    }
    let parts: &[(&str, &str)] = &[
        (
            "xl/workbook.xml",
            r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Emails" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
        ),
        (
            "xl/worksheets/sheet1.xml",
            r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Email Address</t></is></c><c r="C1" t="inlineStr"><is><t>Password</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>flag@example.com</t></is></c><c r="C2" t="inlineStr"><is><t>xlsx-real-pw-42</t></is></c></row><row r="3"><c r="A3" t="inlineStr"><is><t>prot@example.com</t></is></c><c r="C3" t="inlineStr"><is><t>xlsx-prot-pw-99</t></is></c></row><row r="4"><c r="A4" t="inlineStr"><is><t>alias@example.com</t></is></c><c r="C4" t="inlineStr"><is><t>xlsx-alias-pw-99</t></is></c></row></sheetData></worksheet>"#,
        ),
    ];
    let mut zip_bytes = Vec::new();
    let mut central = Vec::new();
    for (name, body_str) in parts {
        let body = body_str.as_bytes();
        let offset = zip_bytes.len() as u32;
        let crc = aterm_codec::crc32::Crc32::of(body);
        let len = body.len() as u32;
        let nlen = name.len() as u16;
        zip_bytes.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        zip_bytes.extend_from_slice(&20u16.to_le_bytes());
        zip_bytes.extend_from_slice(&0u16.to_le_bytes());
        zip_bytes.extend_from_slice(&0u16.to_le_bytes());
        zip_bytes.extend_from_slice(&0u32.to_le_bytes());
        zip_bytes.extend_from_slice(&crc.to_le_bytes());
        zip_bytes.extend_from_slice(&len.to_le_bytes());
        zip_bytes.extend_from_slice(&len.to_le_bytes());
        zip_bytes.extend_from_slice(&nlen.to_le_bytes());
        zip_bytes.extend_from_slice(&0u16.to_le_bytes());
        zip_bytes.extend_from_slice(name.as_bytes());
        zip_bytes.extend_from_slice(body);

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&len.to_le_bytes());
        central.extend_from_slice(&len.to_le_bytes());
        central.extend_from_slice(&nlen.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd_offset = zip_bytes.len() as u32;
    let cd_len = central.len() as u32;
    let count = parts.len() as u16;
    zip_bytes.extend_from_slice(&central);
    zip_bytes.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    zip_bytes.extend_from_slice(&0u16.to_le_bytes());
    zip_bytes.extend_from_slice(&0u16.to_le_bytes());
    zip_bytes.extend_from_slice(&count.to_le_bytes());
    zip_bytes.extend_from_slice(&count.to_le_bytes());
    zip_bytes.extend_from_slice(&cd_len.to_le_bytes());
    zip_bytes.extend_from_slice(&cd_offset.to_le_bytes());
    zip_bytes.extend_from_slice(&0u16.to_le_bytes());
    std::fs::write(sheets.join("AI Email Accounting.xlsx"), zip_bytes).unwrap();

    let mut db = replica(&tmp.path().join("db"));
    bootstrap_from_secrets(&mut db, &sheets).unwrap();
    let view = db.view();
    let password = |key: &str| get_row(&view, "emails", key).unwrap().unwrap()["password"].clone();
    assert_eq!(password("flag@example.com"), json!("xlsx-real-pw-42"));
    assert_eq!(password("prot@example.com"), Value::Null);
    assert_eq!(password("alias@example.com"), Value::Null);
}

#[test]
fn a_snapshot_with_a_misspelled_table_is_refused_rather_than_ignored() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let mut db = replica(&tmp.path().join("db"));
    assert!(load_all(&mut db, &json!({"ai-accounts": []})).is_err());
    assert!(load_all(&mut db, &json!({"computers": {}})).is_err());
}

#[test]
fn concurrent_commands_wait_their_turn() {
    let tmp = fleet_fixture();
    let dir = tmp.path().join("db");
    put_row(
        &mut open_at(&dir).unwrap(),
        "computers",
        "m1",
        json!({"label": "m1"}),
    )
    .unwrap();
    let readers: Vec<_> = (0..12)
        .map(|_| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                let db = open_at(&dir)?;
                tables_summary(&db)
            })
        })
        .collect();
    for reader in readers {
        let summary = reader.join().unwrap().expect("each reader gets its turn");
        assert_eq!(summary["tables"]["computers"], json!(1));
    }
}

#[test]
fn a_peer_that_cannot_be_reached_fails_the_sync() {
    let tmp = fleet_fixture();
    let gone = aterm_uds::kept_out(|| std::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap()
        .local_addr()
        .unwrap();
    let report = sync_peers(&tmp.path().join("db"), &[gone]).unwrap();
    assert_eq!(report["ok"], json!(false));
    let peer = &report["peers"][0];
    assert_eq!(peer["ok"], json!(false));
    assert_eq!(peer["caught_up"], json!(false));
    assert!(peer["error"].as_str().is_some());
    assert_eq!(peer["sent"], json!(0));
}

#[test]
fn tables_reports_how_much_of_each_quota_is_used() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let mut db = replica(&tmp.path().join("db"));
    put_row(&mut db, "computers", "m1", json!({"label": "m1"})).unwrap();
    let capacity = tables_summary(&db).unwrap()["capacity"].clone();
    assert_eq!(capacity["operations"], json!(1));
    assert_eq!(capacity["writer_operations"], json!(1));
    assert_eq!(
        capacity["writer_operations_limit"],
        json!(MAX_WRITER_OPERATIONS as u64)
    );
    assert!(capacity["writer_bytes"].as_u64().unwrap() > 0);
    assert!(capacity["journal_bytes"].as_u64().unwrap() > 0);
}

#[test]
fn fleet_sync_requires_a_settled_inventory_round() {
    let peer = "127.0.0.1:7485".parse().unwrap();
    let mut calls = 0;
    let result = sync_peer_with(peer, |_| {
        calls += 1;
        Ok(if calls == 1 {
            network::Report {
                sent: 3,
                received: 2,
                deferred: 0,
            }
        } else {
            network::Report::default()
        })
    });
    assert_eq!(calls, 2);
    assert_eq!(result["ok"], json!(true));
    assert_eq!(result["caught_up"], json!(true));
    assert_eq!(result["sent"], json!(3));
    assert_eq!(result["received"], json!(2));
    assert!(result.get("error").is_none());

    for (report, expected_calls) in [
        (
            network::Report {
                sent: 0,
                received: 0,
                deferred: 1,
            },
            1,
        ),
        (
            network::Report {
                sent: 1,
                received: 0,
                deferred: 0,
            },
            80,
        ),
    ] {
        let mut calls = 0;
        let result = sync_peer_with(peer, |_| {
            calls += 1;
            Ok(report)
        });
        assert_eq!(calls, expected_calls);
        assert_eq!(result["ok"], json!(false));
        assert_eq!(result["caught_up"], json!(false));
        assert_eq!(result["sent"], json!(report.sent * calls));
        assert_eq!(result["deferred"], json!(report.deferred));
        assert!(result.get("error").and_then(Value::as_str).is_some());
    }
}

#[test]
fn a_sync_beside_a_neighbour_gets_every_row() {
    let tmp = fleet_fixture();
    let secret = resolve_fleet_secret(&tmp.path().join("a")).unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    local::ensure_fleet(&a, &secret).unwrap();
    local::ensure_fleet(&b, &secret).unwrap();
    let mut peer = Database::open(&a).unwrap();
    for n in 0..20 {
        let key = format!("m{n}");
        put_row(&mut peer, "computers", &key, json!({"label": key})).unwrap();
    }
    drop(peer);

    let listener =
        aterm_uds::kept_out(|| std::net::TcpListener::bind("127.0.0.1:0")).expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let stop = std::sync::Arc::new(aterm_uds::stop::Stop::new());
    let server = {
        let (a, stop) = (a.clone(), stop.clone());
        std::thread::spawn(move || {
            network::serve_fleet_until(
                &a,
                listener,
                stop,
                std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            )
        })
    };
    let neighbour = {
        let (b, stop) = (b.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.is_set() {
                let held = Database::open(&b);
                std::thread::sleep(std::time::Duration::from_millis(2));
                drop(held);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    };
    let report = sync_peers(&b, &[addr]).unwrap();
    stop.set();
    neighbour.join().unwrap();
    let _ = server.join();

    assert_eq!(report["ok"], json!(true), "{report}");
    let db = Database::open(&b).unwrap();
    assert_eq!(scan_table(&db.view(), "computers").unwrap().len(), 20);
}

#[test]
fn what_a_peer_holds_and_this_replica_does_not_keeps_a_sync_from_being_caught_up() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let mut a = replica(&tmp.path().join("a"));
    put_row(&mut a, "computers", "m1", json!({"label": "m1"})).unwrap();
    let held: BTreeSet<_> = a.history.index().inventory().copied().collect();
    let bdir = tmp.path().join("b");
    let b = replica(&bdir);
    drop(a);
    drop(b);
    assert_eq!(network::unretained(&bdir, &held).unwrap(), held.len());

    let adir = tmp.path().join("a");
    let (mut a, mut b) = (
        Database::open(&adir).unwrap(),
        Database::open(&bdir).unwrap(),
    );
    exchange(&mut a, &mut b);
    drop(b);
    assert_eq!(network::unretained(&bdir, &held).unwrap(), 0);
}

#[test]
fn tables_is_not_ok_while_a_row_waits_for_an_operation_it_depends_on() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let mut a = replica(&tmp.path().join("a"));
    put_row(&mut a, "computers", "m1", json!({"label": "m1"})).unwrap();
    put_row(&mut a, "computers", "m2", json!({"label": "m2"})).unwrap();
    let later = a
        .history
        .index()
        .inventory()
        .find(|id| {
            a.history
                .index()
                .get(id)
                .is_some_and(|header| header.sequence == 2)
        })
        .map(|id| a.history.wire(id).unwrap().to_vec())
        .unwrap();

    let mut b = replica(&tmp.path().join("b"));
    b.ingest(&later).unwrap();
    let summary = tables_summary(&b).unwrap();
    assert_eq!(summary["pending_operations"], json!(1));
    assert_eq!(summary["ok"], json!(false));
}

#[test]
fn fleet_sync_keeps_partial_progress_on_failure() {
    let mut calls = 0;
    let result = sync_peer_with("127.0.0.1:7485".parse().unwrap(), |_| {
        calls += 1;
        if calls == 1 {
            Ok(network::Report {
                sent: 2,
                received: 3,
                deferred: 1,
            })
        } else {
            Err(Error::new("test peer disconnected"))
        }
    });
    assert_eq!(calls, 2);
    assert_eq!(result["ok"], json!(false));
    assert_eq!(result["caught_up"], json!(false));
    assert_eq!(result["sent"], json!(2));
    assert_eq!(result["received"], json!(3));
    assert_eq!(result["deferred"], json!(1));
    assert_eq!(result["error"], json!("test peer disconnected"));

    let mut calls = 0;
    let peer = "127.0.0.1:7485".parse().unwrap();
    let refused = sync_peer_with(peer, |_| {
        calls += 1;
        Err(Error::from(std::io::Error::from_raw_os_error(65)))
    });
    assert_eq!(
        calls, 1,
        "a failed round (no route to the host) is not retried"
    );
    assert_eq!(refused["ok"], json!(false));
    assert_eq!(refused["caught_up"], json!(false));
    assert!(refused["error"].as_str().unwrap().contains("os error 65"));

    let root = crate::tests::tmp();
    let absent = root.path().join("no-peer");
    assert!(sync_peers(&absent, &[]).is_err());
    assert!(!absent.exists(), "an empty sync must not create a database");
}

fn fleet_fixture() -> aterm_tempfile::TempDir {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let key = hex::encode_lower(&crypto::random::<32>().unwrap());
    aterm_uds::private_file::write_private(tmp.path(), "fleet.key", key.as_bytes()).unwrap();
    tmp
}

#[test]
fn missing_fleet_key_does_not_create_an_enrollment() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let absent = tmp.path().join("absent");
    assert!(read_fleet_secret(std::slice::from_ref(&absent)).is_err());
    assert!(!absent.exists());
    assert!(!tmp.path().join("fleet.key").exists());
    let empty = tmp.path().join("ordinary-folder");
    std::fs::create_dir(&empty).unwrap();
    std::fs::set_permissions(&empty, std::fs::Permissions::from_mode(0o755)).unwrap();
    let provisioned = fleet_fixture();
    let before = std::fs::read(provisioned.path().join("fleet.key")).unwrap();
    assert_eq!(
        read_fleet_secret(&[empty.clone(), provisioned.path().to_owned()]).unwrap(),
        before
    );
    assert!(!empty.join("fleet.key").exists());
}

#[test]
fn invalid_fleet_key_cannot_fall_through_to_another_fleet() {
    use std::os::unix::fs::PermissionsExt as _;
    let first = fleet_fixture();
    let other = fleet_fixture();
    let candidates = [first.path().to_owned(), other.path().to_owned()];
    let key = first.path().join("fleet.key");
    let before = std::fs::read(&key).unwrap();
    assert_eq!(read_fleet_secret(&candidates).unwrap(), before);
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_fleet_secret(&candidates).is_err());
    assert_eq!(std::fs::read(&key).unwrap(), before);
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    aterm_uds::private_file::write_private(first.path(), "fleet.key", b"invalid").unwrap();
    assert!(read_fleet_secret(&candidates).is_err());
    assert_eq!(std::fs::read(key).unwrap(), b"invalid");
}

#[test]
fn fleet_four_tables_crud_and_peer_sync() {
    let tmp = fleet_fixture();
    let node_a = tmp.path().join("node-a");
    let node_b = tmp.path().join("node-b");

    let mut db_a = open_at(&node_a).expect("open node_a");
    // Replicas must never retain `root_seed` (avoiding control-stream forks).
    assert!(!db_a.credentials.has_control_authority());

    put_row(
        &mut db_a,
        "computers",
        "m104",
        json!({
            "label": "m104",
            "hostname": "m104.local",
            "ip": "127.0.0.1",
            "nickname": "desk-max",
            "aliases": ["m104-alias"],
            "chip": "M4 Max",
            "memory_gb": 128,
            "status": "active",
            "notes": "Desk workstation with multiline\nnote"
        }),
    )
    .expect("put computer");

    put_row(
        &mut db_a,
        "phones",
        "+1 (555) 234-5678",
        json!({
            "phone": "+1 (555) 234-5678",
            "label": "phone-1",
            "owner_carrier": "AT&T",
            "status": "active"
        }),
    )
    .expect("put phone");

    // Phone lookup normalizes formatting variants to 555-234-5678.
    let ph_row = get_row(&db_a.view(), "phones", "555.234.5678")
        .expect("get phone")
        .expect("phone present");
    assert_eq!(ph_row["phone"], json!("555-234-5678"));

    put_row(
        &mut db_a,
        "emails",
        "root-m2@example.com",
        json!({
            "email": "root-m2@example.com",
            "provider": "proton",
            "account_type": "root",
            "password": "test-root-password-123",
            "phone": "555-234-5678",
            "protected": false,
            "assigned_computers": ["m104"],
            "notes": "Catch-all Proton root"
        }),
    )
    .expect("put root email");

    put_row(
        &mut db_a,
        "emails",
        "alias-m2@example.com",
        json!({
            "email": "alias-m2@example.com",
            "provider": "proton",
            "account_type": "alias",
            "root_account": "root-m2@example.com",
            "protected": false
        }),
    )
    .expect("put alias email");

    // Protected email must reject storing a password both via `put_row` and raw `Command::decode`.
    assert!(
        put_row(
            &mut db_a,
            "emails",
            "personal-protected@example.com",
            json!({
                "email": "personal-protected@example.com",
                "provider": "google",
                "account_type": "root",
                "password": "should-fail",
                "protected": true
            }),
        )
        .is_err()
    );
    let forged_skey = storage_key("emails", "personal-protected@example.com").unwrap();
    assert!(
        validate_email_record(
            &forged_skey,
            &json!({
                "email": "personal-protected@example.com",
                "protected": true,
                "password": "forged-wire-password"
            })
        )
        .is_err()
    );
    assert!(
        validate_email_record(
            &forged_skey,
            &json!({
                "_deleted": true,
                "password": "smuggled-in-tombstone"
            })
        )
        .is_err()
    );

    put_row(
        &mut db_a,
        "emails",
        "personal-protected@example.com",
        json!({
            "email": "personal-protected@example.com",
            "provider": "google",
            "account_type": "root",
            "protected": true
        }),
    )
    .expect("put protected email without password");

    // Once `protected: true`, attempting to downgrade `protected` to `false` or attach a password fails.
    assert!(
        put_row(
            &mut db_a,
            "emails",
            "personal-protected@example.com",
            json!({
                "email": "personal-protected@example.com",
                "provider": "google",
                "account_type": "root",
                "protected": false,
                "password": "downgrade-attempt"
            }),
        )
        .is_err()
    );

    put_row(
        &mut db_a,
        "ai_accounts",
        "acc-001",
        json!({
            "id": "acc-001",
            "provider": "anthropic",
            "service": "claude",
            "email": "alias-m2@example.com",
            "receiving_mailbox": "root-m2@example.com",
            "assigned_computers": ["m104"],
            "status": "active"
        }),
    )
    .expect("put ai_account");

    // Redaction hides password by default and reveals when requested.
    let raw_email = get_row(&db_a.view(), "emails", "root-m2@example.com")
        .unwrap()
        .unwrap();
    let redacted = redact_row("emails", raw_email.clone(), false);
    assert_eq!(redacted["password"], json!("<redacted>"));
    assert_eq!(redacted["has_password"], json!(true));
    let revealed = redact_row("emails", raw_email, true);
    assert_eq!(revealed["password"], json!("test-root-password-123"));

    // Deleting a non-existent row must error.
    assert!(delete_row(&mut db_a, "computers", "nonexistent-host").is_err());

    let summary = tables_summary(&db_a).expect("summary");
    assert_eq!(summary["ok"], json!(true));
    assert_eq!(summary["authority_fault"], json!(false));
    assert_eq!(summary["conflicts"], json!(0));
    assert_eq!(summary["tables"]["computers"], json!(1));
    assert_eq!(summary["tables"]["phones"], json!(1));
    assert_eq!(summary["tables"]["emails"]["total"], json!(3));
    assert_eq!(summary["tables"]["emails"]["root_with_password"], json!(1));
    assert_eq!(summary["tables"]["emails"]["aliases"], json!(1));
    assert_eq!(
        summary["tables"]["emails"]["protected_no_password"],
        json!(1)
    );
    assert_eq!(summary["tables"]["ai_accounts"], json!(1));

    // Replicate from node_a to fresh replica node_b with zero invitations or Commons.
    let mut db_b = open_at(&node_b).expect("open node_b");
    for id in db_a.history.index().inventory() {
        let wire = db_a.history.wire(id).expect("wire").to_vec();
        db_b.ingest(&wire).expect("ingest on node_b");
    }
    drop(db_a);

    let view_b = db_b.view();
    let email_row = get_row(&view_b, "emails", "root-m2@example.com")
        .expect("get_row")
        .expect("present");
    assert_eq!(email_row["password"], json!("test-root-password-123"));
    let comp_row = get_row(&view_b, "computers", "desk-max")
        .expect("get_row by nickname")
        .expect("present");
    assert_eq!(comp_row["label"], json!("m104"));
    assert_eq!(
        comp_row["notes"],
        json!("Desk workstation with multiline\nnote")
    );
    drop(db_b);

    // Also verify live TLS anti-entropy sync over aterm-net (FleetOp::Database).
    let node_c = tmp.path().join("node-c");
    ensure_fleet_at(&node_c).expect("ensure_fleet node_c");
    let listener =
        aterm_uds::kept_out(|| std::net::TcpListener::bind("127.0.0.1:0")).expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let stop = std::sync::Arc::new(aterm_uds::stop::Stop::new());
    let stop_srv = stop.clone();
    let node_b_srv = node_b.clone();
    let srv = std::thread::spawn(move || {
        network::serve_fleet_until(
            &node_b_srv,
            listener,
            stop_srv,
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        )
    });
    let sync_res = sync_peers(&node_c, &[addr]).expect("sync_peers over TLS");
    let node_d = tmp.path().join("node-d");
    ensure_fleet_at(&node_d).expect("ensure_fleet node_d");
    let delegated = crate::cli::run(&[
        "sync".to_owned(),
        "--dir".to_owned(),
        node_d.to_string_lossy().into_owned(),
        "--peer".to_owned(),
        addr.to_string(),
    ])
    .expect("explicit-directory sync over TLS");
    assert_eq!(delegated["ok"], json!(true));
    assert_eq!(delegated["caught_up"], json!(true));
    stop.set();
    let _ = srv.join();
    assert_eq!(sync_res["ok"], json!(true));
    let db_c = open_at(&node_c).expect("open node_c");
    let summary_c = tables_summary(&db_c).expect("summary_c");
    assert_eq!(summary_c["ok"], json!(true));
    assert_eq!(summary_c["tables"]["emails"]["total"], json!(3));
    let db_d = open_at(&node_d).expect("open delegated replica");
    assert_eq!(
        db_d.history
            .index()
            .inventory()
            .copied()
            .collect::<BTreeSet<_>>(),
        db_c.history
            .index()
            .inventory()
            .copied()
            .collect::<BTreeSet<_>>(),
    );
}

#[test]
fn fleet_ambiguous_alias_and_broken_fk_reporting() {
    let tmp = fleet_fixture();
    let dir = tmp.path().join("node");
    let mut db = open_at(&dir).expect("open");

    put_row(
        &mut db,
        "computers",
        "m1",
        json!({"label": "m1", "aliases": ["shared-alias"]}),
    )
    .unwrap();
    put_row(
        &mut db,
        "computers",
        "m2",
        json!({"label": "m2", "aliases": ["shared-alias"]}),
    )
    .unwrap();

    // Ambiguous alias must return an error rather than picking an arbitrary computer.
    assert!(get_row(&db.view(), "computers", "shared-alias").is_err());

    // Broken foreign key in ai_accounts must set ok = false in tables_summary.
    put_row(
        &mut db,
        "ai_accounts",
        "acc-broken",
        json!({
            "id": "acc-broken",
            "email": "missing@example.com",
            "assigned_computers": ["missing-comp"]
        }),
    )
    .unwrap();
    let summary = tables_summary(&db).unwrap();
    assert_eq!(summary["ok"], json!(false));
    assert!(
        !summary["integrity"]["broken_account_emails"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !summary["integrity"]["broken_computer_refs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

/// `aterm db load` reads the snapshot a user names, and that may be a stream: a pipe named by
/// `/dev/fd/N` (`aterm db load <(…)`, `/dev/stdin`) loads like a file does, while a directory
/// is still refused.
/// NEGATIVE CONTROL: with the `load` arm back on `atomic_file::read_regular`, the pipe is
/// refused: `` called `Result::unwrap()` on an `Err` value: Error("not a regular file") ``.
#[cfg(unix)]
#[test]
fn load_reads_a_snapshot_from_a_pipe_and_refuses_a_directory() {
    use std::io::Write as _;
    use std::os::fd::AsRawFd as _;
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    aterm_uds::private_file::write_private(tmp.path(), "fleet.key", SECRET).unwrap();
    let dir = tmp.path().join("db");
    drop(replica(&dir));
    let fleet = |list: &[&str]| {
        let dir = dir.clone();
        let words: Vec<String> = list.iter().map(|w| (*w).to_owned()).collect();
        try_run_with(&words, move || Ok(dir))
    };
    let (reader, mut writer) = aterm_uds::pipe().expect("pipe");
    let doc = br#"{"computers": [{"label": "m1", "notes": "from a pipe"}]}"#;
    let feeder = std::thread::spawn(move || writer.write_all(doc));
    let named = format!("/dev/fd/{}", reader.as_raw_fd());
    let report = fleet(&["load", &named]).unwrap().unwrap();
    feeder.join().unwrap().unwrap();
    drop(reader);
    assert_eq!(report["operations_committed"], json!(1), "{report}");
    let db = Database::open(&dir).expect("open");
    let shown = dump_all(&db, true).unwrap();
    assert_eq!(
        shown["computers"][0]["notes"],
        json!("from a pipe"),
        "{shown}"
    );
    assert!(fleet(&["load", tmp.path().to_str().unwrap()]).is_err());
}
